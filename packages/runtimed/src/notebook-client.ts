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
import type { CommRequestMessage, NotebookRequest, NotebookResponse } from "./request-types";

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
      return await this.sendRequest({ type: "interrupt" });
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
  async syncEnvironment(): Promise<NotebookResponse> {
    try {
      const response = await this.sendRequest({ type: "sync_environment" });
      if ((response as { result: string }).result === "error") {
        this.log.error("[notebook-client] Sync env failed:", (response as { error: string }).error);
      }
      return response;
    } catch (e) {
      this.log.error("[notebook-client] Sync environment failed:", e);
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
