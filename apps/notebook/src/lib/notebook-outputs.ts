import { useMemo, useSyncExternalStore } from "react";
import type { JupyterOutput } from "../types";

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

function emitOutputChange(output_id: string): void {
  const subs = _subscribers.get(output_id);
  if (!subs) return;
  for (const cb of subs) {
    try {
      cb();
    } catch {
      // subscriber errors must not break the dispatch loop
    }
  }
}

// ── Hooks ───────────────────────────────────────────────────────────────

/** Subscribe to a single output by id. Re-renders only when that output changes. */
export function useOutput(output_id: string): JupyterOutput | undefined {
  const subscribe = useMemo(() => subscribeOutputById(output_id), [output_id]);
  const getSnapshot = useMemo(() => getOutputSnapshot(output_id), [output_id]);
  return useSyncExternalStore(subscribe, getSnapshot);
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
