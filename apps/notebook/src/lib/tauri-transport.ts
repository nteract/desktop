/**
 * TauriTransport — NotebookTransport implementation for the Tauri desktop app.
 *
 * Bridges the runtimed SyncEngine to the daemon via Tauri IPC:
 *   - sendFrame → invoke("send_frame", bytes)
 *   - onFrame → getCurrentWebview().listen("notebook:frame")
 */

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { FrameListener, NotebookTransport } from "runtimed";

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

    // webview.listen returns Promise<UnlistenFn>. We track it for cleanup.
    let unlistenFn: (() => void) | null = null;
    let cancelled = false;

    const unlistenPromise = webview.listen<number[]>(
      "notebook:frame",
      (event) => {
        callback(event.payload);
      },
    );

    unlistenPromise
      .then((fn) => {
        if (cancelled) {
          fn();
        } else {
          unlistenFn = fn;
        }
      })
      .catch(() => {});

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

  disconnect(): void {
    this._connected = false;
    for (const unlisten of this.unlisteners) {
      unlisten();
    }
    this.unlisteners = [];
  }
}
