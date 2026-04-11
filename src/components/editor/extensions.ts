import { closeBrackets, closeBracketsKeymap, completionKeymap } from "@codemirror/autocomplete";
import { defaultKeymap, history, historyKeymap } from "@codemirror/commands";
import {
  bracketMatching,
  defaultHighlightStyle,
  indentOnInput,
  syntaxHighlighting,
} from "@codemirror/language";
import { lintKeymap } from "@codemirror/lint";
import { EditorState, type Extension } from "@codemirror/state";
import {
  crosshairCursor,
  drawSelection,
  dropCursor,
  EditorView,
  keymap,
  rectangularSelection,
} from "@codemirror/view";

/**
 * Custom editor styles for notebook contexts
 */
export const notebookEditorTheme = EditorView.theme({
  // Transparent background so editor inherits from container
  // (overrides theme's background)
  "&.cm-editor": {
    backgroundColor: "transparent",
    // Pull the editor up to compensate for the extra .cm-content paddingTop
    // that reserves space for peer cursor labels above line 1.
    marginTop: "-0.75rem",
  },
  // Remove focus dotted outline
  "&.cm-focused": {
    outline: "none",
  },
  // Top padding inside editor so peer cursor labels have room above line 1
  // Flag is ~19px tall (11px font × 1.2 line-height + 4px padding + 1px margin)
  ".cm-content": {
    paddingTop: "1.375rem",
  },
  // Reset line padding so code aligns with output areas
  // (CodeMirror's base theme adds "padding: 0 2px 0 6px" to .cm-line)
  ".cm-line": {
    paddingLeft: "0",
    caretColor: "var(--foreground)", // Ensure cursor is visible even when not focused
  },
  // Mobile-friendly padding
  "@media (max-width: 640px)": {
    ".cm-content": {
      padding: "0.75rem 0.5rem",
    },
  },
  // Slightly thicker cursor for better visibility
  // Explicit colors for both light and dark themes - GitHub themes don't always set these
  ".cm-cursor": {
    borderLeftWidth: "2px",
    borderLeftColor: "var(--foreground)",
  },
  ".cm-focused .cm-cursor": {
    borderLeftColor: "var(--foreground)",
  },
});

/**
 * Core editor setup with all standard features
 * Includes: history, bracket matching, autocomplete, syntax highlighting
 */
export const coreSetup: Extension = (() => [
  history(),
  drawSelection(),
  dropCursor(),
  EditorState.allowMultipleSelections.of(true),
  indentOnInput(),
  syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
  bracketMatching(),
  closeBrackets(),
  rectangularSelection(),
  crosshairCursor(),
  keymap.of([
    ...closeBracketsKeymap,
    ...defaultKeymap,
    ...historyKeymap,
    ...completionKeymap,
    ...lintKeymap,
  ]),
  notebookEditorTheme,
])();

/**
 * Minimal editor setup without autocomplete
 * Useful for AI prompts or simple text input
 */
export const minimalSetup: Extension = (() => [
  history(),
  drawSelection(),
  dropCursor(),
  EditorState.allowMultipleSelections.of(true),
  indentOnInput(),
  syntaxHighlighting(defaultHighlightStyle, { fallback: true }),
  bracketMatching(),
  closeBrackets(),
  rectangularSelection(),
  crosshairCursor(),
  keymap.of([...closeBracketsKeymap, ...defaultKeymap, ...historyKeymap, ...lintKeymap]),
  notebookEditorTheme,
])();

/**
 * Default extensions bundle for notebook cells
 * Uses core setup - add your own theme on top
 */
export const defaultExtensions: Extension[] = [coreSetup];

/**
 * Extensions bundle without autocomplete
 */
export const minimalExtensions: Extension[] = [minimalSetup];
