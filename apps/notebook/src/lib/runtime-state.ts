/**
 * Runtime state store — reactive state from the daemon's RuntimeStateDoc.
 *
 * The daemon syncs kernel status, execution queue, environment sync state,
 * and last-saved timestamp via a per-notebook Automerge document. The WASM
 * layer deserializes it into a RuntimeState snapshot. This module holds the
 * latest snapshot and notifies subscribers on change.
 *
 * Components use `useRuntimeState()` to subscribe.
 */

import { useSyncExternalStore } from "react";

// ── Types ────────────────────────────────────────────────────────────

export interface KernelState {
  status: string;
  name: string;
  language: string;
  env_source: string;
}

export interface QueueEntry {
  cell_id: string;
  execution_id: string;
}

export interface QueueState {
  executing: QueueEntry | null;
  queued: QueueEntry[];
}

export interface EnvState {
  in_sync: boolean;
  added: string[];
  removed: string[];
  channels_changed: boolean;
  deno_changed: boolean;
}

export interface TrustState {
  status: string;
  needs_approval: boolean;
}

export interface RuntimeState {
  kernel: KernelState;
  queue: QueueState;
  env: EnvState;
  trust: TrustState;
  last_saved: string | null;
}

// ── Default state ────────────────────────────────────────────────────

const DEFAULT_RUNTIME_STATE: RuntimeState = {
  kernel: {
    status: "not_started",
    name: "",
    language: "",
    env_source: "",
  },
  queue: {
    executing: null,
    queued: [],
  },
  env: {
    in_sync: true,
    added: [],
    removed: [],
    channels_changed: false,
    deno_changed: false,
  },
  trust: {
    status: "no_dependencies",
    needs_approval: false,
  },
  last_saved: null,
};

// ── Store ────────────────────────────────────────────────────────────

let currentState: RuntimeState = DEFAULT_RUNTIME_STATE;
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
  notifySubscribers();
}

/** Reset to default state (e.g., on disconnect). */
export function resetRuntimeState(): void {
  currentState = DEFAULT_RUNTIME_STATE;
  notifySubscribers();
}

/** Read the current snapshot (non-reactive). */
export function getRuntimeState(): RuntimeState {
  return currentState;
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
