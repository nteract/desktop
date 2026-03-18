import { invoke } from "@tauri-apps/api/core";

import {
  open as openDialog,
  save as saveDialog,
} from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useRef, useState } from "react";
import { getBlobPort, refreshBlobPort } from "../lib/blob-port";
import { frame_types, sendFrame } from "../lib/frame-types";
import { Subject, debounceTime, merge, switchMap, from } from "rxjs";
import { createFramePipeline } from "../lib/frame-pipeline";
import { fromTauriEvent } from "../lib/tauri-rx";
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
import { subscribeBroadcast } from "../lib/notebook-frame-bus";
import { setNotebookHandle } from "../lib/notebook-metadata";
import type { DaemonBroadcast, JupyterOutput } from "../types";
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
  const cellIds = useCellIds();
  const [focusedCellId, setFocusedCellId] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const [isLoading, setIsLoading] = useState(true);

  // The WASM handle is mutated in place — must live in a ref.
  const handleRef = useRef<NotebookHandle | null>(null);
  const awaitingInitialSyncRef = useRef(true);

  // Stable session ID for provenance — generated once so the actor label
  // remains consistent across bootstrap() re-invocations (daemon:ready,
  // file-opened, etc.).
  const sessionIdRef = useRef(crypto.randomUUID().slice(0, 8));

  // Output manifest cache (shared with materialize-cells utilities).
  const outputCacheRef = useRef<Map<string, JupyterOutput>>(new Map());

  // Blob port is managed by the blob-port store (lib/blob-port.ts).
  // Refresh on mount; daemon:ready handler refreshes on reconnect.
  useEffect(() => {
    refreshBlobPort();
  }, []);

  // Clear dirty state when daemon autosaves the notebook to disk.
  useEffect(() => {
    return subscribeBroadcast((payload) => {
      const broadcast = payload as DaemonBroadcast;
      if (broadcast.event === "notebook_autosaved") {
        setDirty(false);
        invoke("mark_notebook_clean").catch(() => {});
      }
    });
  }, []);

  // ── Helpers ────────────────────────────────────────────────────────

  /**
   * Read cells from the WASM doc and push them into the external store.
   * Resolves blob manifest hashes as needed.
   */
  const materializeCells = useCallback(async (handle: NotebookHandle) => {
    const json = handle.get_cells_json();
    const snapshots: CellSnapshot[] = JSON.parse(json);
    const blobPort = getBlobPort();
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
      sendFrame(frame_types.AUTOMERGE_SYNC, msg).catch((e: unknown) =>
        logger.warn("[automerge-notebook] sync to relay failed:", e),
      );
    }
  }, []);

  // RxJS subjects for debounced outbound sync pipelines.
  // Subjects are stable refs — subscriptions are managed in useEffect.

  // Source sync (20ms debounce): batches rapid keystrokes into a single IPC
  // call. Structural mutations (add/delete/move) still use syncToRelay
  // directly for immediate consistency.
  const sourceSync$ = useRef(new Subject<void>());

  // Sync reply (50ms debounce): coalesces multiple inbound receives into a
  // single outbound reply. The Automerge sync protocol is safe to batch:
  // receive,receive,receive → generate covers all received changes.
  // Matches automerge-repo's syncDebounceRate pattern (they use 100ms).
  const syncReply$ = useRef(new Subject<void>());

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

    // Tag this peer's edits with a "human" actor label for provenance.
    // The session suffix (stable for this hook instance) ensures uniqueness
    // across concurrent tabs without fragmenting provenance on re-bootstrap.
    const handle = NotebookHandle.create_empty_with_actor(
      `human:${sessionIdRef.current}`,
    );

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

    // ── Daemon lifecycle (RxJS) ─────────────────────────────────────
    //
    // daemon:ready (reconnect) and notebook:file-opened (new file in this
    // window) both require a fresh bootstrap. Merged into one observable
    // so the switchMap cancels any in-flight bootstrap when a new event
    // arrives (e.g. rapid file-open, file-open during reconnect).
    const lifecycleSub = merge(
      fromTauriEvent("daemon:ready"),
      fromTauriEvent("notebook:file-opened"),
    )
      .pipe(
        switchMap(() => {
          // The Tauri event name isn't carried through merge, but we can
          // distinguish by checking: daemon:ready always refreshes the
          // blob port, file-opened always resets cells. Doing both is
          // safe and idempotent, so we just do both unconditionally.
          refreshBlobPort();
          resetNotebookCells();
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

    // ── Inbound frame pipeline (RxJS) ───────────────────────────────
    //
    // All frame types (AutomergeSync, Broadcast, Presence) arrive through
    // one Tauri event. The RxJS pipeline owns WASM demux, coalescing,
    // materialization, and fan-out to the frame bus. Replaces the old
    // imperative listener + scheduleMaterialize + 3 timer/accumulator refs.
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
    });

    // ── Debounced outbound sync pipelines (RxJS) ────────────────────

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

    // ── Bulk output clearing (run-all / restart-and-run-all) ─────────
    const unlistenClearOutputs = fromTauriEvent<string[]>(
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
      // Unsubscribe debounce pipelines (cancels pending timers)
      sourceSyncSub.unsubscribe();
      syncReplySub.unsubscribe();
      unlistenClearOutputs.unsubscribe();
      // Flush any pending sync before teardown — unsubscribing the
      // debounce pipelines above cancels their timers, so we do a
      // final sync + reply here to avoid dropping in-flight changes.
      if (handleRef.current) {
        syncToRelay(handleRef.current);
        const reply = handleRef.current.generate_sync_reply();
        if (reply) {
          sendFrame(frame_types.AUTOMERGE_SYNC, reply).catch((e: unknown) =>
            logger.warn("[automerge-notebook] teardown sync reply failed:", e),
          );
        }
      }
      // Free WASM handle.
      resetNotebookCells();
      setNotebookHandle(null);
      handleRef.current?.free();
      handleRef.current = null;
    };
  }, [bootstrap, materializeCells, syncToRelay]);

  // ── Cell mutations ─────────────────────────────────────────────────

  const updateCellSource = useCallback((cellId: string, source: string) => {
    const handle = handleRef.current;
    if (!handle || awaitingInitialSyncRef.current) return;

    // Mutate WASM (instant, local-first)
    const updated = handle.update_source(cellId, source);
    if (!updated) return;

    // Fast-path: update only the affected cell in the store — triggers only
    // that cell's subscribers, not all cells.
    updateCellById(cellId, (c) => ({ ...c, source }));

    // Debounced sync to daemon — batches rapid keystrokes
    sourceSync$.current.next();

    setDirty(true);
  }, []);

  const clearCellOutputs = useCallback((cellId: string) => {
    updateCellById(cellId, (c) =>
      c.cell_type === "code" ? { ...c, outputs: [], execution_count: null } : c,
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

  /**
   * Flush any pending debounced sync immediately so the daemon has the
   * latest source. Call before execute/runAll to avoid stale code.
   */
  const flushSync = useCallback(async () => {
    const handle = handleRef.current;
    if (!handle) return;

    // Generate and send sync message immediately, bypassing the debounce.
    // Any pending debounced emission becomes a no-op (generate_sync_message
    // returns null when there's nothing new to sync).
    const msg = handle.generate_sync_message();
    if (msg) {
      await sendFrame(frame_types.AUTOMERGE_SYNC, msg);
    }
  }, []);

  // ── Save / Open / Clone ────────────────────────────────────────────

  const save = useCallback(async () => {
    try {
      // Flush any pending sync so the daemon has the latest source before
      // writing to disk.
      await flushSync();

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
  }, [flushSync]);

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
    updateCellById(cellId, (c) =>
      c.cell_type === "code" ? { ...c, execution_count: count } : c,
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
    cellIds,
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
    flushSync,
  };
}
