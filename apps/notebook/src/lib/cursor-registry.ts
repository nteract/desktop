/**
 * Cursor registry — connects presence events from the frame bus to
 * CodeMirror EditorViews via direct StateEffect dispatch.
 *
 * This is the hot path for remote cursor rendering. No React involvement —
 * presence events arrive synchronously from the frame bus and are dispatched
 * as CodeMirror StateEffects to registered EditorViews.
 *
 * Flow:
 *   frame bus emitPresence() → subscribePresence callback
 *     → group cursors/selections by cell_id
 *       → setRemoteCursors(view, ...) / setRemoteSelections(view, ...)
 */

import type { EditorView } from "@codemirror/view";
import {
  peerColor,
  type RemoteCursorState,
  type RemoteSelectionState,
  setRemoteCursors,
  setRemoteSelections,
} from "@/components/editor/remote-cursors";
import { logger } from "./logger";
import { subscribePresence } from "./notebook-frame-bus";

// ── Types (presence message shapes from WASM decode) ─────────────────

interface CursorData {
  cell_id: string;
  line: number;
  column: number;
}

interface SelectionData {
  cell_id: string;
  anchor_line: number;
  anchor_col: number;
  head_line: number;
  head_col: number;
}

interface ChannelEntry {
  channel: "cursor" | "selection" | "focus" | "kernel_state" | "custom";
  data: unknown;
}

interface PresenceUpdate {
  type: "update";
  peer_id: string;
  peer_label?: string;
  channel: string;
  data: unknown;
}

interface PresenceSnapshot {
  type: "snapshot";
  peer_id: string;
  peers: Array<{
    peer_id: string;
    peer_label: string;
    channels: ChannelEntry[];
  }>;
}

interface PresenceLeft {
  type: "left";
  peer_id: string;
}

interface PresenceHeartbeat {
  type: "heartbeat";
  peer_id: string;
}

interface PresenceClearChannel {
  type: "clear_channel";
  peer_id: string;
  channel: string;
}

type PresenceMessage =
  | PresenceUpdate
  | PresenceSnapshot
  | PresenceLeft
  | PresenceHeartbeat
  | PresenceClearChannel;

// ── Peer state ───────────────────────────────────────────────────────

export interface PeerCursorInfo {
  peerId: string;
  peerLabel: string;
  color: string;
  cursor?: CursorData;
  selection?: SelectionData;
  focus?: { cell_id: string };
}

// ── Registry state ───────────────────────────────────────────────────

/** Map of cellId → registered EditorView */
const editors = new Map<string, EditorView>();

/** Map of peerId → current cursor/selection state */
const peers = new Map<string, PeerCursorInfo>();

/** The local peer ID (excluded from remote cursor rendering) */
let localPeerId: string | null = null;

// ── Editor registration ──────────────────────────────────────────────

/**
 * Register a CodeMirror EditorView for a cell. The registry will dispatch
 * remote cursor StateEffects to this view when presence updates arrive.
 */
export function registerEditor(cellId: string, view: EditorView): void {
  editors.set(cellId, view);
  // Immediately render any existing cursors for this cell
  dispatchToCell(cellId);
}

/**
 * Unregister an EditorView when a cell unmounts or the view changes.
 */
export function unregisterEditor(cellId: string): void {
  editors.delete(cellId);
  for (const peer of peers.values()) {
    if (peer.cursor?.cell_id === cellId) {
      peer.cursor = undefined;
    }
    if (peer.selection?.cell_id === cellId) {
      peer.selection = undefined;
    }
    if (peer.focus?.cell_id === cellId) {
      peer.focus = undefined;
    }
  }
}

// ── Dispatch helpers ─────────────────────────────────────────────────

/** Collect all remote cursors for a cell and dispatch to its EditorView. */
function dispatchToCell(cellId: string): void {
  const view = editors.get(cellId);
  if (!view) {
    logger.debug(`[cursor-registry] dispatchToCell: no view for ${cellId}`);
    return;
  }

  const cursors: RemoteCursorState[] = [];
  const selections: RemoteSelectionState[] = [];

  for (const [peerId, peer] of peers) {
    if (peerId === localPeerId) continue;

    if (peer.cursor?.cell_id === cellId) {
      cursors.push({
        peerId,
        peerLabel: peer.peerLabel,
        line: peer.cursor.line,
        column: peer.cursor.column,
        color: peer.color,
      });
    }

    if (peer.selection?.cell_id === cellId) {
      selections.push({
        peerId,
        peerLabel: peer.peerLabel,
        anchorLine: peer.selection.anchor_line,
        anchorCol: peer.selection.anchor_col,
        headLine: peer.selection.head_line,
        headCol: peer.selection.head_col,
        color: peer.color,
      });
    }
  }

  logger.debug(
    `[cursor-registry] dispatchToCell ${cellId}: ${cursors.length} cursors, ${selections.length} selections`,
  );
  setRemoteCursors(view, cursors);
  setRemoteSelections(view, selections);
}

