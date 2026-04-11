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
  if (
    (state.kernel.status === "not_started" && !state.kernel.env_source) ||
    state.kernel.status === "shutdown" ||
    state.kernel.status === "error" ||
    state.kernel.status === "awaiting_trust"
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
 * Derive a throttled kernel status observable from a RuntimeState stream.
 *
 * Convenience wrapper: extracts `kernel.status` and applies `throttleBusyStatus()`.
 */
export function kernelStatus$(
  runtimeState$: Observable<RuntimeState>,
  threshold?: number,
): Observable<KernelStatus> {
  return runtimeState$.pipe(
    map((s) => s.kernel.status),
    throttleBusyStatus(threshold),
  );
}
