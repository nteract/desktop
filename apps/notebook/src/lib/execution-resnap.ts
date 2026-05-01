export interface ExecutionResnapDecisionInput {
  focusedCellId: string | null;
  outputsVersion: number;
  executingCellCount: number;
  resnapCancelled: boolean;
  resnapUntil: number;
  now: number;
  activeWindowMs?: number;
}

export interface ExecutionResnapDecision {
  shouldResnap: boolean;
  nextResnapUntil: number;
}

export function decideExecutionResnap({
  focusedCellId,
  outputsVersion,
  executingCellCount,
  resnapCancelled,
  resnapUntil,
  now,
  activeWindowMs = 3500,
}: ExecutionResnapDecisionInput): ExecutionResnapDecision {
  if (!focusedCellId || outputsVersion === 0 || resnapCancelled) {
    return { shouldResnap: false, nextResnapUntil: resnapUntil };
  }

  if (executingCellCount > 0) {
    return { shouldResnap: true, nextResnapUntil: now + activeWindowMs };
  }

  if (now > resnapUntil) {
    return { shouldResnap: false, nextResnapUntil: resnapUntil };
  }

  return { shouldResnap: true, nextResnapUntil: resnapUntil };
}