/** Dispatch updates to all cells that might be affected by a peer change. */
function dispatchToAffectedCells(affectedCellIds: Set<string>): void {
  for (const cellId of affectedCellIds) {
    dispatchToCell(cellId);
  }
  // Also notify any cell-level subscribers
  notifyCellSubscribers(affectedCellIds);
}

// ── Presence event handler ───────────────────────────────────────────

function handlePresence(payload: unknown): void {
  const msg = payload as PresenceMessage;

  switch (msg.type) {
    case "update": {
      if (msg.peer_id === localPeerId) return;

      const existing = peers.get(msg.peer_id);
      const peer: PeerCursorInfo = existing ?? {
        peerId: msg.peer_id,
        peerLabel: msg.peer_label || "Peer",
        color: peerColor(msg.peer_id),
      };

      // Update label if the message carries one (e.g. "Agent" from MCP)
      if (msg.peer_label) {
        peer.peerLabel = msg.peer_label;
      }

      const affectedCells = new Set<string>();

      if (msg.channel === "cursor") {
        const data = msg.data as CursorData;
        // Clear old cell, add new cell
        if (peer.cursor && peer.cursor.cell_id !== data.cell_id) {
          affectedCells.add(peer.cursor.cell_id);
        }
        // Cursor replaces focus
        if (peer.focus) {
          affectedCells.add(peer.focus.cell_id);
          peer.focus = undefined;
        }
        // Cursor-only means no selection (anchor === head in sender)
        if (peer.selection) {
          affectedCells.add(peer.selection.cell_id);
          peer.selection = undefined;
        }
        peer.cursor = data;
        affectedCells.add(data.cell_id);
      } else if (msg.channel === "selection") {
        const data = msg.data as SelectionData;
        if (peer.selection && peer.selection.cell_id !== data.cell_id) {
          affectedCells.add(peer.selection.cell_id);
        }
        peer.selection = data;
        affectedCells.add(data.cell_id);
      } else if (msg.channel === "focus") {
        const data = msg.data as { cell_id: string };
        // Focus replaces cursor
        if (peer.cursor) {
          affectedCells.add(peer.cursor.cell_id);
          peer.cursor = undefined;
        }
        if (peer.focus && peer.focus.cell_id !== data.cell_id) {
          affectedCells.add(peer.focus.cell_id);
        }
        peer.focus = data;
        affectedCells.add(data.cell_id);
      }

      peers.set(msg.peer_id, peer);
      dispatchToAffectedCells(affectedCells);
      break;
    }

    case "snapshot": {
      // Replace all peer state with snapshot data
      const affectedCells = new Set<string>();

      // Track cells that had cursors before (to clear them)
      for (const peer of peers.values()) {
        if (peer.cursor) affectedCells.add(peer.cursor.cell_id);
        if (peer.selection) affectedCells.add(peer.selection.cell_id);
        if (peer.focus) affectedCells.add(peer.focus.cell_id);
      }

      peers.clear();

      for (const snap of msg.peers) {
        if (snap.peer_id === localPeerId) continue;

        const peer: PeerCursorInfo = {
          peerId: snap.peer_id,
          peerLabel: snap.peer_label,
          color: peerColor(snap.peer_id),
        };

        for (const ch of snap.channels) {
          if (ch.channel === "cursor") {
            peer.cursor = ch.data as CursorData;
            affectedCells.add(peer.cursor.cell_id);
          } else if (ch.channel === "selection") {
            peer.selection = ch.data as SelectionData;
            affectedCells.add(peer.selection.cell_id);
          } else if (ch.channel === "focus") {
            peer.focus = ch.data as { cell_id: string };
            affectedCells.add(peer.focus.cell_id);
          }
        }

        peers.set(snap.peer_id, peer);
      }

      dispatchToAffectedCells(affectedCells);
      break;
    }

    case "left": {
      const peer = peers.get(msg.peer_id);
      if (!peer) return;

      const affectedCells = new Set<string>();
      if (peer.cursor) affectedCells.add(peer.cursor.cell_id);
      if (peer.selection) affectedCells.add(peer.selection.cell_id);
      if (peer.focus) affectedCells.add(peer.focus.cell_id);

      peers.delete(msg.peer_id);
      dispatchToAffectedCells(affectedCells);
      break;
    }

    case "clear_channel": {
      const peer = peers.get(msg.peer_id);
      if (!peer) return;
      const affectedCells = new Set<string>();
      if (msg.channel === "cursor" && peer.cursor) {
        affectedCells.add(peer.cursor.cell_id);
        peer.cursor = undefined;
      } else if (msg.channel === "selection" && peer.selection) {
        affectedCells.add(peer.selection.cell_id);
        peer.selection = undefined;
      } else if (msg.channel === "focus" && peer.focus) {
        affectedCells.add(peer.focus.cell_id);
        peer.focus = undefined;
      }
      dispatchToAffectedCells(affectedCells);
      break;
    }

    case "heartbeat":
      // No visual change needed
      break;
  }
}

