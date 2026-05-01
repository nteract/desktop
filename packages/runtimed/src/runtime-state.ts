/**
 * Runtime state types from the daemon's RuntimeStateDoc.
 *
 * Pure module — no React, no Tauri. Consumers that need a reactive store
 * (e.g. React's useSyncExternalStore) build their own on top of these types.
 */

// ── Types ────────────────────────────────────────────────────────────

/**
 * Observable activity of a running kernel.
 *
 * Mirror of `runtime_doc::KernelActivity`. Only meaningful when the
 * runtime lifecycle is `Running`.
 */
export type KernelActivity = "Unknown" | "Idle" | "Busy";

/**
 * Typed reason accompanying a [`RuntimeLifecycle`] `Error` transition.
 *
 * Mirror of `runtime_doc::KernelErrorReason`. The daemon writes one of
 * these strings to `kernel.error_reason`; readers can use the exported
 * [`KERNEL_ERROR_REASON`] constants instead of bare string literals when
 * gating UI on a specific cause.
 */
export type KernelErrorReasonKey =
  | "missing_ipykernel"
  | "dependency_cache_missing_ipykernel"
  | "ipykernel_site_packages_mismatch"
  | "conda_env_yml_missing";

/**
 * Typed error-reason strings. Mirrors `KernelErrorReason::as_str()` on
 * the Rust side — both ends use the same literal so the CRDT value is
 * unambiguous.
 */
export const KERNEL_ERROR_REASON = {
  MISSING_IPYKERNEL: "missing_ipykernel",
  DEPENDENCY_CACHE_MISSING_IPYKERNEL: "dependency_cache_missing_ipykernel",
  IPYKERNEL_SITE_PACKAGES_MISMATCH: "ipykernel_site_packages_mismatch",
  /**
   * environment.yml declares a conda env that isn't built on this
   * machine. Daemon sets this instead of silently falling back to a
   * pool env so the UI can render a specific "build your env" banner.
   * `kernel.error_details` carries the declared env name and the
   * remediation command.
   */
  CONDA_ENV_YML_MISSING: "conda_env_yml_missing",
} as const satisfies Record<string, KernelErrorReasonKey>;

/**
 * Lifecycle of a runtime, from not-started through running to shutdown.
 *
 * Discriminated union on the `lifecycle` tag, matching Rust's serde
 * tag+content format (`#[serde(tag = "lifecycle", content = "activity")]`).
 * Only `Running` carries an `activity` payload — everything else is a
 * lone tag.
 *
 * Mirror of `runtime_doc::RuntimeLifecycle`.
 */
export type RuntimeLifecycle =
  | { lifecycle: "NotStarted" }
  | { lifecycle: "AwaitingTrust" }
  | { lifecycle: "AwaitingEnvBuild" }
  | { lifecycle: "Resolving" }
  | { lifecycle: "PreparingEnv" }
  | { lifecycle: "Launching" }
  | { lifecycle: "Connecting" }
  | { lifecycle: "Running"; activity: KernelActivity }
  | { lifecycle: "Error" }
  | { lifecycle: "Shutdown" };

export interface KernelState {
  /** Typed lifecycle. The authoritative view of kernel state. */
  lifecycle: RuntimeLifecycle;
  /**
   * Typed reason populated for lifecycle states that carry a specific cause
   * such as `Error` or `AwaitingEnvBuild`.
   * `null` when the kernel map is absent; empty string when scaffolded but
   * unset. Most consumers can treat both as "no reason."
   */
  error_reason: string | null;
  /**
   * Free-form details accompanying an error or user-decision state, shown
   * to the user via the banner/dialog. Carries specifics that don't fit in the typed
   * `error_reason` enum — e.g., the name of a conda env declared in
   * environment.yml that isn't built on this machine, with a suggested
   * remediation command. `null`/empty when absent or unset.
   */
  error_details: string | null;
  name: string;
  language: string;
  env_source: string;
}

export interface QueueEntry {
  cell_id: string;
  execution_id: string;
}

export interface QueueState {
  executing: QueueEntry | null;
  queued: QueueEntry[];
}

export interface EnvState {
  in_sync: boolean;
  added: string[];
  removed: string[];
  channels_changed: boolean;
  deno_changed: boolean;
  prewarmed_packages: string[];
  progress: EnvProgressEvent | null;
}

export type EnvProgressEnvType = "conda" | "uv" | "pixi" | (string & {});

