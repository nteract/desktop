/**
 * DirectTransport — a test transport that connects a SyncEngine to a
 * "server" NotebookHandle without any network or IPC.
 *
 * Simulates the daemon side: inbound frames from the server are delivered
 * to the client's `onFrame` subscribers, and outbound frames from the
 * client are applied to the server handle.
 *
 * The request/response channel uses a pluggable handler function so tests
 * can simulate ExecuteCell, LaunchKernel, etc.
 *
 * Usage:
 * ```ts
 * const server = new NotebookHandle("test-server");
 * const transport = new DirectTransport(server);
 *
 * // Optional: handle requests
 * transport.onRequest = (req) => ({ result: "ok" });
 *
 * const engine = new SyncEngine(clientHandle, transport);
 * engine.start();
 *
 * // Push server changes to the client:
 * server.update_source("cell-1", "hello");
 * transport.pushServerChanges();
 * ```
 *
 * @module
 */

import { FrameType } from "./transport.ts";
import type {
  NotebookTransport,
  FrameTypeValue,
  Unsubscribe,
} from "./transport.ts";

/**
 * Minimal interface for the server-side NotebookHandle used by DirectTransport.
 *
 * This is the subset of NotebookHandle methods needed to simulate the daemon:
 * - Generate sync messages (to push to the client)
 * - Receive sync messages (from the client)
 * - Generate runtime state sync replies
 */
export interface ServerHandle {
  /** Generate a sync message for pending changes. */
  flush_local_changes(): Uint8Array | undefined;

  /** Apply a sync message from the client. */
  receive_sync_message(message: Uint8Array): boolean;

  /** Reset sync state (for reconnection simulation). */
  reset_sync_state(): void;
}

/** Handler for request/response simulation. */
export type RequestHandler = (request: unknown) => unknown | Promise<unknown>;

/**
 * Test transport that connects a client SyncEngine directly to a server
 * NotebookHandle, with no network layer.
 *
 * Frame delivery is synchronous by default — `pushServerChanges()` generates
 * a sync message from the server and delivers it to all `onFrame` subscribers
 * immediately. This makes tests deterministic.
 *
 * For async scenarios (testing debounce, retry, etc.), use `pushServerChangesAsync()`
 * which defers delivery to the next microtask.
 */
export class DirectTransport implements NotebookTransport {
  readonly #server: ServerHandle;
  #subscribers: Set<(frame: Uint8Array) => void> = new Set();
  #connected = true;

  /**
   * Pluggable request handler. Tests set this to simulate daemon responses.
   *
   * ```ts
   * transport.onRequest = (req) => {
   *   if (req.action === "execute_cell") return { result: "CellQueued", cell_id: req.cell_id };
   *   return { result: "error", error: "unknown action" };
   * };
   * ```
   */
  onRequest: RequestHandler = () => ({ result: "ok" });

  /** Track sent frames for test assertions. */
  readonly sentFrames: Array<{ frameType: FrameTypeValue; payload: Uint8Array }> =
    [];

  /** Track whether cancel_last_flush would have been needed (for test diagnostics). */
  sendFailureCount = 0;

  /**
   * If true, `sendFrame` will reject — simulating transport failure.
   * Used to test rollback behavior (cancel_last_flush).
   */
  simulateFailure = false;

  constructor(server: ServerHandle) {
    this.#server = server;
  }

  // ── NotebookTransport implementation ───────────────────────────

  get connected(): boolean {
    return this.#connected;
  }

  async sendFrame(
    frameType: FrameTypeValue,
    payload: Uint8Array,
  ): Promise<void> {
    this.#assertConnected();

    if (this.simulateFailure) {
      this.sendFailureCount++;
      throw new Error("DirectTransport: simulated send failure");
    }

    this.sentFrames.push({ frameType, payload });

    // Route the frame to the server based on type.
    switch (frameType) {
      case FrameType.AUTOMERGE_SYNC:
        // Client → server sync message.
        this.#server.receive_sync_message(payload);
        break;

      case FrameType.RUNTIME_STATE_SYNC:
        // Client → server runtime state sync reply.
        // In a real daemon this updates the server's state_sync_state.
        // For tests we just record it.
        break;

      case FrameType.PRESENCE:
        // Presence frames — record only.
        break;

      default:
        // Request/Response frames shouldn't come through sendFrame.
        break;
    }
  }

