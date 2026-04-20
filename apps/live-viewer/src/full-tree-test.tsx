/**
 * FULL TREE IMPORT TEST
 *
 * This file attempts to import every significant component and hook from
 * apps/notebook/src/ to discover coupling boundaries. Each import is tested
 * independently — when the build fails, the error message tells us exactly
 * what's coupled and why.
 *
 * NOT MEANT TO RUN — just to compile and find edges.
 */

// ─── App-specific hooks ─────────────────────────────────────────────────

// The big one: useAutomergeNotebook owns the WASM handle and sync pipeline
export { useAutomergeNotebook } from "notebook-app/hooks/useAutomergeNotebook";

// Cell UI state store (module-level singletons)
export {
  useCellIds,
  useCell,
  replaceNotebookCells,
  updateCellById,
} from "notebook-app/lib/notebook-cells";

// Transient UI state
export {
  useFocusedCellId,
  useIsCellFocused,
  useIsCellExecuting,
  setFocusedCellId,
  setExecutingCellIds,
} from "notebook-app/lib/cell-ui-state";

// Frame bus (module-level pub/sub)
export {
  subscribeBroadcast,
  emitBroadcast,
  subscribePresence,
} from "notebook-app/lib/notebook-frame-bus";

// ─── App-specific components ────────────────────────────────────────────

// The full NotebookView with dnd-kit, stable DOM order, cell adders
export { NotebookView } from "notebook-app/components/NotebookView";

// CodeCell with CRDT bridge, presence, keyboard nav
export { CodeCell } from "notebook-app/components/CodeCell";

// MarkdownCell with edit/preview toggle
export { MarkdownCell } from "notebook-app/components/MarkdownCell";

// ─── Supporting hooks ───────────────────────────────────────────────────

// CRDT bridge (CodeMirror ↔ Automerge)
export { useCrdtBridge } from "notebook-app/hooks/useCrdtBridge";

// Keyboard navigation
export { useCellKeyboardNavigation } from "notebook-app/hooks/useCellKeyboardNavigation";

// Editor registry
export { useEditorRegistry, EditorRegistryProvider } from "notebook-app/hooks/useEditorRegistry";

// Daemon kernel hook (execution, broadcasts, widget comm)
export { useDaemonKernel } from "notebook-app/hooks/useDaemonKernel";

// ─── Contexts ───────────────────────────────────────────────────────────

// Presence context
export { usePresenceContext, PresenceProvider } from "notebook-app/contexts/PresenceContext";
