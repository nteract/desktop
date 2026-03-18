import { invoke } from "@tauri-apps/api/core";
import {
  open as openDialog,
  save as saveDialog,
} from "@tauri-apps/plugin-dialog";
import { logger } from "./logger";

/**
 * Notebook file operations — save, open, clone.
 *
 * Pure Tauri IPC calls with no WASM/sync/store dependencies.
 * Extracted from useAutomergeNotebook to keep the hook focused on
 * document sync and cell mutations.
 */

/**
 * Save the current notebook to disk.
 *
 * If the notebook already has a path, saves in place. Otherwise opens
 * a save dialog for the user to choose a location.
 *
 * @param flushSync - Flush any pending debounced sync before saving so
 *   the daemon has the latest source when writing to disk.
 * @returns `true` if saved successfully, `false` on cancel or error.
 */
export async function saveNotebook(
  flushSync: () => Promise<void>,
): Promise<boolean> {
  try {
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
      if (!filePath) return false;
      await invoke("save_notebook_as", { path: filePath });
    }

    return true;
  } catch (e) {
    logger.error("[notebook-file-ops] Save failed:", e);
    return false;
  }
}

/**
 * Open a notebook file in a new window via a file picker dialog.
 */
export async function openNotebookFile(): Promise<void> {
  try {
    const filePath = await openDialog({
      multiple: false,
      filters: [{ name: "Jupyter Notebook", extensions: ["ipynb"] }],
    });
    if (!filePath || typeof filePath !== "string") return;
    await invoke("open_notebook_in_new_window", { path: filePath });
  } catch (e) {
    logger.error("[notebook-file-ops] Open failed:", e);
  }
}

/**
 * Clone the current notebook to a new file and open it in a new window.
 */
export async function cloneNotebookFile(): Promise<void> {
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
    logger.error("[notebook-file-ops] Clone failed:", e);
  }
}
