/**
 * Materialization helpers for inbound sync batches.
 *
 * The sync pipeline itself lives in the `runtimed` package (SyncEngine).
 * This module provides the app-specific materialization logic that
 * transforms coalesced CellChangesets into React store updates.
 */

import {
  needsPlugin,
  preWarmForMimes,
} from "@/components/isolated/iframe-libraries";
import type { JupyterOutput } from "../types";
import type { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";
import { getBlobPort } from "./blob-port";
import type { CellChangeset } from "./cell-changeset";
import { logger } from "./logger";
import { materializeCellFromWasm } from "./materialize-cells";
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

// ── Plugin pre-warm helper ──────────────────────────────────────────

/**
 * Pre-warm the isolated-renderer plugin cache for any rich MIME types in
 * a cell's raw output list. Walks the structured output manifests (which
 * carry MIME keys even before content refs are resolved) and kicks off
 * background plugin loads so `<OutputArea>` doesn't have to await them.
 *
 * We intentionally read MIME keys from the raw manifests rather than the
 * resolved outputs — plugin discovery needs only the MIME set, not the
 * decoded payload, so the outputs store can finish resolution in parallel.
 */
function preWarmPluginsForRawOutputs(rawOutputs: unknown[]): void {
  const mimes: string[] = [];
  for (const raw of rawOutputs) {
    if (!raw || typeof raw !== "object") continue;
    const type = (raw as { output_type?: unknown }).output_type;
    if (type !== "execute_result" && type !== "display_data") continue;
    const data = (raw as { data?: unknown }).data;
    if (!data || typeof data !== "object") continue;
    for (const mime of Object.keys(data as Record<string, unknown>)) {
      if (needsPlugin(mime)) mimes.push(mime);
    }
  }
  if (mimes.length > 0) preWarmForMimes(mimes);
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
    logger.debug(
      "[frame-pipeline] full materialization: no changeset from WASM",
    );
    await deps.materializeCells(handle);
    notifyMetadataChanged();
    return;
  }

  // Structural changes (cells added/removed/reordered) or resolved_assets
  // changes require full materialization. resolved_assets are only available
  // from get_cells_json() (full serialization), not per-cell WASM accessors,
  // so the incremental path would serve stale values from the previous cell.
  const hasResolvedAssetChanges = changeset.changed.some(
    (c) => c.fields.resolved_assets,
  );
  if (
    changeset.added.length > 0 ||
    changeset.removed.length > 0 ||
    changeset.order_changed ||
    hasResolvedAssetChanges
  ) {
    logger.debug(
      `[frame-pipeline] full materialization: +${changeset.added.length} -${changeset.removed.length} reorder=${changeset.order_changed} assets=${hasResolvedAssetChanges}`,
    );
    await deps.materializeCells(handle);
    notifyMetadataChanged();
    return;
  }

  // ── Per-cell incremental materialization ───────────────────────────

  const cache = deps.outputCache;
  const blobPort = getBlobPort();
  let cellStoreTouched = 0;
  let outputOnlySkipped = 0;

  for (const { cell_id: cellId, fields } of changeset.changed) {
    // Phase C-lite: outputs live in the per-output / per-execution stores
    // (see notebook-outputs.ts, notebook-executions.ts). The cell store
    // still carries an `outputs: JupyterOutput[]` field for legacy readers
    // on full materialization, but the frame pipeline no longer touches
    // that field on incremental updates — the outputs store is the source
    // of truth for <OutputArea>.
    const chromeChanged =
      fields.source ||
      fields.execution_count ||
      fields.cell_type ||
      fields.metadata ||
      fields.position;

    if (!chromeChanged) {
      // Output-only change — the outputs store already has the new data
      // from `applyOutputChangeset`. Still warm the plugin cache for any
      // rich MIME types so <OutputArea> renders without waiting for async
      // loads, but don't touch the cell store.
      if (fields.outputs) {
        outputOnlySkipped++;
        const rawOutputs: unknown[] = handle.get_cell_outputs(cellId) ?? [];
        preWarmPluginsForRawOutputs(rawOutputs);
      }
      continue;
    }

    // Chrome-level change — re-read source / execution_count / metadata
    // from WASM and write to the cell store. The outputs array in the
    // store is left as-is (preserved by `materializeCellFromWasm` via
    // `reuseOutputsIfUnchanged` when all refs are cache hits) — the
    // outputs store is the source of truth for <OutputArea>.
    cellStoreTouched++;
    const cell = materializeCellFromWasm(
      handle,
      cellId,
      cache,
      getCellById(cellId),
      blobPort,
    );
    if (!cell) continue;

    if (!fields.source) {
      const existing = getCellById(cellId);
      if (existing) cell.source = existing.source;
    }

    if (fields.outputs) {
      // Warm plugin cache so the <OutputArea> iframe has renderers ready.
      const rawOutputs: unknown[] = handle.get_cell_outputs(cellId) ?? [];
      preWarmPluginsForRawOutputs(rawOutputs);
    }
    updateCellById(cellId, () => cell);
  }

  if (changeset.changed.length > 0) {
    const fieldSummary = changeset.changed
      .map((c) => {
        const f = c.fields;
        const flags = [
          f.source && "src",
          f.outputs && "out",
          f.execution_count && "ec",
          f.metadata && "meta",
        ].filter(Boolean);
        return `${c.cell_id.slice(0, 8)}(${flags.join(",")})`;
      })
      .join(" ");
    logger.debug(
      `[frame-pipeline] incremental: ${changeset.changed.length} cells [${fieldSummary}] cell-store=${cellStoreTouched} outputs-only-skipped=${outputOnlySkipped}`,
    );
  }

  notifyMetadataChanged();
}
