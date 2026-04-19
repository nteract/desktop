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

/**
 * Grace period after CRDT flush before clearing optimistic keys (ms).
 *
 * Covers the full round trip: CRDT write → sync flush (20ms) → daemon
 * receives sync → diffs → sends comm_msg to kernel → kernel processes
 * @interact callback → echoes on IOPub → 16ms coalesce → CRDT write →
 * sync back to frontend. With a slow callback this can take 200–500ms.
 */
const ECHO_GRACE_MS = 500;

/**
 * Structural equality for JSON-serializable widget values.
 *
 * Widget state travels through comm messages as JSON, so values are
 * limited to null, booleans, numbers, strings, arrays, and plain
 * objects. `Object.is` handles primitives (including NaN); arrays and
 * plain objects recurse.
 */
function structuralEqual(a: unknown, b: unknown): boolean {
  if (Object.is(a, b)) return true;
  if (a === null || b === null) return false;
  if (typeof a !== "object" || typeof b !== "object") return false;

  const aIsArray = Array.isArray(a);
  const bIsArray = Array.isArray(b);
  if (aIsArray !== bIsArray) return false;
  if (aIsArray && bIsArray) {
    if (a.length !== b.length) return false;
    for (let i = 0; i < a.length; i++) {
      if (!structuralEqual(a[i], b[i])) return false;
    }
    return true;
  }

  const ao = a as Record<string, unknown>;
  const bo = b as Record<string, unknown>;
  const aKeys = Object.keys(ao);
  if (aKeys.length !== Object.keys(bo).length) return false;
  for (const key of aKeys) {
    if (!Object.prototype.hasOwnProperty.call(bo, key)) return false;
    if (!structuralEqual(ao[key], bo[key])) return false;
  }
  return true;
}

export interface WidgetUpdateManagerOptions {
  getStore: () => WidgetStore | null;
  getCrdtWriter: () => CrdtCommWriter | null;
}

export class WidgetUpdateManager {
  private readonly getStore: () => WidgetStore | null;
  private readonly getCrdtWriter: () => CrdtCommWriter | null;

  /** Accumulated patches waiting for debounced flush, per comm. */
  private pendingState = new Map<string, Record<string, unknown>>();
  /**
   * Trail of recently-written values per key, for echo deduplication.
   *
   * During rapid drag (slider moving at 60/sec), the kernel processes
   * our writes in order and echoes each back. A stale echo of `1.2`
   * while the user is already at `1.4` would otherwise clobber the
   * optimistic state. Storing only the latest value catches the final
   * echo but misses in-flight intermediates.
   *
   * We keep every value we wrote during the grace window and suppress
   * any echo whose value matches any entry in the trail. Legitimate
   * kernel corrections (clamping `5.1` back to `5.0` when we never
   * wrote `5.0`) fall through because the corrected value isn't in
   * the trail.
   */
  private optimisticKeys = new Map<string, Map<string, unknown[]>>();
  /** Per-comm debounce timers. */
  private flushTimers = new Map<string, ReturnType<typeof setTimeout>>();
  /** Per-comm grace timers for delayed optimistic key cleanup. */
  private echoGraceTimers = new Map<string, ReturnType<typeof setTimeout>>();

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

    // 2. Append each written value to the per-key trail.
    let keys = this.optimisticKeys.get(commId);
    if (!keys) {
      keys = new Map();
      this.optimisticKeys.set(commId, keys);
    }
    for (const [key, value] of Object.entries(patch)) {
      let trail = keys.get(key);
      if (!trail) {
        trail = [];
        keys.set(key, trail);
      }
      trail.push(value);
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
   * Filter an incoming CRDT echo, suppressing keys whose echoed value
   * matches any recent local write for that key.
   *
   * Echoes of any value in the trail are dropped. Kernel corrections
   * (values we never wrote, e.g. ipywidgets clamping `5.1` back to
   * `5.0`) pass through and update the store.
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
      const trail = keys.get(key);
      if (trail && trail.some((written) => structuralEqual(written, value))) {
        continue;
      }
      filtered[key] = value;
      hasKeys = true;
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
    for (const timer of this.echoGraceTimers.values()) {
      clearTimeout(timer);
    }
    this.flushTimers.clear();
    this.echoGraceTimers.clear();
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
    const graceTimer = this.echoGraceTimers.get(commId);
    if (graceTimer !== undefined) {
      clearTimeout(graceTimer);
      this.echoGraceTimers.delete(commId);
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

    // Keep optimistic keys alive for a grace period after flush.
    // The CRDT write triggers a sync chain (frontend → daemon →
    // kernel → IOPub echo → CRDT → frontend) that can take 200-500ms.
    // If we cleared immediately, the stale kernel echo would pass
    // through shouldSuppressEcho and clobber the user's value.
    const existingGrace = this.echoGraceTimers.get(commId);
    if (existingGrace !== undefined) {
      clearTimeout(existingGrace);
    }
    this.echoGraceTimers.set(
      commId,
      setTimeout(() => {
        this.optimisticKeys.delete(commId);
        this.echoGraceTimers.delete(commId);
      }, ECHO_GRACE_MS),
    );
  }
}
