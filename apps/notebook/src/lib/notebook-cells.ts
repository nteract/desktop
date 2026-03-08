import { useSyncExternalStore } from "react";
import type { NotebookCell } from "../types";

// ---------------------------------------------------------------------------
// Reactive cell store backed by the WASM Automerge document.
//
// useAutomergeNotebook owns the WASM NotebookHandle and writes cell snapshots
// into this store after bootstrap, sync, and optimistic local updates.
// Components read cells via useSyncExternalStore so concurrent rendering stays
// aligned with the current notebook snapshot.
// ---------------------------------------------------------------------------

let _cellsSnapshot: NotebookCell[] = [];
const _subscribers = new Set<() => void>();

function emitChange(): void {
  for (const cb of _subscribers) cb();
}

function subscribe(callback: () => void): () => void {
  _subscribers.add(callback);
  return () => _subscribers.delete(callback);
}

function getSnapshot(): NotebookCell[] {
  return _cellsSnapshot;
}

export function useNotebookCells(): NotebookCell[] {
  return useSyncExternalStore(subscribe, getSnapshot);
}

export function getNotebookCellsSnapshot(): NotebookCell[] {
  return _cellsSnapshot;
}

export function replaceNotebookCells(cells: NotebookCell[]): void {
  _cellsSnapshot = cells;
  emitChange();
}

export function updateNotebookCells(
  updater: (cells: NotebookCell[]) => NotebookCell[],
): NotebookCell[] {
  _cellsSnapshot = updater(_cellsSnapshot);
  emitChange();
  return _cellsSnapshot;
}

export function resetNotebookCells(): void {
  replaceNotebookCells([]);
}
