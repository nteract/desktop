/**
 * SyncEngine — core Automerge sync management for notebook documents.
 *
 * Extracted from the frontend's `frame-pipeline.ts`, `useAutomergeNotebook.ts`,
 * and `notebook-metadata.ts` into a transport-agnostic, framework-agnostic
 * module. This is the single owner of sync state — no other code should call
 * `flush_local_changes`, `cancel_last_flush`, or read inline replies directly.
 *
 * The engine:
 * 1. Receives inbound frames from a {@link NotebookTransport}
 * 2. Feeds them to the WASM `NotebookHandle.receive_frame()` for demuxing
 * 3. Sends inline sync replies back through the transport (with rollback on failure)
 * 4. Flushes local CRDT mutations on a debounced schedule
 * 5. Emits typed events for consumers (cell changes, broadcasts, presence, runtime state)
 *
 * Consumers (React frontend, Deno scripts, agents) subscribe to events
 * and call mutation methods. They never touch sync state directly.
 *
 * @module
 */

import type { NotebookTransport } from "./transport.ts";
import { FrameType } from "./transport.ts";

// ── Types ────────────────────────────────────────────────────────────

// We use the WASM handle as an opaque type so this module doesn't
// import wasm-bindgen directly — the caller provides the handle.
// This keeps the package decoupled from a specific WASM build path.

/**
 * Minimal interface for the WASM NotebookHandle methods the sync engine needs.
 *
 * This is a subset of the full `NotebookHandle` API — only the methods
 * involved in sync, frame processing, and local change flushing. Cell
 * mutations (add_cell, update_source, etc.) are called directly by
 * consumers and don't flow through the engine.
 */
export interface SyncableHandle {
  /** Demux an inbound frame and return typed events. Generates inline sync replies. */
  receive_frame(frame_bytes: Uint8Array): FrameEvent[] | undefined;

  /** Generate a sync message for pending local changes. */
  flush_local_changes(): Uint8Array | undefined;

  /** Roll back sync state after a failed flush or reply send. */
  cancel_last_flush(): void;

  /** Generate a sync reply for the RuntimeStateDoc. */
  generate_runtime_state_sync_reply(): Uint8Array | undefined;

  /** Reset all sync state (reconnection / page reload equivalent). */
  reset_sync_state(): void;
}

/**
 * A function that returns the current SyncableHandle, or null if unavailable.
 *
 * Using a getter instead of a direct reference ensures the engine always
 * reads the latest handle — critical for React strict mode where effects
 * mount/cleanup/mount and the handle ref can change between async operations.
 */
export type HandleGetter = () => SyncableHandle | null;

// ── Frame events (mirrored from runtimed-wasm FrameEvent enum) ──────

/** Changeset describing which cells changed and how. */
export interface CellChangeset {
  readonly changed: ChangedCell[];
  readonly added: string[];
  readonly removed: string[];
  readonly order_changed: boolean;
}

export interface ChangedCell {
  readonly cell_id: string;
  readonly fields: ChangedFields;
}

export interface ChangedFields {
  readonly source: boolean;
  readonly outputs: boolean;
  readonly cell_type: boolean;
  readonly execution_count: boolean;
  readonly metadata: boolean;
  readonly position: boolean;
}

export interface TextAttribution {
  readonly cell_id: string;
  readonly actor: string;
  readonly start: number;
  readonly end: number;
  readonly inserted: number;
  readonly deleted: number;
  readonly actors: string[];
}

/** Events produced by `NotebookHandle.receive_frame()`. */
export type FrameEvent =
  | SyncAppliedEvent
  | BroadcastEvent
  | PresenceEvent
  | RuntimeStateSyncAppliedEvent
  | UnknownFrameEvent;

export interface SyncAppliedEvent {
  readonly type: "sync_applied";
  readonly changed: boolean;
  readonly changeset?: CellChangeset;
  readonly attributions?: TextAttribution[];
  /** Inline sync reply bytes — send immediately via transport. */
  readonly reply?: number[];
}

export interface BroadcastEvent {
  readonly type: "broadcast";
  readonly payload: unknown;
}

export interface PresenceEvent {
  readonly type: "presence";
  readonly payload: unknown;
}

export interface RuntimeStateSyncAppliedEvent {
  readonly type: "runtime_state_sync_applied";
  readonly changed: boolean;
  readonly state?: unknown;
}

