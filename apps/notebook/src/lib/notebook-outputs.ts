import { useEffect, useMemo, useReducer, useSyncExternalStore } from "react";
import type { JupyterOutput } from "../types";
import {
  getCellExecutionId,
  getExecutionById,
  useCellExecutionId,
  useExecution,
} from "./notebook-executions";

// ---------------------------------------------------------------------------
// Reactive outputs store keyed by `output_id`.
//
// Outputs are the hottest-changing piece of notebook state — a single cell
// can emit hundreds of stream frames per second. The cell store's old model
// carried the full `outputs` array on each `NotebookCell`, so any append
// produced a new cell reference and a full cell subtree re-render.
//
// This store is keyed by the UUIDv4 `output_id` the daemon stamps on every
// output manifest. The `<Output output_id={id}>` component subscribes per
// output: stream appends notify the append target's subscribers only; the
// parent <CellContainer> sees no store change at all.
//
// Responsibilities:
//   _outputMap    — output manifest by output_id
//   subscribers   — per-output set of callbacks
//
// Writers: the frame pipeline, fed by `RuntimeStateSyncApplied.output_changed_ids`
// from the WASM handle. See `frame-pipeline.ts` for the dispatch path.
// ---------------------------------------------------------------------------

const _outputMap: Map<string, JupyterOutput> = new Map();

const _subscribers = new Map<string, Set<() => void>>();

// Coarse version counter bumped on every setOutput / deleteOutput. This
// is intentionally NOT used by `useCellOutputs` (per-cell hooks subscribe
// only to their own output_ids so a noisy cell can't fan re-renders to
// every other cell — see P1 review note). It exists for a narrow set of
// cross-cell derived consumers (NotebookView's hidden-group membership
// and error-count totals) that need to recompute whenever any output in
// the notebook changes.
let _outputsVersion = 0;
const _outputsVersionSubscribers = new Set<() => void>();

function emitOutputChange(output_id: string): void {
  const subs = _subscribers.get(output_id);
  if (subs) {
    for (const cb of subs) {
      try {
        cb();
      } catch {
        // subscriber errors must not break the dispatch loop
      }
    }
  }
  _outputsVersion = (_outputsVersion + 1) | 0;
  for (const cb of _outputsVersionSubscribers) {
    try {
      cb();
    } catch {
      // subscriber errors must not break the dispatch loop
    }
  }
}

/** Singleton returned by `useCellOutputs` when a cell has no outputs.
 *  Having a stable reference avoids downstream re-renders that would
 *  otherwise see a fresh `[]` identity on every mount. */
const EMPTY_OUTPUTS: readonly JupyterOutput[] = Object.freeze([]);

// ── Hooks ───────────────────────────────────────────────────────────────

/** Subscribe to a single output by id. Re-renders only when that output changes. */
export function useOutput(output_id: string): JupyterOutput | undefined {
  const subscribe = useMemo(() => subscribeOutputById(output_id), [output_id]);
  const getSnapshot = useMemo(() => getOutputSnapshot(output_id), [output_id]);
  return useSyncExternalStore(subscribe, getSnapshot);
}

/**
 * Resolve the current output list for a cell.
 *
 * Chains `useCellExecutionId(cell_id)` -> `useExecution(execution_id)` ->
 * `_outputMap[output_id]`. Subscribes only to THIS cell's execution and
 * this cell's `output_ids` (via per-output subscribers) so an output
 * append in another cell does not re-render this cell. Reference-stable:
 * returns the same array identity across renders when nothing this cell
 * depends on has changed.
 *
 * Returns `EMPTY_OUTPUTS` (a frozen singleton) when the cell has no
 * execution yet, so components that start in the "no output" state
 * don't see a fresh `[]` each render.
 */
export function useCellOutputs(cell_id: string): JupyterOutput[] {
  const execution_id = useCellExecutionId(cell_id);
  // Pulls `output_ids` (plus status fingerprint) for this cell's current
  // execution. Snapshot equality in the executions store makes this a
  // no-op re-render when only outputs change without a new output_id
  // landing.
  const execution = useExecution(execution_id);

  // Register per-output subscriptions so appends to any of THIS cell's
  // outputs bump a local tick, re-running the memo below.
  const [tick, bumpTick] = useReducer((x: number) => (x + 1) | 0, 0);
  const output_ids = execution?.output_ids;
  useEffect(() => {
    if (!output_ids || output_ids.length === 0) return;
    const unsubs: Array<() => void> = [];
    for (const id of output_ids) {
      unsubs.push(subscribeOutputById(id)(bumpTick));
    }
    // Fire once in case an output changed between the render and the
    // subscription (rare, but worth guarding).
    bumpTick();
    return () => {
      for (const off of unsubs) off();
    };
  }, [output_ids]);

  return useMemo(() => {
    if (!output_ids || output_ids.length === 0) {
      return EMPTY_OUTPUTS as JupyterOutput[];
    }
    const resolved: JupyterOutput[] = [];
    for (const output_id of output_ids) {
      const out = _outputMap.get(output_id);
      if (out) resolved.push(out);
    }
    if (resolved.length === 0) return EMPTY_OUTPUTS as JupyterOutput[];
    return resolved;
    // `tick` intentionally in deps: per-output subscribers bump it when
    // this cell's outputs mutate. The value isn't read inside.
  }, [output_ids, tick]);
}

