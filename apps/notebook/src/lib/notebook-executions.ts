import { useMemo, useSyncExternalStore } from "react";

// ---------------------------------------------------------------------------
// Reactive executions store keyed by `execution_id`.
//
// Kyle's direction: cells associate with an `execution_id`; everything about
// THIS execution (count, status, output_ids, success, error) lives in this
// store. `In[N]` and `Out[N]` derive from the execution snapshot so they
// stay correct across multi-execution scenarios (restart, re-run, etc.).
//
// The WASM handle feeds the store via `get_execution_by_id(execution_id)`,
// which returns the small summary below. Full output manifests live in the
// outputs store (keyed by `output_id`) — outputs_ids is a pointer list.
// ---------------------------------------------------------------------------

export interface ExecutionSnapshot {
  cell_id: string;
  execution_count: number | null;
  /** "queued" | "running" | "done" | "error" */
  status: string;
  /** true/false after the execution finishes, null while still running. */
  success: boolean | null;
  /** Output IDs in emission order. Resolve via `useOutput(output_id)`. */
  output_ids: string[];
}

const _executionMap: Map<string, ExecutionSnapshot> = new Map();

/** Per-cell reverse index: cell_id -> latest execution_id. */
const _cellToExecution: Map<string, string> = new Map();

const _subscribers = new Map<string, Set<() => void>>();
const _cellExecutionSubscribers = new Map<string, Set<() => void>>();

function emitExecutionChange(execution_id: string): void {
  const subs = _subscribers.get(execution_id);
  if (!subs) return;
  for (const cb of subs) {
    try {
      cb();
    } catch {
      // subscriber errors must not break the dispatch loop
    }
  }
}

function emitCellExecutionPointerChange(cell_id: string): void {
  const subs = _cellExecutionSubscribers.get(cell_id);
  if (!subs) return;
  for (const cb of subs) {
    try {
      cb();
    } catch {}
  }
}

function snapshotsEqual(a: ExecutionSnapshot, b: ExecutionSnapshot): boolean {
  if (a === b) return true;
  if (
    a.cell_id !== b.cell_id ||
    a.execution_count !== b.execution_count ||
    a.status !== b.status ||
    a.success !== b.success
  ) {
    return false;
  }
  if (a.output_ids.length !== b.output_ids.length) return false;
  for (let i = 0; i < a.output_ids.length; i++) {
    if (a.output_ids[i] !== b.output_ids[i]) return false;
  }
  return true;
}

// ── Hooks ───────────────────────────────────────────────────────────────

/** Subscribe to a single execution by id. Re-renders only when it changes. */
export function useExecution(
  execution_id: string | null,
): ExecutionSnapshot | undefined {
  const id = execution_id ?? "";
  const subscribe = useMemo(() => subscribeExecutionById(id), [id]);
  const getSnapshot = useMemo(() => getExecutionSnapshotGetter(id), [id]);
  return useSyncExternalStore(subscribe, getSnapshot);
}

/**
 * Subscribe to the `execution_id` pointer for a cell.
 *
 * Re-renders when the cell transitions from one execution to the next
 * (e.g. fresh run). Useful for `<CellLabel>` / prompt components that need
 * to chain `useCellExecutionId(cellId)` → `useExecution(execution_id)`.
 */
export function useCellExecutionId(cell_id: string): string | null {
  const subscribe = useMemo(() => subscribeCellExecutionPointer(cell_id), [cell_id]);
  const getSnapshot = useMemo(() => getCellExecutionIdGetter(cell_id), [cell_id]);
  return useSyncExternalStore(subscribe, getSnapshot);
}

// ── Subscription helpers ────────────────────────────────────────────────