export interface UnknownFrameEvent {
  readonly type: "unknown";
  readonly frame_type: number;
}

// ── Sync engine events ──────────────────────────────────────────────

/**
 * Events emitted by the SyncEngine.
 *
 * These are higher-level than raw FrameEvents — the engine handles
 * sync reply sending internally, so consumers only see meaningful
 * state changes.
 */
export type SyncEngineEvent =
  | {
      type: "cells_changed";
      changeset: CellChangeset | null;
      attributions: TextAttribution[];
    }
  | { type: "initial_sync_complete" }
  | { type: "broadcast"; payload: unknown }
  | { type: "presence"; payload: unknown }
  | { type: "runtime_state_changed"; state: unknown }
  | { type: "sync_retry" }
  | { type: "error"; error: unknown; context: string };

export type SyncEngineEventType = SyncEngineEvent["type"];

type EventCallback = (event: SyncEngineEvent) => void;
type TypedEventCallback<T extends SyncEngineEventType> = (
  event: Extract<SyncEngineEvent, { type: T }>,
) => void;

// ── Configuration ───────────────────────────────────────────────────

export interface SyncEngineOptions {
  /**
   * Debounce interval (ms) for flushing local CRDT mutations.
   * Multiple rapid edits within this window are batched into a
   * single sync message. Default: 20ms.
   */
  flushDebounceMs?: number;

  /**
   * Timeout (ms) for the initial sync exchange. If the daemon doesn't
   * deliver document content within this window, the engine resets
   * sync state and retries. Default: 3000ms.
   */
  initialSyncTimeoutMs?: number;
}

const DEFAULT_FLUSH_DEBOUNCE_MS = 20;
const DEFAULT_INITIAL_SYNC_TIMEOUT_MS = 3000;

// ── SyncEngine ──────────────────────────────────────────────────────

/**
 * Core sync management for a notebook document.
 *
 * Owns the sync lifecycle:
 * - Inbound frame processing (demux → reply → emit events)
 * - Outbound sync (debounced flush of local CRDT mutations)
 * - Initial sync handshake with retry
 * - Runtime state doc sync replies
 *
 * The engine is framework-agnostic — it emits events via a simple
 * callback API. React hooks, Zustand stores, or plain callbacks can
 * all subscribe.
 *
 * Usage:
 * ```ts
 * const engine = new SyncEngine(() => handleRef.current, transport, { flushDebounceMs: 20 });
 * engine.on("cells_changed", (e) => updateStore(e.changeset));
 * engine.on("broadcast", (e) => handleBroadcast(e.payload));
 * engine.start();
 *
 * // After local CRDT mutations:
 * handleRef.current.update_source("cell-1", "new code");
 * engine.scheduleFlush();
 *
 * // Cleanup:
 * engine.stop();
 * ```
 */
export class SyncEngine {
  readonly #getHandle: HandleGetter;
  readonly #transport: NotebookTransport;
  readonly #options: Required<SyncEngineOptions>;

  // ── State ───────────────────────────────────────────────────────
  #running = false;
  #awaitingInitialSync = true;
  #unsubscribeFrame: (() => void) | null = null;

  // ── Flush debounce ──────────────────────────────────────────────
  #flushTimer: ReturnType<typeof setTimeout> | null = null;

  // ── Initial sync retry ─────────────────────────────────────────
  #retryTimer: ReturnType<typeof setTimeout> | null = null;

  // ── Event subscribers ──────────────────────────────────────────
  #listeners: Map<SyncEngineEventType, Set<EventCallback>> = new Map();

