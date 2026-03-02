import {
  acceptCompletion,
  completionStatus,
  startCompletion,
} from "@codemirror/autocomplete";
import { indentLess, indentMore } from "@codemirror/commands";
import { type Extension, Prec } from "@codemirror/state";
import { type EditorView, keymap } from "@codemirror/view";

/**
 * Check if cursor is after code where completion makes sense.
 * Returns true if cursor follows a word character or dot.
 */
function shouldTriggerCompletion(view: EditorView): boolean {
  const { state } = view;
  const { from, to } = state.selection.main;

  // Don't trigger with active selection (user might want to indent selection)
  if (from !== to) return false;

  const line = state.doc.lineAt(from);
  const textBefore = state.sliceDoc(line.from, from);

  // Trigger if cursor is directly after word character or dot
  return /[\w.]$/.test(textBefore);
}

/**
 * VS Code-like tab completion keymap:
 * - Tab after text: triggers completion
 * - Tab with completion open: accepts selection
 * - Tab on empty/whitespace: indents
 * - Shift+Tab: always dedent
 */
export const tabCompletionKeymap: Extension = Prec.high(
  keymap.of([
    {
      key: "Tab",
      run: (view) => {
        const status = completionStatus(view.state);
        // If completion is active or pending, always consume Tab to prevent focus escape.
        // acceptCompletion can return false during interactionDelay (~75ms), but we
        // still want Tab captured so it doesn't bubble out of the editor.
        if (status === "active" || status === "pending") {
          acceptCompletion(view);
          return true;
        }
        // If after code, trigger completion
        if (shouldTriggerCompletion(view)) {
          return startCompletion(view);
        }
        // Otherwise indent
        return indentMore(view);
      },
    },
    {
      key: "Shift-Tab",
      run: indentLess,
    },
  ]),
);
