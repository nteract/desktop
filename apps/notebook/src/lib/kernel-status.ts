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
  statusKeyToLegacyStatus,
  type KernelActivity,
  type KernelStatus,
  type RuntimeLifecycle,
  type RuntimeStatusKey,
} from "runtimed";

import {
  KERNEL_STATUS,
  RUNTIME_STATUS,
  runtimeStatusKey,
  type KernelStatus,
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
 * Render a user-facing label for a [`RuntimeStatusKey`].
 *
 * Uses the expanded vocabulary — each starting sub-phase and each
 * `Running(_)` activity get their own label. The `Error` case appends
 * the typed reason when one is present; other states ignore `errorReason`.
 *
 * Prefer this form when the caller already has a (possibly throttled)
 * status key. Call sites that still hold the raw lifecycle can use
 * [`getLifecycleLabel`] which projects first.
 */
export function getStatusKeyLabel(key: RuntimeStatusKey, errorReason: string | null): string {
  if (key === RUNTIME_STATUS.ERROR && errorReason && errorReason.length > 0) {
    return `error: ${errorReason}`;
  }
  return RUNTIME_STATUS_LABELS[key];
}

/**
 * Render a user-facing label for the typed runtime lifecycle.
 *
 * Thin wrapper around [`getStatusKeyLabel`] that projects the lifecycle
 * to a [`RuntimeStatusKey`] first. Use the key form directly when you
 * have already throttled the `Running(Idle)` / `Running(Busy)` flicker
 * upstream.
 */
export function getLifecycleLabel(
  lifecycle: RuntimeLifecycle,
  errorReason: string | null,
): string {
  return getStatusKeyLabel(runtimeStatusKey(lifecycle), errorReason);
}

export function getTrustApprovalHandoffDisplayStatus({
  pending,
  kernelStatus,
  statusKey,
}: {
  pending: boolean;
  kernelStatus: KernelStatus;
  statusKey: RuntimeStatusKey;
}): { kernelStatus: KernelStatus; statusKey: RuntimeStatusKey } {
  if (
    pending &&
    (statusKey === RUNTIME_STATUS.AWAITING_TRUST || statusKey === RUNTIME_STATUS.NOT_STARTED)
  ) {
    return {
      kernelStatus: KERNEL_STATUS.STARTING,
      statusKey: RUNTIME_STATUS.RESOLVING,
    };
  }

  return { kernelStatus, statusKey };
}
