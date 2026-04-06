import { computeRequiredPlugins } from "@/lib/renderer-plugins";
import type { JupyterOutput, NotebookCell } from "../types";
import type { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";
import { logger } from "./logger";
import {
  isOutputManifest,
  resolveManifest,
  resolveManifestSync,
} from "./manifest-resolution";
import { getExecutionCountForCell, getRuntimeState } from "./runtime-state";

export type { ContentRef, OutputManifest } from "./manifest-resolution";
// Re-export shared manifest types and functions for downstream consumers
export {
  isOutputManifest,
  resolveContentRef,
  resolveDataBundle,
  resolveManifest,
  resolveManifestSync,
} from "./manifest-resolution";

/** Resolve execution_count for a cell from the current RuntimeState snapshot. */
function getExecutionCountFromRuntime(cellId: string): number | null {
  return getExecutionCountForCell(getRuntimeState(), cellId);
}

/**
 * Compute a stable cache key for an output value.
 *
 * - Objects are serialized to JSON (stable across WASM calls since key
 *   ordering comes from serde).
 * - Strings are used directly (legacy JSON or other string values).
 */
export function outputCacheKey(output: unknown): string {
  if (typeof output === "string") return output;
  try {
    return JSON.stringify(output);
  } catch {
    return String(output);
  }
}

/**
 * Snapshot of a cell from the Automerge document.
 * Matches the Rust CellSnapshot struct used by both the Tauri sync client
 * and the runtimed-wasm NotebookHandle.
 */
export interface CellSnapshot {
  id: string;
  cell_type: string;
  position: string; // Fractional index hex string for ordering (e.g., "80", "7F80")
  source: string;
  execution_count: string; // "5" or "null"
  outputs: unknown[]; // Structured output manifests (from WASM) or legacy JSON strings
  metadata: Record<string, unknown>; // Cell metadata (arbitrary JSON object)
  resolved_assets?: Record<string, string>; // asset ref → blob hash (markdown cells)
}

/**
 * Resolve a single output — either a structured manifest object, a raw
 * JupyterOutput object, or a legacy JSON string.
 *
 * - If cached, returns the cached value.
 * - If a structured OutputManifest (has ContentRef data), resolves via
 *   resolveManifest (may fetch blobs).
 * - If an object with output_type but no ContentRefs, treats as raw
 *   JupyterOutput.
 * - If a string, parses as JSON (legacy fallback).
 */
export async function resolveOutput(
  output: unknown,
  blobPort: number | null,
  cache: Map<string, JupyterOutput>,
): Promise<JupyterOutput | null> {
  const key = outputCacheKey(output);
  const cached = cache.get(key);
  if (cached) return cached;

  // Structured manifest from WASM — resolve ContentRefs
  if (isOutputManifest(output)) {
    if (blobPort === null) {
      logger.warn("[materialize-cells] Manifest object but no blob port");
      return null;
    }
    try {
      const resolved = await resolveManifest(output, blobPort);
      cache.set(key, resolved);
      return resolved;
    } catch (e) {
      logger.warn("[materialize-cells] Failed to resolve manifest:", e);
      return null;
    }
  }

  // Object with output_type but no ContentRefs — already a raw JupyterOutput
  if (
    typeof output === "object" &&
    output !== null &&
    "output_type" in output
  ) {
    const jupyterOutput = output as JupyterOutput;
    cache.set(key, jupyterOutput);
    return jupyterOutput;
  }

  // Legacy string path (backward compat during transition)
  if (typeof output === "string") {
    try {
      const parsed = JSON.parse(output) as JupyterOutput;
      cache.set(key, parsed);
      return parsed;
    } catch {
      logger.warn("[materialize-cells] Failed to parse output JSON");
      return null;
    }
  }

  logger.warn("[materialize-cells] Unrecognized output type:", typeof output);
  return null;
}

/**
 * Synchronously resolve a single output from cache or inline manifests.
 *
 * Returns null if the output requires async blob fetches (will be
 * resolved on the next async materialization pass).
 */
function resolveOutputSync(
  output: unknown,
  blobPort: number | null,
  cache: Map<string, JupyterOutput>,
): JupyterOutput | null {
  const key = outputCacheKey(output);
  const cached = cache.get(key);
  if (cached) return cached;

  // Structured manifest — try sync resolution (inline + binary URL only)
  if (isOutputManifest(output)) {
    if (blobPort === null) return null;
    const resolved = resolveManifestSync(output, blobPort);
    if (resolved) {
      cache.set(key, resolved);
      return resolved;
    }
    // Has text blob refs — needs async resolution
    logger.debug(
      "[materialize-cells] Manifest needs async resolution (has blob refs)",
    );
    return null;
  }

  // Object with output_type — already a raw JupyterOutput
  if (
    typeof output === "object" &&
    output !== null &&
    "output_type" in output
  ) {
    const jupyterOutput = output as JupyterOutput;
    cache.set(key, jupyterOutput);
    return jupyterOutput;
  }

  // Legacy string path
  if (typeof output === "string") {
    try {
      const parsed = JSON.parse(output) as JupyterOutput;
      cache.set(key, parsed);
      return parsed;
    } catch {
      return null;
    }
  }

  return null;
}

/**
 * Return the previous outputs array if every element is referentially
 * identical to the resolved outputs. This lets `cellsEqual()` short-circuit
 * on `===` checks and skip React re-renders for cells whose outputs
 * haven't actually changed (all cache hits, same order, same length).
 */
export function reuseOutputsIfUnchanged(
  resolvedOutputs: JupyterOutput[],
  previousOutputs: JupyterOutput[] | undefined,
): JupyterOutput[] {
  if (
    previousOutputs &&
    previousOutputs.length === resolvedOutputs.length &&
    previousOutputs.every((o, i) => o === resolvedOutputs[i])
  ) {
    return previousOutputs;
  }
  return resolvedOutputs;
}

/**
 * Synchronous cell materialization for local mutations.
 *
 * Uses cache-only output resolution (no blob fetches). Safe to call when:
 * - Adding new cells (outputs are empty)
 * - Deleting cells (no new outputs)
 * - Moving cells (no new outputs)
 * - Updating source (outputs unchanged)
 *
 * For daemon sync with potentially new blob refs, use cellSnapshotsToNotebookCells().
 */
export function cellSnapshotsToNotebookCellsSync(
  snapshots: CellSnapshot[],
  cache: Map<string, JupyterOutput>,
  blobPort?: number | null,
): NotebookCell[] {
  return snapshots.map((snap) => {
    const executionCount =
      snap.execution_count === "null"
        ? null
        : Number.parseInt(snap.execution_count, 10);

    const metadata = snap.metadata ?? {};

    if (snap.cell_type === "code") {
      // Resolve outputs from cache or sync-resolvable manifests
      const resolvedOutputs = snap.outputs
        .map((output) => resolveOutputSync(output, blobPort ?? null, cache))
        .filter((o): o is JupyterOutput => o !== null);

      // Resolve execution_count from RuntimeState (source of truth) rather
      // than the stale NotebookDoc field which is always "null".
      const runtimeCount = getExecutionCountFromRuntime(snap.id);
      const ec =
        runtimeCount ?? (Number.isNaN(executionCount) ? null : executionCount);

      return {
        id: snap.id,
        cell_type: "code" as const,
        source: snap.source,
        execution_count: ec,
        outputs: resolvedOutputs,
        requiredPlugins: computeRequiredPlugins(resolvedOutputs),
        metadata,
      };
    }

    if (snap.cell_type === "markdown") {
      return {
        id: snap.id,
        cell_type: "markdown" as const,
        source: snap.source,
        metadata,
        resolvedAssets: snap.resolved_assets,
      };
    }

    return {
      id: snap.id,
      cell_type: "raw" as const,
      source: snap.source,
      metadata,
    };
  });
}

/**
 * Convert CellSnapshots to NotebookCells, resolving manifest content refs.
 *
 * This is the primary materialization function shared between `useNotebook`
 * (which receives CellSnapshots from the Tauri sync client) and
 * `useAutomergeNotebook` (which reads them from the WASM NotebookHandle).
 */
export async function cellSnapshotsToNotebookCells(
  snapshots: CellSnapshot[],
  blobPort: number | null,
  cache: Map<string, JupyterOutput>,
): Promise<NotebookCell[]> {
  return Promise.all(
    snapshots.map(async (snap) => {
      const executionCount =
        snap.execution_count === "null"
          ? null
          : Number.parseInt(snap.execution_count, 10);

      // Metadata defaults to empty object if missing (backward compatibility)
      const metadata = snap.metadata ?? {};

      if (snap.cell_type === "code") {
        // Resolve all outputs (structured manifests or legacy JSON strings)
        const resolvedOutputs = (
          await Promise.all(
            snap.outputs.map((o) => resolveOutput(o, blobPort, cache)),
          )
        ).filter((o): o is JupyterOutput => o !== null);

        // Resolve execution_count from RuntimeState (source of truth)
        const runtimeCount = getExecutionCountFromRuntime(snap.id);
        const ec =
          runtimeCount ??
          (Number.isNaN(executionCount) ? null : executionCount);

        return {
          id: snap.id,
          cell_type: "code" as const,
          source: snap.source,
          execution_count: ec,
          outputs: resolvedOutputs,
          requiredPlugins: computeRequiredPlugins(resolvedOutputs),
          metadata,
        };
      }

      // markdown or raw
      if (snap.cell_type === "markdown") {
        return {
          id: snap.id,
          cell_type: "markdown" as const,
          source: snap.source,
          metadata,
          resolvedAssets: snap.resolved_assets,
        };
      }

      return {
        id: snap.id,
        cell_type: "raw" as const,
        source: snap.source,
        metadata,
      };
    }),
  );
}

/**
 * Read a single cell from the WASM handle and convert to NotebookCell.
 *
 * Uses per-cell WASM accessors (O(1) doc lookups) instead of serializing
 * the entire document. Output resolution uses cache + sync-resolvable
 * manifests only (no blob fetches).
 */
export function materializeCellFromWasm(
  handle: NotebookHandle,
  cellId: string,
  cache: Map<string, JupyterOutput>,
  previousCell?: NotebookCell,
  blobPort?: number | null,
): NotebookCell | null {
  const cellType = handle.get_cell_type(cellId);
  if (!cellType) return null;

  const source = handle.get_cell_source(cellId) ?? "";
  const metadata = handle.get_cell_metadata(cellId) ?? {};

  if (cellType === "code") {
    const rawOutputs: unknown[] = handle.get_cell_outputs(cellId) ?? [];
    const resolvedOutputs = rawOutputs
      .map((output) => resolveOutputSync(output, blobPort ?? null, cache))
      .filter((o): o is JupyterOutput => o !== null);

    const prevOutputs =
      previousCell?.cell_type === "code" ? previousCell.outputs : undefined;
    const outputs = reuseOutputsIfUnchanged(resolvedOutputs, prevOutputs);

    // Resolve execution_count from RuntimeState (source of truth)
    const executionCount = getExecutionCountFromRuntime(cellId);

    return {
      id: cellId,
      cell_type: "code",
      source,
      execution_count: executionCount,
      outputs,
      requiredPlugins: computeRequiredPlugins(outputs),
      metadata,
    };
  }

  if (cellType === "markdown") {
    // Preserve resolvedAssets from the previous cell — these are resolved
    // during full materialization and don't change on source edits.
    const resolvedAssets =
      previousCell?.cell_type === "markdown"
        ? previousCell.resolvedAssets
        : undefined;
    return {
      id: cellId,
      cell_type: "markdown",
      source,
      metadata,
      resolvedAssets,
    };
  }

  return {
    id: cellId,
    cell_type: "raw",
    source,
    metadata,
  };
}
