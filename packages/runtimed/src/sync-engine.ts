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
import {
  diffExecutions,
  type ExecutionState,
  type ExecutionTransition,
  type RuntimeState,
} from "./runtime-state.ts";
import {
  Subject,
  Observable,
  bufferTime,
  filter,
  map,
  share,
  type Subscription,
} from "rxjs";

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

  /** Generate an initial RuntimeStateDoc sync message.
   *  Call during bootstrap so the daemon knows to push queue/kernel state. */
  flush_runtime_state_sync(): Uint8Array | undefined;

  /** Roll back runtime state sync state after a failed send. */
  cancel_last_runtime_state_flush(): void;

  /** Reset all sync state (reconnection / page reload equivalent). */
  reset_sync_state(): void;

  /** Number of cells in the document. Used to detect initial sync completion
   *  when the daemon sent content before the transport was listening. */
  cell_count(): number;
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
  | { type: "runtime_state_changed"; state: RuntimeState }
  | {
      type: "execution_transition";
      transition: ExecutionTransition;
    }
  | { type: "sync_retry" }
  | { type: "error"; error: unknown; context: string };

/**
 * A coalesced batch of cell changesets, emitted after a debounce window.
 *
 * Multiple rapid `cells_changed` events (e.g., during output streaming)
 * are merged into a single batch. If any changeset in the batch is null
 * (meaning a full materialization is needed), `needsFull` is true.
 */
// ── Changeset merging ───────────────────────────────────────────────

/**
 * Merge multiple CellChangesets into one.
 *
 * Used by the coalescing pipeline to combine rapid `cells_changed` events
 * into a single materialization batch.
 *
 * - `changed`: union of all changed cells (fields OR'd together)
 * - `added`: union of all added cell IDs
 * - `removed`: union of all removed cell IDs
 * - `order_changed`: true if any changeset had order_changed
 */
export function mergeChangesets(changesets: CellChangeset[]): CellChangeset {
  if (changesets.length === 0) {
    return { changed: [], added: [], removed: [], order_changed: false };
  }
  if (changesets.length === 1) {
    return changesets[0];
  }

  const changedMap = new Map<string, ChangedFields>();
  const addedSet = new Set<string>();
  const removedSet = new Set<string>();
  let orderChanged = false;

  for (const cs of changesets) {
    orderChanged = orderChanged || cs.order_changed;

    for (const id of cs.added) addedSet.add(id);
    for (const id of cs.removed) removedSet.add(id);

    for (const cell of cs.changed) {
      const existing = changedMap.get(cell.cell_id);
      if (existing) {
        // OR the field flags together
        changedMap.set(cell.cell_id, {
          source: existing.source || cell.fields.source,
          outputs: existing.outputs || cell.fields.outputs,
          cell_type: existing.cell_type || cell.fields.cell_type,
          execution_count:
            existing.execution_count || cell.fields.execution_count,
          metadata: existing.metadata || cell.fields.metadata,
          position: existing.position || cell.fields.position,
        });
      } else {
        changedMap.set(cell.cell_id, { ...cell.fields });
      }
    }
  }

  const changed: ChangedCell[] = Array.from(changedMap.entries()).map(
    ([cell_id, fields]) => ({ cell_id, fields }),
  );

  return {
    changed,
    added: Array.from(addedSet),
    removed: Array.from(removedSet),
    order_changed: orderChanged,
  };
}

export interface CoalescedCellChanges {
  /** Merged changeset, or null if a full materialization is needed. */
  changeset: CellChangeset | null;
  /** True if any individual changeset was null (requires full materialization). */
  needsFull: boolean;
  /** All text attributions from the batch. */
  attributions: TextAttribution[];
  /** Number of individual cells_changed events in this batch. */
  batchSize: number;
}

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

  /**
   * Coalescing window (ms) for batching cell change events before
   * materialization. Multiple rapid `cells_changed` events within
   * this window are merged into a single {@link CoalescedCellChanges}.
   * Default: 32ms (one frame at 30fps).
   */
  coalesceMs?: number;
}

