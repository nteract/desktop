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

export function useNotebook() {
  const [cells, setCells] = useState<NotebookCell[]>([]);
  const [focusedCellId, setFocusedCellId] = useState<string | null>(null);
  const [dirty, setDirty] = useState(false);
  const outputCacheRef = useRef<Map<string, JupyterOutput>>(new Map());
  // Store blob port promise so event handlers can await it
  const blobPortPromiseRef = useRef<Promise<number | null> | null>(null);

  // Helper to refresh blob port (called on mount and daemon:ready)
  const refreshBlobPort = useCallback(() => {
    blobPortPromiseRef.current = invoke<number>("get_blob_port").catch((e) => {
      logger.warn("[notebook] Failed to get blob port:", e);
      return null;
    });
  }, []);

  // Fetch blob port on mount for manifest resolution
  useEffect(() => {
    refreshBlobPort();
  }, [refreshBlobPort]);

  const loadCells = useCallback(() => {
    invoke<NotebookCell[]>("load_notebook")
      .then((loadedCells) => {
        setCells(loadedCells);
        if (loadedCells.length > 0) {
          setFocusedCellId(loadedCells[0].id);
        }
      })
      .catch((e) => logger.error("[notebook] Load failed:", e));
  }, []);

  useEffect(() => {
    loadCells();
  }, [loadCells]);

  // Reload cells when a file is opened via OS file association
  useEffect(() => {
    const webview = getCurrentWebview();
    const unlisten = webview.listen("notebook:file-opened", () => {
      loadCells();
    });
    return () => {
      unlisten.then((fn) => fn());
    };
  }, [loadCells]);

  // Listen for backend-initiated cell source updates (e.g., from formatting)
  useEffect(() => {
    const webview = getCurrentWebview();
    const unlisten = webview.listen<{ cell_id: string; source: string }>(
      "cell:source_updated",
      (event) => {
        setCells((prev) =>
          prev.map((c) =>
            c.id === event.payload.cell_id
              ? { ...c, source: event.payload.source }
              : c,
          ),
        );
        setDirty(true);
      },
    );
    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);

  // Listen for backend-initiated bulk output clearing (run all / restart & run all)
  useEffect(() => {
    const webview = getCurrentWebview();
    const unlisten = webview.listen<string[]>(
      "cells:outputs_cleared",
      (event) => {
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
      unlisten.then((fn) => fn());
    };
  }, []);

  // Listen for cross-window sync updates from the Automerge daemon
  useEffect(() => {
    let isMounted = true;
    const webview = getCurrentWebview();

    const unlisten = webview.listen<CellSnapshot[]>(
      "notebook:updated",
      async (event) => {
        if (!isMounted) return;

        // Wait for blob port to be available (needed for manifest resolution)
        const blobPort = blobPortPromiseRef.current
          ? await blobPortPromiseRef.current
          : null;

        // Resolve manifest hashes to full outputs
        const newCells = await cellSnapshotsToNotebookCells(
          event.payload,
          blobPort,
          outputCacheRef.current,
        );

        // Trust Automerge as source of truth for outputs.
        // The daemon writes outputs to Automerge before broadcasting,
        // so Automerge always has the canonical output state.
        setCells(newCells);
      },
    );

    // Listen for daemon ready signal before requesting Automerge state.
    // The backend emits daemon:ready after notebook sync is initialized.
    const unlistenReady = webview.listen("daemon:ready", () => {
      if (!isMounted) return;
      // Refresh blob port (daemon may have restarted with new port)
      refreshBlobPort();
      invoke("refresh_from_automerge").catch((e) =>
        logger.warn("[notebook-sync] refresh_from_automerge failed:", e),
      );
    });

    // Also try immediately in case daemon:ready was already emitted
    // (handles page reload when daemon is already connected)
    invoke("refresh_from_automerge").catch(() => {
      // Expected to fail if daemon isn't ready yet - daemon:ready listener will retry
    });

    return () => {
      isMounted = false;
      unlisten.then((fn) => fn());
      unlistenReady.then((fn) => fn());
    };
  }, [refreshBlobPort]);

  const updateCellSource = useCallback((cellId: string, source: string) => {
    setCells((prev) =>
      prev.map((c) => (c.id === cellId ? { ...c, source } : c)),
    );
    setDirty(true);
    invoke("update_cell_source", { cellId, source }).catch((e) =>
      logger.error("[notebook] Update cell source failed:", e),
    );
  }, []);

  /**
   * Clear outputs and execution count for a cell.
   * Called before queuing a cell for execution to ensure a clean slate.
   */
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
      // Fire-and-forget — backend uses the frontend-generated cellId
      invoke("add_cell", {
        cellId,
        cellType,
        afterCellId: afterCellId ?? null,
      }).catch((e) => logger.error("[notebook] add_cell sync failed:", e));
      return newCell;
    },
    [],
  );

  const deleteCell = useCallback((cellId: string) => {
    // Optimistic update — guard against deleting the last cell locally
    setCells((prev) => {
      if (prev.length <= 1) return prev;
      return prev.filter((c) => c.id !== cellId);
    });
    setDirty(true);
    // Fire-and-forget sync to backend
    invoke("delete_cell", { cellId }).catch((e) =>
      logger.error("[notebook] delete_cell sync failed:", e),
    );
  }, []);

  const save = useCallback(async () => {
    try {
      // Check if we have a file path
      const hasPath = await invoke<boolean>("has_notebook_path");

      if (hasPath) {
        // Save to existing path
        await invoke("save_notebook");
      } else {
        // Get default directory from backend (~/notebooks)
        const defaultDir = await invoke<string>("get_default_save_directory");

        // Show Save As dialog
        const filePath = await saveDialog({
          filters: [{ name: "Jupyter Notebook", extensions: ["ipynb"] }],
          defaultPath: `${defaultDir}/Untitled.ipynb`,
        });

        if (!filePath) {
          // User cancelled
          return;
        }

        // Save to the selected path
        await invoke("save_notebook_as", { path: filePath });
      }

      setDirty(false);
    } catch (e) {
      logger.error("[notebook] Save failed:", e);
    }
  }, []);

  const openNotebook = useCallback(async () => {
    try {
      const filePath = await openDialog({
        multiple: false,
        filters: [{ name: "Jupyter Notebook", extensions: ["ipynb"] }],
      });

      if (!filePath || typeof filePath !== "string") {
        // User cancelled or unexpected type
        return;
      }

      // Open the notebook in a new window
      await invoke("open_notebook_in_new_window", { path: filePath });
    } catch (e) {
      logger.error("[notebook] Open failed:", e);
    }
  }, []);

  const cloneNotebook = useCallback(async () => {
    try {
      // Get default directory from backend (~/notebooks)
      const defaultDir = await invoke<string>("get_default_save_directory");

      // Show Save dialog for the clone
      const filePath = await saveDialog({
        filters: [{ name: "Jupyter Notebook", extensions: ["ipynb"] }],
        defaultPath: `${defaultDir}/Untitled-Clone.ipynb`,
      });

      if (!filePath) {
        return; // User cancelled
      }

      // Clone notebook with fresh env_id and save to path
      await invoke("clone_notebook_to_path", { path: filePath });

      // Open the cloned notebook in a new window
      await invoke("open_notebook_in_new_window", { path: filePath });
    } catch (e) {
      logger.error("[notebook] Clone failed:", e);
    }
  }, []);

  const appendOutput = useCallback((cellId: string, output: JupyterOutput) => {
    setCells((prev) =>
      prev.map((c) => {
        if (c.id !== cellId || c.cell_type !== "code") return c;
        const outputs = [...c.outputs];
        // Merge consecutive stream outputs of the same name
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

  /**
   * Format a cell's source code using the appropriate formatter.
   * The backend handles the formatting and emits a cell:source_updated event
   * if the source changed, which updates the React state automatically.
   */
  const formatCell = useCallback(async (cellId: string) => {
    try {
      const result = await invoke<{
        source: string;
        changed: boolean;
        error: string | null;
      }>("format_cell", { cellId });

      if (result.error) {
        logger.warn("[notebook] Format cell warning:", result.error);
      }

      return result;
    } catch (e) {
      logger.error("[notebook] Format cell failed:", e);
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
