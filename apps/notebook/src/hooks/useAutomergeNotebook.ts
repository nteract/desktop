import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import type { SyncableHandle } from "runtimed";
import { FrameType, SyncEngine } from "runtimed";
import { concatMap, from, switchMap } from "rxjs";
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
import {
  cloneNotebookFile,
  openNotebookFile,
  saveNotebook,
} from "../lib/notebook-file-ops";
import {
  emitBroadcast,
  emitPresence,
  subscribeBroadcast,
} from "../lib/notebook-frame-bus";
import {
  notifyMetadataChanged,
  setNotebookHandle,
} from "../lib/notebook-metadata";
import {
  type RuntimeState,
  resetRuntimeState,
  setRuntimeState,
} from "../lib/runtime-state";
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
   * After the mutation callback runs, re-materializes and syncs.
   */
  const commitMutation = useCallback(
    (mutate: (handle: NotebookHandle) => boolean) => {
      const handle = handleRef.current;
      const engine = engineRef.current;
      if (!handle || !engine) return false;
      if (!mutate(handle)) return false;
      rematerializeCellsSync(handle);
      engine.flush();
      setDirty(true);
      return true;
    },
    [rematerializeCellsSync],
  );

  // ── Bootstrap ──��──────────────────────────────────��────────────────

  const bootstrap = useCallback(async () => {
    await wasmReady;

    const handle = NotebookHandle.create_empty_with_actor(
      `human:${sessionIdRef.current}`,
    );

    handleRef.current?.free();
    handleRef.current = handle;
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

  // ── Lifecycle (single effect) ─────────────���────────────────────────

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

    // ── Subscribe to SyncEngine observables ───────────────────────

    // Initial sync completion → full materialization.
    const initialSyncSub = engine.initialSyncComplete$.subscribe(() => {
      const handle = handleRef.current;
      if (handle) {
        materializeCells(handle)
          .then(() => {
            setIsLoading(false);
            notifyMetadataChanged();
          })
          .catch((err: unknown) => {
            logger.warn(
              "[automerge-notebook] initial materialize failed:",
              err,
            );
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
              logger.warn(
                "[automerge-notebook] materialize changeset failed:",
                err,
              ),
            ),
          ),
        ),
      )
      .subscribe();

    // Broadcasts → frame bus.
    const broadcastsSub = engine.broadcasts$.subscribe((payload) =>
      emitBroadcast(payload),
    );

    // Presence → frame bus.
    const presenceSub = engine.presence$.subscribe((payload) =>
      emitPresence(payload),
    );

    // Runtime state → store.
    const runtimeStateSub = engine.runtimeState$.subscribe((state) =>
      setRuntimeState(state as RuntimeState),
    );

    // ── Bootstrap ─────────────────────────────────────────────────

    setIsLoading(true);
    void bootstrap().catch((error) => {
      logger.error("[automerge-notebook] Bootstrap failed", error);
      if (!cancelled) {
        setIsLoading(false);
      }
    });

    // ── Daemon lifecycle ────────��─────────────────────────────────

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

    // ── Bulk output clearing ──────────────────────────────────────

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

      // Flush pending local changes before stopping.
      engine.flush();
      engine.stop();
      transport.disconnect();

      initialSyncSub.unsubscribe();
      cellChangesSub.unsubscribe();
      broadcastsSub.unsubscribe();
      presenceSub.unsubscribe();
      runtimeStateSub.unsubscribe();
      lifecycleSub.unsubscribe();
      clearOutputsSub.unsubscribe();

      engineRef.current = null;
      transportRef.current = null;

      resetNotebookCells();
      resetRuntimeState();
      setNotebookHandle(null);
      handleRef.current?.free();
      handleRef.current = null;
    };
  }, [bootstrap, materializeCells]);

  // ── Cell mutations ─────���───────────────────────────────────────────

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

  const clearOutputsLocal = useCallback((cellId: string) => {
    const handle = handleRef.current;
    const engine = engineRef.current;
    if (handle) {
      handle.clear_outputs(cellId);
      handle.set_execution_count(cellId, "null");
      engine?.scheduleFlush();
      setDirty(true);
    }

    updateCellById(cellId, (c) =>
      c.cell_type === "code" ? { ...c, outputs: [], execution_count: null } : c,
    );
  }, []);

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
          ...(cellType === "code"
            ? { outputs: [], execution_count: null }
            : {}),
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

  // ── Sync flush ────��────────────────────────────────────────────────

  /** Flush pending debounced sync immediately (call before execute/save). */
  const flushSync = useCallback(async () => {
    const handle = handleRef.current;
    const transport = transportRef.current;
    if (!handle || !transport) return;

    const msg = handle.flush_local_changes();
    if (msg) {
      try {
        await transport.sendFrame(FrameType.AUTOMERGE_SYNC, msg);
      } catch (e) {
        handle.cancel_last_flush();
        logger.warn("[flushSync] failed, rolled back sync state", e);
      }
    }
  }, []);

  // ── File operations ────────────────────────────────────────────────

  const save = useCallback(async () => {
    const saved = await saveNotebook(flushSync);
    if (saved) setDirty(false);
  }, [flushSync]);

  const openNotebook = useCallback(() => openNotebookFile(), []);

  const cloneNotebook = useCallback(() => cloneNotebookFile(), []);

  // ── Output overlays (optimistic, pre-sync) ──────��──────────────────

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

  const applyExecutionCountFromDaemon = useCallback(
    (cellId: string, count: number) => {
      updateCellById(cellId, (c) =>
        c.cell_type === "code" ? { ...c, execution_count: count } : c,
      );
    },
    [],
  );

  // ── Public interface ───────────���───────────────────────────────────

  const getHandle = useCallback(() => handleRef.current, []);
  const triggerSync = useCallback(() => engineRef.current?.scheduleFlush(), []);
  const localActor = `human:${sessionIdRef.current}`;

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
    triggerSync,
    localActor,
  };
}