const DEFAULT_FLUSH_DEBOUNCE_MS = 20;
const DEFAULT_INITIAL_SYNC_TIMEOUT_MS = 3000;
const DEFAULT_COALESCE_MS = 32;

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

  // ── Runtime state tracking (for execution diffing) ────────────
  #prevExecutions: Record<string, ExecutionState> = {};
  #isInitialRuntimeState = true;

  // ── Event subscribers ──────────────────────────────────────────
  #listeners: Map<SyncEngineEventType, Set<EventCallback>> = new Map();

  // ── RxJS streams ───────────────────────────────────────────────
  readonly #events$ = new Subject<SyncEngineEvent>();
  #rxSub: Subscription | null = null;

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
      coalesceMs: options.coalesceMs ?? DEFAULT_COALESCE_MS,
    };
  }

  // ── RxJS Observables ───────────────────────────────────────────

  /**
   * All engine events as an RxJS Observable.
   *
   * Hot observable — multicasted via `share()`. Emits from the moment
   * `start()` is called until `stop()`. Subscribers receive events as
   * they happen (no replay).
   */
  readonly events$: Observable<SyncEngineEvent> = this.#events$.pipe(share());

  /**
   * Coalesced cell change stream.
   *
   * Batches rapid `cells_changed` events over a configurable window
   * (default 32ms) and merges their changesets. Consumers subscribe to
   * this instead of listening for individual `cells_changed` events to
   * avoid over-materializing during output streaming.
   *
   * If any changeset in the batch is `null`, the merged result has
   * `needsFull: true` — the consumer should do a full materialization
   * instead of incremental.
   *
   * Note: uses a getter to defer pipeline creation until after the
   * constructor sets `#options` (field initializers run before the
   * constructor body).
   */
  #cellChanges$: Observable<CoalescedCellChanges> | null = null;
  get cellChanges$(): Observable<CoalescedCellChanges> {
    if (!this.#cellChanges$) {
      this.#cellChanges$ = this.#events$.pipe(
        filter(
          (e): e is Extract<SyncEngineEvent, { type: "cells_changed" }> =>
            e.type === "cells_changed",
        ),
        bufferTime(this.#options.coalesceMs),
        filter((batch) => batch.length > 0),
        map((batch): CoalescedCellChanges => {
          const needsFull = batch.some((e) => e.changeset === null);
          const attributions = batch.flatMap((e) => e.attributions);

          if (needsFull) {
            return {
              changeset: null,
              needsFull: true,
              attributions,
              batchSize: batch.length,
            };
          }

          const merged = mergeChangesets(batch.map((e) => e.changeset!));
          return {
            changeset: merged,
            needsFull: false,
            attributions,
            batchSize: batch.length,
          };
        }),
        share(),
      );
    }
    return this.#cellChanges$;
  }

  /**
   * Broadcast events as an Observable.
   */
  readonly broadcasts$: Observable<unknown> = this.#events$.pipe(
    filter(
      (e): e is Extract<SyncEngineEvent, { type: "broadcast" }> =>
        e.type === "broadcast",
    ),
    map((e) => e.payload),
    share(),
  );

  /**
   * Presence events as an Observable.
   */
  readonly presence$: Observable<unknown> = this.#events$.pipe(
    filter(
      (e): e is Extract<SyncEngineEvent, { type: "presence" }> =>
        e.type === "presence",
    ),
    map((e) => e.payload),
    share(),
  );

  /**
   * Runtime state changes as an Observable.
   */
  readonly runtimeState$: Observable<RuntimeState> = this.#events$.pipe(
    filter(
      (e): e is Extract<SyncEngineEvent, { type: "runtime_state_changed" }> =>
        e.type === "runtime_state_changed",
    ),
    map((e) => e.state),
    share(),
  );

  /**
   * Execution lifecycle transitions as an Observable.
   *
   * Emits when an execution changes status (queued→running = "started",
   * running→done = "done", running→error = "error"). Consumers can use
   * this to update execution counts, show progress indicators, etc.
   * without manually diffing the executions map.
   */
  readonly executionTransitions$: Observable<ExecutionTransition> =
    this.#events$.pipe(
      filter(
        (
          e,
        ): e is Extract<SyncEngineEvent, { type: "execution_transition" }> =>
          e.type === "execution_transition",
      ),
      map((e) => e.transition),
      share(),
    );

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

    // Also initiate RuntimeStateDoc sync so the daemon sends kernel status,
    // execution queue, trust state, etc. Without this, the frontend never
    // receives queue_changed updates and run-all appears stuck (#runtime-state-race).
    this.#flushRuntimeStateNow();
  }

  /** Stop processing frames and release resources. */
  stop(): void {
    if (!this.#running) return;
    this.#running = false;

    // Unsubscribe from transport.
    this.#unsubscribeFrame?.();
    this.#unsubscribeFrame = null;

    // Tear down RxJS subscriptions.
    this.#rxSub?.unsubscribe();
    this.#rxSub = null;

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

    // Complete the subject (terminates all Observable subscribers).
    this.#events$.complete();

    // Clear callback listeners.
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
    if (!handle) {
      console.warn(
        "[SyncEngine] processFrame: handle getter returned null, skipping frame",
      );
      return;
    }

    let events: FrameEvent[] | undefined;
    try {
      events = handle.receive_frame(frame);
    } catch (err) {
      this.#emit({ type: "error", error: err, context: "receive_frame" });
      return;
    }

    if (!events || !Array.isArray(events)) {
      console.warn(
        "[SyncEngine] processFrame: receive_frame returned non-array:",
        typeof events,
        "frame type byte:",
        frame[0],
        "frame length:",
        frame.length,
      );
      return;
    }

    // Diagnostic: log event types during initial sync
    if (this.#awaitingInitialSync) {
      for (const ev of events) {
        if (ev.type === "sync_applied") {
          const sa = ev as SyncAppliedEvent;
          console.log(
            "[SyncEngine] initial sync: sync_applied changed=%s changeset=%s reply=%s",
            sa.changed,
            sa.changeset ? "yes" : "no",
            sa.reply ? `${sa.reply.length}B` : "none",
          );
        } else {
          console.log("[SyncEngine] initial sync: event type=%s", ev.type);
        }
      }
    }

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
      // Detect initial sync completion. Three signals (any one suffices):
      //
      // 1. `changed=true` — the doc gained content from this sync frame.
      //    Normal path for notebooks that have cells.
      //
      // 2. `cell_count() > 0` — the doc has cells even though `changed`
      //    is false. Happens when the daemon sent content before the
      //    transport was listening (relay emits frames immediately, but
      //    our TauriTransport connects later). The daemon's sync state
      //    has advanced, so subsequent frames report changed=false.
      //
      // 3. `reply` exists — we generated a sync reply, meaning we
      //    successfully received a daemon sync message and responded.
      //    This covers empty notebooks (0 cells, changed=true on first
      //    mount but changed=false on React strict mode's second mount
      //    because the daemon's relay state already advanced). A reply
      //    means the handshake completed — we're in sync, even if
      //    the notebook is genuinely empty.
      const handle = this.#getHandle();
      const hasContent = handle ? handle.cell_count() > 0 : false;
      const hasReply = event.reply != null && event.reply.length > 0;

      if (event.changed || hasContent || hasReply) {
        // Initial sync complete — we've exchanged with the daemon.
        this.#awaitingInitialSync = false;
        this.#clearRetryTimer();
        this.#emit({ type: "initial_sync_complete" });

        // Emit cells_changed so consumers can materialize.
        this.#emit({
          type: "cells_changed",
          changeset: event.changeset ?? null,
          attributions: event.attributions ?? [],
        });
      } else {
        // Handshake round with no reply generated (e.g., the very first
        // sync message we sent, before the daemon responds). Restart the
        // retry timer.
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
      const state = event.state as RuntimeState;
      this.#emit({ type: "runtime_state_changed", state });

      // Diff executions to detect lifecycle transitions.
      // Skip the first state (slow joiner catch-up) to avoid
      // false "started" events for already-running executions.
      const currExecutions = state.executions ?? {};
      if (this.#isInitialRuntimeState) {
        this.#isInitialRuntimeState = false;
      } else {
        const transitions = diffExecutions(
          this.#prevExecutions,
          currExecutions,
        );
        for (const transition of transitions) {
          this.#emit({ type: "execution_transition", transition });
        }
      }
      this.#prevExecutions = currExecutions;
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

  /** Send the initial RuntimeStateDoc sync message to the daemon.
   *  Called once during start() — after that, replies are generated
   *  inline by #handleRuntimeStateSync on each inbound frame. */
  #flushRuntimeStateNow(): void {
    const handle = this.#getHandle();
    if (!handle) return;
    try {
      const msg = handle.flush_runtime_state_sync();
      if (msg) {
        this.#transport
          .sendFrame(FrameType.RUNTIME_STATE_SYNC, new Uint8Array(msg))
          .catch((err) => {
            this.#getHandle()?.cancel_last_runtime_state_flush();
            this.#emit({
              type: "error",
              error: err,
              context: "runtime_state_flush_send",
            });
          });
      }
    } catch (err) {
      this.#emit({
        type: "error",
        error: err,
        context: "flush_runtime_state_sync",
      });
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
    // Push to RxJS Subject (feeds all Observable streams).
    this.#events$.next(event);

    // Push to callback listeners.
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
