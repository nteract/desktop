/**
 * Derived state functions and kernel status types.
 *
 * Pure functions that derive UI-consumable state from RuntimeState.
 * No React, no Tauri, no browser APIs.
 */

import {
  type Observable,
  type OperatorFunction,
  distinctUntilChanged,
  map,
  of,
  switchMap,
  timer,
} from "rxjs";

import type { QueueEntry, RuntimeState } from "./runtime-state";

// ── Kernel status ───────────────────────────────────────────────────

export const KERNEL_STATUS = {
  NOT_STARTED: "not_started",
  STARTING: "starting",
  IDLE: "idle",
  BUSY: "busy",
  ERROR: "error",
  SHUTDOWN: "shutdown",
  AWAITING_TRUST: "awaiting_trust",
} as const;

export type KernelStatus = (typeof KERNEL_STATUS)[keyof typeof KERNEL_STATUS];

const KERNEL_STATUS_SET: ReadonlySet<KernelStatus> = new Set(Object.values(KERNEL_STATUS));

export function isKernelStatus(value: string): value is KernelStatus {
  return KERNEL_STATUS_SET.has(value as KernelStatus);
}

// ── Derived types ───────────────────────────────────────────────────

export interface KernelInfo {
  kernelType: string | undefined;
  envSource: string | undefined;
}

export interface DaemonQueueState {
  executing: QueueEntry | null;
  queued: QueueEntry[];
}

export interface EnvSyncDiff {
  added: string[];
  removed: string[];
  channelsChanged: boolean;
  denoChanged: boolean;
}

export interface EnvSyncState {
  inSync: boolean;
  diff?: EnvSyncDiff;
}

// ── Derivation functions ────────────────────────────────────────────

/** Derive kernel type and environment source from RuntimeState. */
export function deriveKernelInfo(state: RuntimeState): KernelInfo {
  return {
    kernelType: state.kernel.language || undefined,
    envSource: state.kernel.env_source || undefined,
  };
}

/** Derive queue state from RuntimeState. */
export function deriveQueueState(state: RuntimeState): DaemonQueueState {
  return {
    executing: state.queue.executing,
    queued: state.queue.queued,
  };
}

/**
 * Derive environment sync state from RuntimeState.
 *
 * Returns null before kernel launch, on shutdown, or on error — indicating
 * "unknown" to consumers. Returns the sync state otherwise.
 */
export function deriveEnvSyncState(state: RuntimeState): EnvSyncState | null {
  const lc = state.kernel.lifecycle.lifecycle;
  if (
    (lc === "NotStarted" && !state.kernel.env_source) ||
    lc === "Shutdown" ||
    lc === "Error" ||
    lc === "AwaitingTrust"
  ) {
    return null;
  }
  return {
    inSync: state.env.in_sync,
    diff: state.env.in_sync
      ? undefined
      : {
          added: state.env.added,
          removed: state.env.removed,
          channelsChanged: state.env.channels_changed,
          denoChanged: state.env.deno_changed,
        },
  };
}

// ── Busy throttle ───────────────────────────────────────────────────

const DEFAULT_BUSY_THRESHOLD_MS = 60;

/**
 * RxJS operator that throttles kernel "busy" status transitions.
 *
 * The RuntimeStateDoc records every busy→idle transition, including
 * sub-60ms blips from tab completions. This operator delays "busy"
 * by `threshold` ms — if "idle" arrives within that window, the busy
 * is never emitted, preventing UI flicker.
 *
 * Other statuses (starting, error, shutdown, not_started) pass through
 * immediately and cancel any pending busy.
 */
export function throttleBusyStatus(
  threshold = DEFAULT_BUSY_THRESHOLD_MS,
): OperatorFunction<string, KernelStatus> {
  return (source: Observable<string>) =>
    source.pipe(
      distinctUntilChanged(),
      switchMap((raw) => {
        if (!isKernelStatus(raw)) return of<KernelStatus>(KERNEL_STATUS.NOT_STARTED);
        if (raw === KERNEL_STATUS.BUSY) {
          // Delay busy emission — switchMap cancels if next status arrives first
          return timer(threshold).pipe(map(() => KERNEL_STATUS.BUSY));
        }
        // All other statuses emit immediately (and cancel any pending busy via switchMap)
        return of<KernelStatus>(raw);
      }),
      distinctUntilChanged(),
    );
}

/**
 * Project a typed RuntimeLifecycle back to the legacy string status
 * vocabulary (`"idle"`, `"busy"`, `"starting"`, `"error"`, `"shutdown"`,
 * `"not_started"`, `"awaiting_trust"`).
 *
 * Kept for bridging throttleBusyStatus and other legacy-shape consumers.
 * New code should match on `RuntimeLifecycle` directly, or — for a flat
 * string key with every sub-state preserved — project through
 * [`runtimeStatusKey`] instead.
 */
