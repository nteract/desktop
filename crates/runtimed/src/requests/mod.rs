//! Per-request handler modules for the notebook sync server.
//!
//! `handle_notebook_request` dispatches to these handlers
//! based on the `NotebookRequest` variant. Each module owns one variant's logic
//! so the dispatcher stays a thin match and each handler can be read in
//! isolation.
//!
//! Handlers accept references to the per-room state (`NotebookRoom`) and shared
//! daemon state (`Arc<Daemon>`) as parameters. They return `NotebookResponse`.
//! Shared helpers used by multiple handlers live in `helpers.rs`.
//!
//! This is a behavior-preserving split of the old 2k-line match statement —
//! lock scoping, log lines, error strings, and response variants are untouched.

use std::sync::Arc;

use tracing::debug;

use crate::daemon::Daemon;
use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::{NotebookRequest, NotebookResponse};

pub(crate) mod approve_project_environment;
pub(crate) mod approve_trust;
pub(crate) mod clear_outputs;
pub(crate) mod clone_notebook;
pub(crate) mod complete;
pub(crate) mod execute_cell;
pub(crate) mod get_doc_bytes;
pub(crate) mod get_history;
pub(crate) mod guarded;
pub(crate) mod interrupt_execution;
pub(crate) mod launch_kernel;
pub(crate) mod run_all_cells;
pub(crate) mod save_notebook;
pub(crate) mod send_comm;
pub(crate) mod shutdown_kernel;
pub(crate) mod sync_environment;

/// Short label for a request variant, used in telemetry logs.
///
/// Returns a static string — no allocation — suitable for structured logging
/// fields.  Intentionally avoids `Debug` formatting because some variants
/// carry large payloads (doc bytes, comm messages) that would bloat log lines.
pub(crate) fn request_label(req: &NotebookRequest) -> &'static str {
    match req {
        NotebookRequest::LaunchKernel { .. } => "LaunchKernel",
        NotebookRequest::ExecuteCell { .. } => "ExecuteCell",
        NotebookRequest::ExecuteCellGuarded { .. } => "ExecuteCellGuarded",
        NotebookRequest::ClearOutputs { .. } => "ClearOutputs",
        NotebookRequest::InterruptExecution { .. } => "InterruptExecution",
        NotebookRequest::ShutdownKernel { .. } => "ShutdownKernel",
        NotebookRequest::RunAllCells { .. } => "RunAllCells",
        NotebookRequest::RunAllCellsGuarded { .. } => "RunAllCellsGuarded",
        NotebookRequest::SendComm { .. } => "SendComm",
        NotebookRequest::GetHistory { .. } => "GetHistory",
        NotebookRequest::Complete { .. } => "Complete",
        NotebookRequest::SaveNotebook { .. } => "SaveNotebook",
        NotebookRequest::CloneAsEphemeral { .. } => "CloneAsEphemeral",
        NotebookRequest::SyncEnvironment { .. } => "SyncEnvironment",
        NotebookRequest::ApproveTrust { .. } => "ApproveTrust",
        NotebookRequest::ApproveProjectEnvironment { .. } => "ApproveProjectEnvironment",
        NotebookRequest::GetDocBytes { .. } => "GetDocBytes",
    }
}

/// Handle a NotebookRequest and return a NotebookResponse.
pub(crate) async fn handle_notebook_request(
    room: &Arc<NotebookRoom>,
    request: NotebookRequest,
    daemon: Arc<Daemon>,
) -> NotebookResponse {
    debug!(
        "[notebook-sync] Handling request: {}",
        request_label(&request)
    );

    match request {
        NotebookRequest::LaunchKernel {
            kernel_type,
            env_source,
            notebook_path,
        } => launch_kernel::handle(room, &daemon, kernel_type, env_source, notebook_path).await,

        NotebookRequest::ExecuteCell { cell_id } => execute_cell::handle(room, cell_id).await,

        NotebookRequest::ExecuteCellGuarded {
            cell_id,
            observed_heads,
        } => execute_cell::handle_guarded(room, cell_id, observed_heads).await,

        NotebookRequest::ClearOutputs { cell_id } => clear_outputs::handle(room, cell_id).await,

        NotebookRequest::InterruptExecution {} => interrupt_execution::handle(room).await,

        NotebookRequest::ShutdownKernel {} => shutdown_kernel::handle(room).await,

        NotebookRequest::RunAllCells {} => run_all_cells::handle(room).await,

        NotebookRequest::RunAllCellsGuarded { observed_heads } => {
            run_all_cells::handle_guarded(room, observed_heads).await
        }

        NotebookRequest::SendComm { message } => send_comm::handle(room, message).await,

        NotebookRequest::GetHistory { pattern, n, unique } => {
            get_history::handle(room, pattern, n, unique).await
        }

        NotebookRequest::Complete { code, cursor_pos } => {
            complete::handle(room, code, cursor_pos).await
        }

        NotebookRequest::SaveNotebook { format_cells, path } => {
            save_notebook::handle(room, &daemon, format_cells, path).await
        }

        NotebookRequest::CloneAsEphemeral { source_notebook_id } => {
            clone_notebook::handle(&daemon, source_notebook_id).await
        }

        NotebookRequest::SyncEnvironment { guard } => sync_environment::handle(room, guard).await,

        NotebookRequest::ApproveTrust {
            dependency_fingerprint,
        } => approve_trust::handle(room, dependency_fingerprint).await,

        NotebookRequest::ApproveProjectEnvironment { project_file_path } => {
            approve_project_environment::handle(room, project_file_path).await
        }

        NotebookRequest::GetDocBytes {} => get_doc_bytes::handle(room).await,
    }
}
