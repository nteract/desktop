/**
 * Widget Update Manager — CRDT-first widget state persistence.
 *
 * Historical note (was #1580): this used to be a dual-write path —
 * synchronous optimistic `store.updateModel` for instant UI feedback,
 * plus debounced CRDT writes with echo-suppression bookkeeping to
 * prevent in-flight drags from being clobbered by stale daemon echoes.
 * That reconciliation grew the widget-sync stall described in
 * `docs/superpowers/specs/2026-04-17-widget-sync-stall-design.md`.
 *
 * Track A collapsed it into a single source of truth: every write goes
 * to the CRDT, and the `WidgetStore` updates purely from
 * `commChanges$`. Local writes arrive via the same path because
 * `SyncEngine.projectLocalState` fires the projection synchronously
 * after `set_comm_state_batch`.
 *
 * The outbound write rate is throttled per comm so a continuous slider
 * drag doesn't produce ~60 CRDT writes/sec. First tick fires
 * immediately (instant UI feedback via the projected emission);
 * subsequent ticks within the throttle window accumulate into a single
 * trailing write.
 */

import type { WidgetStore } from "./widget-store";

type CrdtCommWriter = (commId: string, patch: Record<string, unknown>) => void;

export interface WidgetUpdateManagerOptions {
  getStore: () => WidgetStore | null;
  getCrdtWriter: () => CrdtCommWriter | null;
}

/** Throttle window for outbound CRDT writes per comm (ms). */
const THROTTLE_MS = 50;

/** Poll interval for draining the pending queue while no writer is set. */
const DRAIN_POLL_MS = 50;

interface CommThrottleState {
  /** Latest accumulated patch waiting to be flushed, or null. */
  pending: Record<string, unknown> | null;
  /** Wall-clock ms of the last successful flush for this comm. */
  lastFlushAt: number;
  /** Trailing-edge flush timer handle, or null. */
  trailingTimer: ReturnType<typeof setTimeout> | null;
}

export class WidgetUpdateManager {
  private readonly getStore: () => WidgetStore | null;
  private readonly getCrdtWriter: () => CrdtCommWriter | null;

  /**
   * Per-comm throttle bookkeeping. Leading tick fires immediately,
   * subsequent ticks within `THROTTLE_MS` accumulate into `pending`
   * and fire together on the trailing-edge timer.
   */
  private throttles = new Map<string, CommThrottleState>();

  /**
   * Patches that arrived before the CRDT writer was registered.
   * Separate from `throttles` because the writer isn't callable yet.
   */
  private bootstrapQueue = new Map<string, Record<string, unknown>>();
  private drainTimer: ReturnType<typeof setTimeout> | null = null;

  constructor(opts: WidgetUpdateManagerOptions) {
    this.getStore = opts.getStore;
    this.getCrdtWriter = opts.getCrdtWriter;
  }

  /**
   * Persist a widget state update.
   *
   * Throttled per comm so bursts (slider drags, text input) coalesce
   * to one write per `THROTTLE_MS`. The first write in a burst fires
   * immediately — the user sees instant feedback via the
   * `projectLocalState` → `commChanges$` → WidgetStore path. Ticks
   * within the window accumulate last-wins into the trailing flush.
   *
   * Binary buffers bypass throttling and are mirrored directly to
   * the local widget model (CRDT doesn't carry ArrayBuffers;
   * kernel delivery of buffers goes through the SendComm RPC path
   * elsewhere).
   */
  updateAndPersist(commId: string, patch: Record<string, unknown>, buffers?: ArrayBuffer[]): void {
    const writer = this.getCrdtWriter();
    if (!writer) {
      this.queueForBootstrap(commId, patch, buffers);
      return;
    }

    // Drain any bootstrap-queue leftovers before processing new
    // writes. Ensures pre-writer patches reach the CRDT in insertion
    // order, even if they landed minutes ago.
    this.drainBootstrap(writer);

    if (buffers?.length) {
      // Buffers bypass the throttle: ArrayBuffers aren't patchable
      // through Automerge, and delaying them would corrupt
      // anywidget model.buffers ordering.
      writer(commId, patch);
      this.getStore()?.updateModel(commId, {}, buffers);
      return;
    }

    this.scheduleThrottled(commId, patch, writer);
  }

