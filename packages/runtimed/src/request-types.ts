/**
 * Frontend-visible NotebookRequest/NotebookResponse wire types from
 * crates/notebook-protocol/src/protocol.rs.
 * These are transport-agnostic — callers encode them as needed.
 */

import type { QueueEntry } from "./runtime-state";

// ── Requests ────────────────────────────────────────────────────────

export interface GuardedNotebookProvenance {
  observed_heads: string[];
}

export interface DependencyGuard {
  observed_heads: string[];
  dependency_fingerprint: string;
}

export type PackageManager = "uv" | "conda" | "pixi" | (string & {});

export type EnvSource =
  | `${PackageManager}:prewarmed`
  | `${PackageManager}:inline`
  | "uv:pyproject"
  | "pixi:toml"
  | "conda:env_yml"
  | `${PackageManager}:pep723`
  | "deno"
  | (string & {});

export type LaunchSpec = "auto" | "" | "prewarmed" | `auto:${PackageManager}` | EnvSource;

export interface FeatureFlags {
  bootstrap_dx?: boolean;
}

export interface DenoLaunchedConfig {
  permissions: string[];
  import_map?: string | null;
  config?: string | null;
  flexible_npm_imports: boolean;
}

export interface LaunchedEnvConfig extends FeatureFlags {
  uv_deps?: string[] | null;
  conda_deps?: string[] | null;
  conda_channels?: string[] | null;
  pixi_deps?: string[] | null;
  pixi_toml_deps?: string[] | null;
  pixi_toml_path?: string | null;
  pyproject_path?: string | null;
  environment_yml_path?: string | null;
  environment_yml_deps?: string[] | null;
  deno_config?: DenoLaunchedConfig | null;
  venv_path?: string | null;
  python_path?: string | null;
  launch_id?: string | null;
  prewarmed_packages?: string[];
}

export type NotebookRequest =
  | {
      type: "launch_kernel";
      kernel_type: string;
      env_source: LaunchSpec;
      notebook_path?: string;
    }
  | { type: "execute_cell"; cell_id: string }
  | { type: "execute_cell_guarded"; cell_id: string; observed_heads: string[] }
  | { type: "clear_outputs"; cell_id: string }
  | { type: "interrupt_execution" }
  | { type: "shutdown_kernel" }
  | { type: "sync_environment"; guard?: DependencyGuard }
  | { type: "approve_trust"; dependency_fingerprint?: string }
  | { type: "approve_project_environment"; project_file_path?: string }
  | { type: "run_all_cells" }
  | { type: "run_all_cells_guarded"; observed_heads: string[] }
  | { type: "send_comm"; message: CommRequestMessage }
  | {
      type: "get_history";
      /** Glob-style pattern to match. null for no filter. */
      pattern: string | null;
      /** Maximum number of entries to return. */
      n: number;
      /** Deduplicate identical entries when true. */
      unique: boolean;
    }
  | { type: "complete"; code: string; cursor_pos: number }
  | {
      type: "save_notebook";
      /** Format code cells (ruff / deno fmt) before writing to disk. */
      format_cells: boolean;
      /** Target path. Omit to save in place. */
      path?: string;
    }
  | { type: "clone_as_ephemeral"; source_notebook_id: string }
  | { type: "get_doc_bytes" };

/** One entry returned by `get_history`. */
export interface HistoryEntry {
  session: number;
  line: number;
  source: string;
}

/** One item returned in a `completion_result`. */
export interface CompletionItem {
  label: string;
  kind?: string | null;
  detail?: string | null;
  source?: string | null;
}

/** Message shape for send_comm requests. */
export interface CommRequestMessage {
  header: Record<string, unknown>;
  parent_header?: Record<string, unknown> | null;
  metadata?: Record<string, unknown>;
  content: Record<string, unknown>;
  buffers: number[][];
  channel: string;
}

// ── Responses ───────────────────────────────────────────────────────

export type NotebookResponse =
  | {
      result: "kernel_launched";
      kernel_type: string;
      env_source: EnvSource;
      launched_config: LaunchedEnvConfig;
    }
  | {
      result: "kernel_already_running";
      kernel_type: string;
      env_source: EnvSource;
      launched_config: LaunchedEnvConfig;
    }
  | { result: "cell_queued"; cell_id: string; execution_id: string }
  | { result: "outputs_cleared"; cell_id: string }
  | { result: "interrupt_sent" }
  | { result: "kernel_shutting_down" }
  | { result: "no_kernel" }
  | { result: "guard_rejected"; reason: string }
  | {
      result: "all_cells_queued";
      queued: QueueEntry[];
    }
  | { result: "ok" }
  | { result: "error"; error: string }
  | { result: "sync_environment_complete"; synced_packages: string[] }
  | {
      result: "sync_environment_failed";
      error: string;
      needs_restart: boolean;
    }
  | { result: "history_result"; entries: HistoryEntry[] }
  | {
      result: "completion_result";
      items: CompletionItem[];
      cursor_start: number;
      cursor_end: number;
    }
  | { result: "notebook_saved"; path: string }
  | { result: "save_error"; error: SaveErrorKind }
  | {
      result: "notebook_cloned";
      notebook_id: string;
      working_dir?: string | null;
    }
  | { result: "doc_bytes"; bytes: number[] };

/**
 * Structured save failures returned in `NotebookResponse::SaveError`.
 * Mirrors `notebook_protocol::protocol::SaveErrorKind`.
 */
export type SaveErrorKind =
  | {
      type: "path_already_open";
      /** UUID of the room that currently holds this path. */
      uuid: string;
      /** The conflicting path. */
      path: string;
    }
  | { type: "io"; message: string };
