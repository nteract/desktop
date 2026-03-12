import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useCallback, useEffect, useRef, useState } from "react";
import { frame_types } from "../lib/frame-types";
import { logger } from "../lib/logger";
import {
  encode_cursor_presence,
  encode_selection_presence,
} from "../wasm/runtimed-wasm/runtimed_wasm.js";

// ── Types ────────────────────────────────────────────────────────────

export interface CursorPosition {
  cell_id: string;
  line: number;
  column: number;
}

export interface SelectionRange {
  cell_id: string;
  anchor_line: number;
  anchor_col: number;
  head_line: number;
  head_col: number;
}

export interface RemotePeer {
  peerId: string;
  peerLabel: string;
  cursor?: CursorPosition;
  selection?: SelectionRange;
}

// ── Presence message types (JSON from WASM decode of CBOR) ───────────

interface PresenceUpdate {
  type: "update";
  peer_id: string;
  channel: "cursor" | "selection" | "kernel_state" | "custom";
  data: CursorPosition | SelectionRange | unknown;
}

interface PresenceSnapshot {
  type: "snapshot";
  peer_id: string;
  peers: Array<{
    peer_id: string;
    peer_label: string;
    channels: Array<{
      channel: "cursor" | "selection" | "kernel_state" | "custom";
      data: unknown;
    }>;
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

type PresenceMessage =
  | PresenceUpdate
  | PresenceSnapshot
  | PresenceLeft
  | PresenceHeartbeat;

// ── Hook ─────────────────────────────────────────────────────────────

/**
 * Manages presence state (remote cursors, selections) from the unified frame pipe.
 *
 * Presence events arrive as decoded JSON via `notebook:presence` webview events,
 * re-emitted by the `useAutomergeNotebook` frame listener. This hook maintains
 * a map of remote peers and exposes helpers for sending local presence and
 * querying remote presence per cell.
 *
 * This is infrastructure-only — no UI rendering. Cell components can use
 * `cursorsForCell` / `selectionsForCell` to render remote indicators.
 *
 * @param peerId The local peer's ID. When `null`, the hook is inactive.
 */
export function usePresence(peerId: string | null) {
  // Ref-based peers map to avoid re-renders on every cursor move.
  const peersRef = useRef<Map<string, RemotePeer>>(new Map());

  // Bumped when the set of peers changes (join/leave/snapshot) to
  // trigger re-renders for components that care about the peer list.
  const [peerVersion, setPeerVersion] = useState(0);

  // ── Incoming presence ────────────────────────────────────────────

  useEffect(() => {
    if (!peerId) return;

    let cancelled = false;
    const webview = getCurrentWebview();

    const unlistenPresence = webview.listen<PresenceMessage>(
      "notebook:presence",
      (event) => {
        if (cancelled) return;
        const msg = event.payload;

        switch (msg.type) {
          case "update": {
            // Ignore our own presence echoed back
            if (msg.peer_id === peerId) return;

            const existing = peersRef.current.get(msg.peer_id);
            const peer: RemotePeer = existing ?? {
              peerId: msg.peer_id,
              peerLabel: "",
            };

            if (msg.channel === "cursor") {
              peer.cursor = msg.data as CursorPosition;
            } else if (msg.channel === "selection") {
              peer.selection = msg.data as SelectionRange;
            }
            // kernel_state and custom channels are ignored for now

            const isNew = !existing;
            peersRef.current.set(msg.peer_id, peer);
            if (isNew) {
              setPeerVersion((v) => v + 1);
            }
            break;
          }

          case "snapshot": {
            const newPeers = new Map<string, RemotePeer>();
            for (const snap of msg.peers) {
              // Skip our own peer entry from snapshots
              if (snap.peer_id === peerId) continue;

              const peer: RemotePeer = {
                peerId: snap.peer_id,
                peerLabel: snap.peer_label,
              };
              for (const ch of snap.channels) {
                if (ch.channel === "cursor") {
                  peer.cursor = ch.data as CursorPosition;
                } else if (ch.channel === "selection") {
                  peer.selection = ch.data as SelectionRange;
                }
              }
              newPeers.set(snap.peer_id, peer);
            }
            peersRef.current = newPeers;
            setPeerVersion((v) => v + 1);
            break;
          }

          case "left": {
            if (peersRef.current.delete(msg.peer_id)) {
              setPeerVersion((v) => v + 1);
            }
            break;
          }

          case "heartbeat":
            // No state change needed — staleness pruning is a future concern
            break;
        }
      },
    );

    return () => {
      cancelled = true;
      unlistenPresence.then((fn) => fn()).catch(() => {});
    };
  }, [peerId]);

  // ── Outgoing presence ────────────────────────────────────────────

  const setCursor = useCallback(
    (cellId: string, line: number, column: number) => {
      if (!peerId) return;
      const payload = encode_cursor_presence(peerId, cellId, line, column);
      const frameData = new Uint8Array(1 + payload.length);
      frameData[0] = frame_types.PRESENCE;
      frameData.set(payload, 1);
      invoke("send_frame", { frameData: Array.from(frameData) }).catch(
        (e: unknown) => logger.warn("[presence] send cursor failed:", e),
      );
    },
    [peerId],
  );

  const setSelection = useCallback(
    (
      cellId: string,
      anchorLine: number,
      anchorCol: number,
      headLine: number,
      headCol: number,
    ) => {
      if (!peerId) return;
      const payload = encode_selection_presence(
        peerId,
        cellId,
        anchorLine,
        anchorCol,
        headLine,
        headCol,
      );
      const frameData = new Uint8Array(1 + payload.length);
      frameData[0] = frame_types.PRESENCE;
      frameData.set(payload, 1);
      invoke("send_frame", { frameData: Array.from(frameData) }).catch(
        (e: unknown) => logger.warn("[presence] send selection failed:", e),
      );
    },
    [peerId],
  );

  // ── Queries ──────────────────────────────────────────────────────
  //
  // Plain functions (no useCallback) — cheap to create and naturally
  // get fresh references when peerVersion triggers a re-render.

  const cursorsForCell = (
    cellId: string,
  ): Array<{ peerId: string; peerLabel: string; cursor: CursorPosition }> => {
    const results: Array<{
      peerId: string;
      peerLabel: string;
      cursor: CursorPosition;
    }> = [];
    for (const peer of peersRef.current.values()) {
      if (peer.cursor?.cell_id === cellId) {
        results.push({
          peerId: peer.peerId,
          peerLabel: peer.peerLabel,
          cursor: peer.cursor,
        });
      }
    }
    return results;
  };

  const selectionsForCell = (
    cellId: string,
  ): Array<{
    peerId: string;
    peerLabel: string;
    selection: SelectionRange;
  }> => {
    const results: Array<{
      peerId: string;
      peerLabel: string;
      selection: SelectionRange;
    }> = [];
    for (const peer of peersRef.current.values()) {
      if (peer.selection?.cell_id === cellId) {
        results.push({
          peerId: peer.peerId,
          peerLabel: peer.peerLabel,
          selection: peer.selection,
        });
      }
    }
    return results;
  };

  return {
    /** All remote peers (read from ref — use peerVersion for reactivity). */
    peers: peersRef.current,
    /** Incremented when peer set changes (join/leave/snapshot). */
    peerVersion,
    /** Set the local cursor position (fire-and-forget). */
    setCursor,
    /** Set the local selection range (fire-and-forget). */
    setSelection,
    /** Get all remote cursors in a specific cell. */
    cursorsForCell,
    /** Get all remote selections in a specific cell. */
    selectionsForCell,
  };
}
