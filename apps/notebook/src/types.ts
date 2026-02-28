export interface CodeCell {
  cell_type: "code";
  id: string;
  source: string;
  execution_count: number | null;
  outputs: JupyterOutput[];
}

export interface MarkdownCell {
  cell_type: "markdown";
  id: string;
  source: string;
}

export interface RawCell {
  cell_type: "raw";
  id: string;
  source: string;
}

export type NotebookCell = CodeCell | MarkdownCell | RawCell;

export type JupyterOutput =
  | {
      output_type: "execute_result" | "display_data";
      data: Record<string, unknown>;
      metadata?: Record<string, unknown>;
      execution_count?: number | null;
      display_id?: string;
    }
  | {
      output_type: "stream";
      name: "stdout" | "stderr";
      text: string;
    }
  | {
      output_type: "error";
      ename: string;
      evalue: string;
      traceback: string[];
    };

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

/** Snapshot of a comm channel's state for multi-window sync */
export interface CommSnapshot {
  comm_id: string;
  target_name: string;
  state: Record<string, unknown>;
  model_module?: string;
  model_name?: string;
  buffers?: number[][];
}

/** Broadcast events from daemon for kernel operations */
export type DaemonBroadcast =
  | {
      event: "kernel_status";
      status: string; // "starting" | "idle" | "busy" | "error" | "shutdown"
      cell_id?: string;
    }
  | {
      event: "execution_started";
      cell_id: string;
      execution_count: number;
    }
  | {
      event: "output";
      cell_id: string;
      output_type: string; // "stream" | "display_data" | "execute_result" | "error"
      output_json: string; // Serialized output in nbformat shape
    }
  | {
      event: "display_update";
      display_id: string;
      data: Record<string, unknown>;
      metadata: Record<string, unknown>;
    }
  | {
      event: "execution_done";
      cell_id: string;
    }
  | {
      event: "queue_changed";
      executing?: string;
      queued: string[];
    }
  | {
      event: "kernel_error";
      error: string;
    }
  | {
      event: "outputs_cleared";
      cell_id: string;
    }
  | {
      event: "comm";
      msg_type: string; // "comm_open" | "comm_msg" | "comm_close"
      content: Record<string, unknown>;
      buffers: number[][]; // Binary buffers as byte arrays
    }
  | {
      event: "comm_sync";
      comms: CommSnapshot[]; // All active comms for widget reconstruction
    }
  | {
      event: "deps_changed";
      uv: UvDependencies | null;
      conda: CondaDependencies | null;
    };

/** UV dependencies stored in Automerge metadata */
export interface UvDependencies {
  dependencies: string[];
  requires_python?: string;
}

/** Conda dependencies stored in Automerge metadata */
export interface CondaDependencies {
  dependencies: string[];
  channels: string[];
  python?: string;
}

/** Response types from daemon notebook requests */
export type DaemonNotebookResponse =
  | { result: "kernel_launched"; kernel_type: string; env_source: string }
  | {
      result: "kernel_already_running";
      kernel_type: string;
      env_source: string;
    }
  | { result: "cell_queued"; cell_id: string }
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
  | { result: "queue_state"; executing?: string; queued: string[] }
  | { result: "all_cells_queued"; count: number }
  | { result: "ok" }
  | { result: "error"; error: string };
