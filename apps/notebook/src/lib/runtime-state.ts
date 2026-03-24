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

export interface ExecutionState {
  cell_id: string;
  status: "queued" | "running" | "done" | "error";
  execution_count: number | null;
  success: boolean | null;
}

/** A detected status transition for a single execution. */
export interface ExecutionTransition {
  execution_id: string;
  cell_id: string;
  kind: "started" | "done" | "error";
  execution_count: number | null;
}

/**
 * Diff two executions maps to detect status transitions.
 *
 * Returns transitions for:
 * - New entry or "queued"→"running" → "started"
 * - "running"→"done" → "done"
 * - "running"→"error" or "queued"→"error" (kernel death) → "error"
 *
 * Slow joiners see the final state — no missed transitions. If a sync
 * batches multiple changes (queued→done in one round), we emit the
 * terminal event only.
 */
export function diffExecutions(
  prev: Record<string, ExecutionState>,
  curr: Record<string, ExecutionState>,
): ExecutionTransition[] {
  const transitions: ExecutionTransition[] = [];

  for (const [eid, entry] of Object.entries(curr)) {
    const prevEntry = prev[eid];
    const prevStatus = prevEntry?.status;
    const currStatus = entry.status;

    // No change
    if (prevStatus === currStatus) continue;

    // Terminal states: done or error
    if (currStatus === "done") {
      transitions.push({
        execution_id: eid,
        cell_id: entry.cell_id,
        kind: "done",
        execution_count: entry.execution_count,
      });
    } else if (currStatus === "error") {
      transitions.push({
        execution_id: eid,
        cell_id: entry.cell_id,
        kind: "error",
        execution_count: entry.execution_count,
      });
    } else if (
      currStatus === "running" &&
      prevStatus !== "done" &&
      prevStatus !== "error"
    ) {
      // Started (queued→running or new→running)
      transitions.push({
        execution_id: eid,
        cell_id: entry.cell_id,
        kind: "started",
        execution_count: entry.execution_count,
      });
    }
  }

  return transitions;
}

export interface RuntimeState {
  kernel: KernelState;
  queue: QueueState;
  env: EnvState;
  trust: TrustState;
  last_saved: string | null;
  executions: Record<string, ExecutionState>;
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
  executions: {},
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
