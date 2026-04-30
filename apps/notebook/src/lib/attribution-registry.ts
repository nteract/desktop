/**
 * Attribution registry — connects text attribution events from the frame bus
 * to CodeMirror EditorViews via direct StateEffect dispatch.
 *
 * Mirrors the cursor-registry pattern: frame bus events arrive synchronously,
 * are mapped to CodeMirror document positions, and dispatched as StateEffects
 * to registered EditorViews. No React involvement on the hot path.
 *
 * Flow:
 *   frame bus emitBroadcast({ type: "text_attribution", attributions })
 *     → subscribeBroadcast callback
 *       → for each attribution, look up the cell's EditorView
 *         → addTextAttributions(view, marks)
 *
 * Editors are shared with the cursor registry — both read from the same
 * Map<cellId, EditorView>. This module imports from cursor-registry to
 * avoid duplicating the editor registration lifecycle.
 *
 * Color assignment reuses `peerColor()` from remote-cursors so that an
 * agent's attribution highlight matches their cursor color.
 */

import { peerColor } from "@/components/editor/remote-cursors";
import {
  type AttributionMark,
  addTextAttributions,
} from "@/components/editor/text-attribution";
import { isTextAttributionEvent, type TextAttribution } from "runtimed";
import { findPeerColorByActorLabel } from "./cursor-registry";
import { getCellEditor } from "./editor-registry";
import { subscribeBroadcast } from "./notebook-frame-bus";

// ── Color cache ──────────────────────────────────────────────────────

/**
 * Derive a highlight color from an actor label.
 *
 * Uses the same `peerColor()` hash as remote cursors so an agent's
 * attribution glow matches their cursor bar color.
 *
 * For multi-actor attributions (rare — usually a single actor per sync
 * batch), we use the first actor's color.
 */
function colorForActors(actors: string[]): string {
  if (actors.length === 0) return "#3b82f6"; // blue-500 fallback

  // Exact match: look up the actor label in the cursor-registry's peer map.
  // Each peer's presence update includes their Automerge actor label, so
  // we can directly map attribution actors to cursor colors.
  for (const actor of actors) {
    const peerMatch = findPeerColorByActorLabel(actor);
    if (peerMatch) return peerMatch;
  }

  return peerColor(actors[0]);
}

// ── Event handler ────────────────────────────────────────────────────

function handleBroadcast(payload: unknown): void {
  if (!isTextAttributionEvent(payload)) {
    return;
  }

  if (payload.attributions.length === 0) return;

  // Defer mark creation by one microtask. The CRDT bridge also
  // subscribes to this broadcast and applies text changes to the CM
  // editor synchronously. If we create marks immediately, we read the
  // pre-change document (old positions, old length), and the subsequent
  // CRDT bridge transaction remaps the marks — typically collapsing
  // insert-then-delete pairs to zero width. By deferring, we guarantee
  // the CM document already reflects the new content when we read
  // positions, so marks land on the correct text.
  const attributions = payload.attributions;
  queueMicrotask(() => dispatchAttributionMarks(attributions));
}

/** Create and dispatch attribution marks after the CRDT bridge has applied text changes. */
function dispatchAttributionMarks(attributions: TextAttribution[]): void {
  // Group attributions by cell_id for batch dispatch
  const byCellId = new Map<string, AttributionMark[]>();

  for (const attr of attributions) {
    const view = getCellEditor(attr.cell_id);
    if (!view) continue;

    // Skip pure deletions — nothing to highlight (the text is gone)
    if (attr.text.length === 0) continue;

    // Skip automated daemon changes (formatting, file watcher, kernel
    // display updates). The text is applied via CRDT sync, but the
    // visual animation (fade-in, underline sweep) is distracting for
    // non-human edits. Daemon actors are either the bare "runtimed" or
    // scoped like "runtimed:ruff". Human and agent actors use different
    // prefixes (e.g., "agent:claude:...", "human").
    if (attr.actors.every((a) => a === "runtimed" || a.startsWith("runtimed:")))
      continue;

    const docLen = view.state.doc.length;
    const from = Math.min(attr.index, docLen);
    const to = Math.min(attr.index + attr.text.length, docLen);

    if (from >= to) continue;

    let marks = byCellId.get(attr.cell_id);
    if (!marks) {
      marks = [];
      byCellId.set(attr.cell_id, marks);
    }

    marks.push({
      from,
      to,
      actors: attr.actors,
      color: colorForActors(attr.actors),
    });
  }

  // Dispatch to each affected cell's EditorView
  for (const [cellId, marks] of byCellId) {
    const view = getCellEditor(cellId);
    if (view) {
      addTextAttributions(view, marks);
    }
  }
}

// ── Lifecycle ────────────────────────────────────────────────────────

/**
 * Start dispatching text attribution events to registered CodeMirror EditorViews.
 *
 * Call once at app startup. Returns a cleanup function.
 */
export function startAttributionDispatch(): () => void {
  const unsubscribe = subscribeBroadcast(handleBroadcast);

  return () => {
    unsubscribe();
  };
}
