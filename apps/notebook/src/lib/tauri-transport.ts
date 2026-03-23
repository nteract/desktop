/**
 * TauriTransport — NotebookTransport implementation for the Tauri desktop app.
 *
 * Bridges the runtimed library to Tauri IPC:
 * - `sendFrame` → `invoke("send_frame", rawBytes)`
 * - `onFrame` → `getCurrentWebview().listen("notebook:frame")`
 * - `sendRequest` → `invoke(commandName, args)` for daemon commands
 *
 * This is the only file in the frontend that imports both the runtimed
 * transport interface and Tauri APIs. The SyncEngine and the rest of the
 * library are transport-agnostic.
 *
 * @module
 */

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import type { FrameTypeValue, NotebookTransport, Unsubscribe } from "runtimed";

// ── TauriTransport ──────────────────────────────────────────────────

export class TauriTransport implements NotebookTransport {
  #connected = false;
  #subscribers: Set<(frame: Uint8Array) => void> = new Set();
  #unlistenFrame: (() => void) | null = null;
  #unlistenPromise: Promise<() => void> | null = null;

  /**
   * Connect to the Tauri webview event system.
   *
   * Starts listening for `notebook:frame` events from the Rust relay.
   * Must be called before `sendFrame` or `onFrame` will work.
   *
   * Safe to call multiple times — subsequent calls are no-ops if
   * already connected.
   */
  async connect(): Promise<void> {
    if (this.#connected) return;

    const webview = getCurrentWebview();

    // Listen for inbound frames from the Rust relay.
    // The payload is `number[]` (byte array) — the full typed frame
    // including the type byte.
    this.#unlistenPromise = webview.listen<number[]>(
      "notebook:frame",
      (event) => {
        if (!this.#connected) return;
        const frame = new Uint8Array(event.payload);
        for (const cb of this.#subscribers) {
          try {
            cb(frame);
          } catch (err) {
            console.error("[TauriTransport] subscriber error:", err);
          }
        }
      },
    );

    // Wait for the listener to be registered before marking connected.
    this.#unlistenFrame = await this.#unlistenPromise;
    this.#connected = true;
  }

  // ── NotebookTransport implementation ───────────────────────────

  get connected(): boolean {
    return this.#connected;
  }

  async sendFrame(
    frameType: FrameTypeValue,
    payload: Uint8Array,
  ): Promise<void> {
    if (!this.#connected) {
      throw new Error("TauriTransport: not connected");
    }

    // Build the wire frame: [type_byte, ...payload]
    const frame = new Uint8Array(1 + payload.length);
    frame[0] = frameType;
    frame.set(payload, 1);

    // Send as raw binary via Tauri IPC.
    // The Rust `send_frame` command accepts `InvokeBody::Raw`.
    await invoke("send_frame", frame);
  }

  onFrame(callback: (frame: Uint8Array) => void): Unsubscribe {
    this.#subscribers.add(callback);
    return () => {
      this.#subscribers.delete(callback);
    };
  }

  async sendRequest<T = unknown>(request: unknown): Promise<T> {
    if (!this.#connected) {
      throw new Error("TauriTransport: not connected");
    }

    // Daemon requests are sent as named Tauri commands.
    // The request object must have an `action` field that maps to a
    // Tauri command name (e.g., "execute_cell" → "execute_cell_via_daemon").
    //
    // This is a thin dispatch layer — the actual command routing is
    // handled by Tauri's command system on the Rust side.
    const req = request as Record<string, unknown>;
    const action = req.action as string;

    if (!action) {
      throw new Error(
        "TauriTransport.sendRequest: request must have an 'action' field",
      );
    }

    // Map action names to Tauri command names.
    const commandName = ACTION_TO_COMMAND[action];
    if (!commandName) {
      throw new Error(`TauriTransport.sendRequest: unknown action '${action}'`);
    }

    // Strip the action field and pass the rest as args.
    const { action: _, ...args } = req;
    return invoke<T>(commandName, args);
  }

  disconnect(): void {
    if (!this.#connected) return;
    this.#connected = false;

    // Tear down the Tauri event listener.
    if (this.#unlistenFrame) {
      this.#unlistenFrame();
      this.#unlistenFrame = null;
    } else if (this.#unlistenPromise) {
      // Listener was still registering — clean up when it resolves.
      this.#unlistenPromise.then((fn) => fn()).catch(() => {});
    }
    this.#unlistenPromise = null;

    // Clear all subscribers.
    this.#subscribers.clear();
  }
}

// ── Action → Tauri command mapping ──────────────────────────────────

/**
 * Maps request action names to Tauri invoke command names.
 *
 * The transport layer translates between the library's generic action
 * names and the app-specific Tauri commands registered in Rust.
 *
 * To add a new daemon command:
 * 1. Register it in `crates/notebook/src/lib.rs` as a `#[tauri::command]`
 * 2. Add the mapping here
 * 3. The library can now `transport.sendRequest({ action: "...", ... })`
 */
const ACTION_TO_COMMAND: Record<string, string> = {
  // Kernel lifecycle
  launch_kernel: "launch_kernel_via_daemon",
  interrupt: "interrupt_via_daemon",
  shutdown_kernel: "shutdown_kernel_via_daemon",
  restart_kernel: "restart_kernel_via_daemon",

  // Cell execution
  execute_cell: "execute_cell_via_daemon",
  run_all_cells: "run_all_cells_via_daemon",
  clear_outputs: "clear_outputs_via_daemon",

  // File operations
  save_notebook: "save_notebook",
  save_notebook_as: "save_notebook_as",

  // Environment
  sync_environment: "sync_environment_via_daemon",
  get_daemon_queue_state: "get_daemon_queue_state",

  // Comm (widgets)
  send_comm: "send_comm_via_daemon",

  // Trust
  approve_trust: "approve_notebook_trust",

  // Misc
  reconnect: "reconnect_to_daemon",
  mark_clean: "mark_notebook_clean",
};
