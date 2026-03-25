/**
 * DirectTransport — test transport that connects a SyncEngine to a
 * "server" handle without any network or IPC.
 *
 * Simulates the daemon side: inbound frames from the server are delivered
 * to the client's `onFrame` subscribers, and outbound frames from the
 * client are applied to the server handle.
 *
 * Usage:
 * ```ts
 * const transport = new DirectTransport(serverHandle);
 * const engine = new SyncEngine({
 *   getHandle: () => clientHandle,
 *   transport,
 * });
 * engine.start();
 *
 * // Push server changes to the client:
 * serverHandle.update_source("cell-1", "hello");
 * transport.pushServerChanges();
 * ```
 */

import { FrameType } from "./transport";
import type { NotebookTransport, FrameListener } from "./transport";

// ── Server handle interface ──────────────────────────────────────────

/**
 * Minimal interface for the server-side handle used by DirectTransport.
 *
 * This is the subset of NotebookHandle methods needed to simulate the daemon:
 * - Generate sync messages (to push to the client)
 * - Receive sync messages (from the client)
 */
export interface ServerHandle {
  /** Generate a sync message for pending changes. */
  flush_local_changes(): Uint8Array | null;

  /** Apply a sync message from the client. Returns whether doc changed. */
  receive_sync_message(message: Uint8Array): boolean;

  /** Reset sync state (for reconnection simulation). */
  reset_sync_state(): void;
}

// ── DirectTransport ──────────────────────────────────────────────────

/**
 * Test transport connecting a client SyncEngine directly to a server handle.
 *
 * Frame delivery is synchronous — `pushServerChanges()` generates a sync
 * message from the server and delivers it immediately. This makes tests
 * deterministic.
 */
export class DirectTransport implements NotebookTransport {
  private readonly server: ServerHandle;
  private subscribers = new Set<FrameListener>();
  private _connected = true;

  /** Track sent frames for test assertions. */
  readonly sentFrames: Array<{ frameType: number; payload: Uint8Array }> = [];

  /** Track transport failures for rollback testing. */
  sendFailureCount = 0;

  /**
   * If true, `sendFrame` will reject — simulating transport failure.
   * Used to test rollback behavior (cancel_last_flush).
   */
  simulateFailure = false;

  constructor(server: ServerHandle) {
    this.server = server;
  }

  // ── NotebookTransport ──────────────────────────────────────────

  get connected(): boolean {
    return this._connected;
  }

  async sendFrame(frameType: number, payload: Uint8Array): Promise<void> {
    if (!this._connected) {
      throw new Error("DirectTransport: not connected");
    }

    if (this.simulateFailure) {
      this.sendFailureCount++;
      throw new Error("DirectTransport: simulated send failure");
    }

    this.sentFrames.push({ frameType, payload });

    // Route to server based on frame type.
    if (frameType === FrameType.AUTOMERGE_SYNC) {
      this.server.receive_sync_message(payload);
    }
    // RUNTIME_STATE_SYNC, PRESENCE, etc. — just record, no server action.
  }

  onFrame(callback: FrameListener): () => void {
    this.subscribers.add(callback);
    return () => {
      this.subscribers.delete(callback);
    };
  }

  disconnect(): void {
    this._connected = false;
    this.subscribers.clear();
  }

  // ── Test helpers ───────────────────────────────────────────────

  /**
   * Push the server's pending changes to all client subscribers.
   *
   * Generates a sync message from the server handle, wraps it as a
   * typed frame (0x00 prefix + payload), and delivers it to all
   * `onFrame` subscribers as a `number[]`.
   *
   * Returns true if a sync message was generated and delivered.
   */
  pushServerChanges(): boolean {
    const msg = this.server.flush_local_changes();
    if (!msg) return false;

    const frame = new Uint8Array(1 + msg.length);
    frame[0] = FrameType.AUTOMERGE_SYNC;
    frame.set(msg, 1);

    // Deliver as number[] to match Tauri's event payload format.
    this.deliver(Array.from(frame));
    return true;
  }

  /**
   * Push a broadcast event to all client subscribers.
   */
  pushBroadcast(payload: unknown): void {
    const json = JSON.stringify(payload);
    const bytes = new TextEncoder().encode(json);
    const frame = new Uint8Array(1 + bytes.length);
    frame[0] = FrameType.BROADCAST;
    frame.set(bytes, 1);
    this.deliver(Array.from(frame));
  }

  /**
   * Push a presence event to all client subscribers.
   */
  pushPresence(payload: Uint8Array): void {
    const frame = new Uint8Array(1 + payload.length);
    frame[0] = FrameType.PRESENCE;
    frame.set(payload, 1);
    this.deliver(Array.from(frame));
  }

  /**
   * Deliver a raw frame (number[]) to all subscribers.
   */
  deliver(frame: number[]): void {
    for (const cb of this.subscribers) {
      try {
        cb(frame);
      } catch (err) {
        console.error("[DirectTransport] subscriber error:", err);
      }
    }
  }

  /**
   * Run sync cycles between server and client until convergence.
   */
  syncUntilConverged(maxRounds = 10): number {
    let rounds = 0;
    for (let i = 0; i < maxRounds; i++) {
      const pushed = this.pushServerChanges();
      if (!pushed) break;
      rounds++;
    }
    return rounds;
  }

  /** Clear recorded sent frames. */
  clearSentFrames(): void {
    this.sentFrames.length = 0;
  }

  /** Reconnect after a disconnect. */
  reconnect(): void {
    this._connected = true;
  }
}
