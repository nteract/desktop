/**
 * TypeScript equivalents of NotebookRequest/NotebookResponse from
 * crates/notebook-protocol/src/protocol.rs.
 *
 * Only includes variants currently used by the frontend client.
 * These are transport-agnostic — callers encode them as needed.
 */

// ── Requests ────────────────────────────────────────────────────────

export type NotebookRequest =
  | {
      type: "launch_kernel";
      kernel_type: string;
      env_source: string;
      notebook_path?: string;
    }
  | { type: "execute_cell"; cell_id: string }
  | { type: "clear_outputs"; cell_id: string }
  | { type: "interrupt_execution" }
  | { type: "shutdown_kernel" }
  | { type: "sync_environment" }
  | { type: "run_all_cells" }
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
  | { type: "complete"; code: string; cursor_pos: number };

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
    }
  | { result: "history_result"; entries: HistoryEntry[] }
  | {
      result: "completion_result";
      items: CompletionItem[];
      cursor_start: number;
      cursor_end: number;
    };
