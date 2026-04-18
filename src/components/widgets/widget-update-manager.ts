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

/**
 * Narrow TTL window for keeping pending-local marks alive after
 * a CRDT flush. The daemon's echo of our last-written value can
 * take a full sync round-trip to arrive; during that gap we still
 * want the local store to win over the projected echo.
 */
const PENDING_TTL_MS = 500;

interface PendingValue {
  /** Cached JSON serialization of the value we wrote. */
  json: string;
  /** Wall-clock ms when the write happened. */
  ts: number;
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
   * Per-comm per-key history of values we've written locally within
   * the TTL window. Consulted by the App-level `commChanges$`
   * subscriber via `isEchoOfPendingWrite`: a projected value that
   * matches *any* recent local write is dropped as a stale echo of
   * our own in-flight writes. A projected value that doesn't
   * appear in the history is authoritative (kernel validator clamp,
   * peer edit) and still applied.
   *
   * We must remember the whole recent history, not just the latest
   * value: `projectLocalState()` runs synchronously after the writer
   * call, but the `commChanges$` emission it produces lands on a
   * microtask. A rapid burst can walk the pending value from 10 →
   * 11 → 12 before the leading-edge projection of 10 is handled; a
   * latest-only check would misclassify that queued echo as
   * authoritative and snap the UI backward.
   */
  private pendingKeys = new Map<string, Map<string, PendingValue[]>>();

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
   * Consume-and-match: is the projected `candidate` value for
   * `(commId, key)` a stale echo of any local write we made within
   * the TTL? Returns true for any value that appears in our recent
   * write history — and removes the matched entry, so the same
   * value can only absorb one echo.
   *
   * The consume step matters for collaborative correctness: if
   * another peer happens to write the same value we recently wrote,
   * only the first emission carrying that value is treated as our
   * echo. The peer's subsequent emission (or a re-write of the same
   * value) falls through and lands in the store.
   *
   * Candidates that never appeared in the history — kernel
   * validator clamp, peer edit with a different value — are
   * applied unchanged.
   */
  isEchoOfPendingWrite(commId: string, key: string, candidate: unknown): boolean {
    const keys = this.pendingKeys.get(commId);
    if (!keys) return false;
    const history = keys.get(key);
    if (!history || history.length === 0) return false;

    const cutoff = Date.now() - PENDING_TTL_MS;
    let firstLive = 0;
    while (firstLive < history.length && history[firstLive].ts < cutoff) firstLive++;
    if (firstLive > 0) history.splice(0, firstLive);
    if (history.length === 0) {
      keys.delete(key);
      if (keys.size === 0) this.pendingKeys.delete(commId);
      return false;
    }

    let candidateJson: string;
    try {
      candidateJson = JSON.stringify(candidate);
    } catch {
      // Non-serializable candidate → can't confirm it's our echo; err
      // on the side of applying the update.
      return false;
    }
    for (let i = 0; i < history.length; i++) {
      if (history[i].json === candidateJson) {
        history.splice(i, 1);
        if (history.length === 0) {
          keys.delete(key);
          if (keys.size === 0) this.pendingKeys.delete(commId);
        }
        return true;
      }
    }
    return false;
  }

  private markPending(commId: string, patch: Record<string, unknown>): void {
    let keys = this.pendingKeys.get(commId);
    if (!keys) {
      keys = new Map();
      this.pendingKeys.set(commId, keys);
    }
    const now = Date.now();
    for (const [key, value] of Object.entries(patch)) {
      let json: string;
      try {
        json = JSON.stringify(value);
      } catch {
        // Skip tracking — we can't compare echoes reliably without
        // JSON. The worst outcome is an occasional redundant store
        // write, which is benign.
        continue;
      }
      let history = keys.get(key);
      if (!history) {
        history = [];
        keys.set(key, history);
      }
      // Dedup: if we just wrote the same value, bump the timestamp
      // rather than growing the list. Keeps a quick AB-AB toggle
      // from ballooning the history.
      const last = history[history.length - 1];
      if (last && last.json === json) {
        last.ts = now;
      } else {
        history.push({ json, ts: now });
      }
    }
  }

  /**
   * Persist a widget state update.
   *
   * Every tick immediately mirrors `patch` into the local
   * `WidgetStore` so UI components (slider thumbs, text inputs) move
   * in lockstep with user input. The outbound CRDT write is
   * throttled per comm so a continuous drag doesn't flood the daemon
   * with ~60 writes/sec. The first write in a burst fires at the
   * leading edge; ticks within the window accumulate last-wins into
   * a trailing flush at `THROTTLE_MS`.
   *
   * The local-then-CRDT ordering is safe because `projectLocalState`
   * re-emits the resolved state on `commChanges$`, and the App-level
   * subscriber diffs that against the current store — so the local
   * pre-write makes the projected echo a no-op rather than a
   * duplicate update. Kernel echoes converge through Automerge merge
   * rather than racing the local store.
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

    // Mirror every tick to the local store so components see
    // continuous motion during a drag even when CRDT writes are
    // throttled. The projection from `writer → projectLocalState →
    // commChanges$` later fans the same values back out for other
    // views; the App-level subscriber's diff check makes that a
    // no-op rather than a redundant re-render.
    this.getStore()?.updateModel(commId, patch, buffers);

    // Mark these keys as pending-local. The `commChanges$`
    // subscriber consults `hasPendingKey` before applying projected
    // values, so a daemon sync frame that carries the pre-flush
    // CRDT view won't roll the local store back mid-drag.
    this.markPending(commId, patch);

    if (buffers?.length) {
      // Buffers bypass the throttle: ArrayBuffers aren't patchable
      // through Automerge, and delaying them would corrupt
      // anywidget model.buffers ordering.
      writer(commId, patch);
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
      // Mark the bootstrap value as a pending local write BEFORE
      // firing the writer. `projectLocalState` will schedule a
      // microtask emission for this value — without the mark, that
      // emission would be treated as authoritative and could roll
      // the store back from a newer user value written after
      // bootstrap.
      this.markPending(commId, patch);
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
    this.pendingKeys.clear();
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
    this.pendingKeys.delete(commId);
  }
}
