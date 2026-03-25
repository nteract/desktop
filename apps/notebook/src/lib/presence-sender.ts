/**
 * CodeMirror extension that broadcasts local cursor/selection presence.
 *
 * Listens to selection changes via `EditorView.updateListener` and calls
 * presence callbacks with throttling to prevent flooding during rapid typing.
 *
 * The extension is created per-cell with the cell ID baked in, so each
 * CodeMirror instance knows which cell it belongs to.
 */

import type { Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";

export interface PresenceSenderCallbacks {
  onCursor: (cellId: string, line: number, column: number) => void;
  onSelection: (
    cellId: string,
    anchorLine: number,
    anchorCol: number,
    headLine: number,
    headCol: number,
  ) => void;
}

/** Throttle interval in ms (~13 updates/sec max) */
const THROTTLE_MS = 75;

/**
 * Create a CodeMirror extension that sends presence updates on selection change.
 *
 * @param cellId The ID of the cell this editor belongs to
 * @param callbacks Presence send functions (typically from PresenceContext)
 */
export function presenceSenderExtension(
  cellId: string,
  callbacks: PresenceSenderCallbacks,
): Extension {
  let throttleTimer: ReturnType<typeof setTimeout> | null = null;
  let pendingUpdate = false;

  return EditorView.updateListener.of((update) => {
    // Only act on selection changes in focused editors — an unfocused editor
    // must never send presence, otherwise the throttle timer can fire after
    // the user moves to another cell and re-broadcast a stale position.
    if (!update.selectionSet || !update.view.hasFocus) return;

    // If a timer is pending, mark that we have a pending update
    if (throttleTimer) {
      pendingUpdate = true;
      return;
    }

    // Send immediately, then start throttle window
    sendPresence(update.view, cellId, callbacks);

    throttleTimer = setTimeout(() => {
      throttleTimer = null;
      // If there was a pending update during the throttle window, send it
      // now — but only if the editor still has focus. If the user clicked
      // into a different cell during the throttle window, skip the send
      // to avoid overwriting their new cursor position with a stale one.
      if (pendingUpdate) {
        pendingUpdate = false;
        if (update.view.hasFocus) {
          sendPresence(update.view, cellId, callbacks);
        }
      }
    }, THROTTLE_MS);
  });
}

function sendPresence(
  view: EditorView,
  cellId: string,
  callbacks: PresenceSenderCallbacks,
): void {
  const sel = view.state.selection.main;
  const doc = view.state.doc;

  // Convert head position to line:column
  const headLineObj = doc.lineAt(sel.head);
  const headLine = headLineObj.number - 1; // 0-based
  const headCol = sel.head - headLineObj.from;

  if (sel.anchor === sel.head) {
    // Cursor only (no selection)
    callbacks.onCursor(cellId, headLine, headCol);
  } else {
    // Selection - also need anchor position
    const anchorLineObj = doc.lineAt(sel.anchor);
    const anchorLine = anchorLineObj.number - 1;
    const anchorCol = sel.anchor - anchorLineObj.from;

    callbacks.onSelection(cellId, anchorLine, anchorCol, headLine, headCol);
  }
}
