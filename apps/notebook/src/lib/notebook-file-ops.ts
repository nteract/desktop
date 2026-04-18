import { invoke } from "@tauri-apps/api/core";
import type { NotebookHost } from "@nteract/notebook-host";
import { logger } from "./logger";

/**
 * Notebook file operations — save, open, clone.
 *
 * Host-agnostic wrappers: dialogs go through `host.dialog`, the invoke
 * calls below remain until the daemon-request thin-wrapper PR. Taking
 * `host` as a parameter (vs. a module-level ref) keeps these functions
 * pure and trivially testable with a stub host.
 */

const IPYNB_FILTER = { name: "Jupyter Notebook", extensions: ["ipynb"] };

/**
 * Save the current notebook to disk.
 *
 * If the notebook already has a path, saves in place. Otherwise opens
 * a save dialog for the user to choose a location.
 *
 * @param host - The notebook host (for dialogs).
 * @param flushSync - Flush any pending debounced sync before saving so
 *   the daemon has the latest source when writing to disk.
 * @returns `true` if saved successfully, `false` on cancel or error.
 */
export async function saveNotebook(
  host: NotebookHost,
  flushSync: () => Promise<void>,
): Promise<boolean> {
  try {
    await flushSync();

    const hasPath = await invoke<boolean>("has_notebook_path");

    if (hasPath) {
      await invoke("save_notebook");
    } else {
      const defaultDir = await invoke<string>("get_default_save_directory");
      const filePath = await host.dialog.saveFile({
        filters: [IPYNB_FILTER],
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
export async function openNotebookFile(host: NotebookHost): Promise<void> {
  try {
    const filePath = await host.dialog.openFile({
      filters: [IPYNB_FILTER],
    });
    if (!filePath) return;
    await invoke("open_notebook_in_new_window", { path: filePath });
  } catch (e) {
    logger.error("[notebook-file-ops] Open failed:", e);
  }
}

/**
 * Clone the current notebook to a new file and open it in a new window.
 */
export async function cloneNotebookFile(host: NotebookHost): Promise<void> {
  try {
    const defaultDir = await invoke<string>("get_default_save_directory");
    const filePath = await host.dialog.saveFile({
      filters: [IPYNB_FILTER],
      defaultPath: `${defaultDir}/Untitled-Clone.ipynb`,
    });
    if (!filePath) return;
    await invoke("clone_notebook_to_path", { path: filePath });
    await invoke("open_notebook_in_new_window", { path: filePath });
  } catch (e) {
    logger.error("[notebook-file-ops] Clone failed:", e);
  }
}
