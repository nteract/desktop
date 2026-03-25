/**
 * Window focus handler — prevents keystroke loss on window reactivation.
 *
 * When the Tauri window loses and regains OS focus (e.g., Cmd+Tab), the
 * WKWebView's text input context for the active contenteditable element
 * may not be immediately re-established. This causes the first few
 * keystrokes to be silently dropped or processed against stale state.
 *
 * Fix: track the focused CodeMirror editor via `focusin`, snapshot its
 * selection on window blur, then explicitly cycle blur→focus on the
 * editor's contentDOM when the window regains focus. The cycle forces
 * WKWebView to reconnect its input pipeline. Because `window.focus`
 * fires synchronously BEFORE any keystroke events, the input context is
 * ready before the first character arrives.
 *
 * The blur→focus cycle is invisible — CM6 debounces blur transactions
 * with a 200ms timeout, and the immediate focus() clears that timeout,
 * so no spurious blur transaction is dispatched and the cursor doesn't
 * flicker (no paint occurs between the synchronous blur and focus calls).
 */

import { EditorView } from "@codemirror/view";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { logger } from "./logger";

// ── State ────────────────────────────────────────────────────────────

/** The last CodeMirror EditorView that had focus. */
let savedView: EditorView | null = null;

/** The selection range captured on window blur. */
let savedSelection: { anchor: number; head: number } | null = null;

/** Whether the window currently has focus (dedup guard). */
let windowFocused = true;

// ── Editor focus tracking ────────────────────────────────────────────

/**
 * Track which CM editor has focus, clearing state when focus moves
 * elsewhere. Uses `focusin` on the document (capture phase) so we
 * always know whether an editor is active, independent of React state
 * or the cursor registry.
 *
 * When focus moves to a non-editor element (find bar, dependency input,
 * dialog, etc.) we clear savedView so that window refocus doesn't steal
 * focus back from the legitimate target.
 */
function trackEditorFocus(e: FocusEvent): void {
  const target = e.target as HTMLElement | null;
  if (!target) return;

  const cmEditor = target.closest?.(".cm-editor");
  if (!cmEditor) {
    // Focus moved to a non-editor control — clear saved state so
    // window refocus doesn't steal focus from dialogs, find bar, etc.
    savedView = null;
    savedSelection = null;
    return;
  }

  const cmContent = cmEditor.querySelector(".cm-content");
  if (!cmContent) return;

  const view = EditorView.findFromDOM(cmContent as HTMLElement);
  if (view) {
    savedView = view;
  }
}

// ── Focus / blur handlers ────────────────────────────────────────────

function handleWindowBlur(): void {
  if (!windowFocused) return; // already blurred (dedup)
  windowFocused = false;

  // Snapshot the selection from the tracked editor. CM6's internal
  // state.selection is always valid regardless of DOM focus.
  if (savedView) {
    const sel = savedView.state.selection.main;
    savedSelection = { anchor: sel.anchor, head: sel.head };
    logger.debug("[window-focus] Saved selection on blur", savedSelection);
  }

  logger.info("[window-focus] Window lost focus");
}

function handleWindowFocus(): void {
  if (windowFocused) return; // already focused (dedup)
  windowFocused = true;

  logger.info("[window-focus] Window gained focus");
  restoreEditorFocus();
}

// ── Focus restoration ────────────────────────────────────────────────

/**
 * Re-establish the text input context for the saved CodeMirror editor.
 *
 * Forces a blur→focus cycle on the editor's contentDOM so that WKWebView
 * creates a fresh input session. Without this, the webview may accept
 * focus at the OS level but not reconnect the IME / text-input pipeline
 * to the contenteditable, silently dropping the first few keystrokes.
 *
 * After the cycle, the saved selection is dispatched to correct any
 * position that CM inferred from the (potentially stale) DOM selection
 * during the focus event.
 */
function restoreEditorFocus(): void {
  if (!savedView?.dom?.isConnected) {
    savedView = null;
    savedSelection = null;
    logger.debug("[window-focus] No live editor to restore");
    return;
  }

  const view = savedView;

  // Cycle focus: blur → focus. A plain focus() call on an already-focused
  // element is a no-op at the DOM level — the blur is necessary to force
  // WKWebView to tear down and recreate the input session.
  //
  // CM6's blur handler sets a 200ms timeout before dispatching a blur
  // transaction. The immediate focus() call triggers CM6's focus handler,
  // which clears that timeout. Net effect: input context is refreshed,
  // no spurious blur transaction is created.
  try {
    if (view.hasFocus) {
      view.contentDOM.blur();
    }
    view.focus();
  } catch (e) {
    logger.warn("[window-focus] Focus cycle failed:", e);
    return;
  }

  // Restore the exact cursor / selection position from before the blur.
  // The focus cycle may cause CM to read a stale DOM selection; this
  // dispatch corrects it. Selection-only dispatches don't trigger the
  // CRDT bridge's outbound path (it filters on docChanged).
  if (savedSelection) {
    const docLen = view.state.doc.length;
    const anchor = Math.min(savedSelection.anchor, docLen);
    const head = Math.min(savedSelection.head, docLen);
    view.dispatch({
      selection: { anchor, head },
    });
    logger.debug("[window-focus] Restored selection", { anchor, head });
  }

  savedSelection = null;
}

// ── Lifecycle ────────────────────────────────────────────────────────

/**
 * Start the window focus handler.
 *
 * Call once at app startup. Returns a cleanup function.
 * Follows the same pattern as {@link startCursorDispatch}.
 */
export function startWindowFocusHandler(): () => void {
  // Track which CM editor has focus (capture phase for earliest signal).
  document.addEventListener("focusin", trackEditorFocus, true);

  // Web-standard focus / blur on the window object. These fire
  // synchronously with the OS focus change — BEFORE any keystroke
  // events — so the input context is re-established in time.
  window.addEventListener("focus", handleWindowFocus);
  window.addEventListener("blur", handleWindowBlur);

  // Tauri-specific window focus signal as a belt-and-suspenders layer.
  // Goes through the IPC bridge so it may arrive slightly after the DOM
  // events, but it's authoritative for Tauri-managed windows.
  const tauriUnlistenPromise = getCurrentWindow().onFocusChanged(
    ({ payload: focused }) => {
      if (focused) {
        handleWindowFocus();
      } else {
        handleWindowBlur();
      }
    },
  );

  // Visibility change covers minimize / restore and (on some platforms)
  // Mission Control transitions that don't fire window blur/focus.
  const handleVisibility = (): void => {
    if (document.hidden) {
      handleWindowBlur();
    } else {
      handleWindowFocus();
    }
  };
  document.addEventListener("visibilitychange", handleVisibility);

  logger.info("[window-focus] Handler started");

  return () => {
    document.removeEventListener("focusin", trackEditorFocus, true);
    window.removeEventListener("focus", handleWindowFocus);
    window.removeEventListener("blur", handleWindowBlur);
    document.removeEventListener("visibilitychange", handleVisibility);
    tauriUnlistenPromise.then((fn) => fn()).catch(() => {});
    savedView = null;
    savedSelection = null;
    logger.info("[window-focus] Handler stopped");
  };
}
