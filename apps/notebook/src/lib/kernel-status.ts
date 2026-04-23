/**
 * Kernel status types and UI labels.
 *
 * Core types re-exported from runtimed; UI-specific labels live here.
 */

export {
  isKernelStatus,
  KERNEL_STATUS,
  RUNTIME_STATUS,
  runtimeStatusKey,
  type KernelActivity,
  type KernelStatus,
  type RuntimeLifecycle,
  type RuntimeStatusKey,
} from "runtimed";

import {
  RUNTIME_STATUS,
  runtimeStatusKey,
  type RuntimeLifecycle,
  type RuntimeStatusKey,
} from "runtimed";

/**
 * User-facing label for each expanded [`RuntimeStatusKey`].
 *
 * Keyed by the flat runtime vocabulary so every lifecycle variant
 * (including each starting sub-phase and all three `Running(_)` cases)
 * gets a dedicated label. Exhaustive `Record` — adding a variant to
 * `RuntimeLifecycle` will fail to typecheck here until a label is added.
 */
export const RUNTIME_STATUS_LABELS: Record<RuntimeStatusKey, string> = {
  [RUNTIME_STATUS.NOT_STARTED]: "initializing",
  [RUNTIME_STATUS.AWAITING_TRUST]: "awaiting approval",
  [RUNTIME_STATUS.RESOLVING]: "resolving environment",
  [RUNTIME_STATUS.PREPARING_ENV]: "preparing environment",
  [RUNTIME_STATUS.LAUNCHING]: "launching kernel",
  [RUNTIME_STATUS.CONNECTING]: "connecting to kernel",
  [RUNTIME_STATUS.RUNNING_IDLE]: "idle",
  [RUNTIME_STATUS.RUNNING_BUSY]: "busy",
  [RUNTIME_STATUS.RUNNING_UNKNOWN]: "running",
  [RUNTIME_STATUS.ERROR]: "error",
  [RUNTIME_STATUS.SHUTDOWN]: "shutdown",
};

/**
 * Render a user-facing label for the typed runtime lifecycle.
 *
 * Uses the expanded vocabulary — each starting sub-phase and each
 * `Running(_)` activity get their own label. The `Error` case appends
 * the typed reason when one is present; other states ignore `errorReason`.
 */
export function getLifecycleLabel(
  lifecycle: RuntimeLifecycle,
  errorReason: string | null,
): string {
  const key = runtimeStatusKey(lifecycle);
  if (key === RUNTIME_STATUS.ERROR && errorReason && errorReason.length > 0) {
    return `error: ${errorReason}`;
  }
  return RUNTIME_STATUS_LABELS[key];
}
