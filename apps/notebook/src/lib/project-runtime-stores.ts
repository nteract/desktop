/**
 * Projection glue between the SyncEngine and the per-execution / per-output
 * React stores.
 *
 * Splitting this out of `useAutomergeNotebook` keeps the hook focused on
 * React wiring and avoids pulling the outputs store's imports into the
 * materialization pipeline.
 */

import type { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";
import { refreshBlobPort } from "./blob-port";
import { logger } from "./logger";
import {
  resolveManifest,
  resolveManifestSync,
} from "./manifest-resolution";
import { isOutputManifest } from "./materialize-cells";
import {
  type ExecutionSnapshot,
  deleteExecutions,
  resetNotebookExecutions,
  setCellExecutionPointer,
  setExecution,
} from "./notebook-executions";
import {
  deleteOutputs,
  getOutputById,
  resetNotebookOutputs,
  setOutput,
} from "./notebook-outputs";
import type { JupyterOutput } from "../types";

// ── Executions store projection ──────────────────────────────────────

/**
 * Module-local record of the previous sync's execution_id set. Used to
 * evict entries the daemon has trimmed out of `RuntimeStateDoc`. Without
 * this, old executions accumulate forever in the store.
 */
let _knownExecutionIds: Set<string> = new Set();

/**
 * Previously-seen scalar fingerprint per execution (`status`, count, success,
 * and output-list length). Lets the projection short-circuit on untouched
 * executions instead of rebuilding `output_ids` for every execution on
 * every tick — critical because `runtimeState$` emits once per stream
 * append.
 */
const _prevExecutionFingerprint: Map<string, string> = new Map();

/**
 * Normalize an output's effective id for the per-output store.
 *
 * Most outputs carry a daemon-stamped UUID in `output_id`. Some legacy
 * fixtures (see `packages/runtimed/tests/fixtures`) and in-flight
 * outputs whose manifest is still being built carry an empty string.
 * Those outputs still need a stable key so `useCellOutputs` can resolve
 * them - we synthesize one using the execution id + position.
 */
function effectiveOutputId(
  execution_id: string,
  index: number,
  raw: unknown,
): string {
  if (raw && typeof raw === "object") {
    const oid = (raw as { output_id?: unknown }).output_id;
    if (typeof oid === "string" && oid.length > 0) return oid;
  }
  return `legacy:${execution_id}:${index}`;
}

function collectExecutionOutputIds(
  execution_id: string,
  raw: { outputs?: unknown[] },
): string[] {
  const ids: string[] = [];
  if (Array.isArray(raw.outputs)) {
    for (let i = 0; i < raw.outputs.length; i++) {
      ids.push(effectiveOutputId(execution_id, i, raw.outputs[i]));
    }
  }
  return ids;
}

function executionFingerprint(
  execution_id: string,
  raw: {
    cell_id?: string;
    execution_count?: number | null;
    status?: string;
    success?: boolean | null;
    outputs?: unknown[];
  },
): string {
  // Include the ordered `output_id` list so same-length replacements
  // (e.g. `clear_output(wait=True)` or a remove+add that keeps the list
  // length constant) still invalidate the snapshot. Without this, the
  // outputs store drifts past the execution's canonical pointer list and
  // `useCellOutputs` resolves stale entries.
  const ids = collectExecutionOutputIds(execution_id, raw);
  return `${raw.cell_id ?? ""}|${raw.execution_count ?? ""}|${raw.status ?? ""}|${raw.success ?? ""}|${ids.join(",")}`;
}

function buildExecutionSnapshot(
  execution_id: string,
  raw: {
    cell_id?: string;
    execution_count?: number | null;
    status?: string;
    success?: boolean | null;
    outputs?: unknown[];
  },
): ExecutionSnapshot {
  return {
    cell_id: raw.cell_id ?? "",
    execution_count: raw.execution_count ?? null,
    status: raw.status ?? "",
    success: raw.success ?? null,
    output_ids: collectExecutionOutputIds(execution_id, raw),
  };
}

/**
 * Project the current RuntimeState into the executions store.
 *
 * Runs on every `runtimeState$` tick. Uses a cheap per-execution scalar
 * fingerprint to skip executions that haven't moved — without this, long
 * sessions pay O(total_outputs) JS work on every stream append because
 * the snapshot list is rebuilt from scratch each time.
 *
 * The cell -> execution pointer is NOT derived here. `RuntimeStateDoc`
 * keeps historical executions for each cell, and the iteration order of
 * a JS object built from a Rust `HashMap` is not the execution order.
 * The notebook doc's `cells.{id}.execution_id` is the canonical pointer;
 * it flows through a separate path (see
 * `updateCellExecutionPointersFromHandle`).
 */
export function projectRuntimeStateToExecutions(state: {
  executions?: Record<string, unknown>;
}): void {
  const execs = state.executions;
  const nextIds = new Set<string>();
  if (execs) {
    for (const [execution_id, raw] of Object.entries(execs)) {
      const entry = raw as Parameters<typeof executionFingerprint>[1];
      nextIds.add(execution_id);
      const fp = executionFingerprint(execution_id, entry);
      if (_prevExecutionFingerprint.get(execution_id) === fp) continue;
      _prevExecutionFingerprint.set(execution_id, fp);
      setExecution(execution_id, buildExecutionSnapshot(execution_id, entry));
      // Fallback seeding for outputs that don't carry a non-empty
      // `output_id` (legacy fixtures, in-flight manifests). The
      // `outputIdChanges$` stream only covers real output ids, so
      // without this the outputs store would miss them and
      // `useCellOutputs` would render empty.
      seedLegacyOutputsForExecution(execution_id, entry);
    }
  }

  // Evict executions the daemon dropped. Keeps the store from drifting
  // monotonically larger across long sessions with restart cycles.
  const removed: string[] = [];
  for (const prev of _knownExecutionIds) {
    if (!nextIds.has(prev)) removed.push(prev);
  }
  if (removed.length > 0) {
    deleteExecutions(removed);
    for (const eid of removed) _prevExecutionFingerprint.delete(eid);
  }
  _knownExecutionIds = nextIds;
}

/**
 * Seed the outputs store for an execution whose outputs lack proper
 * `output_id` values. Real daemon-stamped ids flow through the
 * `outputIdChanges$` pipeline; this is the fallback for legacy snapshots
 * and fixtures. We synthesize a deterministic id (`legacy:<exec>:<idx>`)
 * and push the raw manifest through if it's already directly consumable.
 * Anything that needs async blob resolution is skipped - those paths
 * have their own channel already.
 */
function seedLegacyOutputsForExecution(
  execution_id: string,
  raw: { outputs?: unknown[] },
): void {
  if (!Array.isArray(raw.outputs)) return;
  for (let i = 0; i < raw.outputs.length; i++) {
    const output = raw.outputs[i];
    if (!output || typeof output !== "object") continue;
    const oidField = (output as { output_id?: unknown }).output_id;
    if (typeof oidField === "string" && oidField.length > 0) continue;
    const synthesized = `legacy:${execution_id}:${i}`;
    // Plain `JupyterOutput` shape (already resolved) goes straight in.
    // Structured manifests with ContentRefs need the real pipeline, so
    // skip them - they'd arrive through `applyOutputIdChanges` once the
    // daemon stamps a real id.
    if ("output_type" in output && !isOutputManifest(output)) {
      const existing = getOutputById(synthesized);
      if (existing === output) continue;
      setOutput(synthesized, output as JupyterOutput);
    }
  }
}

/**
 * Re-read every cell's canonical `execution_id` pointer from the WASM
 * handle and update the per-cell pointer store. Call this whenever the
 * notebook doc heads move so `useCellExecutionId(cellId)` reflects the
 * cell's actual current execution rather than whichever RuntimeStateDoc
 * entry happened to land in the store last.
 */
export function updateCellExecutionPointersFromHandle(
  handle: NotebookHandle,
  cell_ids: string[],
): void {
  for (const cellId of cell_ids) {
    const eid = handle.get_cell_execution_id(cellId) ?? null;
    setCellExecutionPointer(cellId, eid);
  }
}

// ── Outputs store projection ─────────────────────────────────────────

/**
 * Resolve and push a batch of `output_id`s into the outputs store.
 *
 * Reads each output from the WASM handle (narrows to the active MIME
 * priority set), resolves any blob ContentRefs, and writes the result to
 * the per-output store. Cache hits take the sync path; misses go through
 * `resolveManifest` (one blob fetch per text ref; binary refs become URLs).
 *
 * Unknown output_ids (handle returns undefined) are skipped. Removed output
 * IDs are dropped from the store via `deleteOutputs`.
 */
export async function applyOutputIdChanges(
  changed_ids: string[],
  removed_ids: string[],
  state: {
    executions?: Record<string, { outputs?: unknown[] }>;
  },
  blobPort: number | null,
  cache: Map<string, JupyterOutput>,
): Promise<void> {
  if (removed_ids.length > 0) {
    deleteOutputs(removed_ids);
  }
  if (changed_ids.length === 0) return;

  // Pluck the changed manifests out of the RuntimeState snapshot we
  // already have in hand. Avoids `handle.get_output_by_id()` per id,
  // which would re-clone and walk the entire state doc each call.
  const byId = indexOutputsById(state);
  const pending: Array<{ output_id: string; raw: unknown }> = [];
  for (const output_id of changed_ids) {
    const raw = byId.get(output_id);
    if (raw === undefined) continue;
    pending.push({ output_id, raw });
  }

  // `output_changed_ids` only fires when an output's manifest changes. If
  // the first stream/error lands before blob-port discovery resolves, we
  // must resolve the port on-demand here — otherwise manifest-backed
  // outputs silently disappear from the store until the next append.
  let port = blobPort;
  if (port === null && pending.some(({ raw }) => isOutputManifest(raw))) {
    port = await refreshBlobPort();
  }

  for (const { output_id, raw } of pending) {
    const sync = tryResolveSync(raw, port, cache);
    if (sync) {
      setOutput(output_id, sync);
    } else if (port !== null) {
      try {
        const resolved = await resolveManifest(
          raw as Parameters<typeof resolveManifest>[0],
          port,
        );
        setOutput(output_id, resolved);
      } catch (err) {
        logger.warn(
          `[outputs-store] Failed to resolve output ${output_id}:`,
          err,
        );
      }
    } else {
      logger.warn(
        `[outputs-store] blob port unavailable; deferring output ${output_id}`,
      );
    }
  }
}

/**
 * Flat `output_id -> manifest` map built from a RuntimeState snapshot.
 *
 * Walks every execution's outputs once per tick. Used by the outputs-store
 * projection and by `applyOutputIdChanges` to avoid per-id WASM reads.
 */
function indexOutputsById(state: {
  executions?: Record<string, { outputs?: unknown[] }>;
}): Map<string, unknown> {
  const result = new Map<string, unknown>();
  const execs = state.executions;
  if (!execs) return result;
  for (const raw of Object.values(execs)) {
    const outputs = (raw as { outputs?: unknown[] }).outputs;
    if (!Array.isArray(outputs)) continue;
    for (const output of outputs) {
      if (output && typeof output === "object") {
        const oid = (output as { output_id?: unknown }).output_id;
        if (typeof oid === "string" && oid.length > 0) {
          result.set(oid, output);
        }
      }
    }
  }
  return result;
}

function tryResolveSync(
  raw: unknown,
  blobPort: number | null,
  _cache: Map<string, JupyterOutput>,
): JupyterOutput | null {
  if (isOutputManifest(raw)) {
    if (blobPort === null) return null;
    return resolveManifestSync(raw, blobPort);
  }
  // Plain JupyterOutput object — no refs, no resolution needed.
  if (typeof raw === "object" && raw !== null && "output_type" in raw) {
    return raw as JupyterOutput;
  }
  if (typeof raw === "string") {
    try {
      return JSON.parse(raw) as JupyterOutput;
    } catch {
      return null;
    }
  }
  return null;
}

export function resetRuntimeStoresProjection(): void {
  _knownExecutionIds = new Set();
  _prevExecutionFingerprint.clear();
  resetNotebookExecutions();
  resetNotebookOutputs();
}
