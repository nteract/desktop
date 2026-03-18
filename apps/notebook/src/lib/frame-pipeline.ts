/**
 * Inbound frame processing pipeline (Pipeline 1).
 *
 * Replaces the imperative `notebook:frame` listener, `scheduleMaterialize`,
 * and three timer/accumulator refs from `useAutomergeNotebook` with a
 * declarative RxJS pipeline that splits incoming frames into sub-streams:
 *
 *   1. sync_applied → coalesce (32ms buffer) → materialize → write to store
 *   2. sync_applied attributions → emitBroadcast
 *   3. broadcast → emitBroadcast
 *   4. presence → emitPresence
 *
 * Usage (in useEffect):
 *   const sub = createFramePipeline(deps);
 *   return () => sub.unsubscribe();
 *
 * Web-worker future: the `mergeMap(payload => ...)` that calls WASM
 * `receive_frame` becomes a `switchMap` through the worker bridge. The
 * rest of the pipeline (coalesce, materialize, write to store) stays
 * identical.
 */

import {
  bufferTime,
  concatMap,
  EMPTY,
  filter,
  from,
  mergeMap,
  Subject,
  Subscription,
  share,
  switchMap,
  timer,
} from "rxjs";

import type { JupyterOutput } from "../types";
import type { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";
import { getBlobPort, refreshBlobPort } from "./blob-port";
import { type CellChangeset, mergeChangesets } from "./cell-changeset";
import { logger } from "./logger";
import {
  isManifestHash,
  materializeCellFromWasm,
  resolveOutput,
} from "./materialize-cells";
import { getCellById, updateCellById } from "./notebook-cells";
import { emitBroadcast, emitPresence } from "./notebook-frame-bus";
import { notifyMetadataChanged } from "./notebook-metadata";
import { fromTauriEvent } from "./tauri-rx";

// Re-export CellChangeset types so existing consumers don't break.
export type {
  CellChangeset,
  ChangedCell,
  ChangedFields,
} from "./cell-changeset";
export { mergeChangesets } from "./cell-changeset";

// ── Constants ────────────────────────────────────────────────────────

/** Coalescing window for incoming sync frames (ms). */
const COALESCE_MS = 32;

/** Timeout before retrying sync if initial sync hasn't produced cells (ms). */
const SYNC_RETRY_MS = 3000;

// ── Internal types ──────────────────────────────────────────────────

/** Attribution for text changes, produced by WASM sync. */
interface TextAttribution {
  cell_id: string;
  index: number;
  text: string;
  deleted: number;
  actors: string[];
}

/** Typed event returned by WASM `receive_frame()`. */
interface FrameEvent {
  type: string;
  changed?: boolean;
  changeset?: CellChangeset;
  attributions?: TextAttribution[];
  payload?: unknown;
}

// ── Pipeline dependencies ───────────────────────────────────────────

/**
 * External dependencies injected by the hook.
 *
 * Getters read from React refs at event-processing time (not capture
 * time) so the pipeline never holds stale closures across bootstrap
 * cycles (daemon:ready, file-opened, etc.).
 */
export interface FramePipelineDeps {
  /** Read the current WASM handle (null during bootstrap). */
  getHandle: () => NotebookHandle | null;

  /** Check if we're still waiting for the first sync from the daemon. */
  getAwaitingInitialSync: () => boolean;

  /** Mark initial sync as received. */
  setAwaitingInitialSync: (value: boolean) => void;

  /** Update the loading state in React. */
  setIsLoading: (loading: boolean) => void;

  /**
   * Full materialization: serialize entire doc → resolve manifests →
   * write to notebook-cells store. The pipeline calls this for initial
   * sync and structural changes (add/remove/reorder cells).
   */
  materializeCells: (handle: NotebookHandle) => Promise<void>;

  /** Shared output manifest cache (mutated in place). */
  outputCache: Map<string, JupyterOutput>;

  /**
   * Called after each `sync_applied` event. The hook uses this to
   * schedule a debounced sync reply (until Phase 2 inlines it as a
   * `debounceTime` operator).
   */
  onSyncApplied: () => void;

  /**
   * Resend the initial sync message. Called by the retry timer when
   * the first sync exchange didn't produce cells within SYNC_RETRY_MS.
   * The empty WASM handle's sync message requests the full document.
   */
  retrySyncToRelay: () => void;
}

// ── Pipeline factory ────────────────────────────────────────────────

/**
 * Create and subscribe the inbound frame processing pipeline.
 *
 * Returns a `Subscription` — call `.unsubscribe()` to tear down the
 * Tauri event listener, all sub-streams, and the coalescing timer.
 *
 * The pipeline is **cold**: nothing happens until this function is
 * called, and everything is cleaned up when the subscription ends.
 */
export function createFramePipeline(deps: FramePipelineDeps): Subscription {
  const subscription = new Subscription();

  // Subject bridging sync_applied events into the coalescing buffer.
  // Each emission is a CellChangeset (incremental) or null (needs full).
  const materialize$ = new Subject<CellChangeset | null>();

  // Subject for the sync retry timer. Each emission restarts the timer.
  // If SYNC_RETRY_MS elapses without a successful initial sync (changed:true),
  // the pipeline resends sync to recover from lost/consumed messages.
  const retrySync$ = new Subject<void>();

  // Arm the retry timer immediately — if no sync_applied arrives at all
  // (e.g., all frames dropped before reaching WASM), this ensures we
  // still retry after SYNC_RETRY_MS rather than hanging indefinitely.
  retrySync$.next();

  // ── Source: Tauri frames → WASM demux → individual FrameEvents ────

  const frameEvents$ = fromTauriEvent<number[]>("notebook:frame").pipe(
    mergeMap((payload) => {
      try {
        const handle = deps.getHandle();
        if (!handle) return EMPTY;
        const bytes = new Uint8Array(payload);
        const result = handle.receive_frame(bytes);
        if (!result || !Array.isArray(result)) return EMPTY;
        return from(result as FrameEvent[]);
      } catch (e) {
        logger.warn("[frame-pipeline] receive_frame failed:", e);
        return EMPTY;
      }
    }),
    share(), // multicast to all sub-pipelines below
  );

  // ── Sub-pipeline: sync_applied → initial sync / coalesce ──────────

  subscription.add(
    frameEvents$
      .pipe(
        filter((e) => e.type === "sync_applied"),
        // concatMap serializes async work (initial materialization) so
        // we don't send a sync reply before the store is populated.
        concatMap((e) => {
          // ── Attributions (fire-and-forget, no async work) ──────────
          if (e.attributions && e.attributions.length > 0) {
            emitBroadcast({
              type: "text_attribution",
              attributions: e.attributions,
            });
          }

          // ── Initial sync: materialize immediately (no coalescing) ──
          if (deps.getAwaitingInitialSync()) {
            if (e.changed) {
              // Sync delivered actual document content — clear the gate
              // and materialize. This is the success path.
              deps.setAwaitingInitialSync(false);
              deps.setIsLoading(false);
              const handle = deps.getHandle();
              if (handle) {
                return from(
                  deps
                    .materializeCells(handle)
                    .then(() => {
                      notifyMetadataChanged();
                      deps.onSyncApplied();
                    })
                    .catch((err: unknown) => {
                      logger.warn(
                        "[frame-pipeline] initial materialize failed:",
                        err,
                      );
                      deps.onSyncApplied();
                    }),
                );
              }
            }
            // changed:false — Automerge sync protocol handshake round
            // (exchanging heads/bloom filters, no actual content yet).
            // Keep awaitingInitialSync=true and isLoading=true so the
            // user sees the loading state until real content arrives.
            // Restart the retry timer in case the exchange stalls.
            retrySync$.next();
            deps.onSyncApplied();
            return EMPTY;
          }

          // ── Steady-state: push changeset into coalescing buffer ────
          if (e.changed) {
            materialize$.next(e.changeset ?? null);
          }
          deps.onSyncApplied();
          return EMPTY;
        }),
      )
      .subscribe(),
  );

  // ── Sync retry timer ──────────────────────────────────────────────
  //
  // If the initial sync exchange doesn't produce cells within
  // SYNC_RETRY_MS (e.g., the daemon's response was consumed by a stale
  // handle during save-as, or the initial sync message was lost), resend
  // the sync message. The empty WASM handle requests the full document.
  // switchMap restarts the timer on each changed:false handshake round.
  subscription.add(
    retrySync$
      .pipe(
        switchMap(() => timer(SYNC_RETRY_MS)),
        filter(() => deps.getAwaitingInitialSync()),
      )
      .subscribe(() => {
        logger.info("[frame-pipeline] Retrying sync after timeout");
        deps.retrySyncToRelay();
      }),
  );

  // ── Coalescing buffer → materialization ────────────────────────────
  //
  // Collects changesets over a COALESCE_MS window, merges them, then
  // materializes once. Replaces pendingMaterializeTimerRef +
  // pendingChangesetRef + pendingFullMaterializeRef.

  subscription.add(
    materialize$
      .pipe(
        bufferTime(COALESCE_MS),
        filter((batch) => batch.length > 0),
        // concatMap(_, 1) serializes materialization — if a batch takes
        // longer than COALESCE_MS (e.g. blob resolution), subsequent
        // batches queue rather than overlapping and racing store writes.
        concatMap((batch) =>
          from(
            materializeFromBatch(batch, deps).catch((err: unknown) =>
              logger.warn("[frame-pipeline] materialize batch failed:", err),
            ),
          ),
        ),
      )
      .subscribe(),
  );

  // ── Sub-pipeline: broadcasts ───────────────────────────────────────

  subscription.add(
    frameEvents$
      .pipe(filter((e) => e.type === "broadcast" && e.payload != null))
      .subscribe((e) => emitBroadcast(e.payload)),
  );

  // ── Sub-pipeline: presence ─────────────────────────────────────────

  subscription.add(
    frameEvents$
      .pipe(filter((e) => e.type === "presence" && e.payload != null))
      .subscribe((e) => emitPresence(e.payload)),
  );

  return subscription;
}

// ── Internal: batch materialization ─────────────────────────────────

/**
 * Process a coalesced batch of changesets.
 *
 * Falls back to full materialization when:
 * - Any frame in the batch lacked a changeset (null entry)
 * - The merged changeset includes structural changes (add/remove/reorder)
 *
 * Otherwise performs surgical per-cell updates using the WASM handle's
 * per-field accessors — O(changed cells) rather than O(all cells).
 */
async function materializeFromBatch(
  batch: Array<CellChangeset | null>,
  deps: FramePipelineDeps,
): Promise<void> {
  // Read handle at fire time — not capture time — to avoid use-after-free
  // if bootstrap() replaced/freed the handle during the coalescing window.
  const handle = deps.getHandle();
  if (!handle) return;

  // Merge all changesets in the batch.
  let merged: CellChangeset | null = null;
  let needsFull = false;

  for (const cs of batch) {
    if (cs === null) {
      needsFull = true;
    } else if (merged === null) {
      merged = cs;
    } else {
      merged = mergeChangesets(merged, cs);
    }
  }

  // ── Full materialization fallback ──────────────────────────────────

  if (needsFull || !merged) {
    await deps.materializeCells(handle);
    notifyMetadataChanged();
    return;
  }

  // Structural changes (cells added/removed/reordered) require full
  // materialization — the cell ID list and ordering need updating.
  if (
    merged.added.length > 0 ||
    merged.removed.length > 0 ||
    merged.order_changed
  ) {
    await deps.materializeCells(handle);
    notifyMetadataChanged();
    return;
  }

  // ── Per-cell incremental materialization ───────────────────────────

  const cache = deps.outputCache;

  for (const { cell_id: cellId, fields } of merged.changed) {
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
        if (cell) updateCellById(cellId, () => cell);
      } else {
        // Cache miss — resolve this cell's outputs async (fetch manifests
        // from blob store) without re-serializing the entire document.
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
        const source = handle.get_cell_source(cellId) ?? "";
        const metadata = handle.get_cell_metadata(cellId) ?? {};

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
      // No output changes — always use fast sync path.
      const cell = materializeCellFromWasm(
        handle,
        cellId,
        cache,
        getCellById(cellId),
      );
      if (cell) updateCellById(cellId, () => cell);
    }
  }
}