// ── Lifecycle ────────────────────────────────────────────────────────

/**
 * Start dispatching presence events to registered CodeMirror EditorViews.
 *
 * Call once at app startup. Returns a cleanup function.
 *
 * @param peerId The local peer's ID — excluded from remote cursor rendering.
 */
export function startCursorDispatch(peerId: string): () => void {
  localPeerId = peerId;

  const unsubscribe = subscribePresence(handlePresence);

  return () => {
    unsubscribe();
    localPeerId = null;
    peers.clear();
    // Clear all editors' cursors on shutdown
    for (const [cellId] of editors) {
      const view = editors.get(cellId);
      if (view) {
        setRemoteCursors(view, []);
        setRemoteSelections(view, []);
      }
    }
    editors.clear();
    // Clear cell subscriptions
    cellSubscribers.clear();
  };
}

// ── Cell-level presence queries ───────────────────────────────────────

/** Map of cellId → Set of subscriber callbacks */
const cellSubscribers = new Map<string, Set<() => void>>();

/**
 * Find a connected peer's color by matching against an actor label.
 *
 * Actor labels follow the convention `"agent:<name>:<session>"` while
 * peer labels are display names like `"Claude"` or `"🤖 Agent"`.
 * We match by checking if the actor label contains the peer label
 * (case-insensitive), so `"agent:claude:ab12cd34"` matches a peer
 * labeled `"Claude"`.
 *
 * Returns the peer's cursor color if found, or `undefined` if no
 * connected peer matches. This lets text attribution highlights use
 * the same color as the peer's live cursor.
 */
export function findPeerColorByLabel(actorLabel: string): string | undefined {
  const lower = actorLabel.toLowerCase();
  for (const peer of peers.values()) {
    if (peer.peerId === localPeerId) continue;
    const peerLower = peer.peerLabel.toLowerCase();
    if (peerLower && lower.includes(peerLower)) {
      return peer.color;
    }
  }
  return undefined;
}

/**
 * Get all remote peers that have a cursor in the given cell.
 * Returns peer info for UI rendering (colored dots, labels, etc.)
 */
export function getPeersForCell(cellId: string): PeerCursorInfo[] {
  const result: PeerCursorInfo[] = [];
  for (const peer of peers.values()) {
    if (peer.peerId === localPeerId) continue;
    if (peer.cursor?.cell_id === cellId || peer.focus?.cell_id === cellId) {
      result.push(peer);
    }
  }
  return result;
}

/**
 * Subscribe to presence changes for a specific cell.
 * The callback is invoked whenever peers enter or leave the cell.
 * Returns an unsubscribe function.
 */
export function subscribeToCell(
  cellId: string,
  callback: () => void,
): () => void {
  let subs = cellSubscribers.get(cellId);
  if (!subs) {
    subs = new Set();
    cellSubscribers.set(cellId, subs);
  }
  subs.add(callback);

  return () => {
    subs?.delete(callback);
    if (subs?.size === 0) {
      cellSubscribers.delete(cellId);
    }
  };
}

/** Notify cell subscribers when presence changes. */
function notifyCellSubscribers(cellIds: Set<string>): void {
  for (const cellId of cellIds) {
    const subs = cellSubscribers.get(cellId);
    if (subs) {
      for (const cb of subs) {
        cb();
      }
    }
  }
}
