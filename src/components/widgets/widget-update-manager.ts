/**
 * Widget Update Manager — debounced CRDT persistence with echo suppression.
 *
 * Separates local store updates (instant, for UI responsiveness) from CRDT
 * persistence (debounced, for daemon/kernel sync). During the debounce window,
 * incoming CRDT echoes for optimistic keys are suppressed to prevent stale
 * values from clobbering the user's in-progress interaction.
 *
 * This solves three problems:
 * 1. jslink feedback loops (CRDT echo triggers re-propagation)
 * 2. Slider CRDT flooding (~60 writes/sec during drag)
 * 3. Stale CRDT echoes overwriting optimistic state
 */

import type { WidgetStore } from "./widget-store";

type CrdtCommWriter = (commId: string, patch: Record<string, unknown>) => void;

/** Debounce interval for CRDT writes (ms). */
const DEBOUNCE_MS = 50;

export interface WidgetUpdateManagerOptions {
  getStore: () => WidgetStore | null;
  getCrdtWriter: () => CrdtCommWriter | null;
}

export class WidgetUpdateManager {
  private readonly getStore: () => WidgetStore | null;
  private readonly getCrdtWriter: () => CrdtCommWriter | null;

  /** Accumulated patches waiting for debounced flush, per comm. */
  private pendingState = new Map<string, Record<string, unknown>>();
  /** Keys with local-only values not yet flushed to CRDT. */
  private optimisticKeys = new Map<string, Set<string>>();
  /** Per-comm debounce timers. */
  private flushTimers = new Map<string, ReturnType<typeof setTimeout>>();

  constructor(opts: WidgetUpdateManagerOptions) {
    this.getStore = opts.getStore;
    this.getCrdtWriter = opts.getCrdtWriter;
  }

  /**
   * Update store immediately + schedule debounced CRDT write.
   *
   * Called by sendUpdate for all widget state changes (sliders, dropdowns,
   * text input, etc.). The store update fires subscriptions instantly for
   * responsive UI. The CRDT write is batched per-comm at 50ms.
   *
   * Binary buffers bypass debouncing and flush immediately.
   */
  updateAndPersist(commId: string, patch: Record<string, unknown>, buffers?: ArrayBuffer[]): void {
    // 1. Instant store update — UI reflects change immediately
    this.getStore()?.updateModel(commId, patch, buffers);

    // 2. Track optimistic keys
    let keys = this.optimisticKeys.get(commId);
    if (!keys) {
      keys = new Set();
      this.optimisticKeys.set(commId, keys);
    }
    for (const key of Object.keys(patch)) {
      keys.add(key);
    }

    // 3. Accumulate patch
    const existing = this.pendingState.get(commId);
    this.pendingState.set(commId, existing ? { ...existing, ...patch } : { ...patch });

    // 4. Binary buffers — flush immediately (can't merge ArrayBuffers)
    if (buffers?.length) {
      this.flushComm(commId);
      return;
    }

    // 5. Debounced flush — reset timer on each update
    const existing_timer = this.flushTimers.get(commId);
    if (existing_timer !== undefined) {
      clearTimeout(existing_timer);
    }
    this.flushTimers.set(
      commId,
      setTimeout(() => this.flushComm(commId), DEBOUNCE_MS),
    );
  }

  /**
   * Filter an incoming CRDT echo, suppressing keys that have pending
   * optimistic values.
   *
   * Returns the filtered patch to apply, or null if entirely suppressed.
   */
  shouldSuppressEcho(
    commId: string,
    incomingPatch: Record<string, unknown>,
  ): Record<string, unknown> | null {
    const keys = this.optimisticKeys.get(commId);
    if (!keys || keys.size === 0) return incomingPatch;

    const filtered: Record<string, unknown> = {};
    let hasKeys = false;
    for (const [key, value] of Object.entries(incomingPatch)) {
      if (!keys.has(key)) {
        filtered[key] = value;
        hasKeys = true;
      }
    }
    return hasKeys ? filtered : null;
  }

  /**
   * Reset all state. Call on kernel restart to ensure fresh echoes
   * from the new session aren't suppressed.
   */
  reset(): void {
    for (const timer of this.flushTimers.values()) {
      clearTimeout(timer);
    }
    this.flushTimers.clear();
    this.pendingState.clear();
    this.optimisticKeys.clear();
  }

  /** Tear down all timers. */
  dispose(): void {
    this.reset();
  }

  // ── Internal ──────────────────────────────────────────────────────

  /**
   * Cancel pending state and timers for a specific comm.
   * Call when a comm is closed to avoid flushing stale state.
   */
  clearComm(commId: string): void {
    const timer = this.flushTimers.get(commId);
    if (timer !== undefined) {
      clearTimeout(timer);
      this.flushTimers.delete(commId);
    }
    this.pendingState.delete(commId);
    this.optimisticKeys.delete(commId);
  }

  private flushComm(commId: string): void {
    // Clear timer
    const timer = this.flushTimers.get(commId);
    if (timer !== undefined) {
      clearTimeout(timer);
      this.flushTimers.delete(commId);
    }

    const patch = this.pendingState.get(commId);
    if (!patch) return;

    // If the CRDT writer isn't available yet (early startup), keep the
    // patch queued and retry after the next debounce interval.
    const writer = this.getCrdtWriter();
    if (!writer) {
      this.flushTimers.set(
        commId,
        setTimeout(() => this.flushComm(commId), DEBOUNCE_MS),
      );
      return;
    }

    this.pendingState.delete(commId);
    writer(commId, patch);

    // Clear optimistic keys after flush. Echoes arriving after this
    // point carry the value we just wrote (or a kernel-validated value)
    // and should pass through.
    this.optimisticKeys.delete(commId);
  }
}