  onFrame(callback: (frame: Uint8Array) => void): Unsubscribe {
    this.#subscribers.add(callback);
    return () => {
      this.#subscribers.delete(callback);
    };
  }

  async sendRequest<T = unknown>(request: unknown): Promise<T> {
    this.#assertConnected();
    const result = await this.onRequest(request);
    return result as T;
  }

  disconnect(): void {
    this.#connected = false;
    this.#subscribers.clear();
  }

  // ── Test helpers ───────────────────────────────────────────────

  /**
   * Push the server's pending changes to all client subscribers.
   *
   * Generates a sync message from the server handle, wraps it as a
   * typed frame (0x00 + payload), and delivers it synchronously to
   * all `onFrame` subscribers.
   *
   * Call this after mutating the server handle to simulate the daemon
   * pushing changes to the client.
   *
   * Returns true if a sync message was generated and delivered.
   */
  pushServerChanges(): boolean {
    const msg = this.#server.flush_local_changes();
    if (!msg) return false;

    const frame = new Uint8Array(1 + msg.length);
    frame[0] = FrameType.AUTOMERGE_SYNC;
    frame.set(msg, 1);

    this.#deliver(frame);
    return true;
  }

  /**
   * Async variant of `pushServerChanges` — delivers on the next microtask.
   * Useful for testing debounce and timing behavior.
   */
  async pushServerChangesAsync(): Promise<boolean> {
    await Promise.resolve(); // yield to microtask queue
    return this.pushServerChanges();
  }

  /**
   * Push a raw broadcast event to all client subscribers.
   *
   * ```ts
   * transport.pushBroadcast({
   *   event: "execution_started",
   *   cell_id: "cell-1",
   *   execution_count: 1,
   * });
   * ```
   */
  pushBroadcast(payload: unknown): void {
    const json = JSON.stringify(payload);
    const bytes = new TextEncoder().encode(json);
    const frame = new Uint8Array(1 + bytes.length);
    frame[0] = FrameType.BROADCAST;
    frame.set(bytes, 1);

    this.#deliver(frame);
  }

  /**
   * Push a raw presence event to all client subscribers.
   */
  pushPresence(payload: Uint8Array): void {
    const frame = new Uint8Array(1 + payload.length);
    frame[0] = FrameType.PRESENCE;
    frame.set(payload, 1);

    this.#deliver(frame);
  }

  /**
   * Run a full sync cycle between server and client.
   *
   * Pushes server changes, then (if the client sent a reply via sendFrame),
   * pushes server changes again to complete the round-trip. Repeats until
   * convergence or maxRounds.
   *
   * This is the DirectTransport equivalent of the `syncHandles` helper
   * in the Deno WASM tests.
   */
  syncUntilConverged(maxRounds = 10): number {
    let rounds = 0;
    for (let i = 0; i < maxRounds; i++) {
      const pushed = this.pushServerChanges();
      const clientSentSync = this.sentFrames.some(
        (f, idx) =>
          idx >= this.sentFrames.length - 10 &&
          f.frameType === FrameType.AUTOMERGE_SYNC,
      );

      if (!pushed && !clientSentSync) break;
      rounds++;
    }
    return rounds;
  }

  /**
   * Clear recorded sent frames. Useful between test phases.
   */
  clearSentFrames(): void {
    this.sentFrames.length = 0;
  }

  /**
   * Reconnect after a disconnect (resets connected state).
   * Does NOT reset sync state — call `handle.reset_sync_state()` if needed.
   */
  reconnect(): void {
    this.#connected = true;
  }

  // ── Internal ───────────────────────────────────────────────────

  #deliver(frame: Uint8Array): void {
    for (const cb of this.#subscribers) {
      try {
        cb(frame);
      } catch (err) {
        console.error("[DirectTransport] subscriber error:", err);
      }
    }
  }

  #assertConnected(): void {
    if (!this.#connected) {
      throw new Error("DirectTransport: not connected");
    }
  }
}
