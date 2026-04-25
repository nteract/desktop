/** Cell metadata (arbitrary JSON object, preserves unknown keys) */
export type CellMetadata = Record<string, unknown>;

export interface CodeCell {
  cell_type: "code";
  id: string;
  source: string;
  execution_count: number | null;
  /**
   * Legacy: after Phase C-lite, the frame pipeline no longer populates
   * this field on incremental output changes. Full materialization and
   * local CRDT mutations still write it, and a few cross-cell readers
   * (drag preview, hidden-group error count) still look at it — those
   * paths recompute on structural changes only and tolerate staleness
   * during a live session. New code should subscribe via
   * `useCellOutputs(cell_id)` from `notebook-outputs.ts` instead.
   */
  outputs: JupyterOutput[];
  metadata: CellMetadata;
}

export interface MarkdownCell {
  cell_type: "markdown";
  id: string;
  source: string;
  metadata: CellMetadata;
  /** Resolved markdown asset refs (`attachment:...`, relative paths) → blob hash */
  resolvedAssets?: Record<string, string>;
}

export interface RawCell {
  cell_type: "raw";
  id: string;
  source: string;
  metadata: CellMetadata;
}

export type NotebookCell = CodeCell | MarkdownCell | RawCell;

/**
 * Common fields on every nbformat output. `output_id` is a stable
 * daemon-stamped UUID — always non-empty on the daemon write path,
 * optional here only so in-flight / local-only outputs typecheck.
 */
interface OutputCommon {
  output_id?: string;
}

export type JupyterOutput =
  | (OutputCommon & {
      output_type: "execute_result" | "display_data";
      data: Record<string, unknown>;
      metadata?: Record<string, unknown>;
      execution_count?: number | null;
      display_id?: string;
    })
  | (OutputCommon & {
      output_type: "stream";
      name: "stdout" | "stderr";
      text: string;
    })
  | (OutputCommon & {
      output_type: "error";
      ename: string;
      evalue: string;
      traceback: string[];
      /**
       * Optional rich-traceback sibling payload (see
       * `src/components/cell/jupyter-output.ts` for the canonical doc).
       * Present when the kernel emitted rich via
       * `application/vnd.nteract.traceback+json` OR the daemon
       * synthesized one from the ANSI traceback at `.ipynb` load.
       */
      rich?: unknown;
    });

export interface KernelspecInfo {
  name: string;
  display_name: string;
  language: string;
}

export interface JupyterMessage {
  header: {
    msg_id: string;
    msg_type: string;
    session: string;
    username: string;
    date: string;
    version: string;
  };
  parent_header?: {
    msg_id: string;
    msg_type: string;
    session: string;
    username: string;
    date: string;
    version: string;
  };
  metadata: Record<string, unknown>;
  content: Record<string, unknown>;
  buffers?: unknown[];
  channel?: string;
  cell_id?: string;
}

// Environment preparation progress events
export type EnvProgressPhase =
  | { phase: "starting"; env_hash: string }
  | { phase: "cache_hit"; env_path: string }
  | { phase: "lock_file_hit" }
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
  env_type: "conda" | "uv";
};

// pixi.toml detection info
export interface PixiInfo {
  path: string;
  relative_path: string;
  workspace_name: string | null;
  has_dependencies: boolean;
  dependency_count: number;
  has_pypi_dependencies: boolean;
  pypi_dependency_count: number;
  python: string | null;
  channels: string[];
}

// environment.yml detection info
export interface EnvironmentYmlInfo {
  path: string;
  relative_path: string;
  name: string | null;
  has_dependencies: boolean;
  dependency_count: number;
  has_pip_dependencies: boolean;
  pip_dependency_count: number;
  python: string | null;
  channels: string[];
}

// =============================================================================
// Daemon Broadcast Types (Phase 8: Daemon-owned kernel execution)
// =============================================================================

/** Broadcast events from daemon.
 *
 * Ephemeral, room-wide events that don't fit the request/response or
 * CRDT-sync model. Kernel state, execution lifecycle, queue, and outputs
 * all live in `RuntimeStateDoc` (frame `0x05`) — the dead `kernel_status`
 * / `execution_*` / `output` / `queue_changed` / `outputs_cleared` /
 * `display_update` / `kernel_error` / `env_sync_state` variants were
 * removed once the doc became authoritative.
 */
export type DaemonBroadcast =
  | {
      event: "comm";
      msg_type: string; // "comm_open" | "comm_msg" | "comm_close"
      content: Record<string, unknown>;
      buffers: number[][]; // Binary buffers as byte arrays
    }
  | ({
      event: "env_progress";
      env_type: "conda" | "uv";
    } & EnvProgressPhase)
  | {
      event: "notebook_autosaved";
      path: string;
    }
  | {
      event: "path_changed";
      /** New `.ipynb` path. `null` on an explicit "close file" rename (rare/future). */
      path: string | null;
    };

/** Response types from daemon notebook requests */
export type DaemonNotebookResponse =
  | { result: "kernel_launched"; kernel_type: string; env_source: string }
  | {
      result: "kernel_already_running";
      kernel_type: string;
      env_source: string;
    }
  | { result: "cell_queued"; cell_id: string; execution_id: string }
  | { result: "outputs_cleared"; cell_id: string }
  | { result: "interrupt_sent" }
  | { result: "kernel_shutting_down" }
  | { result: "no_kernel" }
  | {
      result: "kernel_info";
      kernel_type?: string;
      env_source?: string;
      status: string;
    }
  | {
      result: "queue_state";
      executing?: { cell_id: string; execution_id: string } | null;
      queued: { cell_id: string; execution_id: string }[];
    }
  | {
      result: "all_cells_queued";
      queued: { cell_id: string; execution_id: string }[];
    }
  | { result: "ok" }
  | { result: "error"; error: string }
  | { result: "sync_environment_started"; packages: string[] }
  | { result: "sync_environment_complete"; synced_packages: string[] }
  | {
      result: "sync_environment_failed";
      error: string;
      needs_restart: boolean;
    };

// Pool state types removed — pool state now syncs via PoolDoc (Automerge).
// See apps/notebook/src/lib/pool-state.ts for the new types.
