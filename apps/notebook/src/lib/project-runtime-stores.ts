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
import { deleteOutputs, resetNotebookOutputs, setOutput } from "./notebook-outputs";
import type { JupyterOutput } from "../types";

// ── Executions store projection ──────────────────────────────────────

/**
 * Module-local record of the previous sync's execution_id set. Used to
 * evict entries the daemon has trimmed out of `RuntimeStateDoc`. Without
 * this, old executions accumulate forever in the store.
 */
let _knownExecutionIds: Set<string> = new Set();

/**
 * Project the current RuntimeState into the executions store.
 *
 * Runs on every `runtimeState$` tick. Writes are idempotent and only notify
 * subscribers when a snapshot actually changed (see `setExecution`). Iterating
 * the full executions map is O(executions) per tick — kept tight so the
 * snapshot rate can stay high.
 *
 * The cell -> execution pointer is NOT derived here. `RuntimeStateDoc` keeps
 * historical executions for each cell, and the iteration order of a JS object
 * built from a Rust `HashMap` is not the execution order. The notebook doc's
 * `cells.{id}.execution_id` is the canonical pointer; it flows through a
 * separate path (see `updateCellExecutionPointersFromHandle`).
 */
export function projectRuntimeStateToExecutions(state: {
  executions?: Record<string, unknown>;
}): void {
  const execs = state.executions;
  const nextIds = new Set<string>();
  if (execs) {
    for (const [execution_id, raw] of Object.entries(execs)) {
      const entry = raw as {
        cell_id?: string;
        execution_count?: number | null;
        status?: string;
        success?: boolean | null;
        outputs?: unknown[];
      };
      const output_ids: string[] = [];
      if (Array.isArray(entry.outputs)) {
        for (const output of entry.outputs) {
          if (output && typeof output === "object") {
            const oid = (output as { output_id?: unknown }).output_id;
            if (typeof oid === "string" && oid.length > 0) {
              output_ids.push(oid);
            }
          }
        }
      }
      const snap: ExecutionSnapshot = {
        cell_id: entry.cell_id ?? "",
        execution_count: entry.execution_count ?? null,
        status: entry.status ?? "",
        success: entry.success ?? null,
        output_ids,
      };
      setExecution(execution_id, snap);
      nextIds.add(execution_id);
    }
  }

  // Evict executions the daemon dropped. Keeps the store from drifting
  // monotonically larger across long sessions with restart cycles.
  const removed: string[] = [];
  for (const prev of _knownExecutionIds) {
    if (!nextIds.has(prev)) removed.push(prev);
  }
  if (removed.length > 0) deleteExecutions(removed);
  _knownExecutionIds = nextIds;
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
  handle: NotebookHandle | null,
  changed_ids: string[],
  removed_ids: string[],
  blobPort: number | null,
  cache: Map<string, JupyterOutput>,
): Promise<void> {
  if (removed_ids.length > 0) {
    deleteOutputs(removed_ids);
  }
  if (!handle || changed_ids.length === 0) return;

  // Fetch raw manifests synchronously — cheap WASM call, lets us fast-path
  // cache hits without awaiting.
  const pending: Array<{ output_id: string; raw: unknown }> = [];
  for (const output_id of changed_ids) {
    const raw = handle.get_output_by_id(output_id);
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
  resetNotebookExecutions();
  resetNotebookOutputs();
}
