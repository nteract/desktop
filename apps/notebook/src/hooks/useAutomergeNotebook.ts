/**
 * Local-first Automerge notebook hook.
 *
 * Phase 2 replacement for useNotebook. The frontend owns its own Automerge
 * document — cell mutations (add, delete, edit) apply locally via
 * Automerge.change() and sync to the daemon via binary relay. This eliminates
 * the RPC round-trip for cell operations, making them instant.
 *
 * Outputs still flow via daemon:broadcast for real-time streaming; Automerge
 * sync provides eventual consistency for cross-window state.
 */

import * as Automerge from "@automerge/automerge";
import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  open as openDialog,
  save as saveDialog,
} from "@tauri-apps/plugin-dialog";
import { useCallback, useEffect, useRef, useState } from "react";
import type { NotebookSchema } from "../lib/automerge-schema";
import {
  type CellSnapshot,
  cellSnapshotsToNotebookCells,
} from "../lib/automerge-utils";
import { logger } from "../lib/logger";
import type { JupyterOutput, NotebookCell } from "../types";

/**
 * Materialize cells from an Automerge doc into CellSnapshots.
 */
function materializeCellSnapshots(
  doc: Automerge.Doc<NotebookSchema>,
): CellSnapshot[] {
  if (!doc.cells) return [];
  return doc.cells.map((cell) => ({
    id: cell.id,
    cell_type: cell.cell_type,
    source: cell.source,
    execution_count: cell.execution_count,
    outputs: [...cell.outputs],
  }));
}

