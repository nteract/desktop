import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { from, switchMap, Subscription as RxSubscription } from "rxjs";
import { getBlobPort, refreshBlobPort } from "../lib/blob-port";
import { logger } from "../lib/logger";
import {
  type CellSnapshot,
  cellSnapshotsToNotebookCells,
  cellSnapshotsToNotebookCellsSync,
} from "../lib/materialize-cells";
import {
  getNotebookCellsSnapshot,
  replaceNotebookCells,
  resetNotebookCells,
  updateCellById,
  updateNotebookCells,
  useCellIds,
} from "../lib/notebook-cells";
import { subscribeBroadcast, emitBroadcast } from "../lib/notebook-frame-bus";
import {
  notifyMetadataChanged,
  setNotebookHandle,
} from "../lib/notebook-metadata";
import { resetRuntimeState, setRuntimeState } from "../lib/runtime-state";
import { fromTauriEvent } from "../lib/tauri-rx";
import { TauriTransport } from "../lib/tauri-transport";
import type { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";
import type { DaemonBroadcast, JupyterOutput } from "../types";

import { SyncEngine } from "@nteract/runtimed";

// ── WASM init ────────────────────────────────────────────────────────

const wasmReady = (async () => {
  const { default: init } =
    await import("../wasm/runtimed-wasm/runtimed_wasm.js");
  await init();
})();

// ── Hook ─────────────────────────────────────────────────────────────

/**
 * Core Automerge notebook hook — manages the WASM document, sync lifecycle,
 * and cell store.
 *
 * Uses {@link SyncEngine} from `@nteract/runtimed` for all sync management
 * and {@link TauriTransport} for the IPC layer. The hook subscribes to engine
 * events and writes to the React cell/runtime stores.
 */
export function useAutomergeNotebook() {
  const cellIds = useCellIds();
  const [focusedCellId, setFocusedCellId] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [isLoading, setIsLoading] = useState(true);

  const handleRef = useRef<NotebookHandle | null>(null);
  const sessionIdRef = useRef(crypto.randomUUID().slice(0, 8));
  const outputCacheRef = useRef<Map<string, JupyterOutput>>(new Map());

  // SyncEngine + TauriTransport refs (stable across re-renders).
  const engineRef = useRef<SyncEngine | null>(null);
  const transportRef = useRef<TauriTransport | null>(null);
  const rxSubRef = useRef<RxSubscription | null>(null);

  // Refresh blob port on mount.
  useEffect(() => {
    refreshBlobPort();
  }, []);

  // Clear dirty state on daemon autosave.
  useEffect(() => {
    return subscribeBroadcast((payload) => {
      const broadcast = payload as DaemonBroadcast;
      if (broadcast.event === "notebook_autosaved") {
        setDirty(false);
        invoke("mark_notebook_clean").catch(() => {});
      }
    });
  }, []);

  // ── Core helpers ───────────────────────────────────────────────────

  /** Full materialization: WASM doc → resolve manifests → write to store. */
  const materializeCells = useCallback(async (handle: NotebookHandle) => {
    const json = handle.get_cells_json();
    const snapshots: CellSnapshot[] = JSON.parse(json);
    let blobPort = getBlobPort();
    if (blobPort === null) {
      blobPort = await refreshBlobPort();
    }
    const newCells = await cellSnapshotsToNotebookCells(
      snapshots,
      blobPort,
      outputCacheRef.current,
    );
    replaceNotebookCells(newCells);
  }, []);

  /** Sync re-read cells from WASM (cache-only, no blob fetches). */
  const rematerializeCellsSync = useCallback((handle: NotebookHandle) => {
    const json = handle.get_cells_json();
    const snapshots: CellSnapshot[] = JSON.parse(json);
    const newCells = cellSnapshotsToNotebookCellsSync(
      snapshots,
      outputCacheRef.current,
    );
    replaceNotebookCells(newCells);
  }, []);

  /**
   * Guard + commit helper for WASM mutations.
   * Returns the handle if ready, or null if bootstrapping.
   * After the mutation callback runs, re-materializes and schedules sync.
   */
  const commitMutation = useCallback(
    (mutate: (handle: NotebookHandle) => boolean) => {
      const handle = handleRef.current;
      const engine = engineRef.current;
      if (!handle || !engine || !engine.synced) return false;
      if (!mutate(handle)) return false;
      rematerializeCellsSync(handle);
      engine.scheduleFlush();
      setDirty(true);
      return true;
    },
    [rematerializeCellsSync],
  );

  // ── Bootstrap ──────────────────────────────────────────────────────

  const bootstrap = useCallback(async () => {
    await wasmReady;

    // Create WASM handle.
    const { NotebookHandle: NH } =
      await import("../wasm/runtimed-wasm/runtimed_wasm.js");
    const handle: NotebookHandle = (
      NH as typeof NotebookHandle
    ).create_empty_with_actor(`human:${sessionIdRef.current}`);

    // Clean up previous engine + transport BEFORE freeing the handle.
    // The engine's stop() calls flushNow() which needs the handle alive.
    engineRef.current?.stop();
    engineRef.current = null;
    transportRef.current?.disconnect();
    transportRef.current = null;

    // Now safe to free the old handle.
    handleRef.current?.free();
    handleRef.current = handle;
    setNotebookHandle(handle);

    // Create transport + engine.
    const transport = new TauriTransport();
    await transport.connect();
    transportRef.current = transport;

    const engine = new SyncEngine(() => handleRef.current, transport, {
      flushDebounceMs: 20,
      initialSyncTimeoutMs: 3000,
    });
    engineRef.current = engine;

    // ── Subscribe to engine events ───────────────────────────────

    engine.on("initial_sync_complete", () => {
      setIsLoading(false);
      // Full materialization on initial sync.
      const h = handleRef.current;
      if (h) {
        materializeCells(h)
          .then(() => {
            notifyMetadataChanged();
          })
          .catch((err: unknown) => {
            logger.warn(
              "[automerge-notebook] initial materialize failed:",
              err,
            );
            setIsLoading(false);
          });
      }
    });

    // ── RxJS subscriptions to coalesced engine streams ────────────
    //
    // cellChanges$ buffers rapid cells_changed events over 32ms and
    // merges their changesets. This prevents over-materializing during
    // output streaming (which can cause blob resolution races and
    // CodeMirror splice_source errors from stale positions).

    const rxSub = new RxSubscription();
    rxSubRef.current = rxSub;

    rxSub.add(
      engine.cellChanges$.subscribe((batch) => {
        const h = handleRef.current;
        if (!h) return;

        // Emit text attributions for CodeMirror remote cursors.
        if (batch.attributions.length > 0) {
          emitBroadcast({
            type: "text_attribution",
            attributions: batch.attributions,
          });
        }

        // Full async materialization — resolves manifest hashes from
        // the blob HTTP server. The coalescing ensures we don't start
        // a new materialization while the previous one is resolving blobs.
        materializeCells(h).catch((err: unknown) => {
          logger.warn("[automerge-notebook] materialize failed:", err);
        });
      }),
    );

    rxSub.add(
      engine.broadcasts$.subscribe((payload) => {
        emitBroadcast(payload);
      }),
    );

    rxSub.add(
      engine.runtimeState$.subscribe((state) => {
        setRuntimeState(state as import("../lib/runtime-state").RuntimeState);
      }),
    );

    engine.on("sync_retry", () => {
      logger.info("[automerge-notebook] SyncEngine retrying initial sync");
    });

    engine.on("error", (event) => {
      logger.warn(
        `[automerge-notebook] SyncEngine error (${event.context}):`,
        event.error,
      );
    });

    // Start the engine — sends initial sync, arms retry timer.
    engine.start();

    setIsLoading(true);
    logger.info(
      "[automerge-notebook] Bootstrap: SyncEngine started, awaiting sync",
    );
    return true;
  }, [materializeCells, rematerializeCellsSync]);

  // ── Lifecycle (single effect) ──────────────────────────────────────

  useEffect(() => {
    let cancelled = false;

    setIsLoading(true);
    void bootstrap().catch((error) => {
      logger.error("[automerge-notebook] Bootstrap failed", error);
      if (!cancelled) {
        setIsLoading(false);
      }
    });

    // Daemon lifecycle — daemon:ready triggers a fresh bootstrap.
    // switchMap cancels any in-flight bootstrap on rapid reconnects.
    const lifecycleSub = fromTauriEvent("daemon:ready")
      .pipe(
        switchMap(() => {
          refreshBlobPort();
          resetNotebookCells();
          resetRuntimeState();
          setIsLoading(true);
          return from(
            bootstrap().catch((err: unknown) => {
              logger.error(
                "[automerge-notebook] lifecycle bootstrap failed:",
                err,
              );
            }),
          );
        }),
      )
      .subscribe();

    // Bulk output clearing (run-all / restart-and-run-all).
    const clearOutputsSub = fromTauriEvent<string[]>(
      "cells:outputs_cleared",
    ).subscribe((payload) => {
      const clearedIds = new Set(payload);
      updateNotebookCells((prev) =>
        prev.map((c) =>
          clearedIds.has(c.id) && c.cell_type === "code"
            ? { ...c, outputs: [], execution_count: null }
            : c,
        ),
      );
    });

    return () => {
      cancelled = true;
      rxSubRef.current?.unsubscribe();
      lifecycleSub.unsubscribe();
      clearOutputsSub.unsubscribe();

      // Stop the engine (flushes pending changes).
      engineRef.current?.stop();
      engineRef.current = null;

      // Disconnect transport.
      transportRef.current?.disconnect();
      transportRef.current = null;

      resetNotebookCells();
      resetRuntimeState();
      setNotebookHandle(null);
      handleRef.current?.free();
      handleRef.current = null;
    };
  }, [bootstrap]);

  // ── Cell mutations ─────────────────────────────────────────────────

  const updateCellSource = useCallback((cellId: string, source: string) => {
    const handle = handleRef.current;
    const engine = engineRef.current;
    if (!handle || !engine || !engine.synced) return;

    const updated = handle.update_source(cellId, source);
    if (!updated) return;

    updateCellById(cellId, (c) => ({ ...c, source }));
    engine.scheduleFlush();
    setDirty(true);
  }, []);

  /**
   * Clear outputs for a local UI action (Ctrl-Enter, menu clear).
   * Writes to WASM CRDT + React store optimistically, then syncs.
   */
  const clearOutputsLocal = useCallback((cellId: string) => {
    const handle = handleRef.current;
    const engine = engineRef.current;
    if (handle) {
      handle.clear_outputs(cellId);
      handle.set_execution_count(cellId, "null");
      engine?.scheduleFlush();
      setDirty(true);
    }

    // Store write for instant visual feedback. Safe because the CRDT
    // agrees (or will agree once materialization catches up).
    updateCellById(cellId, (c) =>
      c.cell_type === "code" ? { ...c, outputs: [], execution_count: null } : c,
    );
  }, []);

  /**
   * Clear all outputs for a local UI action (restart-and-run-all, menu).
   * Returns the list of cell IDs that were cleared.
   */
  const clearAllOutputsLocal = useCallback(() => {
    const handle = handleRef.current;
    const engine = engineRef.current;
    if (!handle) return;
    const clearedIds: string[] = handle.clear_all_outputs();
    if (clearedIds.length === 0) return;

    engine?.scheduleFlush();
    setDirty(true);

    const clearedSet = new Set(clearedIds);
    updateNotebookCells((prev) =>
      prev.map((c) =>
        clearedSet.has(c.id) && c.cell_type === "code"
          ? { ...c, outputs: [], execution_count: null }
          : c,
      ),
    );
  }, []);

  /** Clear outputs when notified by a broadcast from another window. */
  const clearOutputsFromDaemon = useCallback((cellId: string) => {
    updateCellById(cellId, (c) =>
      c.cell_type === "code" ? { ...c, outputs: [], execution_count: null } : c,
    );
  }, []);

  const addCell = useCallback(
    (
      cellType: "code" | "markdown" | "raw",
      afterCellId?: string | null,
      initialSource?: string,
    ) => {
      const handle = handleRef.current;
      const engine = engineRef.current;
      if (!handle || !engine || !engine.synced) return null;

      const placeholderId = crypto.randomUUID();

      // Optimistic store write for instant placeholder rendering.
      if (cellType === "code") {
        updateNotebookCells((prev) => {
          const idx = afterCellId
            ? prev.findIndex((c) => c.id === afterCellId) + 1
            : 0;
          const newCell = {
            cell_type: cellType as "code",
            id: placeholderId,
            source: initialSource ?? "",
            outputs: [],
            execution_count: null,
            metadata: {},
          };
          const next = [...prev];
          next.splice(idx, 0, newCell);
          return next;
        });
      } else {
        updateNotebookCells((prev) => {
          const idx = afterCellId
            ? prev.findIndex((c) => c.id === afterCellId) + 1
            : 0;
          const newCell = {
            cell_type: cellType as "markdown",
            id: placeholderId,
            source: initialSource ?? "",
            metadata: {},
          };
          const next = [...prev];
          next.splice(idx, 0, newCell);
          return next;
        });
      }

      // CRDT mutation — this generates the real cell ID.
      const cellId = placeholderId;
      handle.add_cell_after(cellId, cellType, afterCellId ?? undefined);
      if (initialSource) {
        handle.update_source(cellId, initialSource);
      }

      // Re-read from WASM to get the canonical state (replaces placeholder).
      const cell = handle.get_cell(cellId);
      if (cell) {
        if (cell.cell_type === "code") {
          updateCellById(cellId, () => ({
            cell_type: "code" as const,
            id: cell.id,
            source: cell.source,
            outputs: [],
            execution_count: null,
            metadata: cell.metadata_json ? JSON.parse(cell.metadata_json) : {},
          }));
        } else {
          updateCellById(cellId, () => ({
            cell_type: cell.cell_type as "markdown" | "raw",
            id: cell.id,
            source: cell.source,
            metadata: cell.metadata_json ? JSON.parse(cell.metadata_json) : {},
          }));
        }
        cell.free();
      }

      engine.scheduleFlush();
      setDirty(true);
      return cellId;
    },
    [],
  );

  const moveCell = useCallback(
    (cellId: string, afterCellId?: string | null) => {
      commitMutation((handle) => {
        handle.move_cell(cellId, afterCellId ?? undefined);
        return true;
      });
    },
    [commitMutation],
  );

  const deleteCell = useCallback(
    (cellId: string) => {
      commitMutation((handle) => {
        return handle.delete_cell(cellId);
      });
    },
    [commitMutation],
  );

  const setCellSourceHidden = useCallback(
    (cellId: string, hidden: boolean) => {
      commitMutation((handle) => {
        return handle.set_cell_source_hidden(cellId, hidden);
      });
    },
    [commitMutation],
  );

  const setCellOutputsHidden = useCallback(
    (cellId: string, hidden: boolean) => {
      commitMutation((handle) => {
        return handle.set_cell_outputs_hidden(cellId, hidden);
      });
    },
    [commitMutation],
  );

  // ── Sync operations ────────────────────────────────────────────────

  /**
   * Flush local CRDT changes immediately (bypasses debounce).
   *
   * Call before operations that depend on the daemon having the latest
   * state, e.g., before `executeCell` or `save`.
   */
  const flushSync = useCallback(async () => {
    const engine = engineRef.current;
    if (!engine) return;
    try {
      await engine.flush();
    } catch (e) {
      // flush() already rolled back sync state via cancel_last_flush.
      logger.warn("[flushSync] failed, sync state rolled back", e);
    }
  }, []);

  // ── File operations ────────────────────────────────────────────────

  const save = useCallback(async () => {
    const { saveNotebook } = await import("../lib/notebook-file-ops");
    const saved = await saveNotebook(flushSync);
    if (saved) setDirty(false);
  }, [flushSync]);

  const openNotebook = useCallback(() => {
    import("../lib/notebook-file-ops").then((m) => m.openNotebookFile());
  }, []);

  const cloneNotebook = useCallback(() => {
    import("../lib/notebook-file-ops").then((m) => m.cloneNotebookFile());
  }, []);

  // ── Output overlays (optimistic, pre-sync) ─────────────────────────

  const updateOutputByDisplayId = useCallback(
    (
      displayId: string,
      newData: Record<string, unknown>,
      newMetadata?: Record<string, unknown>,
    ) => {
      updateNotebookCells((prev) =>
        prev.map((c) => {
          if (c.cell_type !== "code") return c;
          let changed = false;
          const updatedOutputs = c.outputs.map((output) => {
            if (
              (output.output_type === "display_data" ||
                output.output_type === "execute_result") &&
              output.display_id === displayId
            ) {
              changed = true;
              return {
                ...output,
                data: { ...output.data, ...newData },
                metadata: { ...output.metadata, ...newMetadata },
              };
            }
            return output;
          });
          return changed ? { ...c, outputs: updatedOutputs } : c;
        }),
      );
    },
    [],
  );

  /**
   * Apply execution count from daemon broadcast (optimistic overlay).
   * The CRDT will catch up via sync, but this gives instant visual feedback.
   */
  const applyExecutionCountFromDaemon = useCallback(
    (cellId: string, executionCount: number) => {
      updateCellById(cellId, (c) =>
        c.cell_type === "code" ? { ...c, execution_count: executionCount } : c,
      );
    },
    [],
  );

  // ── CRDT bridge deps ───────────────────────────────────────────────
  // Stable getter for the WASM handle (reads ref at call time).
  const getHandle = useCallback(() => handleRef.current, []);
  // Trigger a debounced sync to the daemon.
  const triggerSync = useCallback(() => {
    engineRef.current?.scheduleFlush();
  }, []);

  return {
    cellIds,
    focusedCellId,
    setFocusedCellId,
    dirty,
    isLoading,

    // Cell mutations
    updateCellSource,
    clearOutputsLocal,
    clearAllOutputsLocal,
    clearOutputsFromDaemon,
    addCell,
    moveCell,
    deleteCell,
    setCellSourceHidden,
    setCellOutputsHidden,
    setDirty,

    // Sync
    flushSync,

    // File operations
    save,
    openNotebook,
    cloneNotebook,

    // Output overlays
    updateOutputByDisplayId,
    applyExecutionCountFromDaemon,

    // For child components that need direct WASM access
    getHandle,
    triggerSync,

    // Snapshot for save/export
    getNotebookCellsSnapshot,
  };
}
