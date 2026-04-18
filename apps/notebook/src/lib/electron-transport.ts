/**
 * ElectronTransport — NotebookTransport for the dev-only Electron harness.
 *
 * Mirrors TauriTransport but routes through `window.electronAPI` exposed by the
 * harness preload (see apps/dev-harness-electron/src/preload/index.js). The
 * harness main process opens the daemon's Unix socket directly — no new network
 * listener is added.
 *
 * Only active when the renderer is running inside the Electron harness. The
 * transport factory in `transport.ts` selects it based on `window.electronAPI`.
 */

import type {
  FrameListener,
  NotebookRequest,
  NotebookTransport,
} from "runtimed";
import { logger } from "./logger";

interface ElectronAPI {
  kind: "dev-harness-electron";
  sendFrame(type: number, payload: Uint8Array): Promise<{ ok: boolean; error?: string }>;
  onFrame(callback: (bytes: number[] | { __daemonDisconnected: true }) => void): () => void;
  sendRequest(request: unknown): Promise<unknown>;
  info(): Promise<{ notebookId: string | null; cellCount: number | null }>;
  signalReady(): Promise<{ ok: boolean }>;
}

declare global {
  interface Window {
    electronAPI?: ElectronAPI;
  }
}

export function isElectronHarness(): boolean {
  return (
    typeof window !== "undefined" &&
    typeof window.electronAPI !== "undefined" &&
    window.electronAPI.kind === "dev-harness-electron"
  );
}

export class ElectronTransport implements NotebookTransport {
  private _connected = true;
  private unlisteners: Array<() => void> = [];

  get connected(): boolean {
    return this._connected;
  }

  async sendFrame(frameType: number, payload: Uint8Array): Promise<void> {
    if (!window.electronAPI) throw new Error("ElectronTransport: no electronAPI");
    const result = await window.electronAPI.sendFrame(frameType, payload);
    if (!result.ok) {
      throw new Error(result.error || `sendFrame(0x${frameType.toString(16)}) failed`);
    }
  }

  onFrame(callback: FrameListener): () => void {
    if (!window.electronAPI) throw new Error("ElectronTransport: no electronAPI");
    const unlisten = window.electronAPI.onFrame((bytes) => {
      if (
        bytes &&
        typeof bytes === "object" &&
        (bytes as { __daemonDisconnected?: true }).__daemonDisconnected
      ) {
        this._connected = false;
        return;
      }
      try {
        callback(bytes as number[]);
      } catch (err) {
        // Same protection TauriTransport added — don't let a single bad
        // frame kill the listener.
        logger.error("[electron-transport] notebook-frame handler threw:", err);
      }
    });
    // Tell main it's safe to replay any frames that arrived before we
    // subscribed. Idempotent on the main side.
    window.electronAPI.signalReady().catch((err) => {
      logger.warn("[electron-transport] signalReady failed:", err);
    });
    this.unlisteners.push(unlisten);
    return unlisten;
  }

  async sendRequest(request: unknown): Promise<unknown> {
    if (!window.electronAPI) throw new Error("ElectronTransport: no electronAPI");
    // NotebookRequest is serialized as-is; the main process wraps it in a
    // type-0x01 frame. Responses come back as a bare `NotebookResponse`.
    const req = request as NotebookRequest;
    return window.electronAPI.sendRequest(req);
  }

  disconnect(): void {
    this._connected = false;
    for (const u of this.unlisteners) u();
    this.unlisteners = [];
  }
}
