/**
 * Projection glue between the SyncEngine and the per-execution / per-output
 * React stores.
 *
 * Splitting this out of `useAutomergeNotebook` keeps the hook focused on
 * React wiring and avoids pulling the outputs store's imports into the
 * materialization pipeline.
 *
 * `output_id` is a daemon-side invariant — `create_manifest` always
 * stamps one (and `outputs_to_manifest_refs` stamps a fallback on the
 * error path). Every output that reaches the frontend has a real id, so
 * there is no synthetic `legacy:<eid>:<idx>` key in these stores.
 */

import {
  buildRuntimeExecutionSnapshot,
  collectOutputIds,
  executionFingerprint,
  type RuntimeState,
} from "runtimed";
import type { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";
import { getBlobPort, refreshBlobPort } from "./blob-port";
import { logger } from "./logger";
import {
  type OutputManifest,
  resolveManifest,
  resolveManifestSync,
} from "./manifest-resolution";
import { isOutputManifest } from "./materialize-cells";
import {
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
 * and output_id list). Lets the projection short-circuit on untouched
 * executions instead of rebuilding `output_ids` for every execution on
 * every tick — critical because `runtimeState$` emits once per stream
 * append.
 */
const _prevExecutionFingerprint: Map<string, string> = new Map();

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
export function projectRuntimeStateToExecutions(state: RuntimeState): void {
  const nextIds = new Set<string>();
  for (const [execution_id, entry] of Object.entries(state.executions)) {
    nextIds.add(execution_id);
    const fp = executionFingerprint(entry);
    if (_prevExecutionFingerprint.get(execution_id) === fp) continue;
    _prevExecutionFingerprint.set(execution_id, fp);
    setExecution(execution_id, buildRuntimeExecutionSnapshot(entry));
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
 * Seed the outputs / executions stores directly from the WASM handle.
 *
 * `initialSyncComplete$` can fire before the RuntimeStateDoc sync frame
 * lands, so `projectRuntimeStateToExecutions` alone may run against an
 * empty snapshot on notebook open. Walking the handle for each cell's
 * current `execution_id` fills the gap - any outputs already stored in
 * the notebook doc show up in `useCellOutputs` immediately rather than
 * waiting for the runtime-state catch-up tick.
 */
export function seedOutputStoresFromHandle(
  handle: NotebookHandle,
  cell_ids: string[],
): void {
  for (const cellId of cell_ids) {
    const executionId = handle.get_cell_execution_id(cellId) ?? null;
    if (!executionId) continue;
    const rawOutputs = (handle.get_cell_outputs(cellId) as unknown[]) ?? [];
    if (rawOutputs.length === 0) continue;

    const output_ids = collectOutputIds(rawOutputs);
    if (output_ids.length === 0) continue;

    // Build a minimal execution snapshot. We don't have status / success
    // in the notebook doc (those live in RuntimeStateDoc) - fill them in
    // as defaults so `useExecution` resolves to something; the runtime-
    // state projection will overwrite with authoritative values when the
    // doc's sync frame lands.
    setExecution(executionId, {
      cell_id: cellId,
      execution_count: null,
      status: "done",
      success: null,
      output_ids,
    });

    // Populate the outputs store for each output. Plain JupyterOutputs
    // (already resolved by full materialization) go in directly;
    // manifests kick through the sync resolver.
    for (let i = 0; i < rawOutputs.length; i++) {
      const output = rawOutputs[i];
      if (!output || typeof output !== "object") continue;
      const oid = output_ids[i];
      if (!oid) continue;
      const existing = getOutputById(oid);
      if (existing === output) continue;
      const sync = tryResolveSync(output, getBlobPort());
      if (sync) setOutput(oid, sync);
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
 * Apply the per-output changeset emitted by WASM.
 *
 * `changed` carries `(output_id, narrowed_manifest)` pairs — manifests
 * are already MIME-narrowed and have binary ContentRefs resolved to
 * `{url}` variants. The only remaining work here is fetching text blob
 * refs (which still require an HTTP round trip) before handing the
 * fully-resolved `JupyterOutput` to the store.
 *
 * Removed output_ids are dropped from the store.
 */
export async function applyOutputChangeset(
  changed: Array<[string, unknown]>,
  removed_ids: string[],
): Promise<void> {
  if (removed_ids.length > 0) {
    deleteOutputs(removed_ids);
  }
  if (changed.length === 0) return;

  // Stream/display-update manifests with text blob refs need the blob
  // port to resolve. Fetch it on demand so a race between the first
  // manifest and blob-port discovery doesn't drop outputs on the floor.
  let port = getBlobPort();
  if (port === null && changed.some(([, raw]) => isOutputManifest(raw))) {
    port = await refreshBlobPort();
  }

  for (const [output_id, raw] of changed) {
    const sync = tryResolveSync(raw, port);
    if (sync) {
      setOutput(output_id, sync);
      continue;
    }
    if (port === null) {
      logger.warn(
        `[outputs-store] blob port unavailable; deferring output ${output_id}`,
      );
      continue;
    }
    if (!isOutputManifest(raw)) continue;
    try {
      const resolved = await resolveManifest(raw, port);
      setOutput(output_id, resolved);
    } catch (err) {
      logger.warn(
        `[outputs-store] Failed to resolve output ${output_id}:`,
        err,
      );
    }
  }
}

function tryResolveSync(
  raw: unknown,
  blobPort: number | null,
): JupyterOutput | null {
  if (isOutputManifest(raw)) {
    if (blobPort === null) return null;
    return resolveManifestSync(raw as OutputManifest, blobPort);
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
