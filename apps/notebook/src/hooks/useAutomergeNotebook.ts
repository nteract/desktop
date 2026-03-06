import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  open as openDialog,
  save as saveDialog,
} from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useRef, useState } from "react";
import { logger } from "../lib/logger";
import {
  type CellSnapshot,
  cellSnapshotsToNotebookCells,
} from "../lib/materialize-cells";
import type { JupyterOutput, NotebookCell } from "../types";
import init, { NotebookHandle } from "../wasm/runtimed-wasm/runtimed_wasm.js";

// ---------------------------------------------------------------------------
// Module-level WASM initialization — runs once per page load.
// ---------------------------------------------------------------------------
let wasmReady: Promise<void> | null = null;

function ensureWasmInit(): Promise<void> {
  if (!wasmReady) {
    wasmReady = init().then(() => {
      logger.info("[automerge-notebook] WASM initialized");
    });
  }
  return wasmReady;
}

// ---------------------------------------------------------------------------
// Hook
// ---------------------------------------------------------------------------

/**
 * Local-first notebook hook backed by `runtimed-wasm` NotebookHandle.
 *
 * All document mutations (add/delete cell, edit source) execute instantly
 * inside the WASM Automerge document.  React state is derived from the doc.
 * Sync messages flow through the Tauri relay to the daemon — the frontend
 * NEVER creates Automerge objects via the JS library.
 */