  constructor(
    getHandle: HandleGetter | SyncableHandle,
    transport: NotebookTransport,
    options: SyncEngineOptions = {},
  ) {
    // Accept either a getter function or a direct handle (for tests).
    this.#getHandle =
      typeof getHandle === "function" && !("receive_frame" in getHandle)
        ? (getHandle as HandleGetter)
        : () => getHandle as SyncableHandle;
    this.#transport = transport;
    this.#options = {
      flushDebounceMs: options.flushDebounceMs ?? DEFAULT_FLUSH_DEBOUNCE_MS,
      initialSyncTimeoutMs:
        options.initialSyncTimeoutMs ?? DEFAULT_INITIAL_SYNC_TIMEOUT_MS,
    };
  }

  // ── Lifecycle ──────────────────────────────────────────────────

  /** Start processing inbound frames and managing sync. */
  start(): void {
    if (this.#running) return;
    this.#running = true;
    this.#awaitingInitialSync = true;

    // Subscribe to inbound frames from the transport.
    this.#unsubscribeFrame = this.#transport.onFrame((frame) => {
      this.#processFrame(frame);
    });

    // Arm the initial sync retry timer.
    this.#armRetryTimer();

    // Send the first sync message (empty doc requests full state from daemon).
    this.#flushNow();
  }

  /** Stop processing frames and release resources. */
  stop(): void {
    if (!this.#running) return;
    this.#running = false;

    // Unsubscribe from transport.
    this.#unsubscribeFrame?.();
    this.#unsubscribeFrame = null;

    // Cancel timers.
    if (this.#flushTimer !== null) {
      clearTimeout(this.#flushTimer);
      this.#flushTimer = null;
    }
    if (this.#retryTimer !== null) {
      clearTimeout(this.#retryTimer);
      this.#retryTimer = null;
    }

    // Final flush — best effort.
    this.#flushNow();

    // Clear listeners.
    this.#listeners.clear();
  }

  /** Whether the engine has completed the initial sync handshake. */
  get synced(): boolean {
    return !this.#awaitingInitialSync;
  }

  /** Whether the engine is actively processing frames. */
  get running(): boolean {
    return this.#running;
  }

  // ── Local mutation sync ────────────────────────────────────────

  /**
   * Schedule a debounced flush of local CRDT mutations to the daemon.
   *
   * Call this after any local mutation on the handle (update_source,
   * add_cell, set_metadata, etc.). Multiple calls within the debounce
   * window are coalesced into a single sync message.
   */
  scheduleFlush(): void {
    if (!this.#running) return;
    if (this.#flushTimer !== null) clearTimeout(this.#flushTimer);
    this.#flushTimer = setTimeout(() => {
      this.#flushTimer = null;
      this.#flushNow();
    }, this.#options.flushDebounceMs);
  }

  /**
   * Immediately flush local changes (bypasses debounce).
   *
   * Use before operations that depend on the daemon having the latest
   * state, e.g., before `sendRequest({ action: "execute_cell", ... })`.
   *
   * Returns a promise that resolves when the flush is sent (or rejects
   * if the transport fails — sync state is rolled back in that case).
   */
  async flush(): Promise<void> {
    if (this.#flushTimer !== null) {
      clearTimeout(this.#flushTimer);
      this.#flushTimer = null;
    }
    await this.#flushAsync();
  }

  // ── Event API ──────────────────────────────────────────────────

  /**
   * Subscribe to a specific event type.
   * Returns an unsubscribe function.
   */
  on<T extends SyncEngineEventType>(
    type: T,
    callback: TypedEventCallback<T>,
  ): () => void {
    let set = this.#listeners.get(type);
    if (!set) {
      set = new Set();
      this.#listeners.set(type, set);
    }
    const cb = callback as EventCallback;
    set.add(cb);
    return () => {
      set!.delete(cb);
    };
  }

  // ── Internal: frame processing ─────────────────────────────────

  #processFrame(frame: Uint8Array): void {
    if (!this.#running) return;

    const handle = this.#getHandle();
    if (!handle) return;

    let events: FrameEvent[] | undefined;
    try {
      events = handle.receive_frame(frame);
    } catch (err) {
      this.#emit({ type: "error", error: err, context: "receive_frame" });
      return;
    }

    if (!events || !Array.isArray(events)) return;

    for (const event of events) {
      switch (event.type) {
        case "sync_applied":
          this.#handleSyncApplied(event);
          break;
        case "broadcast":
          this.#emit({ type: "broadcast", payload: event.payload });
          break;
        case "presence":
          this.#emit({ type: "presence", payload: event.payload });
          break;
        case "runtime_state_sync_applied":
          this.#handleRuntimeStateSync(event);
          break;
        case "unknown":
          // Ignore unknown frame types silently.
          break;
      }
    }
  }

  #handleSyncApplied(event: SyncAppliedEvent): void {
    // ── Send inline sync reply immediately ───────────────────────
    // Generated atomically inside WASM's receive_frame (#1067/#1068 fix).
    // On failure, roll back sync state to prevent sent_hashes stranding.
    if (event.reply) {
      this.#transport
        .sendFrame(FrameType.AUTOMERGE_SYNC, new Uint8Array(event.reply))
        .catch((err) => {
          this.#getHandle()?.cancel_last_flush();
          this.#emit({
            type: "error",
            error: err,
            context: "inline_sync_reply_send",
          });
        });
    }

    // ── Initial sync handshake ───────────────────────────────────
    if (this.#awaitingInitialSync) {
      if (event.changed) {
        // First real content from the daemon — initial sync complete.
        this.#awaitingInitialSync = false;
        this.#clearRetryTimer();
        this.#emit({ type: "initial_sync_complete" });

        // Also emit the cells_changed so consumers can materialize.
        this.#emit({
          type: "cells_changed",
          changeset: event.changeset ?? null,
          attributions: event.attributions ?? [],
        });
      } else {
        // Handshake round (bloom filter exchange, no content yet).
        // Restart the retry timer.
        this.#armRetryTimer();
      }
      return;
    }

    // ── Steady-state: emit cells_changed if doc actually changed ──
    if (event.changed) {
      this.#emit({
        type: "cells_changed",
        changeset: event.changeset ?? null,
        attributions: event.attributions ?? [],
      });
    }
  }

  #handleRuntimeStateSync(event: RuntimeStateSyncAppliedEvent): void {
    // Emit state change if the doc changed.
    if (event.changed && event.state) {
      this.#emit({ type: "runtime_state_changed", state: event.state });
    }

    // Send runtime state sync reply so the daemon knows our heads.
    const handle = this.#getHandle();
    if (!handle) return;
    try {
      const reply = handle.generate_runtime_state_sync_reply();
      if (reply) {
        this.#transport
          .sendFrame(FrameType.RUNTIME_STATE_SYNC, reply)
          .catch((err) => {
            this.#emit({
              type: "error",
              error: err,
              context: "runtime_state_sync_reply_send",
            });
          });
      }
    } catch (err) {
      this.#emit({
        type: "error",
        error: err,
        context: "generate_runtime_state_sync_reply",
      });
    }
  }

  // ── Internal: flush ────────────────────────────────────────────

  /** Synchronous flush — fire-and-forget, errors emitted as events. */
  #flushNow(): void {
    const handle = this.#getHandle();
    if (!handle) return;
    try {
      const msg = handle.flush_local_changes();
      if (msg) {
        this.#transport
          .sendFrame(FrameType.AUTOMERGE_SYNC, msg)
          .catch((err) => {
            this.#getHandle()?.cancel_last_flush();
            this.#emit({ type: "error", error: err, context: "flush_send" });
          });
      }
    } catch (err) {
      this.#emit({ type: "error", error: err, context: "flush_local_changes" });
    }
  }

  /** Async flush — awaits the send so callers can sequence after it. */
  async #flushAsync(): Promise<void> {
    const handle = this.#getHandle();
    if (!handle) return;
    const msg = handle.flush_local_changes();
    if (!msg) return;
    try {
      await this.#transport.sendFrame(FrameType.AUTOMERGE_SYNC, msg);
    } catch (err) {
      this.#getHandle()?.cancel_last_flush();
      this.#emit({ type: "error", error: err, context: "flush_send" });
      throw err;
    }
  }

  // ── Internal: initial sync retry ───────────────────────────────

  #armRetryTimer(): void {
    this.#clearRetryTimer();
    this.#retryTimer = setTimeout(() => {
      this.#retryTimer = null;
      if (!this.#awaitingInitialSync || !this.#running) return;

      // Reset sync state and re-send the initial sync message.
      // This handles the case where the first message was lost or
      // consumed by a stale handle.
      this.#getHandle()?.reset_sync_state();
      this.#flushNow();
      this.#emit({ type: "sync_retry" });

      // Re-arm in case this retry also doesn't produce content.
      this.#armRetryTimer();
    }, this.#options.initialSyncTimeoutMs);
  }

  #clearRetryTimer(): void {
    if (this.#retryTimer !== null) {
      clearTimeout(this.#retryTimer);
      this.#retryTimer = null;
    }
  }

  // ── Internal: event emission ───────────────────────────────────

  #emit(event: SyncEngineEvent): void {
    const set = this.#listeners.get(event.type);
    if (set) {
      for (const cb of set) {
        try {
          cb(event);
        } catch (err) {
          // Don't let a subscriber error kill the engine.
          console.error(
            `[SyncEngine] subscriber error for '${event.type}':`,
            err,
          );
        }
      }
    }
  }
}
