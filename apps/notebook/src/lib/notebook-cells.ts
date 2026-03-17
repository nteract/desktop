import { useMemo, useSyncExternalStore } from "react";
import type { NotebookCell } from "../types";

// ---------------------------------------------------------------------------
// Reactive cell store backed by the WASM Automerge document.
//
// Dual representation for efficient per-cell subscriptions:
//   _cellIds  — ordered cell ID list (changes on add/delete/move/full replace)
//   _cellMap  — cell data by ID (individual entries update independently)
//
// useAutomergeNotebook owns the WASM NotebookHandle and writes cell snapshots
// into this store after bootstrap, sync, and optimistic local updates.
//
// Components subscribe at two granularities:
//   useCellIds()  — re-renders only on structural changes (add/delete/move)
//   useCell(id)   — re-renders only when that specific cell changes
// ---------------------------------------------------------------------------

// ── Internal state ──────────────────────────────────────────────────────

let _cellIds: string[] = [];
let _cellMap: Map<string, NotebookCell> = new Map();

// Subscribers for the ordered ID list (structural changes)
const _idsSubscribers = new Set<() => void>();

// Per-cell subscribers (keyed by cell ID)
const _cellSubscribers = new Map<string, Set<() => void>>();

// Materialization version — bumps on replaceNotebookCells and
// updateNotebookCells (full-array ops), but NOT on updateCellById.
// Used by components that derive cross-cell state (e.g., hiddenGroups).
let _materializeVersion = 0;
const _materializeSubscribers = new Set<() => void>();

function emitIdsChange(): void {
  for (const cb of _idsSubscribers) cb();
}

function emitMaterializeChange(): void {
  _materializeVersion++;
  for (const cb of _materializeSubscribers) cb();
}

function emitCellChange(id: string): void {
  const subs = _cellSubscribers.get(id);
  if (subs) {
    for (const cb of subs) cb();
  }
}

function emitAllCellChanges(): void {
  for (const [, subs] of _cellSubscribers) {
    for (const cb of subs) cb();
  }
}

// ── Hooks ───────────────────────────────────────────────────────────────

/** Subscribe to the ordered cell ID list. Re-renders on structural changes only. */
export function useCellIds(): string[] {
  return useSyncExternalStore(subscribeIds, getIdsSnapshot);
}

/** Subscribe to a single cell by ID. Re-renders only when this cell changes. */
export function useCell(id: string): NotebookCell | undefined {
  const subscribe = useMemo(() => subscribeCellById(id), [id]);
  const getSnapshot = useMemo(() => getCellSnapshot(id), [id]);
  return useSyncExternalStore(subscribe, getSnapshot);
}

/**
 * Subscribe to the materialization version counter. Re-renders on
 * replaceNotebookCells / updateNotebookCells (full-array ops) but NOT
 * on per-cell updateCellById. Useful for cross-cell derived state.
 */
export function useMaterializeVersion(): number {
  return useSyncExternalStore(subscribeMaterialize, getMaterializeSnapshot);
}

// ── Subscription helpers ────────────────────────────────────────────────

function subscribeIds(callback: () => void): () => void {
  _idsSubscribers.add(callback);
  return () => _idsSubscribers.delete(callback);
}

function getIdsSnapshot(): string[] {
  return _cellIds;
}

function subscribeMaterialize(callback: () => void): () => void {
  _materializeSubscribers.add(callback);
  return () => _materializeSubscribers.delete(callback);
}

function getMaterializeSnapshot(): number {
  return _materializeVersion;
}

function subscribeCellById(id: string): (cb: () => void) => () => void {
  return (callback: () => void) => {
    let subs = _cellSubscribers.get(id);
    if (!subs) {
      subs = new Set();
      _cellSubscribers.set(id, subs);
    }
    subs.add(callback);
    return () => {
      subs.delete(callback);
      if (subs.size === 0) _cellSubscribers.delete(id);
    };
  };
}

function getCellSnapshot(id: string): () => NotebookCell | undefined {
  return () => _cellMap.get(id);
}

// ── Write operations ────────────────────────────────────────────────────

/**
 * Update a single cell by ID. Only notifies that cell's subscribers.
 * Does NOT trigger ID list subscribers — use for source edits, output
 * updates, execution count changes, etc.
 */
export function updateCellById(
  id: string,
  updater: (cell: NotebookCell) => NotebookCell,
): void {
  const cell = _cellMap.get(id);
  if (!cell) return;
  const updated = updater(cell);
  _cellMap.set(id, updated);
  emitCellChange(id);
}

/**
 * Replace all cells (full materialization from WASM/sync).
 * Notifies ID list subscribers AND all per-cell subscribers.
 */
export function replaceNotebookCells(cells: NotebookCell[]): void {
  const newIds = cells.map((c) => c.id);
  const idsChanged =
    newIds.length !== _cellIds.length ||
    newIds.some((id, i) => id !== _cellIds[i]);

  _cellMap = new Map(cells.map((c) => [c.id, c]));

  if (idsChanged) {
    _cellIds = newIds;
    emitIdsChange();
  }

  emitMaterializeChange();
  emitAllCellChanges();
}

/**
 * Apply an updater function across all cells.
 * Notifies all per-cell subscribers for cells that changed.
 * Keeps existing behavior for cross-cell operations (e.g., update_display_data).
 */
export function updateNotebookCells(
  updater: (cells: NotebookCell[]) => NotebookCell[],
): NotebookCell[] {
  const prevCells = _cellIds.map((id) => _cellMap.get(id)!);
  const newCells = updater(prevCells);

  const newIds = newCells.map((c) => c.id);
  const idsChanged =
    newIds.length !== _cellIds.length ||
    newIds.some((id, i) => id !== _cellIds[i]);

  _cellMap = new Map(newCells.map((c) => [c.id, c]));

  if (idsChanged) {
    _cellIds = newIds;
    emitIdsChange();
  }

  emitMaterializeChange();

  // Notify per-cell subscribers for cells that actually changed
  for (let i = 0; i < newCells.length; i++) {
    if (i >= prevCells.length || newCells[i] !== prevCells[i]) {
      emitCellChange(newCells[i].id);
    }
  }
  // Notify subscribers for removed cells
  for (let i = newCells.length; i < prevCells.length; i++) {
    emitCellChange(prevCells[i].id);
  }

  return newCells;
}

/** Read the current cells as an ordered array (no subscription). */
export function getNotebookCellsSnapshot(): NotebookCell[] {
  return _cellIds.map((id) => _cellMap.get(id)!);
}

/** Get a single cell by ID (no subscription). */
export function getCellById(id: string): NotebookCell | undefined {
  return _cellMap.get(id);
}

/** Clear all cells. */
export function resetNotebookCells(): void {
  _cellIds = [];
  _cellMap = new Map();
  emitIdsChange();
  emitAllCellChanges();
}
