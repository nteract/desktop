import type { ExecutionState, RuntimeState } from "./runtime-state";

export interface RuntimeExecutionSnapshot {
  cell_id: string;
  execution_count: number | null;
  status: ExecutionState["status"];
  success: boolean | null;
  output_ids: string[];
}

export function extractOutputId(output: unknown): string | null {
  if (!output || typeof output !== "object") return null;
  const oid = (output as { output_id?: unknown }).output_id;
  return typeof oid === "string" && oid.length > 0 ? oid : null;
}

export function collectOutputIds(outputs: readonly unknown[] | undefined): string[] {
  const ids: string[] = [];
  if (!outputs) return ids;
  for (const output of outputs) {
    const oid = extractOutputId(output);
    if (oid) ids.push(oid);
  }
  return ids;
}

export function collectExecutionOutputIds(raw: ExecutionState): string[] {
  return collectOutputIds(raw.outputs);
}

export function executionFingerprint(raw: ExecutionState): string {
  // Include the ordered `output_id` list so same-length replacements
  // (e.g. clear_output(wait=True)) still invalidate cached snapshots.
  const ids = collectExecutionOutputIds(raw);
  return `${raw.cell_id}|${raw.execution_count ?? ""}|${raw.status}|${raw.success ?? ""}|${ids.join(",")}`;
}

export function buildRuntimeExecutionSnapshot(raw: ExecutionState): RuntimeExecutionSnapshot {
  return {
    cell_id: raw.cell_id,
    execution_count: raw.execution_count,
    status: raw.status,
    success: raw.success,
    output_ids: collectExecutionOutputIds(raw),
  };
}

export interface RuntimeExecutionProjection {
  upserts: Array<[execution_id: string, snapshot: RuntimeExecutionSnapshot]>;
  removed_execution_ids: string[];
}

/**
 * Stateful planner for projecting daemon-authored RuntimeState executions
 * into an execution store.
 *
 * The package owns the RuntimeState diff/cache semantics; consumers provide
 * the concrete store writes. This keeps React stores, Tauri apps, and tests
 * from each carrying their own interpretation of execution eviction and
 * output-id fingerprinting.
 */
export class RuntimeExecutionProjector {
  private knownExecutionIds = new Set<string>();
  private prevExecutionFingerprint = new Map<string, string>();

  project(state: RuntimeState): RuntimeExecutionProjection {
    const nextIds = new Set<string>();
    const upserts: RuntimeExecutionProjection["upserts"] = [];

    for (const [execution_id, entry] of Object.entries(state.executions)) {
      nextIds.add(execution_id);
      const fp = executionFingerprint(entry);
      if (this.prevExecutionFingerprint.get(execution_id) === fp) continue;
      this.prevExecutionFingerprint.set(execution_id, fp);
      upserts.push([execution_id, buildRuntimeExecutionSnapshot(entry)]);
    }

    const removed_execution_ids: string[] = [];
    for (const prev of this.knownExecutionIds) {
      if (!nextIds.has(prev)) removed_execution_ids.push(prev);
    }
    if (removed_execution_ids.length > 0) {
      for (const eid of removed_execution_ids) {
        this.prevExecutionFingerprint.delete(eid);
      }
    }
    this.knownExecutionIds = nextIds;

    return { upserts, removed_execution_ids };
  }

  reset(): void {
    this.knownExecutionIds.clear();
    this.prevExecutionFingerprint.clear();
  }
}