export function lifecycleToLegacyStatus(lc: RuntimeState["kernel"]["lifecycle"]): KernelStatus {
  switch (lc.lifecycle) {
    case "NotStarted":
      return KERNEL_STATUS.NOT_STARTED;
    case "AwaitingTrust":
      return KERNEL_STATUS.AWAITING_TRUST;
    case "Resolving":
    case "PreparingEnv":
    case "Launching":
    case "Connecting":
      return KERNEL_STATUS.STARTING;
    case "Running":
      return lc.activity === "Busy" ? KERNEL_STATUS.BUSY : KERNEL_STATUS.IDLE;
    case "Error":
      return KERNEL_STATUS.ERROR;
    case "Shutdown":
      return KERNEL_STATUS.SHUTDOWN;
  }
}

// ── Expanded runtime status vocabulary ──────────────────────────────

/**
 * One flat string key per runtime state.
 *
 * Unlike the compressed [`KERNEL_STATUS`] vocabulary (where the four
 * starting sub-phases collapse to `"starting"` and `Running`'s activity
 * is a separate axis), `RUNTIME_STATUS` preserves every variant with its
 * own key. The `Running` cases are prefixed `"running-"` so the family
 * relationship is grep-able and so table lookups can be exhaustive
 * `Record<RuntimeStatusKey, X>` without a special-case `Unknown` duck.
 *
 * Use this for CSS classes, icon tables, label tables, and any other
 * lookup keyed on "what is the runtime doing right now." Use
 * [`KERNEL_STATUS`] only when interoperating with the compressed legacy
 * wire vocabulary.
 */
export const RUNTIME_STATUS = {
  NOT_STARTED: "not_started",
  AWAITING_TRUST: "awaiting_trust",
  RESOLVING: "resolving",
  PREPARING_ENV: "preparing_env",
  LAUNCHING: "launching",
  CONNECTING: "connecting",
  RUNNING_IDLE: "running-idle",
  RUNNING_BUSY: "running-busy",
  RUNNING_UNKNOWN: "running-unknown",
  ERROR: "error",
  SHUTDOWN: "shutdown",
} as const;

export type RuntimeStatusKey = (typeof RUNTIME_STATUS)[keyof typeof RUNTIME_STATUS];

/**
 * Project a typed RuntimeLifecycle to its flat [`RuntimeStatusKey`].
 *
 * Exhaustive over both the lifecycle union and the inner activity, so
 * adding a variant will fail to typecheck here until handled.
 */
export function runtimeStatusKey(lc: RuntimeState["kernel"]["lifecycle"]): RuntimeStatusKey {
  switch (lc.lifecycle) {
    case "NotStarted":
      return RUNTIME_STATUS.NOT_STARTED;
    case "AwaitingTrust":
      return RUNTIME_STATUS.AWAITING_TRUST;
    case "Resolving":
      return RUNTIME_STATUS.RESOLVING;
    case "PreparingEnv":
      return RUNTIME_STATUS.PREPARING_ENV;
    case "Launching":
      return RUNTIME_STATUS.LAUNCHING;
    case "Connecting":
      return RUNTIME_STATUS.CONNECTING;
    case "Running":
      switch (lc.activity) {
        case "Idle":
          return RUNTIME_STATUS.RUNNING_IDLE;
        case "Busy":
          return RUNTIME_STATUS.RUNNING_BUSY;
        case "Unknown":
          return RUNTIME_STATUS.RUNNING_UNKNOWN;
      }
    // eslint-disable-next-line no-fallthrough -- inner switch is exhaustive
    case "Error":
      return RUNTIME_STATUS.ERROR;
    case "Shutdown":
      return RUNTIME_STATUS.SHUTDOWN;
  }
}

/**
 * Derive a throttled kernel status observable from a RuntimeState stream.
 *
 * Convenience wrapper: projects `kernel.lifecycle` back to the legacy
 * status vocabulary and applies `throttleBusyStatus()`. Retained for
 * consumers of the pre-lifecycle API; new code should match on
 * `RuntimeLifecycle` directly.
 */
export function kernelStatus$(
  runtimeState$: Observable<RuntimeState>,
  threshold?: number,
): Observable<KernelStatus> {
  return runtimeState$.pipe(
    map((s) => lifecycleToLegacyStatus(s.kernel.lifecycle)),
    throttleBusyStatus(threshold),
  );
}