export type EnvProgressPhase =
  | { phase: "starting"; env_hash: string }
  | { phase: "cache_hit"; env_path: string }
  | { phase: "lock_file_hit" }
  | { phase: "offline_hit" }
  | { phase: "fetching_repodata"; channels: string[] }
  | { phase: "repodata_complete"; record_count: number; elapsed_ms: number }
  | { phase: "solving"; spec_count: number }
  | { phase: "solve_complete"; package_count: number; elapsed_ms: number }
  | { phase: "installing"; total: number }
  | {
      phase: "download_progress";
      completed: number;
      total: number;
      current_package: string;
      bytes_downloaded: number;
      bytes_total: number | null;
      bytes_per_second: number;
    }
  | {
      phase: "link_progress";
      completed: number;
      total: number;
      current_package: string;
    }
  | { phase: "install_complete"; elapsed_ms: number }
  | { phase: "creating_venv" }
  | { phase: "installing_packages"; packages: string[] }
  | { phase: "ready"; env_path: string; python_path: string }
  | { phase: "error"; message: string };

export type EnvProgressEvent = EnvProgressPhase & {
  env_type: EnvProgressEnvType;
};

/**
 * Trust status mirrors `runt_trust::TrustStatus` serialized with
 * `rename_all = "snake_case"`.
 */
export type TrustStatus = "trusted" | "untrusted" | "signature_invalid" | "no_dependencies";

export interface TrustState {
  status: TrustStatus;
  needs_approval: boolean;
  approved_uv_dependencies: string[];
  approved_conda_dependencies: string[];
  approved_pixi_dependencies: string[];
  approved_pixi_pypi_dependencies: string[];
}

export interface ExecutionState {
  cell_id: string;
  status: "queued" | "running" | "done" | "error";
  execution_count: number | null;
  success: boolean | null;
  /** Queue sequence number from RuntimeStateDoc; used as execution recency. */
  seq?: number | null;
  /**
   * Output manifests in emission order, as the WASM runtime-state snapshot
   * exposes them. Each entry is the raw on-the-wire manifest (un-narrowed,
   * with `{inline}`/`{blob}` ContentRefs). Resolved manifests live in the
   * per-output store, not here.
   */
  outputs?: unknown[];
}

/** Snapshot of a comm channel from RuntimeStateDoc. */
export interface CommDocEntry {
  target_name: string;
  model_module: string;
  model_name: string;
  /** Widget state as a native object (stored as native Automerge map). */
  state: Record<string, unknown>;
  /** Output manifest hashes (OutputModel widgets only). */
  outputs: string[];
  /** Insertion order for dependency-correct replay. */
  seq: number;
}

/** A detected status transition for a single execution. */
export interface ExecutionTransition {
  execution_id: string;
  cell_id: string;
  kind: "started" | "done" | "error";
  execution_count: number | null;
}

/**
 * Kind of project file the daemon walked up and found for the notebook.
 * Mirrors `runtime_doc::ProjectFileKind`.
 */
export type ProjectFileKind = "PyprojectToml" | "PixiToml" | "EnvironmentYml";

/** Pointer to the project file on the daemon's disk. */
export interface ProjectFile {
  kind: ProjectFileKind;
  absolute_path: string;
  relative_to_notebook: string;
}

/**
 * Kind-specific parsed extras. Discriminated by `kind`; matches the
 * Rust `ProjectFileExtras` tagged enum.
 */
export type ProjectFileExtras =
  | { kind: "None" }
  | { kind: "Pixi"; channels: string[]; pypi_dependencies: string[] }
  | { kind: "EnvironmentYml"; channels: string[]; pip: string[] };

/** Snapshot of the daemon's parse of a detected project file. */
export interface ProjectFileParsed {
  dependencies: string[];
  /**
   * Dev-only dependencies. Populated from pyproject.toml's
   * `[tool.uv.dev-dependencies]`; always empty for pixi and
   * environment.yml (they carry their own sublists in `extras`).
   */
  dev_dependencies: string[];
  requires_python: string | null;
  prerelease: string | null;
  extras: ProjectFileExtras;
}

/**
 * Daemon-observed project-file context for a notebook, discriminated
 * by `state`. Matches the Rust `ProjectContext` tagged enum. Untitled
 * notebooks and fresh peers see `Pending` until the first daemon write
 * arrives via sync.
 */
export type ProjectContext =
  | { state: "Pending" }
  | { state: "NotFound"; observed_at: string }
  | {
      state: "Detected";
      project_file: ProjectFile;
      parsed: ProjectFileParsed;
      observed_at: string;
    }
  | {
      state: "Unreadable";
      path: string;
      reason: string;
      observed_at: string;
    };

