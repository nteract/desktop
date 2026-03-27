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

// Source edit version — bumps on ANY cell source change (including
// updateCellById). Used by cross-cell features like global find that
// need to recompute when any source text changes.
let _sourceVersion = 0;
const _sourceSubscribers = new Set<() => void>();

function emitIdsChange(): void {
  for (const cb of _idsSubscribers) cb();
}

function emitMaterializeChange(): void {
  _materializeVersion++;
  for (const cb of _materializeSubscribers) cb();
}

function emitSourceChange(): void {
  _sourceVersion++;
  for (const cb of _sourceSubscribers) cb();
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

/**
 * Subscribe to source edit version. Re-renders on ANY source change —
 * both per-cell (updateCellById) and full-array (replace/update).
 * Use for features like global find that need to recompute across all cells.
 */
export function useSourceVersion(): number {
  return useSyncExternalStore(subscribeSource, getSourceSnapshot);
}

// ── Subscription helpers ────────────────────────────────────────────────

/** Subscribe to structural changes (add/delete/move). Exported for cell-ui-state neighbor hooks. */
export function subscribeIds(callback: () => void): () => void {
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

function subscribeSource(callback: () => void): () => void {
  _sourceSubscribers.add(callback);
  return () => _sourceSubscribers.delete(callback);
}

function getSourceSnapshot(): number {
  return _sourceVersion;
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

// ── Cell comparison ─────────────────────────────────────────────────────

function shallowRecordEqual(
  a: Record<string, unknown> | undefined,
  b: Record<string, unknown> | undefined,
): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  const keysA = Object.keys(a);
  const keysB = Object.keys(b);
  if (keysA.length !== keysB.length) return false;
  for (const key of keysA) {
    if (a[key] !== b[key]) return false;
  }
  return true;
}

function cellsEqual(a: NotebookCell, b: NotebookCell): boolean {
  if (a === b) return true;
  if (a.cell_type !== b.cell_type) return false;
  if (a.source !== b.source) return false;
  // Metadata can contain nested objects (e.g. metadata.jupyter = { source_hidden: true })
  // that get new references on every WASM deserialization. JSON.stringify handles
  // arbitrary nesting; WASM serialization produces consistent key ordering.
  if (JSON.stringify(a.metadata) !== JSON.stringify(b.metadata)) return false;

  // cell_type-specific fields
  if (a.cell_type === "code") {
    const bc = b as typeof a;
    if (a.execution_count !== bc.execution_count) return false;
    // outputs — same length and each element referentially equal
    if (a.outputs.length !== bc.outputs.length) return false;
    for (let i = 0; i < a.outputs.length; i++) {
      if (a.outputs[i] !== bc.outputs[i]) return false;
    }
  } else if (a.cell_type === "markdown") {
    const bm = b as typeof a;
    if (
      !shallowRecordEqual(
        a.resolvedAssets as Record<string, unknown> | undefined,
        bm.resolvedAssets as Record<string, unknown> | undefined,
      )
    )
      return false;
  }

  return true;
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
  if (cellsEqual(cell, updated)) return;
  _cellMap.set(id, updated);
  emitCellChange(id);
  if (updated.source !== cell.source) {
    emitSourceChange();
  }
}

/**
 * Replace all cells (full materialization from WASM/sync).
 * Diffs each cell against the previous snapshot so only changed cells
 * trigger per-cell subscriber notifications.
 */
export function replaceNotebookCells(cells: NotebookCell[]): void {
  const newIds = cells.map((c) => c.id);
  const idsChanged =
    newIds.length !== _cellIds.length ||
    newIds.some((id, i) => id !== _cellIds[i]);

  const oldIds = _cellIds;
  const newMap = new Map<string, NotebookCell>();
  const changedIds: string[] = [];
  let anySourceChanged = false;

  for (const cell of cells) {
    const prev = _cellMap.get(cell.id);
    if (prev && cellsEqual(prev, cell)) {
      // Structurally identical — preserve old reference, skip notification
      newMap.set(cell.id, prev);
    } else {
      // Even when the cell changed (e.g. metadata), preserve the output
      // array reference if every element is identical. This prevents
      // OutputArea's handleFrameReady from firing and re-rendering iframes.
      let stored = cell;
      if (prev && prev.cell_type === "code" && cell.cell_type === "code") {
        const prevOutputs = prev.outputs;
        if (
          prevOutputs.length === cell.outputs.length &&
          prevOutputs.every((o, i) => o === cell.outputs[i])
        ) {
          stored = { ...cell, outputs: prevOutputs };
        }
      }
      newMap.set(stored.id, stored);
      changedIds.push(cell.id);
      if (!prev || prev.source !== cell.source) {
        anySourceChanged = true;
      }
    }
  }

  _cellMap = newMap;

  if (idsChanged) {
    _cellIds = newIds;
    emitIdsChange();
  }

  emitMaterializeChange();

  if (anySourceChanged) {
    emitSourceChange();
  }

  // Emit for cells that actually changed
  for (const id of changedIds) {
    emitCellChange(id);
  }

  // Emit for removed cells (present in old list but absent from new map)
  const newIdSet = new Set(newIds);
  for (const id of oldIds) {
    if (!newIdSet.has(id)) {
      emitCellChange(id);
    }
  }
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
  emitSourceChange();

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

/** Read the current cell ID list (no subscription). */
export function getCellIdsSnapshot(): string[] {
  return _cellIds;
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
