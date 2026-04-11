/**
 * Runtime state types from the daemon's RuntimeStateDoc.
 *
 * Pure module — no React, no Tauri. Consumers that need a reactive store
 * (e.g. React's useSyncExternalStore) build their own on top of these types.
 */

// ── Types ────────────────────────────────────────────────────────────

export interface KernelState {
  status: string;
  starting_phase: string;
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
  prewarmed_packages: string[];
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
  outputs?: string[];
}

/** Snapshot of a comm channel from RuntimeStateDoc. */
export interface CommDocEntry {
  target_name: string;
  model_module: string;
  model_name: string;
  /** Widget state as a native object (stored as native Automerge map). */
  state: Record<string, unknown>;
  /** Output manifest hashes (OutputModel widgets only). */
  outputs: string[];
  /** Insertion order for dependency-correct replay. */
  seq: number;
}

/** A detected status transition for a single execution. */
export interface ExecutionTransition {
  execution_id: string;
  cell_id: string;
  kind: "started" | "done" | "error";
  execution_count: number | null;
}

export interface RuntimeState {
  kernel: KernelState;
  queue: QueueState;
  env: EnvState;
  trust: TrustState;
  last_saved: string | null;
  executions: Record<string, ExecutionState>;
  comms: Record<string, CommDocEntry>;
}

// ── Defaults ─────────────────────────────────────────────────────────

export const DEFAULT_RUNTIME_STATE: RuntimeState = {
  kernel: {
    status: "not_started",
    starting_phase: "",
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
    prewarmed_packages: [],
  },
  trust: {
    status: "no_dependencies",
    needs_approval: false,
  },
  last_saved: null,
  executions: {},
  comms: {},
};

// ── Utilities ────────────────────────────────────────────────────────

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

    // Same status — check if execution_count arrived (kernel sends
    // execute_input after the status transitions to "running").
    if (prevStatus === currStatus) {
      if (
        currStatus === "running" &&
        entry.execution_count != null &&
        prevEntry?.execution_count == null
      ) {
        transitions.push({
          execution_id: eid,
          cell_id: entry.cell_id,
          kind: "started",
          execution_count: entry.execution_count,
        });
      }
      continue;
    }

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
    } else if (currStatus === "running" && prevStatus !== "done" && prevStatus !== "error") {
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

/**
 * Resolve the most recent execution_count for a cell from RuntimeState.
 *
 * The daemon writes execution_count to RuntimeStateDoc (not NotebookDoc),
 * so the WASM handle's get_cell_execution_count always returns "null".
 * This mirrors runt-mcp's get_cell_execution_count_from_runtime: find
 * the most recent execution for the cell that has a count set.
 */
export function getExecutionCountForCell(state: RuntimeState, cellId: string): number | null {
  let best: number | null = null;
  for (const exec of Object.values(state.executions)) {
    if (exec.cell_id === cellId && exec.execution_count != null) {
      if (best === null || exec.execution_count > best) {
        best = exec.execution_count;
      }
    }
  }
  return best;
}
