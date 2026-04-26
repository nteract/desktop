/**
 * Derived state functions and kernel status types.
 *
 * Pure functions that derive UI-consumable state from RuntimeState.
 * No React, no Tauri, no browser APIs.
 */

import type { QueueEntry, RuntimeState } from "./runtime-state";

// ── Kernel status ───────────────────────────────────────────────────

/**
 * Compressed seven-state status vocabulary for UI-level conditionals.
 *
 * `KERNEL_STATUS` groups the full [`RuntimeLifecycle`] union into buckets
 * that map cleanly onto common UI predicates: a single "starting" bucket
 * for the four launch sub-phases, flat "idle" / "busy" for `Running`'s
 * activity axis, and one-to-one tags for the terminal states. Project
 * via [`lifecycleToLegacyStatus`] at the component layer; use the
 * expanded [`RUNTIME_STATUS`] vocabulary when you need every sub-phase
 * (label tables, CSS classes).
 */
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

/**
 * Env-manager the notebook is running under — `"uv"` / `"conda"` /
 * `"pixi"`. Drives toolbar badge + dep-header panel selection.
 *
 * Distinct from `RuntimeKind` (`"python"` / `"deno"`): a Python notebook
 * always has exactly one manager, a Deno notebook has none.
 */
export type EnvManager = "uv" | "conda" | "pixi";

/**
 * Runtime kind — `"python"` / `"deno"`. Matches `kernelspec.name` /
 * `language_info.name` as sampled from the notebook doc's metadata,
 * plus a fallback when the daemon has detected a project file but the
 * notebook metadata hasn't been stamped yet.
 */
export type RuntimeKind = "python" | "deno";

/**
 * Inline-metadata inputs to `deriveEnvManager`. Sourced from the WASM
 * Automerge doc (not `RuntimeState`), so we pass them in instead of
 * making the deriver depend on the WASM handle.
 */
export interface EnvManagerMetadataInputs {
  /** `runt.uv.dependencies` present on the notebook (regardless of count). */
  isUvConfigured: boolean;
  /** `runt.conda.dependencies` present on the notebook (regardless of count). */
  isCondaConfigured: boolean;
  /** environment.yml detected with a non-empty `dependencies:` list. */
  environmentYmlHasDeps: boolean;
  /** pixi.toml detected with non-empty `[dependencies]` or `[pypi-dependencies]`. */
  pixiHasDeps: boolean;
}

// ── Derivation functions ────────────────────────────────────────────

/** Derive kernel type and environment source from RuntimeState. */
export function deriveKernelInfo(state: RuntimeState): KernelInfo {
  return {
    kernelType: state.kernel.language || undefined,
    envSource: state.kernel.env_source || undefined,
  };
}

/**
 * Derive the env manager (`"uv"` / `"conda"` / `"pixi"`) a Python
 * notebook is using, from the authoritative sources in priority order:
 *
 * 1. Running kernel's `env_source` (`"uv:..."` / `"conda:..."` /
 *    `"pixi:..."`). The daemon has spoken — nothing else matters.
 * 2. Inline notebook metadata (`runt.uv` / `runt.conda`). The user has
 *    declared deps even if the kernel isn't up yet.
 * 3. Detected project file via `RuntimeState.project_context`:
 *    pixi.toml → pixi, environment.yml → conda, pyproject.toml → uv.
 *    The daemon walked up from the notebook and found one, so we trust
 *    it even before the notebook's own kernelspec lands.
 *
 * Returns `null` when none of these match (fresh untitled notebook with
 * no deps and no project file nearby).
 *
 * Inline-metadata signals (#2) are passed in by the caller because they
 * live in the WASM Automerge doc, not in `RuntimeState`.
 */
