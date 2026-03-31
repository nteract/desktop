/**
 * Shared editor registry — single Map<cellId, EditorView> used by both
 * cursor-registry and attribution-registry.
 *
 * Cell components register/unregister their EditorView here once.
 * Both presence cursors and text attribution highlights dispatch
 * StateEffects to the same view instance.
 */

import type { EditorView } from "@codemirror/view";

const editors = new Map<string, EditorView>();

/**
 * Register a CodeMirror EditorView for a cell.
 * Both cursor and attribution dispatchers will use this view.
 */
export function registerCellEditor(cellId: string, view: EditorView): void {
  editors.set(cellId, view);
}

/**
 * Unregister an EditorView when a cell unmounts or the view changes.
 */
export function unregisterCellEditor(cellId: string): void {
  editors.delete(cellId);
}

/** Get the EditorView for a cell, or undefined if not registered. */
export function getCellEditor(cellId: string): EditorView | undefined {
  return editors.get(cellId);
}

/** Get all registered cell IDs and their EditorViews. */
export function getAllCellEditors(): Map<string, EditorView> {
  return editors;
}
