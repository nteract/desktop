import { invoke } from "@tauri-apps/api/core";
import type { NotebookHost } from "@nteract/notebook-host";
import { NotebookClient, SaveNotebookError } from "runtimed";
import { logger } from "./logger";

/**
 * Notebook file operations — save, open, clone.
 *
 * In-place save goes straight to the daemon via `host.transport`. Save-as
 * and clone still route through Tauri commands because they have
 * Tauri-only side effects (file dialog, recent-menu updates, window
 * creation, kernel relaunch).
 */

const IPYNB_FILTER = { name: "Jupyter Notebook", extensions: ["ipynb"] };

/**
 * Save the current notebook to disk.
 *
 * If the notebook already has a path, saves in place via the daemon.
 * Otherwise opens a save dialog for the user to choose a location and
 * forwards to `save_notebook_as` (still Tauri-side because save-as has
 * window/menu side effects).
 *
 * @param host - The notebook host (for dialogs and transport).
 * @param flushSync - Flush any pending debounced sync before saving so
 *   the daemon has the latest source when writing to disk.
 * @param hasPath - Whether the notebook has a saved path. Read from
 *   `runtimeState.path` by the caller; passed in so this helper doesn't
 *   need to round-trip to Tauri for the check.
 * @returns `true` if saved successfully, `false` on cancel or error.
 */
export async function saveNotebook(
  host: NotebookHost,
  flushSync: () => Promise<boolean | void>,
  hasPath: boolean,
): Promise<boolean> {
  try {
    const flushed = await flushSync();
    if (flushed === false) return false;

    if (hasPath) {
      const client = new NotebookClient({ transport: host.transport });
      await client.saveNotebook({ formatCells: true });
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
    if (e instanceof SaveNotebookError) {
      logger.error("[notebook-file-ops] Save failed:", e.message);
    } else {
      logger.error("[notebook-file-ops] Save failed:", e);
    }
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
 * Fork the current notebook into a new ephemeral (in-memory) notebook and
 * open it in a new window. No file dialog — the daemon seeds a new room
 * from the current doc, the window attaches to it. User can Save-As to
 * persist later.
 *
 * The `host` parameter is retained for signature compatibility with the
 * other file ops (save/open) but is currently unused; all state lookups
 * happen server-side in the Tauri command.
 */
export async function cloneNotebookFile(_host: NotebookHost): Promise<void> {
  try {
    await invoke("clone_notebook_to_ephemeral");
  } catch (e) {
    logger.error("[notebook-file-ops] Clone failed:", e);
  }
}
