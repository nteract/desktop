import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import type { SyncableHandle } from "runtimed";
import { DEFAULT_MIME_PRIORITY, SyncEngine } from "runtimed";
import { concatMap, from, switchMap } from "rxjs";
import { needsPlugin, preWarmForMimes } from "@/components/isolated/iframe-libraries";
import { getBlobPort, refreshBlobPort } from "../lib/blob-port";
import { materializeChangeset } from "../lib/frame-pipeline";
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
import { cloneNotebookFile, openNotebookFile, saveNotebook } from "../lib/notebook-file-ops";
import { emitBroadcast, emitPresence, subscribeBroadcast } from "../lib/notebook-frame-bus";
import { notifyMetadataChanged, setNotebookHandle } from "../lib/notebook-metadata";
import { type PoolState, resetPoolState, setPoolState } from "../lib/pool-state";
import { type RuntimeState, resetRuntimeState, setRuntimeState } from "../lib/runtime-state";
import { fromTauriEvent } from "../lib/tauri-rx";
import { TauriTransport } from "../lib/tauri-transport";
import type { DaemonBroadcast, JupyterOutput } from "../types";
import init, { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";

// Module-level WASM init — runs before React renders.
const wasmReady: Promise<void> = init().then(() => {
  logger.info("[automerge-notebook] WASM initialized");
});

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

/**
 * Local-first notebook hook backed by `runtimed-wasm` NotebookHandle.
 *
 * All document mutations execute instantly inside the WASM Automerge
 * document. The external store is derived from the doc. Sync messages
 * flow through the SyncEngine → TauriTransport to the daemon.
 */
export function useAutomergeNotebook() {
  const cellIds = useCellIds();
  const [focusedCellId, setFocusedCellId] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [isLoading, setIsLoading] = useState(true);

  const handleRef = useRef<NotebookHandle | null>(null);
  const sessionIdRef = useRef(crypto.randomUUID().slice(0, 8));
  const outputCacheRef = useRef<Map<string, JupyterOutput>>(new Map());

  // SyncEngine and transport refs — stable across re-renders.
  const engineRef = useRef<SyncEngine | null>(null);
  const transportRef = useRef<TauriTransport | null>(null);

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
    const start = performance.now();
    // Resolve blob port BEFORE reading cells — WASM needs it to
    // convert binary ContentRefs to Url variants in get_cells_json().
    let blobPort = getBlobPort();
    if (blobPort === null) {
      blobPort = await refreshBlobPort();
    }
    if (blobPort !== null) {
      handle.set_blob_port(blobPort);
    }
    const json = handle.get_cells_json();
    const snapshots: CellSnapshot[] = JSON.parse(json);
    const newCells = await cellSnapshotsToNotebookCells(
      snapshots,
      blobPort,
      outputCacheRef.current,
    );
    // Pre-warm plugin cache from output MIME types so iframe rendering
    // doesn't wait for async loads
    const pluginMimes: string[] = [];
    for (const c of newCells) {
      if (c.cell_type === "code") {
        for (const output of c.outputs) {
          if (output.output_type === "execute_result" || output.output_type === "display_data") {
            for (const mime of Object.keys(output.data)) {
              if (needsPlugin(mime)) pluginMimes.push(mime);
            }
          }
        }
      }
    }
    if (pluginMimes.length > 0) preWarmForMimes(pluginMimes);
    replaceNotebookCells(newCells);
    logger.debug(
      `[automerge-notebook] Full materialization: ${snapshots.length} cells in ${(performance.now() - start).toFixed(1)}ms`,
    );
  }, []);

  /** Sync re-read cells from WASM (cache-only, no blob fetches). */
  const rematerializeCellsSync = useCallback((handle: NotebookHandle) => {
    const json = handle.get_cells_json();
    const snapshots: CellSnapshot[] = JSON.parse(json);
    const newCells = cellSnapshotsToNotebookCellsSync(
      snapshots,
      outputCacheRef.current,
      getBlobPort(),
    );
    replaceNotebookCells(newCells);
  }, []);

  /**
   * Guard + commit helper for WASM mutations.
   * After the mutation callback runs, re-materializes and syncs.
   */
  const commitMutation = useCallback(
    (mutate: (handle: NotebookHandle) => boolean) => {
      const handle = handleRef.current;
      const engine = engineRef.current;
      if (!handle || !engine) {
        logger.debug("[automerge-notebook] commitMutation skipped: no handle/engine");
        return false;
      }
      if (!mutate(handle)) return false;
      rematerializeCellsSync(handle);
      engine.flush();
      setDirty(true);
      return true;
    },
    [rematerializeCellsSync],
  );

  // ── Bootstrap ──────────────────────────────────────────────────────

  const bootstrap = useCallback(async () => {
    await wasmReady;

    const handle = NotebookHandle.create_empty_with_actor(`human:${sessionIdRef.current}`);

    handleRef.current?.free();
    handleRef.current = handle;
    handle.set_mime_priority(DEFAULT_MIME_PRIORITY);
    const initialBlobPort = getBlobPort();
    if (initialBlobPort !== null) {
      handle.set_blob_port(initialBlobPort);
    }
    setNotebookHandle(handle);

    setIsLoading(true);

    // Flush initial sync message through the engine.
    const engine = engineRef.current;
    if (engine) {
      engine.resetForBootstrap();
      engine.flush();
    }

    logger.info("[automerge-notebook] Bootstrap: empty handle, awaiting sync");
    return true;
  }, []);

  // ── Lifecycle (single effect) ──────────────────────────────────────

  useEffect(() => {
    let cancelled = false;

    // Create transport and engine for this lifecycle.
    const transport = new TauriTransport();
    const engine = new SyncEngine({
      getHandle: () => handleRef.current as SyncableHandle | null,
      transport,
      logger,
    });

    transportRef.current = transport;
    engineRef.current = engine;

    // Start the engine (subscribes to transport frames).
    engine.start();

    // Signal the Rust relay that the JS frame listener is active.
    // The relay buffers daemon frames until this fires, preventing
    // frame loss during WASM init (see #1421).
    invoke("notify_sync_ready").catch((e: unknown) => {
      logger.warn("[automerge-notebook] Failed to signal sync ready:", e);
    });

    // ── Subscribe to SyncEngine observables ───────────────────────

    // Initial sync completion → full materialization.
    const initialSyncSub = engine.initialSyncComplete$.subscribe(() => {
      logger.info("[automerge-notebook] Initial sync complete, materializing");
      const handle = handleRef.current;
      if (handle) {
        materializeCells(handle)
          .then(() => {
            setIsLoading(false);
            notifyMetadataChanged();
            logger.info("[automerge-notebook] Initial materialization done");
          })
          .catch((err: unknown) => {
            logger.warn("[automerge-notebook] initial materialize failed:", err);
            setIsLoading(false);
          });
      } else {
        setIsLoading(false);
      }
    });

    // Steady-state cell changes → incremental materialization.
    // concatMap serializes async work — if a batch awaits blob resolution,
    // subsequent batches queue rather than overlapping store writes.
    const cellChangesSub = engine.cellChanges$
      .pipe(
        concatMap((changeset) =>
          from(
            materializeChangeset(changeset, {
              getHandle: () => handleRef.current,
              materializeCells,
              outputCache: outputCacheRef.current,
            }).catch((err: unknown) =>
              logger.warn("[automerge-notebook] materialize changeset failed:", err),
            ),
          ),
        ),
      )
      .subscribe();

    // Broadcasts → frame bus.
    const broadcastsSub = engine.broadcasts$.subscribe((payload) => emitBroadcast(payload));

    // Presence → frame bus.
    const presenceSub = engine.presence$.subscribe((payload) => emitPresence(payload));

    // Runtime state → store.
    const runtimeStateSub = engine.runtimeState$.subscribe((state) =>
      setRuntimeState(state as RuntimeState),
    );

    // Pool state → store.
    const poolStateSub = engine.poolState$.subscribe((state) => setPoolState(state as PoolState));

    // ── Bootstrap ─────────────────────────────────────────────────

    setIsLoading(true);
    void bootstrap().catch((error) => {
      logger.error("[automerge-notebook] Bootstrap failed", error);
      if (!cancelled) {
        setIsLoading(false);
      }
    });

    // ── Daemon lifecycle ─────────────────────────────────────────

    const lifecycleSub = fromTauriEvent("daemon:ready")
      .pipe(
        switchMap(() => {
          logger.info("[automerge-notebook] daemon:ready — re-bootstrapping");
          refreshBlobPort();
          resetNotebookCells();
          resetRuntimeState();
          resetPoolState();
          outputCacheRef.current.clear();
          setIsLoading(true);
          return from(
            bootstrap().catch((err: unknown) => {
              logger.error("[automerge-notebook] lifecycle bootstrap failed:", err);
            }),
          );
        }),
      )
      .subscribe();

    // ── Bulk output clearing ──────────────────────────────────────

    const clearOutputsSub = fromTauriEvent<string[]>("cells:outputs_cleared").subscribe(
      (payload) => {
        const clearedIds = new Set(payload);
        updateNotebookCells((prev) =>
          prev.map((c) =>
            clearedIds.has(c.id) && c.cell_type === "code"
              ? { ...c, outputs: [], execution_count: null }
              : c,
          ),
        );
      },
    );

    return () => {
      cancelled = true;
      logger.info("[automerge-notebook] Cleanup: flushing and stopping engine");

      // Flush pending local changes before stopping.
      engine.flush();
      engine.stop();
      transport.disconnect();

      initialSyncSub.unsubscribe();
      cellChangesSub.unsubscribe();
      broadcastsSub.unsubscribe();
      presenceSub.unsubscribe();
      runtimeStateSub.unsubscribe();
      poolStateSub.unsubscribe();
      lifecycleSub.unsubscribe();
      clearOutputsSub.unsubscribe();

      engineRef.current = null;
      transportRef.current = null;

      resetNotebookCells();
      resetRuntimeState();
      resetPoolState();
      setNotebookHandle(null);
      handleRef.current?.free();
      handleRef.current = null;
    };
  }, [bootstrap, materializeCells]);

  // ── Cell mutations ─────────────────────────────────────────────────

  const updateCellSource = useCallback((cellId: string, source: string) => {
    const handle = handleRef.current;
    const engine = engineRef.current;
    if (!handle || !engine) return;

    const updated = handle.update_source(cellId, source);
    if (!updated) return;

    updateCellById(cellId, (c) => ({ ...c, source }));
    engine.scheduleFlush();
    setDirty(true);
  }, []);

  const clearOutputsLocal = useCallback((_cellId: string) => {
    // No-op: the SyncEngine clears outputs via cellChanges$ when the
    // RuntimeStateDoc reports execution started for this cell. Clearing
    // here previously caused a CRDT race under rapid ctrl-enter.
  }, []);

  const clearAllOutputsLocal = useCallback(() => {
    // No-op: each cell is cleared individually by the SyncEngine
    // when the RuntimeStateDoc reports execution started.
  }, []);

  const clearOutputsFromDaemon = useCallback((cellId: string) => {
    updateCellById(cellId, (c) =>
      c.cell_type === "code" ? { ...c, outputs: [], execution_count: null } : c,
    );
  }, []);

  const addCell = useCallback(
    (cellType: "code" | "markdown" | "raw", afterCellId?: string | null) => {
      const handle = handleRef.current;
      const engine = engineRef.current;

      if (!handle || !engine) {
        const placeholderId = crypto.randomUUID();
        return cellType === "code"
          ? {
              cell_type: "code" as const,
              id: placeholderId,
              source: "",
              outputs: [],
              execution_count: null,
              metadata: {},
            }
          : {
              cell_type: cellType,
              id: placeholderId,
              source: "",
              metadata: {},
            };
      }

      const cellId = crypto.randomUUID();
      handle.add_cell_after(cellId, cellType, afterCellId ?? null);
      rematerializeCellsSync(handle);
      engine.flush();
      setFocusedCellId(cellId);
      setDirty(true);

      const cell = getNotebookCellsSnapshot().find((c) => c.id === cellId);
      return (
        cell ?? {
          cell_type: cellType,
          id: cellId,
          source: "",
          ...(cellType === "code" ? { outputs: [], execution_count: null } : {}),
          metadata: {},
        }
      );
    },
    [rematerializeCellsSync],
  );

  const moveCell = useCallback(
    (cellId: string, afterCellId?: string | null) => {
      commitMutation((handle) => {
        handle.move_cell(cellId, afterCellId ?? null);
        return true;
      });
    },
    [commitMutation],
  );

  const deleteCell = useCallback(
    (cellId: string) => {
      commitMutation((handle) => {
        if (handle.cell_count() <= 1) return false;
        return !!handle.delete_cell(cellId);
      });
    },
    [commitMutation],
  );

  const setCellSourceHidden = useCallback(
    (cellId: string, hidden: boolean) => {
      commitMutation((handle) => {
        return !!handle.set_cell_source_hidden(cellId, hidden);
      });
    },
    [commitMutation],
  );

  const setCellOutputsHidden = useCallback(
    (cellId: string, hidden: boolean) => {
      commitMutation((handle) => {
        return !!handle.set_cell_outputs_hidden(cellId, hidden);
      });
    },
    [commitMutation],
  );

  // ── Sync flush ─────────────────────────────────────────────────────

  /**
   * Flush pending sync immediately (call before execute/save).
   *
   * Delegates to the SyncEngine's `flushAndWait()` which:
   * 1. Awaits any in-flight debounced flush (prevents race where the debounce
   *    timer claims changes but its IPC hasn't completed yet).
   * 2. Flushes remaining local changes and awaits delivery.
   */
  const flushSync = useCallback(async () => {
    const engine = engineRef.current;
    if (!engine) {
      logger.debug("[flushSync] skipped: no engine");
      return;
    }
    await engine.flushAndWait();
  }, []);

  // ── File operations ────────────────────────────────────────────────

  const save = useCallback(async () => {
    const saved = await saveNotebook(flushSync);
    if (saved) setDirty(false);
  }, [flushSync]);

  const openNotebook = useCallback(() => openNotebookFile(), []);

  const cloneNotebook = useCallback(() => cloneNotebookFile(), []);

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
              (output.output_type === "display_data" || output.output_type === "execute_result") &&
              output.display_id === displayId
            ) {
              changed = true;
              return { ...output, data: newData, metadata: newMetadata };
            }
            return output;
          });
          return changed ? { ...c, outputs: updatedOutputs } : c;
        }),
      );
    },
    [],
  );

  const applyExecutionCountFromDaemon = useCallback((cellId: string, count: number) => {
    updateCellById(cellId, (c) => (c.cell_type === "code" ? { ...c, execution_count: count } : c));
  }, []);

  // ── Public interface ───────────────────────────────────────────────

  const getHandle = useCallback(() => handleRef.current, []);
  const triggerSync = useCallback(() => engineRef.current?.scheduleFlush(), []);
  const localActor = `human:${sessionIdRef.current}`;

  /** Accessor for the SyncEngine (for subscribing to commChanges$ etc.). */
  const getEngine = useCallback(() => engineRef.current, []);

  return {
    cellIds,
    isLoading,
    focusedCellId,
    setFocusedCellId,
    updateCellSource,
    clearOutputsLocal,
    clearAllOutputsLocal,
    clearOutputsFromDaemon,
    addCell,
    moveCell,
    deleteCell,
    save,
    openNotebook,
    cloneNotebook,
    dirty,
    setDirty,
    updateOutputByDisplayId,
    applyExecutionCountFromDaemon,
    setCellSourceHidden,
    setCellOutputsHidden,
    flushSync,
    // CRDT bridge context deps
    getHandle,
    getEngine,
    triggerSync,
    localActor,
  };
}
