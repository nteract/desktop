/**
 * `TauriTransport` — `NotebookTransport` implementation for the Tauri desktop app.
 *
 * Bridges the `runtimed` `SyncEngine` to the daemon via Tauri IPC:
 *   - `sendFrame` → `invoke("send_frame", bytes)`
 *   - `onFrame` → `getCurrentWebview().listen("notebook:frame")`
 *
 * Lives in this package (not in `apps/notebook`) so other hosts can depend
 * on a single canonical Tauri transport rather than re-implementing it.
 */

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { FrameListener, NotebookRequest, NotebookTransport } from "runtimed";

export class TauriTransport implements NotebookTransport {
  private _connected = true;
  private unlisteners: Array<() => void> = [];

  get connected(): boolean {
    return this._connected;
  }

  async sendFrame(frameType: number, payload: Uint8Array): Promise<void> {
    const frame = new Uint8Array(1 + payload.length);
    frame[0] = frameType;
    frame.set(payload, 1);
    return invoke("send_frame", frame);
  }

  onFrame(callback: FrameListener): () => void {
    const webview = getCurrentWebview();

    let unlistenFn: (() => void) | null = null;
    let cancelled = false;

    // IMPORTANT: wrap the callback in try/catch. Tauri's event system drops
    // listeners whose handlers throw — a single exception escaping here
    // silently unsubscribes the webview for the rest of its lifetime, and
    // the daemon's subsequent frames land nowhere. Catching preserves the
    // listener across a bad frame and surfaces the exception to the console
    // so the underlying issue is fixable.
    const unlistenPromise = webview.listen<number[]>("notebook:frame", (event) => {
      try {
        callback(event.payload);
      } catch (err) {
        console.error("[tauri-transport] notebook:frame handler threw:", err);
      }
    });

    unlistenPromise
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlistenFn = fn;
        }
      })
      .catch((err) => {
        console.error("[tauri-transport] failed to register notebook:frame listener:", err);
      });

    const unlisten = () => {
      cancelled = true;
      if (unlistenFn) {
        unlistenFn();
        unlistenFn = null;
      }
    };

    this.unlisteners.push(unlisten);
    return unlisten;
  }

  async sendRequest(request: unknown): Promise<unknown> {
    const req = request as NotebookRequest;
    switch (req.type) {
      case "launch_kernel":
        return invoke("launch_kernel_via_daemon", {
          kernelType: req.kernel_type,
          envSource: req.env_source,
          notebookPath: req.notebook_path,
        });
      case "execute_cell":
        return invoke("execute_cell_via_daemon", { cellId: req.cell_id });
      case "clear_outputs":
        return invoke("clear_outputs_via_daemon", { cellId: req.cell_id });
      case "interrupt":
        return invoke("interrupt_via_daemon");
      case "shutdown_kernel":
        return invoke("shutdown_kernel_via_daemon");
      case "sync_environment":
        return invoke("sync_environment_via_daemon");
      case "run_all_cells":
        return invoke("run_all_cells_via_daemon");
      case "send_comm":
        return invoke("send_comm_via_daemon", { message: req.message });
      default:
        throw new Error(`TauriTransport: unknown request type: ${(req as { type: string }).type}`);
    }
  }

  disconnect(): void {
    this._connected = false;
    for (const unlisten of this.unlisteners) {
      unlisten();
    }
    this.unlisteners = [];
  }
}
