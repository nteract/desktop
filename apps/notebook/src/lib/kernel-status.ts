/**
 * Kernel status types and UI labels.
 *
 * Core types re-exported from runtimed; UI-specific labels live here.
 */

export { isKernelStatus, KERNEL_STATUS, type KernelStatus } from "runtimed";

import { KERNEL_STATUS, type KernelStatus } from "runtimed";

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

export function getKernelStatusLabel(
  status: KernelStatus,
  startingPhase?: string,
): string {
  if (status === KERNEL_STATUS.STARTING && startingPhase) {
    return STARTING_PHASE_LABELS[startingPhase] ?? "starting";
  }
  return KERNEL_STATUS_LABELS[status];
}
