import type { JupyterOutput, NotebookCell } from "../types";
import type { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";
import { logger } from "./logger";
import type { OutputManifest } from "./manifest-resolution";
import { isManifestHash, resolveManifest } from "./manifest-resolution";

export type { ContentRef, OutputManifest } from "./manifest-resolution";
// Re-export shared manifest types and functions for downstream consumers
export {
  isManifestHash,
  resolveContentRef,
  resolveDataBundle,
  resolveManifest,
} from "./manifest-resolution";

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
  outputs: string[]; // JSON-encoded Jupyter outputs or manifest hashes
  metadata: Record<string, unknown>; // Cell metadata (arbitrary JSON object)
  resolved_assets?: Record<string, string>; // asset ref → blob hash (markdown cells)
}

/**
 * Resolve a single output string — either raw JSON or a manifest hash.
 *
 * - If cached, returns the cached value.
 * - If not a manifest hash, parses as raw JSON.
 * - If a manifest hash, fetches from blob store and resolves the manifest.
 */
export async function resolveOutput(
  outputStr: string,
  blobPort: number | null,
  cache: Map<string, JupyterOutput>,
): Promise<JupyterOutput | null> {
  const cached = cache.get(outputStr);
  if (cached) return cached;

  if (!isManifestHash(outputStr)) {
    try {
      const output = JSON.parse(outputStr) as JupyterOutput;
      cache.set(outputStr, output);
      return output;
    } catch {
      logger.warn("[materialize-cells] Failed to parse output JSON");
      return null;
    }
  }

  if (blobPort === null) {
    logger.warn("[materialize-cells] Manifest hash but no blob port");
    return null;
  }

  try {
    const response = await fetch(
      `http://127.0.0.1:${blobPort}/blob/${outputStr}`,
    );
    if (!response.ok) {
      logger.warn(
        `[materialize-cells] Failed to fetch manifest: ${response.status}`,
      );
      return null;
    }

    const manifestJson = await response.text();
    const manifest = JSON.parse(manifestJson) as OutputManifest;
    const output = await resolveManifest(manifest, blobPort);

    cache.set(outputStr, output);
    return output;
  } catch (e) {
    logger.warn("[materialize-cells] Failed to resolve manifest:", e);
    return null;
  }
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
 * For daemon sync with potentially new blob hashes, use cellSnapshotsToNotebookCells().
 */
export function cellSnapshotsToNotebookCellsSync(
  snapshots: CellSnapshot[],
  cache: Map<string, JupyterOutput>,
): NotebookCell[] {
  return snapshots.map((snap) => {
    const executionCount =
      snap.execution_count === "null"
        ? null
        : Number.parseInt(snap.execution_count, 10);

    const metadata = snap.metadata ?? {};

    if (snap.cell_type === "code") {
      // Resolve outputs from cache only — skip blob fetches
      const resolvedOutputs = snap.outputs
        .map((outputStr) => {
          const cached = cache.get(outputStr);
          if (cached) return cached;

          // If not a manifest hash, parse as JSON
          if (!isManifestHash(outputStr)) {
            try {
              const output = JSON.parse(outputStr) as JupyterOutput;
              cache.set(outputStr, output);
              return output;
            } catch {
              return null;
            }
          }

          // Manifest hash but not cached — return null (will resolve on daemon sync)
          logger.debug(
            "[materialize-cells] Manifest hash not in cache during sync materialization:",
            outputStr.slice(0, 16),
          );
          return null;
        })
        .filter((o): o is JupyterOutput => o !== null);

      return {
        id: snap.id,
        cell_type: "code" as const,
        source: snap.source,
        execution_count: Number.isNaN(executionCount) ? null : executionCount,
        outputs: resolvedOutputs,
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
 * Convert CellSnapshots to NotebookCells, resolving manifest hashes.
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
        // Resolve all outputs (may be manifest hashes or raw JSON)
        const resolvedOutputs = (
          await Promise.all(
            snap.outputs.map((o) => resolveOutput(o, blobPort, cache)),
          )
        ).filter((o): o is JupyterOutput => o !== null);

        return {
          id: snap.id,
          cell_type: "code" as const,
          source: snap.source,
          execution_count: Number.isNaN(executionCount) ? null : executionCount,
          outputs: resolvedOutputs,
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
 * the entire document. Output resolution uses cache-only (no blob fetches).
 */
export function materializeCellFromWasm(
  handle: NotebookHandle,
  cellId: string,
  cache: Map<string, JupyterOutput>,
  previousCell?: NotebookCell,
): NotebookCell | null {
  const cellType = handle.get_cell_type(cellId);
  if (!cellType) return null;

  const source = handle.get_cell_source(cellId) ?? "";
  const metadata = handle.get_cell_metadata(cellId) ?? {};

  if (cellType === "code") {
    const ecStr = handle.get_cell_execution_count(cellId);
    const executionCount =
      !ecStr || ecStr === "null" ? null : Number.parseInt(ecStr, 10);

    const rawOutputs: string[] = handle.get_cell_outputs(cellId) ?? [];
    const resolvedOutputs = rawOutputs
      .map((outputStr: string) => {
        const cached = cache.get(outputStr);
        if (cached) return cached;

        if (!isManifestHash(outputStr)) {
          try {
            const output = JSON.parse(outputStr) as JupyterOutput;
            cache.set(outputStr, output);
            return output;
          } catch {
            return null;
          }
        }
        logger.debug(
          "[materialize-cells] materializeCellFromWasm: uncached manifest hash dropped for cell %s: %s",
          cellId,
          outputStr.slice(0, 16),
        );
        return null;
      })
      .filter((o): o is JupyterOutput => o !== null);

    const prevOutputs =
      previousCell?.cell_type === "code" ? previousCell.outputs : undefined;
    const outputs = reuseOutputsIfUnchanged(resolvedOutputs, prevOutputs);

    return {
      id: cellId,
      cell_type: "code",
      source,
      execution_count: Number.isNaN(executionCount) ? null : executionCount,
      outputs,
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