export function deriveEnvManager(
  state: RuntimeState,
  metadata: EnvManagerMetadataInputs,
): EnvManager | null {
  const envSource = state.kernel.env_source;
  if (envSource.startsWith("pixi:")) return "pixi";
  if (envSource.startsWith("conda:")) return "conda";
  if (envSource.startsWith("uv:")) return "uv";

  if (metadata.isUvConfigured) return "uv";
  if (metadata.isCondaConfigured || metadata.environmentYmlHasDeps) return "conda";
  if (metadata.pixiHasDeps) return "pixi";

  const ctx = state.project_context;
  if (ctx.state === "Detected") {
    switch (ctx.project_file.kind) {
      case "PixiToml":
        return "pixi";
      case "EnvironmentYml":
        return "conda";
      case "PyprojectToml":
        return "uv";
    }
  }

  return null;
}

/**
 * Derive the notebook's runtime kind — `"python"` / `"deno"` — from
 * WASM-resolved kernelspec/language_info, a pre-metadata daemon hint,
 * and a last-ditch project-file fallback.
 *
 * The first two inputs are produced by the frontend (WASM metadata
 * detection + daemon:ready hint). The project-context fallback kicks in
 * when the notebook hasn't yet persisted its kernelspec but the daemon
 * already found a Python project file next to it — "untitled notebook
 * sitting in a pyproject directory" is the canonical case.
 *
 * None of the three project-file kinds are Deno indicators, so a
 * detected project file implies Python. A Deno runtime only shows up
 * through the first two signals.
 */
export function deriveRuntimeKind(
  state: RuntimeState,
  detectedRuntime: string | null,
  runtimeHint: string | null,
): RuntimeKind | null {
  if (detectedRuntime === "python" || detectedRuntime === "deno") return detectedRuntime;
  if (runtimeHint === "python" || runtimeHint === "deno") return runtimeHint;
  if (state.project_context.state === "Detected") return "python";
  return null;
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

/**
 * Project a typed [`RuntimeLifecycle`] into the compressed
 * [`KERNEL_STATUS`] bucket vocabulary. Four starting sub-phases collapse
 * to `"starting"`; `Running`'s activity axis flattens to `"idle"` /
 * `"busy"`.
 *
 * Used at UI boundaries that want the simpler bucket shape for
 * predicates ("kernel is startable", "kernel is healthy enough to
 * execute"). For exhaustive label / icon / CSS tables keyed on every
 * sub-state, project through [`runtimeStatusKey`] instead.
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
 * [`KERNEL_STATUS`] when the simpler seven-bucket shape matches your
 * UI predicate.
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
 * Project a flat [`RuntimeStatusKey`] down into the compressed
 * [`KERNEL_STATUS`] bucket vocabulary.
 *
 * The three `Running(_)` keys collapse the same way
 * [`lifecycleToLegacyStatus`] does — `RUNNING_BUSY` → `BUSY`, everything
 * else in the family → `IDLE`. Useful when a consumer has already produced
 * a `RuntimeStatusKey` (e.g. after a throttle step) and needs the
 * compressed shape for predicates or color-branches.
 */
export function statusKeyToLegacyStatus(key: RuntimeStatusKey): KernelStatus {
  switch (key) {
    case RUNTIME_STATUS.NOT_STARTED:
      return KERNEL_STATUS.NOT_STARTED;
    case RUNTIME_STATUS.AWAITING_TRUST:
      return KERNEL_STATUS.AWAITING_TRUST;
    case RUNTIME_STATUS.RESOLVING:
    case RUNTIME_STATUS.PREPARING_ENV:
    case RUNTIME_STATUS.LAUNCHING:
    case RUNTIME_STATUS.CONNECTING:
      return KERNEL_STATUS.STARTING;
    case RUNTIME_STATUS.RUNNING_BUSY:
      return KERNEL_STATUS.BUSY;
    case RUNTIME_STATUS.RUNNING_IDLE:
    case RUNTIME_STATUS.RUNNING_UNKNOWN:
      return KERNEL_STATUS.IDLE;
    case RUNTIME_STATUS.ERROR:
      return KERNEL_STATUS.ERROR;
    case RUNTIME_STATUS.SHUTDOWN:
      return KERNEL_STATUS.SHUTDOWN;
  }
}