export function useAutomergeNotebook() {
  const [cells, setCells] = useState<NotebookCell[]>([]);
  const [focusedCellId, setFocusedCellId] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const outputCacheRef = useRef<Map<string, JupyterOutput>>(new Map());
  const blobPortPromiseRef = useRef<Promise<number | null> | null>(null);

  // Automerge document and sync state
  const docRef = useRef<Automerge.Doc<NotebookSchema> | null>(null);
  const syncStateRef = useRef<Automerge.SyncState>(Automerge.initSyncState());
  const initializedRef = useRef(false);

  const refreshBlobPort = useCallback(() => {
    blobPortPromiseRef.current = invoke<number>("get_blob_port").catch((e) => {
      logger.warn("[automerge-notebook] Failed to get blob port:", e);
      return null;
    });
  }, []);

  useEffect(() => {
    refreshBlobPort();
  }, [refreshBlobPort]);

  /**
   * Materialize cells from the local Automerge doc and update React state.
   */
  const materializeAndSetCells = useCallback(async () => {
    const doc = docRef.current;
    if (!doc) return;

    const snapshots = materializeCellSnapshots(doc);
    const blobPort = blobPortPromiseRef.current
      ? await blobPortPromiseRef.current
      : null;

    const notebookCells = await cellSnapshotsToNotebookCells(
      snapshots,
      blobPort,
      outputCacheRef.current,
    );
    setCells(notebookCells);
  }, []);

  /**
   * Generate and send an Automerge sync message to the backend.
   */
  const syncToBackend = useCallback(() => {
    const doc = docRef.current;
    if (!doc) return;

    const [nextSyncState, message] = Automerge.generateSyncMessage(
      doc,
      syncStateRef.current,
    );
    syncStateRef.current = nextSyncState;

    if (message) {
      invoke("send_automerge_sync", {
        syncMessage: Array.from(message),
      }).catch((e) =>
        logger.error("[automerge-notebook] Failed to send sync message:", e),
      );
    }
  }, []);

  // Initialize: load doc bytes from Tauri and set up sync relay
  useEffect(() => {
    let isMounted = true;
    const webview = getCurrentWebview();

    const initialize = async () => {
      try {
        const bytes = await invoke<number[]>("get_automerge_doc_bytes");
        if (!isMounted) return;

        const doc = Automerge.load<NotebookSchema>(new Uint8Array(bytes));
        docRef.current = doc;
        initializedRef.current = true;

        await materializeAndSetCells();

        // After loading, generate initial sync message to align states
        syncToBackend();

        logger.info(
          `[automerge-notebook] Initialized with ${doc.cells?.length ?? 0} cells`,
        );
      } catch (e) {
        logger.warn(
          "[automerge-notebook] Failed to initialize, falling back to refresh:",
          e,
        );
        // Fall back to traditional load if automerge doc bytes not available
        invoke("refresh_from_automerge").catch(() => {});
      }
    };

    // Listen for incoming sync messages from daemon (relayed through Tauri)
    const unlistenSync = webview.listen<number[]>(
      "automerge:from-daemon",
      (event) => {
        if (!isMounted || !docRef.current) return;

        const message = new Uint8Array(event.payload);
        const [newDoc, newSyncState] = Automerge.receiveSyncMessage(
          docRef.current,
          syncStateRef.current,
          message,
        );
        docRef.current = newDoc;
        syncStateRef.current = newSyncState;

        materializeAndSetCells();

        // May need to send a response message
        syncToBackend();
      },
    );

    // Also listen for notebook:updated as fallback (transitional compatibility)
    const unlistenUpdated = webview.listen<CellSnapshot[]>(
      "notebook:updated",
      async (event) => {
        if (!isMounted) return;
        // Only use this fallback if automerge doc isn't initialized yet
        if (initializedRef.current) return;

        const blobPort = blobPortPromiseRef.current
          ? await blobPortPromiseRef.current
          : null;

        const newCells = await cellSnapshotsToNotebookCells(
          event.payload,
          blobPort,
          outputCacheRef.current,
        );
        setCells(newCells);
      },
    );

    // Listen for daemon ready signal
    const unlistenReady = webview.listen("daemon:ready", () => {
      if (!isMounted) return;
      refreshBlobPort();
      // Re-initialize when daemon reconnects
      initialize();
    });

    // Listen for file opened via OS association
    const unlistenFileOpened = webview.listen("notebook:file-opened", () => {
      if (!isMounted) return;
      initializedRef.current = false;
      initialize();
    });

    // Listen for backend formatting changes
    const unlistenFormat = webview.listen<{
      cell_id: string;
      source: string;
    }>("cell:source_updated", (event) => {
      if (!isMounted) return;
      // Apply formatting change to local doc
      const doc = docRef.current;
      if (doc) {
        const cellIdx = doc.cells?.findIndex(
          (c) => c.id === event.payload.cell_id,
        );
        if (cellIdx !== undefined && cellIdx >= 0) {
          docRef.current = Automerge.change(doc, (d) => {
            Automerge.updateText(
              d,
              ["cells", cellIdx, "source"],
              event.payload.source,
            );
          });
          syncToBackend();
        }
      }
      // Also update React state directly for immediate feedback
      setCells((prev) =>
        prev.map((c) =>
          c.id === event.payload.cell_id
            ? { ...c, source: event.payload.source }
            : c,
        ),
      );
      setDirty(true);
    });

    // Listen for bulk output clearing
    const unlistenClear = webview.listen<string[]>(
      "cells:outputs_cleared",
      (event) => {
        if (!isMounted) return;
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

    initialize();

    return () => {
      isMounted = false;
      unlistenSync.then((fn) => fn());
      unlistenUpdated.then((fn) => fn());
      unlistenReady.then((fn) => fn());
      unlistenFileOpened.then((fn) => fn());
      unlistenFormat.then((fn) => fn());
      unlistenClear.then((fn) => fn());
    };
  }, [refreshBlobPort, materializeAndSetCells, syncToBackend]);

  // ── Cell mutations (local-first) ──────────────────────────────────

  const updateCellSource = useCallback(
    (cellId: string, source: string) => {
      const doc = docRef.current;
      if (doc) {
        const cellIdx = doc.cells?.findIndex((c) => c.id === cellId);
        if (cellIdx !== undefined && cellIdx >= 0) {
          docRef.current = Automerge.change(doc, (d) => {
            Automerge.updateText(d, ["cells", cellIdx, "source"], source);
          });
          syncToBackend();
        }
      }
      // Optimistic React state update
      setCells((prev) =>
        prev.map((c) => (c.id === cellId ? { ...c, source } : c)),
      );
      setDirty(true);
    },
    [syncToBackend],
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

      const doc = docRef.current;
      if (doc) {
        const afterIdx = afterCellId
          ? doc.cells?.findIndex((c) => c.id === afterCellId)
          : undefined;
        const insertIdx =
          afterIdx !== undefined && afterIdx >= 0 ? afterIdx + 1 : 0;

        docRef.current = Automerge.change(doc, (d) => {
          if (!d.cells) return;
          Automerge.splice(d, ["cells"], insertIdx, 0, [
            {
              id: cellId,
              cell_type: cellType,
              source: "",
              execution_count: "null",
              outputs: [],
            },
          ]);
        });
        syncToBackend();
      }

      // Also update React state optimistically
      setCells((prev) => {
        if (!afterCellId) return [newCell, ...prev];
        const idx = prev.findIndex((c) => c.id === afterCellId);
        if (idx === -1) return [newCell, ...prev];
        const next = [...prev];
        next.splice(idx + 1, 0, newCell);
        return next;
      });
      setFocusedCellId(cellId);
      setDirty(true);

      // Fire-and-forget to backend for legacy compatibility
      invoke("add_cell", {
        cellId,
        cellType,
        afterCellId: afterCellId ?? null,
      }).catch((e) => logger.error("[automerge-notebook] add_cell sync:", e));

      return newCell;
    },
    [syncToBackend],
  );

  const deleteCell = useCallback(
    (cellId: string) => {
      const doc = docRef.current;
      if (doc) {
        const cellIdx = doc.cells?.findIndex((c) => c.id === cellId);
        if (
          cellIdx !== undefined &&
          cellIdx >= 0 &&
          (doc.cells?.length ?? 0) > 1
        ) {
          docRef.current = Automerge.change(doc, (d) => {
            if (!d.cells) return;
            Automerge.splice(d, ["cells"], cellIdx, 1);
          });
          syncToBackend();
        }
      }

      // Optimistic React state update
      setCells((prev) => {
        if (prev.length <= 1) return prev;
        return prev.filter((c) => c.id !== cellId);
      });
      setDirty(true);

      // Fire-and-forget for legacy compatibility
      invoke("delete_cell", { cellId }).catch((e) =>
        logger.error("[automerge-notebook] delete_cell sync:", e),
      );
    },
    [syncToBackend],
  );

  // ── File operations (still through Tauri invoke) ──────────────────

  const save = useCallback(async () => {
    try {
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

  // ── Output callbacks (used by useDaemonKernel for real-time streaming) ──

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
