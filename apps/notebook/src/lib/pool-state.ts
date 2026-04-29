/**
 * Pool state store — reactive state from the daemon's PoolDoc.
 *
 * The daemon writes pool stats (UV/Conda availability, errors) to a global
 * Automerge document (PoolDoc). Clients sync read-only via frame type 0x06.
 * This module provides a useSyncExternalStore-based React hook for reading
 * the pool state.
 */

import { useSyncExternalStore } from "react";
import { DEFAULT_POOL_STATE, type PoolState } from "runtimed";

// ── Types ────────────────────────────────────────────────────────────

export { DEFAULT_POOL_STATE, type PoolState, type RuntimePoolState } from "runtimed";

/** Pool error info with timestamp, used by PoolErrorBanner. */
export interface PoolErrorWithTimestamp {
  message: string;
  failed_package?: string;
  error_kind?: string;
  consecutive_failures: number;
  retry_in_secs: number;
  /** When this state was received (epoch ms). */
  receivedAt: number;
}

// ── Store ────────────────────────────────────────────────────────────

let currentState: PoolState = DEFAULT_POOL_STATE;
const subscribers = new Set<() => void>();

function normalizePoolState(state: PoolState): PoolState {
  const partial = state as Partial<PoolState>;
  if (partial.uv && partial.conda && partial.pixi) {
    return state;
  }
  return {
    uv: partial.uv ?? DEFAULT_POOL_STATE.uv,
    conda: partial.conda ?? DEFAULT_POOL_STATE.conda,
    pixi: partial.pixi ?? DEFAULT_POOL_STATE.pixi,
  };
}

function notifySubscribers(): void {
  for (const cb of subscribers) {
    try {
      cb();
    } catch {
      // Subscriber errors must not break the dispatch loop
    }
  }
}

/** Update the pool state snapshot. Called by the frame pipeline. */
export function setPoolState(state: PoolState): void {
  currentState = normalizePoolState(state);
  notifySubscribers();
}

/** Reset to default state (e.g., on disconnect). */
export function resetPoolState(): void {
  currentState = DEFAULT_POOL_STATE;
  notifySubscribers();
}

/** Read the current snapshot (non-reactive). */
export function getPoolState(): PoolState {
  return currentState;
}

// ── React hook ───────────────────────────────────────────────────

function subscribe(cb: () => void): () => void {
  subscribers.add(cb);
  return () => {
    subscribers.delete(cb);
  };
}

function getSnapshot(): PoolState {
  return currentState;
}

/**
 * Subscribe to the pool state from the daemon's PoolDoc.
 *
 * Returns the raw PoolState. For error-specific UI with dismiss logic,
 * use `usePoolErrors()` instead.
 */
export function usePoolState(): PoolState {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}
