/**
 * Kernel status types and UI labels.
 *
 * Core types re-exported from runtimed; UI-specific labels live here.
 */

export {
  isKernelStatus,
  KERNEL_STATUS,
  type KernelStatus,
  type KernelActivity,
  type RuntimeLifecycle,
} from "runtimed";

import {
  KERNEL_STATUS,
  type KernelStatus,
  type RuntimeLifecycle,
} from "runtimed";

export const KERNEL_STATUS_LABELS: Record<KernelStatus, string> = {
  [KERNEL_STATUS.NOT_STARTED]: "initializing",
  [KERNEL_STATUS.STARTING]: "starting",
  [KERNEL_STATUS.IDLE]: "idle",
  [KERNEL_STATUS.BUSY]: "busy",
  [KERNEL_STATUS.ERROR]: "error",
  [KERNEL_STATUS.SHUTDOWN]: "shutdown",
  [KERNEL_STATUS.AWAITING_TRUST]: "awaiting approval",
};

const STARTING_PHASE_LABELS: Record<string, string> = {
  resolving: "resolving environment",
  preparing_env: "preparing environment",
  launching: "launching kernel",
  connecting: "connecting to kernel",
};

/**
 * @deprecated Use `getLifecycleLabel(lifecycle, errorReason)` instead.
 * Retained for any caller outside NotebookToolbar that hasn't migrated.
 */
export function getKernelStatusLabel(
  status: KernelStatus,
  startingPhase?: string,
): string {
  if (status === KERNEL_STATUS.STARTING && startingPhase) {
    return STARTING_PHASE_LABELS[startingPhase] ?? "starting";
  }
  return KERNEL_STATUS_LABELS[status];
}

/**
 * Render a user-facing label for the typed runtime lifecycle.
 *
 * Uses the full typed shape: each starting sub-phase and the Error-with-
 * reason case get their own label. `errorReason` is consulted only when
 * the lifecycle is `Error`; other states ignore it.
 */
export function getLifecycleLabel(
  lifecycle: RuntimeLifecycle,
  errorReason: string | null,
): string {
  switch (lifecycle.lifecycle) {
    case "NotStarted":
      return "initializing";
    case "AwaitingTrust":
      return "awaiting approval";
    case "Resolving":
      return "resolving environment";
    case "PreparingEnv":
      return "preparing environment";
    case "Launching":
      return "launching kernel";
    case "Connecting":
      return "connecting to kernel";
    case "Running":
      return lifecycle.activity === "Busy" ? "busy" : "idle";
    case "Error":
      return errorReason && errorReason.length > 0
        ? `error: ${errorReason}`
        : "error";
    case "Shutdown":
      return "shutdown";
  }
}
