import type { ExecutionState } from "./runtime-state";

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
