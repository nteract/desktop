/**
 * Pool state store — reactive state from the daemon's PoolDoc.
 *
 * The daemon writes pool stats (UV/Conda availability, errors) to a global
 * Automerge document (PoolDoc). Clients sync read-only via frame type 0x06.
 * This module provides a useSyncExternalStore-based React hook for reading
 * the pool state.
 */

import { useSyncExternalStore } from "react";

// ── Types ────────────────────────────────────────────────────────────

/** State of a single runtime pool (UV or Conda). */
export interface RuntimePoolState {
  available: number;
  warming: number;
  pool_size: number;
  /** Human-readable error message (undefined if healthy). */
  error?: string;
  /** Package that failed to install (undefined if not identified). */
  failed_package?: string;
  /** Error classification: "timeout", "invalid_package", "import_error", "setup_failed". */
  error_kind?: string;
  /** Number of consecutive failures (0 if healthy). */
  consecutive_failures: number;
  /** Seconds until next retry (0 if retry is imminent or healthy). */
  retry_in_secs: number;
}

/** Full pool state snapshot from the PoolDoc. */
export interface PoolState {
  uv: RuntimePoolState;
  conda: RuntimePoolState;
}

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

const DEFAULT_RUNTIME_POOL: RuntimePoolState = {
  available: 0,
  warming: 0,
  pool_size: 0,
  consecutive_failures: 0,
  retry_in_secs: 0,
};

export const DEFAULT_POOL_STATE: PoolState = {
  uv: { ...DEFAULT_RUNTIME_POOL },
  conda: { ...DEFAULT_RUNTIME_POOL },
};

// ── Store ────────────────────────────────────────────────────────────

let currentState: PoolState = DEFAULT_POOL_STATE;
const subscribers = new Set<() => void>();

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
  currentState = state;
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