export interface RuntimeState {
  kernel: KernelState;
  queue: QueueState;
  env: EnvState;
  trust: TrustState;
  last_saved: string | null;
  /**
   * Path to the notebook's `.ipynb` on the daemon's disk. `null` for
   * untitled notebooks; the daemon writes this on save / save-as.
   */
  path: string | null;
  executions: Record<string, ExecutionState>;
  comms: Record<string, CommDocEntry>;
  /**
   * Daemon-observed project file context. Clients read this instead of
   * walking the filesystem themselves.
   */
  project_context: ProjectContext;
}

// ── Defaults ─────────────────────────────────────────────────────────

export const DEFAULT_RUNTIME_STATE: RuntimeState = {
  kernel: {
    lifecycle: { lifecycle: "NotStarted" },
    error_reason: "",
    error_details: "",
    name: "",
    language: "",
    env_source: "",
  },
  queue: {
    executing: null,
    queued: [],
  },
  env: {
    in_sync: true,
    added: [],
    removed: [],
    channels_changed: false,
    deno_changed: false,
    prewarmed_packages: [],
    progress: null,
  },
  trust: {
    status: "no_dependencies",
    needs_approval: false,
    approved_uv_dependencies: [],
    approved_conda_dependencies: [],
    approved_pixi_dependencies: [],
    approved_pixi_pypi_dependencies: [],
  },
  last_saved: null,
  path: null,
  executions: {},
  comms: {},
  project_context: { state: "Pending" },
};

// ── Utilities ────────────────────────────────────────────────────────

/**
 * Diff two executions maps to detect status transitions.
 *
 * Returns transitions for:
 * - New entry or "queued"→"running" → "started"
 * - "running"→"done" → "done"
 * - "running"→"error" or "queued"→"error" (kernel death) → "error"
 *
 * Slow joiners see the final state — no missed transitions. If a sync
 * batches multiple changes (queued→done in one round), we emit the
 * terminal event only.
 */
export function diffExecutions(
  prev: Record<string, ExecutionState>,
  curr: Record<string, ExecutionState>,
): ExecutionTransition[] {
  const transitions: ExecutionTransition[] = [];

  for (const [eid, entry] of Object.entries(curr)) {
    const prevEntry = prev[eid];
    const prevStatus = prevEntry?.status;
    const currStatus = entry.status;

    // Same status — check if execution_count arrived (kernel sends
    // execute_input after the status transitions to "running").
    if (prevStatus === currStatus) {
      if (
        currStatus === "running" &&
        entry.execution_count != null &&
        prevEntry?.execution_count == null
      ) {
        transitions.push({
          execution_id: eid,
          cell_id: entry.cell_id,
          kind: "started",
          execution_count: entry.execution_count,
        });
      }
      continue;
    }

    // Terminal states: done or error
    if (currStatus === "done") {
      transitions.push({
        execution_id: eid,
        cell_id: entry.cell_id,
        kind: "done",
        execution_count: entry.execution_count,
      });
    } else if (currStatus === "error") {
      transitions.push({
        execution_id: eid,
        cell_id: entry.cell_id,
        kind: "error",
        execution_count: entry.execution_count,
      });
    } else if (currStatus === "running" && prevStatus !== "done" && prevStatus !== "error") {
      // Started (queued→running or new→running)
      transitions.push({
        execution_id: eid,
        cell_id: entry.cell_id,
        kind: "started",
        execution_count: entry.execution_count,
      });
    }
  }

  return transitions;
}

/**
 * Resolve the most recent execution_count for a cell from RuntimeState.
 *
 * RuntimeStateDoc is the live source of truth. NotebookDoc may carry a
 * persisted nbformat-history fallback when runtime state is unavailable.
 * This mirrors runt-mcp's get_cell_execution_count_from_runtime: find
 * the most recent execution for the cell that has a count set.
 */
export function getExecutionCountForCell(state: RuntimeState, cellId: string): number | null {
  let best: { count: number; seq: number | null } | null = null;
  for (const exec of Object.values(state.executions)) {
    if (exec.cell_id === cellId && exec.execution_count != null) {
      const seq = exec.seq ?? null;
      // Keep in sync with RuntimeState::execution_count_for_cell in runtime-doc.
      if (
        best === null ||
        (seq !== null &&
          (best.seq === null ||
            seq > best.seq ||
            (seq === best.seq && exec.execution_count > best.count))) ||
        (seq === null && best.seq === null && exec.execution_count > best.count)
      ) {
        best = { count: exec.execution_count, seq };
      }
    }
  }
  return best?.count ?? null;
}
