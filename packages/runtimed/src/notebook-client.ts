/**
 * NotebookClient — typed kernel command interface.
 *
 * Provides typed methods for all kernel operations (execute, launch,
 * interrupt, etc.) over a pluggable transport. Separate from SyncEngine
 * because request/response is a different pattern than CRDT sync.
 *
 * Zero Tauri / React / browser dependencies.
 */

import type { SyncEngineLogger } from "./sync-engine";
import type { NotebookTransport } from "./transport";
import type {
  CommRequestMessage,
  CompletionItem,
  DependencyGuard,
  GuardedNotebookProvenance,
  HistoryEntry,
  NotebookRequest,
  NotebookResponse,
  SaveErrorKind,
} from "./request-types";

/**
 * Thrown when `NotebookClient.saveNotebook` receives a structured
 * `SaveErrorKind` from the daemon. Callers that need to branch on the
 * kind (e.g., to surface the conflicting UUID on `path_already_open`)
 * can inspect `.kind`. `.message` is a user-facing rendering suitable
 * for display.
 */
export class SaveNotebookError extends Error {
  readonly kind: SaveErrorKind;
  constructor(kind: SaveErrorKind) {
    super(formatSaveError(kind));
    this.name = "SaveNotebookError";
    this.kind = kind;
  }
}

function formatSaveError(kind: SaveErrorKind): string {
  switch (kind.type) {
    case "path_already_open":
      return (
        `Cannot save: ${kind.path} is already open in another notebook window. ` +
        `Close that window first, or choose a different path.`
      );
    case "io":
      return `Failed to save notebook: ${kind.message}`;
  }
}

const nullLogger: SyncEngineLogger = {
  debug() {},
  info() {},
  warn() {},
  error() {},
};

export interface NotebookClientOptions {
  transport: NotebookTransport;
  logger?: SyncEngineLogger;
}

export class NotebookClient {
  private readonly transport: NotebookTransport;
  private readonly log: SyncEngineLogger;

  constructor(opts: NotebookClientOptions) {
    this.transport = opts.transport;
    this.log = opts.logger ?? nullLogger;
  }

  /** Send a typed request and return the response. */
  async sendRequest(request: NotebookRequest): Promise<NotebookResponse> {
    return this.transport.sendRequest(request) as Promise<NotebookResponse>;
  }

  /** Launch a kernel via the daemon. */
  async launchKernel(
    kernelType: string,
    envSource: string,
    notebookPath?: string,
  ): Promise<NotebookResponse> {
    this.log.debug("[notebook-client] Launching kernel:", kernelType, envSource);
    try {
      return await this.sendRequest({
        type: "launch_kernel",
        kernel_type: kernelType,
        env_source: envSource,
        notebook_path: notebookPath,
      });
    } catch (e) {
      this.log.error("[notebook-client] Launch failed:", e);
      throw e;
    }
  }

  /** Execute a cell (daemon reads source from synced document). */
  async executeCell(cellId: string): Promise<NotebookResponse> {
    this.log.debug("[notebook-client] Executing cell:", cellId);
    try {
      return await this.sendRequest({
        type: "execute_cell",
        cell_id: cellId,
      });
    } catch (e) {
      this.log.error("[notebook-client] Execute failed:", e);
      throw e;
    }
  }

  /** Execute a cell only if it still matches the observed trust-dialog state. */
  async executeCellGuarded(
    cellId: string,
    provenance: GuardedNotebookProvenance,
  ): Promise<NotebookResponse> {
    this.log.debug("[notebook-client] Executing guarded cell:", cellId);
    try {
      return await this.sendRequest({
        type: "execute_cell_guarded",
        cell_id: cellId,
        observed_heads: provenance.observed_heads,
      });
    } catch (e) {
      this.log.error("[notebook-client] Guarded execute failed:", e);
      throw e;
    }
  }

  /** Clear outputs for a cell. */
  async clearOutputs(cellId: string): Promise<NotebookResponse> {
    try {
      return await this.sendRequest({
        type: "clear_outputs",
        cell_id: cellId,
      });
    } catch (e) {
      this.log.error("[notebook-client] Clear outputs failed:", e);
      throw e;
    }
  }

  /** Interrupt kernel execution. */
  async interruptKernel(): Promise<NotebookResponse> {
    try {
      return await this.sendRequest({ type: "interrupt_execution" });
    } catch (e) {
      this.log.error("[notebook-client] Interrupt failed:", e);
      throw e;
    }
  }

  /** Shutdown the kernel. */
  async shutdownKernel(): Promise<NotebookResponse> {
    try {
      return await this.sendRequest({ type: "shutdown_kernel" });
    } catch (e) {
      this.log.error("[notebook-client] Shutdown failed:", e);
      throw e;
    }
  }

  /** Hot-sync environment — install new packages without restart (UV only). */
  async syncEnvironment(guard?: DependencyGuard): Promise<NotebookResponse> {
    try {
      const response = await this.sendRequest({
        type: "sync_environment",
        ...(guard
          ? {
              guard: {
                observed_heads: guard.observed_heads,
                dependency_fingerprint: guard.dependency_fingerprint,
              },
            }
          : {}),
      });
      if ((response as { result: string }).result === "error") {
        this.log.error("[notebook-client] Sync env failed:", (response as { error: string }).error);
      }
      return response;
    } catch (e) {
      this.log.error("[notebook-client] Sync environment failed:", e);
      throw e;
    }
  }

