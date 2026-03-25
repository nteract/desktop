/**
 * Materialization helpers for inbound sync batches.
 *
 * The sync pipeline itself lives in the `runtimed` package (SyncEngine).
 * This module provides the app-specific materialization logic that
 * transforms coalesced CellChangesets into React store updates.
 */

import type { JupyterOutput } from "../types";
import type { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";
import { getBlobPort, refreshBlobPort } from "./blob-port";
import type { CellChangeset } from "./cell-changeset";
import {
  isManifestHash,
  materializeCellFromWasm,
  resolveOutput,
} from "./materialize-cells";
import { getCellById, updateCellById } from "./notebook-cells";
import { notifyMetadataChanged } from "./notebook-metadata";

// Re-export CellChangeset types so existing consumers don't break.
export type {
  CellChangeset,
  ChangedCell,
  ChangedFields,
} from "./cell-changeset";
export { mergeChangesets } from "./cell-changeset";

// ── Materialization dependencies ─────────────────────────────────────

export interface MaterializeDeps {
  /** Read the current WASM handle (null during bootstrap). */
  getHandle: () => NotebookHandle | null;

  /**
   * Full materialization: serialize entire doc → resolve manifests →
   * write to notebook-cells store.
   */
  materializeCells: (handle: NotebookHandle) => Promise<void>;

  /** Shared output manifest cache (mutated in place). */
  outputCache: Map<string, JupyterOutput>;
}

// ── Batch materialization ────────────────────────────────────────────

/**
 * Process a coalesced CellChangeset from the SyncEngine.
 *
 * Falls back to full materialization when:
 * - The changeset is null (WASM couldn't produce one)
 * - The changeset includes structural changes (add/remove/reorder)
 *
 * Otherwise performs surgical per-cell updates using the WASM handle's
 * per-field accessors — O(changed cells) rather than O(all cells).
 */
export async function materializeChangeset(
  changeset: CellChangeset | null,
  deps: MaterializeDeps,
): Promise<void> {
  const handle = deps.getHandle();
  if (!handle) return;

  // ── Full materialization fallback ──────────────────────────────────

  if (!changeset) {
    await deps.materializeCells(handle);
    notifyMetadataChanged();
    return;
  }

  // Structural changes (cells added/removed/reordered) require full
  // materialization — the cell ID list and ordering need updating.
  if (
    changeset.added.length > 0 ||
    changeset.removed.length > 0 ||
    changeset.order_changed
  ) {
    await deps.materializeCells(handle);
    notifyMetadataChanged();
    return;
  }

  // ── Per-cell incremental materialization ───────────────────────────

  const cache = deps.outputCache;

  for (const { cell_id: cellId, fields } of changeset.changed) {
    if (fields.outputs) {
      // Check if every output for this cell is already cached.
      const rawOutputs: string[] = handle.get_cell_outputs(cellId) ?? [];
      const allCached = rawOutputs.every(
        (o) => cache.has(o) || !isManifestHash(o),
      );

      if (allCached) {
        // All outputs resolved from cache — fast sync path.
        const cell = materializeCellFromWasm(
          handle,
          cellId,
          cache,
          getCellById(cellId),
        );
        if (cell) {
          if (!fields.source) {
            const existing = getCellById(cellId);
            if (existing) cell.source = existing.source;
          }
          updateCellById(cellId, () => cell);
        }
      } else {
        // Cache miss — resolve this cell's outputs async.
        let blobPort = getBlobPort();
        if (blobPort === null) {
          blobPort = await refreshBlobPort();
        }
        const resolved = (
          await Promise.all(
            rawOutputs.map((o) => resolveOutput(o, blobPort, cache)),
          )
        ).filter((o): o is JupyterOutput => o !== null);

        const ecStr = handle.get_cell_execution_count(cellId);
        const ec =
          !ecStr || ecStr === "null" ? null : Number.parseInt(ecStr, 10);
        const metadata = handle.get_cell_metadata(cellId) ?? {};

        const existingCell = getCellById(cellId);
        const source = fields.source
          ? (handle.get_cell_source(cellId) ?? "")
          : (existingCell?.source ?? handle.get_cell_source(cellId) ?? "");

        updateCellById(cellId, () => ({
          id: cellId,
          cell_type: "code" as const,
          source,
          execution_count: Number.isNaN(ec) ? null : ec,
          outputs: resolved,
          metadata,
        }));
      }
    } else {
      // No output changes — fast sync path.
      const cell = materializeCellFromWasm(
        handle,
        cellId,
        cache,
        getCellById(cellId),
      );
      if (cell) {
        if (!fields.source) {
          const existing = getCellById(cellId);
          if (existing) cell.source = existing.source;
        }
        updateCellById(cellId, () => cell);
      }
    }
  }

  notifyMetadataChanged();
}
