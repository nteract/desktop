import { invoke } from "@tauri-apps/api/core";
import { useCallback, useEffect, useRef, useState } from "react";
import { debounceTime, from, Subject, switchMap } from "rxjs";
import { getBlobPort, refreshBlobPort } from "../lib/blob-port";
import { createFramePipeline } from "../lib/frame-pipeline";
import { frame_types, sendFrame } from "../lib/frame-types";
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
import { subscribeBroadcast } from "../lib/notebook-frame-bus";
import { setNotebookHandle } from "../lib/notebook-metadata";
import { resetRuntimeState } from "../lib/runtime-state";
import { fromTauriEvent } from "../lib/tauri-rx";
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
 * flow through the Tauri relay to the daemon.
 */
export function useAutomergeNotebook() {
  const cellIds = useCellIds();
  const [focusedCellId, setFocusedCellId] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [isLoading, setIsLoading] = useState(true);

  const handleRef = useRef<NotebookHandle | null>(null);
  const awaitingInitialSyncRef = useRef(true);
  const sessionIdRef = useRef(crypto.randomUUID().slice(0, 8));
  const outputCacheRef = useRef<Map<string, JupyterOutput>>(new Map());

  // RxJS subjects for debounced outbound sync.
  const sourceSync$ = useRef(new Subject<void>());
  const syncReply$ = useRef(new Subject<void>());

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

  /** Send a sync message to the Tauri relay (fire-and-forget). */
  const syncToRelay = useCallback((handle: NotebookHandle) => {
    const msg = handle.generate_sync_message();
    if (msg) {
      sendFrame(frame_types.AUTOMERGE_SYNC, msg).catch((e: unknown) =>
        logger.warn("[automerge-notebook] sync to relay failed:", e),
      );
    }
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
   * After the mutation callback runs, re-materializes and syncs.
   */
  const commitMutation = useCallback(
    (mutate: (handle: NotebookHandle) => boolean) => {
      const handle = handleRef.current;
      if (!handle || awaitingInitialSyncRef.current) return false;
      if (!mutate(handle)) return false;
      rematerializeCellsSync(handle);
      syncToRelay(handle);
      setDirty(true);
      return true;
    },
    [rematerializeCellsSync, syncToRelay],
  );

  // ── Bootstrap ──────────────────────────────────────────────────────

  const bootstrap = useCallback(async () => {
    await wasmReady;

    const handle = NotebookHandle.create_empty_with_actor(
      `human:${sessionIdRef.current}`,
    );

    handleRef.current?.free();
    handleRef.current = handle;
    setNotebookHandle(handle);

    awaitingInitialSyncRef.current = true;
    setIsLoading(true);

    syncToRelay(handle);
    logger.info("[automerge-notebook] Bootstrap: empty handle, awaiting sync");
    return true;
  }, [syncToRelay]);

  // ── Lifecycle (single effect) ──────────────────────────────────────

  useEffect(() => {
    let cancelled = false;

    awaitingInitialSyncRef.current = true;
    setIsLoading(true);
    void bootstrap().catch((error) => {
      logger.error("[automerge-notebook] Bootstrap failed", error);
      if (!cancelled) {
        awaitingInitialSyncRef.current = false;
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
          awaitingInitialSyncRef.current = true;
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

    // Inbound frame pipeline (WASM demux → coalesce → materialize → store).
    const frameSub = createFramePipeline({
      getHandle: () => handleRef.current,
      getAwaitingInitialSync: () => awaitingInitialSyncRef.current,
      setAwaitingInitialSync: (v) => {
        awaitingInitialSyncRef.current = v;
      },
      setIsLoading,
      materializeCells,
      outputCache: outputCacheRef.current,
      onSyncApplied: () => syncReply$.current.next(),
      retrySyncToRelay: () => {
        const handle = handleRef.current;
        if (!handle) return;
        // Reset sync state so generate_sync_message() produces a fresh
        // request instead of returning null (which it does when the
        // handle believes it's already in sync with the peer).
        handle.reset_sync_state();
        syncToRelay(handle);
      },
    });

    // Source sync: 20ms debounce for batching rapid keystrokes.
    const sourceSyncSub = sourceSync$.current
      .pipe(debounceTime(20))
      .subscribe(() => {
        const handle = handleRef.current;
        if (handle) syncToRelay(handle);
      });

    // Sync reply: 50ms debounce for coalescing inbound frame replies.
    const syncReplySub = syncReply$.current
      .pipe(debounceTime(50))
      .subscribe(() => {
        const handle = handleRef.current;
        if (!handle) return;
        const reply = handle.generate_sync_reply();
        if (reply) {
          sendFrame(frame_types.AUTOMERGE_SYNC, reply).catch((e: unknown) =>
            logger.warn("[automerge-notebook] sync reply failed:", e),
          );
        }
      });

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
      frameSub.unsubscribe();
      lifecycleSub.unsubscribe();
      sourceSyncSub.unsubscribe();
      syncReplySub.unsubscribe();
      clearOutputsSub.unsubscribe();

      // Flush pending sync before freeing handle.
      if (handleRef.current) {
        syncToRelay(handleRef.current);
        const reply = handleRef.current.generate_sync_reply();
        if (reply) {
          sendFrame(frame_types.AUTOMERGE_SYNC, reply).catch((e: unknown) =>
            logger.warn("[automerge-notebook] teardown sync reply failed:", e),
          );
        }
      }

      resetNotebookCells();
      resetRuntimeState();
      setNotebookHandle(null);
      handleRef.current?.free();
      handleRef.current = null;
    };
  }, [bootstrap, materializeCells, syncToRelay]);

  // ── Cell mutations ─────────────────────────────────────────────────

  const updateCellSource = useCallback((cellId: string, source: string) => {
    const handle = handleRef.current;
    if (!handle || awaitingInitialSyncRef.current) return;

    const updated = handle.update_source(cellId, source);
    if (!updated) return;

    updateCellById(cellId, (c) => ({ ...c, source }));
    sourceSync$.current.next();
    setDirty(true);
  }, []);

  /**
   * Clear outputs for a local UI action (Ctrl-Enter, menu clear).
   * Writes to the CRDT first so the store stays in sync with the
   * source of truth, then updates the store for instant feedback.
   */
  const clearOutputsLocal = useCallback((cellId: string) => {
    const handle = handleRef.current;
    if (handle) {
      handle.clear_outputs(cellId);
      handle.set_execution_count(cellId, "null");
      sourceSync$.current.next();
      setDirty(true);
    }

    // Store write for instant visual feedback. Safe because the CRDT
    // agrees (or will agree once materialization catches up).
    updateCellById(cellId, (c) =>
      c.cell_type === "code" ? { ...c, outputs: [], execution_count: null } : c,
    );
  }, []);

  /**
   * Clear outputs from every code cell via a single WASM call.
   * Updates the CRDT atomically, then refreshes the store.
   */
  const clearAllOutputsLocal = useCallback(() => {
    const handle = handleRef.current;
    if (!handle) return;
    const clearedIds: string[] = handle.clear_all_outputs();
    if (clearedIds.length === 0) return;

    sourceSync$.current.next();
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

  /**
   * Apply a daemon output-clear into the store. Store-only —
   * the daemon already wrote to the CRDT, so we just update the
   * local store. No CRDT mutation, no sync, no dirty flag.
   */
  const clearOutputsFromDaemon = useCallback((cellId: string) => {
    updateCellById(cellId, (c) =>
      c.cell_type === "code" ? { ...c, outputs: [], execution_count: null } : c,
    );
  }, []);

  const addCell = useCallback(
    (cellType: "code" | "markdown" | "raw", afterCellId?: string | null) => {
      const handle = handleRef.current;

      if (!handle || awaitingInitialSyncRef.current) {
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
      syncToRelay(handle);
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
    [rematerializeCellsSync, syncToRelay],
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

  /** Flush pending debounced sync immediately (call before execute/save). */
  const flushSync = useCallback(async () => {
    const handle = handleRef.current;
    if (!handle) return;
    // Bypasses the debounce; any pending emission becomes a no-op.
    const msg = handle.generate_sync_message();
    if (msg) {
      try {
        await sendFrame(frame_types.AUTOMERGE_SYNC, msg);
      } catch (e) {
        // Best-effort: don't block callers (execute, save) if the relay
        // is temporarily unable to forward the sync frame.  The daemon
        // will catch up on the next successful sync round-trip.
        logger.warn("[flushSync] failed to send sync frame, continuing", e);
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

  /**
   * Apply a daemon execution-count update into the store. Store-only —
   * the daemon already wrote to the CRDT.
   */
  const applyExecutionCountFromDaemon = useCallback(
    (cellId: string, count: number) => {
      updateCellById(cellId, (c) =>
        c.cell_type === "code" ? { ...c, execution_count: count } : c,
      );
    },
    [],
  );

  // ── Public interface ───────────────────────────────────────────────

  // ── CRDT bridge deps ───────────────────────────────────────────────
  // Stable getter for the WASM handle (reads ref at call time).
  const getHandle = useCallback(() => handleRef.current, []);
  // Trigger a debounced sync to the daemon (same Subject the old
  // updateCellSource used via sourceSync$).
  const triggerSync = useCallback(() => sourceSync$.current.next(), []);

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
  };
}
