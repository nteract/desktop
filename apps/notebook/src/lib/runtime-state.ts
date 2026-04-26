/**
 * Runtime state store — reactive state from the daemon's RuntimeStateDoc.
 *
 * Types and diffing logic live in the `runtimed` package (pure, no React).
 * This module adds the React-specific store (useSyncExternalStore) on top.
 */

import { useSyncExternalStore } from "react";

// Re-export all types from the package so existing imports work.
export type {
  CommDocEntry,
  EnvState,
  ExecutionState,
  ExecutionTransition,
  KernelState,
  QueueEntry,
  QueueState,
  RuntimeState,
  TrustState,
} from "runtimed";

export {
  DEFAULT_RUNTIME_STATE,
  diffExecutions,
  getExecutionCountForCell,
} from "runtimed";

import { DEFAULT_RUNTIME_STATE, type RuntimeState } from "runtimed";

// ── Store ────────────────────────────────────────────────────────────

let currentState: RuntimeState = DEFAULT_RUNTIME_STATE;
// Tracks whether the daemon has pushed at least one RuntimeStateDoc frame
// since connect/reset. While false, `currentState` is the static default,
// and fields like `trust.status` must NOT be used as authoritative signals
// (the default "no_dependencies" would fail-open a trust gate).
let loaded = false;
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

/** Update the runtime state snapshot. Called by the frame pipeline. */
export function setRuntimeState(state: RuntimeState): void {
  currentState = state;
  loaded = true;
  notifySubscribers();
}

/** Reset to default state (e.g., on disconnect). */
export function resetRuntimeState(): void {
  currentState = DEFAULT_RUNTIME_STATE;
  loaded = false;
  notifySubscribers();
}

/** Read the current snapshot (non-reactive). */
export function getRuntimeState(): RuntimeState {
  return currentState;
}

/**
 * Whether the daemon has pushed at least one RuntimeStateDoc snapshot
 * since connect. Consumers that make authoritative decisions from state
 * (trust gates, dirty tracking) must check this first — a false value
 * means the current snapshot is `DEFAULT_RUNTIME_STATE`, not a real read.
 */
export function isRuntimeStateLoaded(): boolean {
  return loaded;
}

// ── React hook ───────────────────────────────────────────────────────

function subscribe(cb: () => void): () => void {
  subscribers.add(cb);
  return () => {
    subscribers.delete(cb);
  };
}

function getSnapshot(): RuntimeState {
  return currentState;
}

/**
 * Subscribe to the runtime state from the daemon's RuntimeStateDoc.
 *
 * Re-renders only when the daemon pushes a new state snapshot via
 * Automerge sync. The frontend never writes to this state.
 */
export function useRuntimeState(): RuntimeState {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

/**
 * Subscribe to the loaded flag. Re-renders on the transition from
 * "no snapshot yet" to "first snapshot applied" (and back on reset).
 */
export function useRuntimeStateLoaded(): boolean {
  return useSyncExternalStore(subscribe, () => loaded, () => loaded);
}
