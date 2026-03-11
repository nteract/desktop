import type { JupyterOutput, NotebookCell } from "../types";
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
 * Merge consecutive stream outputs sharing the same name (stdout/stderr).
 * Handles both `string` and `string[]` text formats.
 */
export function mergeConsecutiveStreams(
  outputs: JupyterOutput[],
): JupyterOutput[] {
  return outputs.reduce<JupyterOutput[]>((merged, output) => {
    if (output.output_type === "stream" && merged.length > 0) {
      const last = merged[merged.length - 1];
      if (last.output_type === "stream" && last.name === output.name) {
        const lastText = Array.isArray(last.text)
          ? last.text.join("")
          : last.text;
        const outputText = Array.isArray(output.text)
          ? output.text.join("")
          : output.text;
        merged[merged.length - 1] = {
          ...last,
          text: lastText + outputText,
        };
        return merged;
      }
    }
    merged.push(output);
    return merged;
  }, []);
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

      const outputs = mergeConsecutiveStreams(resolvedOutputs);

      return {
        id: snap.id,
        cell_type: "code" as const,
        source: snap.source,
        execution_count: Number.isNaN(executionCount) ? null : executionCount,
        outputs,
        metadata,
      };
    }

    return {
      id: snap.id,
      cell_type: snap.cell_type as "markdown" | "raw",
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

        // Merge consecutive stream outputs as a fallback for unmerged data
        const outputs = mergeConsecutiveStreams(resolvedOutputs);

        return {
          id: snap.id,
          cell_type: "code" as const,
          source: snap.source,
          execution_count: Number.isNaN(executionCount) ? null : executionCount,
          outputs,
          metadata,
        };
      }

      // markdown or raw
      return {
        id: snap.id,
        cell_type: snap.cell_type as "markdown" | "raw",
        source: snap.source,
        metadata,
      };
    }),
  );
}
