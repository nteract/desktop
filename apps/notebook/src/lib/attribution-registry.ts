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
import { findPeerColorByLabel } from "./cursor-registry";
import { subscribeBroadcast } from "./notebook-frame-bus";

// ── Types (text_attribution event shape from WASM) ───────────────────

interface TextAttributionEvent {
  type: "text_attribution";
  attributions: Array<{
    cell_id: string;
    index: number;
    text: string;
    deleted: number;
    actors: string[];
  }>;
}

// ── Editor registry ──────────────────────────────────────────────────
//
// Cells register/unregister their EditorView here. This is a separate map
// from cursor-registry because the two systems have independent lifecycles
// and we want to avoid tight coupling. The registration calls are cheap
// (Map.set/delete) and happen at the same points as cursor registration.

import type { EditorView } from "@codemirror/view";

const editors = new Map<string, EditorView>();

/**
 * Register a CodeMirror EditorView for a cell. The registry will dispatch
 * text attribution highlights to this view when attribution events arrive.
 */
export function registerAttributionEditor(
  cellId: string,
  view: EditorView,
): void {
  editors.set(cellId, view);
}

/**
 * Unregister an EditorView when a cell unmounts or the view changes.
 */
export function unregisterAttributionEditor(cellId: string): void {
  editors.delete(cellId);
}

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

  // TODO(rgbkrk): This fuzzy matching is fragile — it works when the actor
  // label contains the peer's display name (e.g., "agent:claude:ab12cd34"
  // matches peer label "Claude") but breaks for generic labels like "Agent".
  // The real fix is unifying peer_label and actor_label so both systems hash
  // the same string. See the discussion on identity alignment in #833.
  for (const actor of actors) {
    const peerMatch = findPeerColorByLabel(actor);
    if (peerMatch) return peerMatch;
  }

  return peerColor(actors[0]);
}

// ── Event handler ────────────────────────────────────────────────────

function handleBroadcast(payload: unknown): void {
  // Type-narrow: only handle text_attribution events
  if (
    !payload ||
    typeof payload !== "object" ||
    (payload as { type?: string }).type !== "text_attribution"
  ) {
    return;
  }

  const event = payload as TextAttributionEvent;
  if (!event.attributions || event.attributions.length === 0) return;

  // Defer mark creation by one microtask. The CRDT bridge also
  // subscribes to this broadcast and applies text changes to the CM
  // editor synchronously. If we create marks immediately, we read the
  // pre-change document (old positions, old length), and the subsequent
  // CRDT bridge transaction remaps the marks — typically collapsing
  // insert-then-delete pairs to zero width. By deferring, we guarantee
  // the CM document already reflects the new content when we read
  // positions, so marks land on the correct text.
  const attributions = event.attributions;
  queueMicrotask(() => dispatchAttributionMarks(attributions));
}

/** Create and dispatch attribution marks after the CRDT bridge has applied text changes. */
function dispatchAttributionMarks(
  attributions: TextAttributionEvent["attributions"],
): void {
  // Group attributions by cell_id for batch dispatch
  const byCellId = new Map<string, AttributionMark[]>();

  for (const attr of attributions) {
    const view = editors.get(attr.cell_id);
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
    const view = editors.get(cellId);
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
    editors.clear();
  };
}