// ── Subscription helpers ────────────────────────────────────────────────

function subscribeOutputById(output_id: string): (cb: () => void) => () => void {
  return (callback: () => void) => {
    let subs = _subscribers.get(output_id);
    if (!subs) {
      subs = new Set();
      _subscribers.set(output_id, subs);
    }
    subs.add(callback);
    const set = subs;
    return () => {
      set.delete(callback);
      if (set.size === 0) _subscribers.delete(output_id);
    };
  };
}

function getOutputSnapshot(output_id: string): () => JupyterOutput | undefined {
  return () => _outputMap.get(output_id);
}

// ── Write operations ────────────────────────────────────────────────────

/**
 * Upsert a single output. Notifies only that output's subscribers.
 *
 * This is the store-side counterpart to WASM `get_output_by_id(output_id)`.
 * Resolved manifests flow in here after blob-ref resolution.
 */
export function setOutput(output_id: string, output: JupyterOutput): void {
  const prev = _outputMap.get(output_id);
  if (prev === output) return;
  _outputMap.set(output_id, output);
  emitOutputChange(output_id);
}

/** Remove a single output. Notifies its subscribers with `undefined`. */
export function deleteOutput(output_id: string): void {
  if (!_outputMap.has(output_id)) return;
  _outputMap.delete(output_id);
  emitOutputChange(output_id);
}

/** Bulk drop outputs. Useful on clear_outputs / execution restart. */
export function deleteOutputs(output_ids: Iterable<string>): void {
  for (const id of output_ids) deleteOutput(id);
}

/** Read a single output without subscribing. */
export function getOutputById(output_id: string): JupyterOutput | undefined {
  return _outputMap.get(output_id);
}

/** Reset the entire store. Called on notebook switch or full reset. */
export function resetNotebookOutputs(): void {
  const ids = [..._outputMap.keys()];
  _outputMap.clear();
  for (const id of ids) emitOutputChange(id);
}

// ── Cross-cell derived state ──────────────────────────────────────────

/**
 * Subscribe to a coarse "any output changed" counter.
 *
 * **Do not use from per-cell components** - wiring a cell's render to
 * this counter would fan every output append across the notebook into
 * every cell's render loop. This hook is reserved for cross-cell derived
 * state (hidden-group membership, error-count totals). Individual cell
 * outputs should use `useCellOutputs(cell_id)` which subscribes only to
 * that cell's own output ids.
 */
export function useOutputsVersion(): number {
  return useSyncExternalStore(
    subscribeOutputsVersion,
    getOutputsVersionSnapshot,
  );
}

function subscribeOutputsVersion(cb: () => void): () => void {
  _outputsVersionSubscribers.add(cb);
  return () => {
    _outputsVersionSubscribers.delete(cb);
  };
}

function getOutputsVersionSnapshot(): number {
  return _outputsVersion;
}

// ── Sync helpers for cross-cell derived state ─────────────────────────

/**
 * Snapshot the current outputs for a cell without subscribing.
 *
 * Used by cross-cell readers (hidden-group calculation, drag preview)
 * that recompute on structural / materialization ticks rather than per-
 * output ticks. If those consumers need live updates they should switch
 * to `useCellOutputs`; this helper exists for the ones that just need a
 * value at recompute time.
 */
export function getCellOutputsSnapshot(cell_id: string): JupyterOutput[] {
  const executionId = getCellExecutionId(cell_id);
  if (!executionId) return EMPTY_OUTPUTS as JupyterOutput[];
  const snap = getExecutionById(executionId);
  if (!snap || snap.output_ids.length === 0) {
    return EMPTY_OUTPUTS as JupyterOutput[];
  }
  const resolved: JupyterOutput[] = [];
  for (const oid of snap.output_ids) {
    const out = _outputMap.get(oid);
    if (out) resolved.push(out);
  }
  if (resolved.length === 0) return EMPTY_OUTPUTS as JupyterOutput[];
  return resolved;
}