export function useAutomergeNotebook() {
  const [cells, setCells] = useState<NotebookCell[]>([]);
  const [focusedCellId, setFocusedCellId] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);

  // The WASM handle is mutated in place — must live in a ref.
  const handleRef = useRef<NotebookHandle | null>(null);

  // Keep a ref-mirror of cells so callbacks can read current state without
  // re-creating (avoids stale closures in useCallback with [] deps).
  const cellsRef = useRef<NotebookCell[]>([]);
  useEffect(() => {
    cellsRef.current = cells;
  }, [cells]);

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
   * Read cells from the WASM doc and push them into React state.
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
    setCells(newCells);
  }, []);

  /**
   * Generate a sync message from the local doc and forward it to the
   * Tauri relay.  Fire-and-forget — the relay handles daemon forwarding.
   */
  const syncToRelay = useCallback((handle: NotebookHandle) => {
    const msg = handle.generate_sync_message();
    if (msg) {
      invoke("send_automerge_sync", {
        syncMessage: Array.from(msg),
      }).catch((e: unknown) =>
        logger.warn("[automerge-notebook] sync to relay failed:", e),
      );
    }
  }, []);

  // ── Bootstrap ──────────────────────────────────────────────────────

  /**
   * Load the Automerge doc bytes from the Tauri relay and create a local
   * NotebookHandle.  Returns `true` on success.
   *
   * IMPORTANT: We do NOT call `syncToRelay()` after loading.  The plan
   * doc's hard-won lessons say: let the first mutation or daemon message
   * trigger the first sync exchange, not the load itself.
   */
  const bootstrap = useCallback(async () => {
    await ensureWasmInit();
    try {
      const bytes = await invoke<number[]>("get_automerge_doc_bytes");
      const handle = NotebookHandle.load(new Uint8Array(bytes));

      // Dispose previous handle (WASM allocation).
      handleRef.current?.free();
      handleRef.current = handle;

      await materializeCells(handle);
      logger.info(
        `[automerge-notebook] Bootstrap complete — ${handle.cell_count()} cells`,
      );
      return true;
    } catch (e) {
      logger.warn(
        "[automerge-notebook] Bootstrap failed (daemon may not be ready):",
        e,
      );
      return false;
    }
  }, [materializeCells]);

  // ── Lifecycle (effects) ────────────────────────────────────────────

  useEffect(() => {
    let cancelled = false;

    // Initial bootstrap — may fail if daemon isn't ready yet, that's OK.
    bootstrap().then((ok) => {
      if (cancelled) return;
      if (!ok) {
        logger.info("[automerge-notebook] Will retry on daemon:ready");
      }
    });

    const webview = getCurrentWebview();

    // Re-bootstrap when the daemon (re)connects.
    const unlistenReady = webview.listen("daemon:ready", async () => {
      if (cancelled) return;
      refreshBlobPort();
      // Reset sync state so the new relay session starts clean.
      handleRef.current?.reset_sync_state();
      await bootstrap();
    });

    // Re-bootstrap when a different file is opened via OS association.
    const unlistenFileOpened = webview.listen(
      "notebook:file-opened",
      async () => {
        if (cancelled) return;
        await bootstrap();
      },
    );

    // ── Incoming Automerge sync from daemon (via Tauri relay) ────────
    const unlistenSync = webview.listen<number[]>(
      "automerge:from-daemon",
      async (event) => {
        if (cancelled) return;
        const handle = handleRef.current;
        if (!handle) return;
        try {
          const bytes = new Uint8Array(event.payload);
          const changed = handle.receive_sync_message(bytes);
          if (changed) {
            await materializeCells(handle);
          }
          // The sync protocol may need multiple roundtrips — always
          // check whether we have something to send back.
          syncToRelay(handle);
        } catch (e) {
          logger.warn("[automerge-notebook] receive sync failed:", e);
        }
      },
    );

    // ── Backend-initiated cell source updates (e.g. formatting) ──────
    const unlistenSourceUpdated = webview.listen<{
      cell_id: string;
      source: string;
    }>("cell:source_updated", (event) => {
      if (cancelled) return;
      setCells((prev) =>
        prev.map((c) =>
          c.id === event.payload.cell_id
            ? { ...c, source: event.payload.source }
            : c,
        ),
      );
      setDirty(true);
    });

    // ── Bulk output clearing (run-all / restart-and-run-all) ─────────
    const unlistenClearOutputs = webview.listen<string[]>(
      "cells:outputs_cleared",
      (event) => {
        if (cancelled) return;
        const clearedIds = new Set(event.payload);
        setCells((prev) =>
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
      unlistenReady.then((fn) => fn());
      unlistenFileOpened.then((fn) => fn());
      unlistenSync.then((fn) => fn());
      unlistenSourceUpdated.then((fn) => fn());
      unlistenClearOutputs.then((fn) => fn());
      // Free WASM handle.
      handleRef.current?.free();
      handleRef.current = null;
    };
  }, [bootstrap, materializeCells, syncToRelay, refreshBlobPort]);

  // ── Cell mutations ─────────────────────────────────────────────────

  const updateCellSource = useCallback(
    (cellId: string, source: string) => {
      // Optimistic React update (instant keystroke feedback).
      setCells((prev) =>
        prev.map((c) => (c.id === cellId ? { ...c, source } : c)),
      );
      setDirty(true);

      const handle = handleRef.current;
      if (!handle) return;
      handle.update_source(cellId, source);
      syncToRelay(handle);
    },
    [syncToRelay],
  );

  const clearCellOutputs = useCallback((cellId: string) => {
    setCells((prev) =>
      prev.map((c) =>
        c.id === cellId && c.cell_type === "code"
          ? { ...c, outputs: [], execution_count: null }
          : c,
      ),
    );
  }, []);

  const addCell = useCallback(
    (cellType: "code" | "markdown", afterCellId?: string | null) => {
      const cellId = crypto.randomUUID();
      const newCell: NotebookCell =
        cellType === "code"
          ? {
              cell_type: "code",
              id: cellId,
              source: "",
              outputs: [],
              execution_count: null,
            }
          : { cell_type: "markdown", id: cellId, source: "" };

      // Compute insertion index from current React state (via ref).
      const current = cellsRef.current;
      let idx: number;
      if (!afterCellId) {
        idx = 0;
      } else {
        const afterIdx = current.findIndex((c) => c.id === afterCellId);
        idx = afterIdx === -1 ? 0 : afterIdx + 1;
      }

      // Mutate the WASM doc first — this is the source of truth.
      const handle = handleRef.current;
      if (handle) {
        handle.add_cell(idx, cellId, cellType);
        syncToRelay(handle);
      }

      // Optimistic React update.
      setCells((prev) => {
        if (!afterCellId) return [newCell, ...prev];
        const i = prev.findIndex((c) => c.id === afterCellId);
        if (i === -1) return [newCell, ...prev];
        const next = [...prev];
        next.splice(i + 1, 0, newCell);
        return next;
      });

      setFocusedCellId(cellId);
      setDirty(true);
      return newCell;
    },
    [syncToRelay],
  );

  const deleteCell = useCallback(
    (cellId: string) => {
      // Guard: never delete the last cell.
      setCells((prev) => {
        if (prev.length <= 1) return prev;
        return prev.filter((c) => c.id !== cellId);
      });
      setDirty(true);

      const handle = handleRef.current;
      if (!handle) return;
      handle.delete_cell(cellId);
      syncToRelay(handle);
    },
    [syncToRelay],
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
          await invoke("send_automerge_sync", {
            syncMessage: Array.from(msg),
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
  // Canonical outputs arrive through Automerge sync.  These callbacks
  // give instant feedback from daemon broadcasts before sync lands.

  const appendOutput = useCallback((cellId: string, output: JupyterOutput) => {
    setCells((prev) =>
      prev.map((c) => {
        if (c.id !== cellId || c.cell_type !== "code") return c;
        const outputs = [...c.outputs];
        if (output.output_type === "stream" && outputs.length > 0) {
          const last = outputs[outputs.length - 1];
          if (last.output_type === "stream" && last.name === output.name) {
            outputs[outputs.length - 1] = {
              ...last,
              text: last.text + output.text,
            };
            return { ...c, outputs };
          }
        }
        return { ...c, outputs: [...outputs, output] };
      }),
    );
  }, []);

  const updateOutputByDisplayId = useCallback(
    (
      displayId: string,
      newData: Record<string, unknown>,
      newMetadata?: Record<string, unknown>,
    ) => {
      setCells((prev) =>
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
    setCells((prev) =>
      prev.map((c) =>
        c.id === cellId && c.cell_type === "code"
          ? { ...c, execution_count: count }
          : c,
      ),
    );
  }, []);

  const formatCell = useCallback(async (cellId: string) => {
    try {
      const result = await invoke<{
        source: string;
        changed: boolean;
        error: string | null;
      }>("format_cell", { cellId });

      if (result.error) {
        logger.warn("[automerge-notebook] Format cell warning:", result.error);
      }

      return result;
    } catch (e) {
      logger.error("[automerge-notebook] Format cell failed:", e);
      return null;
    }
  }, []);

  // ── Public interface ───────────────────────────────────────────────

  return {
    cells,
    setCells,
    focusedCellId,
    setFocusedCellId,
    updateCellSource,
    clearCellOutputs,
    addCell,
    deleteCell,
    save,
    openNotebook,
    cloneNotebook,
    dirty,
    appendOutput,
    updateOutputByDisplayId,
    setExecutionCount,
    formatCell,
  };
}