  private scheduleThrottled(
    commId: string,
    patch: Record<string, unknown>,
    writer: CrdtCommWriter,
  ): void {
    const now = Date.now();
    let state = this.throttles.get(commId);
    if (!state) {
      state = { pending: null, lastFlushAt: 0, trailingTimer: null };
      this.throttles.set(commId, state);
    }

    const sinceLast = now - state.lastFlushAt;
    if (sinceLast >= THROTTLE_MS && state.trailingTimer === null) {
      // Leading tick — flush immediately.
      writer(commId, patch);
      state.lastFlushAt = now;
      return;
    }

    // Burst in progress — accumulate and schedule the trailing fire.
    state.pending = state.pending ? { ...state.pending, ...patch } : { ...patch };
    if (state.trailingTimer === null) {
      const wait = Math.max(0, THROTTLE_MS - sinceLast);
      state.trailingTimer = setTimeout(() => this.fireTrailing(commId), wait);
    }
  }

  private fireTrailing(commId: string): void {
    const state = this.throttles.get(commId);
    if (!state) return;
    state.trailingTimer = null;
    const patch = state.pending;
    state.pending = null;
    if (!patch) return;
    const writer = this.getCrdtWriter();
    if (!writer) {
      // Writer vanished mid-throttle (reset, unmount). Redirect to
      // the bootstrap queue so the change isn't lost if a new
      // writer shows up later.
      this.queueForBootstrap(commId, patch);
      return;
    }
    writer(commId, patch);
    state.lastFlushAt = Date.now();
  }

  private queueForBootstrap(
    commId: string,
    patch: Record<string, unknown>,
    buffers?: ArrayBuffer[],
  ): void {
    const existing = this.bootstrapQueue.get(commId) ?? {};
    this.bootstrapQueue.set(commId, { ...existing, ...patch });
    // Mirror the patch to the local store so the UI isn't stuck on
    // the pre-interaction value during bootstrap.
    this.getStore()?.updateModel(commId, patch, buffers);
    this.scheduleDrain();
  }

  private drainBootstrap(writer: CrdtCommWriter): void {
    if (this.bootstrapQueue.size === 0) return;
    for (const [commId, patch] of this.bootstrapQueue) {
      writer(commId, patch);
    }
    this.bootstrapQueue.clear();
    if (this.drainTimer) {
      clearTimeout(this.drainTimer);
      this.drainTimer = null;
    }
  }

  private scheduleDrain(): void {
    if (this.drainTimer) return;
    this.drainTimer = setTimeout(() => {
      this.drainTimer = null;
      const writer = this.getCrdtWriter();
      if (writer) {
        this.drainBootstrap(writer);
      } else if (this.bootstrapQueue.size > 0) {
        this.scheduleDrain();
      }
    }, DRAIN_POLL_MS);
  }

  /**
   * Reset all bookkeeping — called on kernel restart. Drops pending
   * throttle state and the bootstrap queue: patches the old kernel
   * would have received shouldn't reach a freshly-launched one.
   */
  reset(): void {
    for (const state of this.throttles.values()) {
      if (state.trailingTimer) clearTimeout(state.trailingTimer);
    }
    this.throttles.clear();
    this.bootstrapQueue.clear();
    if (this.drainTimer) {
      clearTimeout(this.drainTimer);
      this.drainTimer = null;
    }
  }

  /** Tear down. */
  dispose(): void {
    this.reset();
  }

  /** Drop per-comm state on comm_close. */
  clearComm(commId: string): void {
    const state = this.throttles.get(commId);
    if (state?.trailingTimer) clearTimeout(state.trailingTimer);
    this.throttles.delete(commId);
    this.bootstrapQueue.delete(commId);
  }
}
