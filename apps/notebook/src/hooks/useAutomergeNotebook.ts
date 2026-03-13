import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  open as openDialog,
  save as saveDialog,
} from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useRef, useState } from "react";
import { frame_types } from "../lib/frame-types";
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
  updateNotebookCells,
  useNotebookCells,
} from "../lib/notebook-cells";
import { emitBroadcast, emitPresence } from "../lib/notebook-frame-bus";
import {
  notifyMetadataChanged,
  setNotebookHandle,
} from "../lib/notebook-metadata";
import type { JupyterOutput } from "../types";
import init, { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";

// ---------------------------------------------------------------------------
// Module-level WASM initialization — starts loading immediately when module
// is imported. This runs before React renders, eliminating WASM init latency
// from the critical path that causes the "empty notebook" flash.
// ---------------------------------------------------------------------------
const wasmReady: Promise<void> = init().then(() => {
  logger.info("[automerge-notebook] WASM initialized");
});

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

/**
 * Local-first notebook hook backed by `runtimed-wasm` NotebookHandle.
 *
 * All document mutations (add/delete cell, edit source) execute instantly
 * inside the WASM Automerge document. The external store is derived from the doc.
 * Sync messages flow through the Tauri relay to the daemon — the frontend
 * NEVER creates Automerge objects via the JS library.
 */
export function useAutomergeNotebook() {
  const cells = useNotebookCells();
  const [focusedCellId, setFocusedCellId] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [isLoading, setIsLoading] = useState(true);

  // The WASM handle is mutated in place — must live in a ref.
  const handleRef = useRef<NotebookHandle | null>(null);
  const awaitingInitialSyncRef = useRef(true);

  // Output manifest cache (shared with materialize-cells utilities).
  const outputCacheRef = useRef<Map<string, JupyterOutput>>(new Map());

  // Blob port for manifest resolution.
  const blobPortPromiseRef = useRef<Promise<number | null> | null>(null);

  const refreshBlobPort = useCallback(() => {
    blobPortPromiseRef.current = invoke<number>("get_blob_port").catch((e) => {
      logger.warn("[automerge-notebook] Failed to get blob port:", e);
      return null;
    });
  }, []);

  useEffect(() => {
    refreshBlobPort();
  }, [refreshBlobPort]);

  // ── Helpers ────────────────────────────────────────────────────────

  /**
   * Read cells from the WASM doc and push them into the external store.
   * Resolves blob manifest hashes as needed.
   */
  const materializeCells = useCallback(async (handle: NotebookHandle) => {
    const json = handle.get_cells_json();
    const snapshots: CellSnapshot[] = JSON.parse(json);
    const blobPort = blobPortPromiseRef.current
      ? await blobPortPromiseRef.current
      : null;
    const newCells = await cellSnapshotsToNotebookCells(
      snapshots,
      blobPort,
      outputCacheRef.current,
    );
    replaceNotebookCells(newCells);
  }, []);

  /**
   * Generate a sync message from the local doc and forward it to the
   * Tauri relay.  Fire-and-forget — the relay handles daemon forwarding.
   */
  const syncToRelay = useCallback((handle: NotebookHandle) => {
    const msg = handle.generate_sync_message();
    if (msg) {
      // Prepend frame type byte for the unified send_frame command
      const frameData = new Uint8Array(1 + msg.length);
      frameData[0] = frame_types.AUTOMERGE_SYNC;
      frameData.set(msg, 1);
      invoke("send_frame", {
        frameData: Array.from(frameData),
      }).catch((e: unknown) =>
        logger.warn("[automerge-notebook] sync to relay failed:", e),
      );
    }
  }, []);

  /**
   * Synchronously re-read cells from WASM after a local mutation.
   * Uses cache-only output resolution (no blob fetches).
   */
  const rematerializeCellsSync = useCallback((handle: NotebookHandle) => {
    const json = handle.get_cells_json();
    const snapshots: CellSnapshot[] = JSON.parse(json);
    const newCells = cellSnapshotsToNotebookCellsSync(
      snapshots,
      outputCacheRef.current,
    );
    replaceNotebookCells(newCells);
  }, []);

  // ── Bootstrap ──────────────────────────────────────────────────────

  /**
   * Create an empty WASM NotebookHandle for sync-only bootstrap.
   *
   * The handle starts with a zero-operation Automerge doc. The sync
   * protocol delivers everything — cells, outputs, metadata — from the
   * daemon through the pipe. No `GetDocBytes` call needed.
   *
   * Bootstrap itself is a local operation that always completes once the
   * WASM runtime is ready, but it immediately kicks off the sync protocol
   * via `syncToRelay()`, which performs an IPC `invoke` under the hood.
   * Any IPC failures are logged and do not cause `bootstrap()` to reject.
   *
   * Loading state is set to `true` here and is cleared when the first
   * `notebook:frame` sync message is received, regardless of its
   * `changed` flag.
   */
  const bootstrap = useCallback(async () => {
    await wasmReady;

    const handle = NotebookHandle.create_empty();

    // Dispose previous handle (WASM allocation).
    handleRef.current?.free();
    handleRef.current = handle;
    setNotebookHandle(handle);

    awaitingInitialSyncRef.current = true;
    setIsLoading(true);

    // Kick off the sync protocol. If the relay isn't connected yet this
    // fails silently — the daemon's Phase 1 message (or the daemon:ready
    // retry) will start the exchange.
    syncToRelay(handle);

    logger.info("[automerge-notebook] Bootstrap: empty handle, awaiting sync");
    return true;
  }, [syncToRelay]);

  // ── Lifecycle (effects) ────────────────────────────────────────────

  useEffect(() => {
    let cancelled = false;

    // Create empty handle immediately — sync will populate it.
    awaitingInitialSyncRef.current = true;
    setIsLoading(true);
    void bootstrap().catch((error) => {
      logger.error("[automerge-notebook] Bootstrap failed", error);
      if (!cancelled) {
        awaitingInitialSyncRef.current = false;
        setIsLoading(false);
      }
    });

    const webview = getCurrentWebview();

    // On (re)connect, create a fresh empty handle and let sync deliver
    // everything. We must NOT reset_sync_state() on an existing handle —
    // that creates an infinite loop of 85-byte sync messages that never
    // converge (the WASM keeps re-requesting content it already has).
    const unlistenReady = webview.listen("daemon:ready", async () => {
      if (cancelled) return;
      refreshBlobPort();
      awaitingInitialSyncRef.current = true;
      setIsLoading(true);
      await bootstrap();
    });

    // Different file opened in this window — need a fresh handle since the
    // old handle's doc has the previous notebook's content.
    const unlistenFileOpened = webview.listen(
      "notebook:file-opened",
      async () => {
        if (cancelled) return;
        awaitingInitialSyncRef.current = true;
        setIsLoading(true);
        resetNotebookCells();
        await bootstrap();
      },
    );

    // ── Incoming frames from daemon (unified pipe) ──────────────────
    //
    // All frame types (AutomergeSync, Broadcast, Presence) arrive through
    // one event. The WASM handle.receive_frame() demuxes by the first byte,
    // applies sync internally, and returns typed FrameEvent JSON.
    //
    // Broadcasts and presence are dispatched via the frame bus (in-memory
    // pub/sub) to useDaemonKernel, useEnvProgress, and usePresence.
    const unlistenFrame = webview.listen<number[]>(
      "notebook:frame",
      async (event) => {
        if (cancelled) return;
        const handle = handleRef.current;
        if (!handle) return;
        try {
          const bytes = new Uint8Array(event.payload);
          const result = handle.receive_frame(bytes);
          if (!result || !Array.isArray(result)) return;

          const events = result as Array<{
            type: string;
            changed?: boolean;
            reply?: Uint8Array;
            payload?: unknown;
          }>;

          for (const frameEvent of events) {
            switch (frameEvent.type) {
              case "sync_applied": {
                if (awaitingInitialSyncRef.current) {
                  awaitingInitialSyncRef.current = false;
                  setIsLoading(false);
                }
                if (frameEvent.changed) {
                  await materializeCells(handle);
                  notifyMetadataChanged();
                }
                break;
              }
              case "sync_reply": {
                // WASM generated a sync response — send it back to the daemon
                if (frameEvent.reply) {
                  const reply = frameEvent.reply;
                  const replyData = new Uint8Array(1 + reply.length);
                  replyData[0] = frame_types.AUTOMERGE_SYNC;
                  replyData.set(reply, 1);
                  invoke("send_frame", {
                    frameData: Array.from(replyData),
                  }).catch((e: unknown) =>
                    logger.warn("[automerge-notebook] sync reply failed:", e),
                  );
                }
                break;
              }
              case "broadcast": {
                if (frameEvent.payload) {
                  emitBroadcast(frameEvent.payload);
                }
                break;
              }
              case "presence": {
                if (frameEvent.payload) {
                  emitPresence(frameEvent.payload);
                }
                break;
              }
            }
          }
        } catch (e) {
          logger.warn("[automerge-notebook] receive frame failed:", e);
        }
      },
    );

    // ── Bulk output clearing (run-all / restart-and-run-all) ─────────
    const unlistenClearOutputs = webview.listen<string[]>(
      "cells:outputs_cleared",
      (event) => {
        if (cancelled) return;
        const clearedIds = new Set(event.payload);
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
      unlistenReady.then((fn) => fn()).catch(() => {});
      unlistenFileOpened.then((fn) => fn()).catch(() => {});
      unlistenFrame.then((fn) => fn()).catch(() => {});
      unlistenClearOutputs.then((fn) => fn()).catch(() => {});
      // Free WASM handle.
      resetNotebookCells();
      setNotebookHandle(null);
      handleRef.current?.free();
      handleRef.current = null;
    };
  }, [bootstrap, materializeCells, refreshBlobPort]);

  // ── Cell mutations ─────────────────────────────────────────────────

  const updateCellSource = useCallback(
    (cellId: string, source: string) => {
      const handle = handleRef.current;
      if (!handle || awaitingInitialSyncRef.current) return;

      // Mutate WASM (instant, local-first)
      const updated = handle.update_source(cellId, source);
      if (!updated) return;

      // Fast-path: update only the affected cell in the store (avoids full
      // rematerialization on every keystroke, which would cause typing lag)
      updateNotebookCells((prev) =>
        prev.map((c) => (c.id === cellId ? { ...c, source } : c)),
      );

      // Sync to daemon (fire-and-forget)
      syncToRelay(handle);

      setDirty(true);
    },
    [syncToRelay],
  );

  const clearCellOutputs = useCallback((cellId: string) => {
    updateNotebookCells((prev) =>
      prev.map((c) =>
        c.id === cellId && c.cell_type === "code"
          ? { ...c, outputs: [], execution_count: null }
          : c,
      ),
    );
  }, []);

  const addCell = useCallback(
    (cellType: "code" | "markdown" | "raw", afterCellId?: string | null) => {
      const handle = handleRef.current;

      // Don't allow adding cells while bootstrapping or if no handle
      if (!handle || awaitingInitialSyncRef.current) {
        // Return a placeholder cell without mutating state
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

      // Mutate WASM (instant, local-first)
      handle.add_cell_after(cellId, cellType, afterCellId ?? null);

      // Re-read from WASM (single source of truth)
      rematerializeCellsSync(handle);

      // Sync to daemon (fire-and-forget)
      syncToRelay(handle);

      setFocusedCellId(cellId);
      setDirty(true);

      // Return the cell from the store (derived from WASM)
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
      const handle = handleRef.current;
      if (!handle || awaitingInitialSyncRef.current) return;

      // Mutate WASM (instant, local-first)
      handle.move_cell(cellId, afterCellId ?? null);

      // Re-read from WASM (single source of truth)
      rematerializeCellsSync(handle);

      // Sync to daemon (fire-and-forget)
      syncToRelay(handle);

      setDirty(true);
    },
    [rematerializeCellsSync, syncToRelay],
  );

  const deleteCell = useCallback(
    (cellId: string) => {
      const handle = handleRef.current;
      if (!handle || awaitingInitialSyncRef.current) return;

      // Guard: never delete the last cell
      if (handle.cell_count() <= 1) return;

      // Mutate WASM (instant, local-first)
      const deleted = handle.delete_cell(cellId);
      if (!deleted) return;

      // Re-read from WASM (single source of truth)
      rematerializeCellsSync(handle);

      // Sync to daemon (fire-and-forget)
      syncToRelay(handle);

      setDirty(true);
    },
    [rematerializeCellsSync, syncToRelay],
  );

  // ── Save / Open / Clone ────────────────────────────────────────────

  const save = useCallback(async () => {
    try {
      // Flush any pending sync to the relay so the daemon has the latest
      // source before writing to disk.
      const handle = handleRef.current;
      if (handle) {
        const msg = handle.generate_sync_message();
        if (msg) {
          const frameData = new Uint8Array(1 + msg.length);
          frameData[0] = frame_types.AUTOMERGE_SYNC;
          frameData.set(msg, 1);
          await invoke("send_frame", {
            frameData: Array.from(frameData),
          });
        }
      }

      const hasPath = await invoke<boolean>("has_notebook_path");

      if (hasPath) {
        await invoke("save_notebook");
      } else {
        const defaultDir = await invoke<string>("get_default_save_directory");
        const filePath = await saveDialog({
          filters: [{ name: "Jupyter Notebook", extensions: ["ipynb"] }],
          defaultPath: `${defaultDir}/Untitled.ipynb`,
        });
        if (!filePath) return;
        await invoke("save_notebook_as", { path: filePath });
      }

      setDirty(false);
    } catch (e) {
      logger.error("[automerge-notebook] Save failed:", e);
    }
  }, []);

  const openNotebook = useCallback(async () => {
    try {
      const filePath = await openDialog({
        multiple: false,
        filters: [{ name: "Jupyter Notebook", extensions: ["ipynb"] }],
      });
      if (!filePath || typeof filePath !== "string") return;
      await invoke("open_notebook_in_new_window", { path: filePath });
    } catch (e) {
      logger.error("[automerge-notebook] Open failed:", e);
    }
  }, []);

  const cloneNotebook = useCallback(async () => {
    try {
      const defaultDir = await invoke<string>("get_default_save_directory");
      const filePath = await saveDialog({
        filters: [{ name: "Jupyter Notebook", extensions: ["ipynb"] }],
        defaultPath: `${defaultDir}/Untitled-Clone.ipynb`,
      });
      if (!filePath) return;
      await invoke("clone_notebook_to_path", { path: filePath });
      await invoke("open_notebook_in_new_window", { path: filePath });
    } catch (e) {
      logger.error("[automerge-notebook] Clone failed:", e);
    }
  }, []);

  // ── Output / execution (optimistic overlays) ───────────────────────
  //
  // Canonical outputs arrive through Automerge sync (materializeCells).
  // These callbacks give instant feedback from daemon broadcasts for
  // display updates and execution counts before sync lands.

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

  const setExecutionCount = useCallback((cellId: string, count: number) => {
    updateNotebookCells((prev) =>
      prev.map((c) =>
        c.id === cellId && c.cell_type === "code"
          ? { ...c, execution_count: count }
          : c,
      ),
    );
  }, []);

  // ── Cell visibility ─────────────────────────────────────────────────

  const setCellSourceHidden = useCallback(
    (cellId: string, hidden: boolean) => {
      const handle = handleRef.current;
      if (!handle || awaitingInitialSyncRef.current) return;

      // Mutate WASM (instant, local-first)
      const updated = handle.set_cell_source_hidden(cellId, hidden);
      if (!updated) return;

      // Re-read from WASM (single source of truth)
      rematerializeCellsSync(handle);

      // Sync to daemon (fire-and-forget)
      syncToRelay(handle);

      setDirty(true);
    },
    [rematerializeCellsSync, syncToRelay],
  );

  const setCellOutputsHidden = useCallback(
    (cellId: string, hidden: boolean) => {
      const handle = handleRef.current;
      if (!handle || awaitingInitialSyncRef.current) return;

      // Mutate WASM (instant, local-first)
      const updated = handle.set_cell_outputs_hidden(cellId, hidden);
      if (!updated) return;

      // Re-read from WASM (single source of truth)
      rematerializeCellsSync(handle);

      // Sync to daemon (fire-and-forget)
      syncToRelay(handle);

      setDirty(true);
    },
    [rematerializeCellsSync, syncToRelay],
  );

  // ── Public interface ───────────────────────────────────────────────

  return {
    cells,
    isLoading,
    focusedCellId,
    setFocusedCellId,
    updateCellSource,
    clearCellOutputs,
    addCell,
    moveCell,
    deleteCell,
    save,
    openNotebook,
    cloneNotebook,
    dirty,
    updateOutputByDisplayId,
    setExecutionCount,
    setCellSourceHidden,
    setCellOutputsHidden,
  };
}