function subscribeExecutionById(
  execution_id: string,
): (cb: () => void) => () => void {
  return (callback: () => void) => {
    let subs = _subscribers.get(execution_id);
    if (!subs) {
      subs = new Set();
      _subscribers.set(execution_id, subs);
    }
    const set = subs;
    set.add(callback);
    return () => {
      set.delete(callback);
      if (set.size === 0) _subscribers.delete(execution_id);
    };
  };
}

function getExecutionSnapshotGetter(
  execution_id: string,
): () => ExecutionSnapshot | undefined {
  return () => (execution_id ? _executionMap.get(execution_id) : undefined);
}

function subscribeCellExecutionPointer(
  cell_id: string,
): (cb: () => void) => () => void {
  return (callback: () => void) => {
    let subs = _cellExecutionSubscribers.get(cell_id);
    if (!subs) {
      subs = new Set();
      _cellExecutionSubscribers.set(cell_id, subs);
    }
    const set = subs;
    set.add(callback);
    return () => {
      set.delete(callback);
      if (set.size === 0) _cellExecutionSubscribers.delete(cell_id);
    };
  };
}

function getCellExecutionIdGetter(cell_id: string): () => string | null {
  return () => _cellToExecution.get(cell_id) ?? null;
}

// ── Write operations ────────────────────────────────────────────────────

/**
 * Upsert an execution snapshot. Notifies only that execution's subscribers.
 *
 * Does NOT update the cell -> execution pointer. `RuntimeStateDoc` keeps
 * historical executions per cell, so iterating it cannot reliably pick
 * the current one. Callers set the cell pointer explicitly via
 * `setCellExecutionPointer`, driven by the canonical `cells/{id}/execution_id`
 * field in the notebook doc.
 */
export function setExecution(
  execution_id: string,
  snap: ExecutionSnapshot,
): void {
  const prev = _executionMap.get(execution_id);
  if (prev && snapshotsEqual(prev, snap)) return;
  _executionMap.set(execution_id, snap);
  emitExecutionChange(execution_id);
}

/**
 * Explicitly set the active execution_id for a cell.
 *
 * Used on execution_started broadcasts and on clearOutputs (pass null).
 */
export function setCellExecutionPointer(
  cell_id: string,
  execution_id: string | null,
): void {
  const prev = _cellToExecution.get(cell_id) ?? null;
  if (prev === execution_id) return;
  if (execution_id === null) {
    _cellToExecution.delete(cell_id);
  } else {
    _cellToExecution.set(cell_id, execution_id);
  }
  emitCellExecutionPointerChange(cell_id);
}

/**
 * Drop a batch of executions from the store.
 *
 * Notifies per-execution subscribers with `undefined` and clears any cell
 * pointer that referenced one of the removed ids. Used by the projection
 * when the daemon trims an execution out of `RuntimeStateDoc` so the
 * store doesn't drift monotonically larger.
 */
export function deleteExecutions(execution_ids: Iterable<string>): void {
  for (const eid of execution_ids) {
    const snap = _executionMap.get(eid);
    if (!_executionMap.delete(eid)) continue;
    emitExecutionChange(eid);
    if (snap && _cellToExecution.get(snap.cell_id) === eid) {
      _cellToExecution.delete(snap.cell_id);
      emitCellExecutionPointerChange(snap.cell_id);
    }
  }
}

/** Read a snapshot without subscribing. */
export function getExecutionById(
  execution_id: string,
): ExecutionSnapshot | undefined {
  return _executionMap.get(execution_id);
}

/** Read the cell's current execution_id without subscribing. */
export function getCellExecutionId(cell_id: string): string | null {
  return _cellToExecution.get(cell_id) ?? null;
}

/** Reset the entire store. Called on notebook switch or full reset. */
export function resetNotebookExecutions(): void {
  const eids = [..._executionMap.keys()];
  const cells = [..._cellToExecution.keys()];
  _executionMap.clear();
  _cellToExecution.clear();
  for (const eid of eids) emitExecutionChange(eid);
  for (const cid of cells) emitCellExecutionPointerChange(cid);
}