  /** Approve the current dependency metadata and let the daemon write trust fields. */
  async approveTrust(dependencyFingerprint?: string): Promise<NotebookResponse> {
    try {
      return await this.sendRequest({
        type: "approve_trust",
        ...(dependencyFingerprint !== undefined
          ? { dependency_fingerprint: dependencyFingerprint }
          : {}),
      });
    } catch (e) {
      this.log.error("[notebook-client] Approve trust failed:", e);
      throw e;
    }
  }

  /** Run all code cells (daemon reads from synced doc). */
  async runAllCells(): Promise<NotebookResponse> {
    this.log.debug("[notebook-client] Running all cells");
    try {
      return await this.sendRequest({ type: "run_all_cells" });
    } catch (e) {
      this.log.error("[notebook-client] Run all cells failed:", e);
      throw e;
    }
  }

  /** Run all code cells only if they still match the observed trust-dialog state. */
  async runAllCellsGuarded(provenance: GuardedNotebookProvenance): Promise<NotebookResponse> {
    this.log.debug("[notebook-client] Running all cells with guard");
    try {
      return await this.sendRequest({
        type: "run_all_cells_guarded",
        observed_heads: provenance.observed_heads,
      });
    } catch (e) {
      this.log.error("[notebook-client] Guarded run all cells failed:", e);
      throw e;
    }
  }

  /**
   * Search the kernel's input history.
   *
   * Throws on `no_kernel` (caller can prompt the user to start one) or on
   * daemon-side errors — callers rely on these rejections to distinguish
   * "no results" from "can't search right now" (e.g., the
   * `HistorySearchDialog`'s "Start a kernel to search history" state).
   */
  async getHistory(pattern: string | null, n: number, unique: boolean): Promise<HistoryEntry[]> {
    try {
      const response = await this.sendRequest({
        type: "get_history",
        pattern,
        n,
        unique,
      });
      const result = (response as { result: string }).result;
      if (result === "history_result") {
        return (response as { result: "history_result"; entries: HistoryEntry[] }).entries;
      }
      if (result === "no_kernel") {
        throw new Error("No kernel running");
      }
      if (result === "error") {
        throw new Error((response as { error: string }).error);
      }
      throw new Error(`Unexpected response for get_history: ${result}`);
    } catch (e) {
      this.log.error("[notebook-client] Get history failed:", e);
      throw e;
    }
  }

  /** Request code completions from the kernel. */
  async complete(
    code: string,
    cursorPos: number,
  ): Promise<{ items: CompletionItem[]; cursorStart: number; cursorEnd: number }> {
    try {
      const response = await this.sendRequest({
        type: "complete",
        code,
        cursor_pos: cursorPos,
      });
      if ((response as { result: string }).result === "completion_result") {
        const r = response as {
          result: "completion_result";
          items: CompletionItem[];
          cursor_start: number;
          cursor_end: number;
        };
        return { items: r.items, cursorStart: r.cursor_start, cursorEnd: r.cursor_end };
      }
      return { items: [], cursorStart: cursorPos, cursorEnd: cursorPos };
    } catch (e) {
      this.log.error("[notebook-client] Complete failed:", e);
      throw e;
    }
  }

  /**
   * Save the notebook to disk via the daemon.
   *
   * Pass `path` to save-as. Without `path`, saves in place — the daemon
   * uses the room's current path and returns `save_error` if the room
   * is still untitled.
   *
   * Throws `SaveNotebookError` on structured `save_error` responses (the
   * `.kind` payload carries `path_already_open` / `io` details). Throws
   * a plain `Error` on transport failures or unexpected response shapes.
   */
  async saveNotebook(options: { formatCells: boolean; path?: string }): Promise<{ path: string }> {
    const request: NotebookRequest = {
      type: "save_notebook",
      format_cells: options.formatCells,
      ...(options.path !== undefined ? { path: options.path } : {}),
    };

    let response: NotebookResponse;
    try {
      response = await this.sendRequest(request);
    } catch (e) {
      this.log.error("[notebook-client] Save request failed:", e);
      throw e;
    }

    switch (response.result) {
      case "notebook_saved":
        return { path: response.path };
      case "save_error":
        throw new SaveNotebookError(response.error);
      case "error":
        throw new Error(`Daemon save failed: ${response.error}`);
      default:
        throw new Error(`Unexpected save_notebook response: ${JSON.stringify(response)}`);
    }
  }

  /** Send a comm message to the kernel (for widget interactions). */
  async sendComm(message: {
    header: Record<string, unknown>;
    parent_header?: Record<string, unknown> | null;
    metadata?: Record<string, unknown>;
    content: Record<string, unknown>;
    buffers?: ArrayBuffer[];
    channel?: string;
  }): Promise<NotebookResponse> {
    const msgType = message.header.msg_type as string;
    this.log.debug("[notebook-client] Sending comm message:", msgType);
    try {
      // Convert ArrayBuffer[] to number[][] for JSON serialization
      const buffers: number[][] = (message.buffers ?? []).map((buf) =>
        Array.from(new Uint8Array(buf)),
      );

      const fullMessage: CommRequestMessage = {
        header: message.header,
        parent_header: message.parent_header ?? null,
        metadata: message.metadata ?? {},
        content: message.content,
        buffers,
        channel: message.channel ?? "shell",
      };

      const response = await this.sendRequest({
        type: "send_comm",
        message: fullMessage,
      });

      if ((response as { result: string }).result === "error") {
        this.log.error(
          "[notebook-client] Send comm failed:",
          (response as { error: string }).error,
        );
      } else if ((response as { result: string }).result === "no_kernel") {
        this.log.error("[notebook-client] Send comm failed: no kernel running");
      }
      return response;
    } catch (e) {
      this.log.error("[notebook-client] Send comm failed:", e);
      throw e;
    }
  }
}
