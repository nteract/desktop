//! Execution pipeline: submit cell → wait for broadcast → collect outputs.
//!
//! This module handles the async execution lifecycle for `execute_cell` and
//! tools that use `and_run`. It subscribes to daemon broadcasts to detect
//! execution completion, then collects outputs from the CRDT.

use std::time::{Duration, Instant};

use notebook_protocol::protocol::{NotebookBroadcast, NotebookRequest, NotebookResponse};
use notebook_sync::handle::DocHandle;
use notebook_sync::BroadcastReceiver;
use runtimed_client::output_resolver;
use runtimed_client::resolved_output::Output;
use tracing::warn;

/// Result of executing a cell.
pub struct ExecutionResult {
    /// The cell ID that was executed.
    pub cell_id: String,
    /// Resolved outputs from the cell after execution.
    pub outputs: Vec<Output>,
    /// Execution count (e.g., "5" for In[5]).
    pub execution_count: Option<String>,
    /// Final status: "idle", "error", "running" (if timed out).
    pub status: String,
    /// Whether the execution completed successfully.
    pub success: bool,
}

/// Execute a cell and wait for completion.
///
/// 1. Calls `confirm_sync()` to ensure the daemon has the latest cell source.
/// 2. Sends `ExecuteCell` request with a broadcast channel.
/// 3. Waits for `ExecutionDone` broadcast (or timeout).
/// 4. Collects and resolves outputs from the CRDT.
pub async fn execute_and_wait(
    handle: &DocHandle,
    broadcast_rx: &mut BroadcastReceiver,
    cell_id: &str,
    timeout: Duration,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<std::path::PathBuf>,
) -> ExecutionResult {
    // Step 1: Ensure daemon has our latest edits
    if let Err(e) = handle.confirm_sync().await {
        warn!("confirm_sync failed before execution: {e}");
    }

    // Step 2: Submit execution request. Broadcasts (ExecutionStarted,
    // ExecutionDone, Output) arrive on the session's broadcast_rx.
    let request = NotebookRequest::ExecuteCell {
        cell_id: cell_id.to_string(),
    };
    let response = handle.send_request(request).await;

    // Check if the request itself failed
    let execution_id = match response {
        Ok(NotebookResponse::CellQueued { execution_id, .. }) => Some(execution_id),
        Ok(_) => None,
        Err(_e) => {
            return ExecutionResult {
                cell_id: cell_id.to_string(),
                outputs: Vec::new(),
                execution_count: None,
                status: "error".to_string(),
                success: false,
            };
        }
    };

    // Step 3: Wait for ExecutionDone broadcast (or timeout).
    let mut final_status = "running".to_string();
    let mut success = false;
    let deadline = Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        match tokio::time::timeout(remaining, broadcast_rx.recv()).await {
            Ok(Some(broadcast)) => match &broadcast {
                NotebookBroadcast::ExecutionDone {
                    cell_id: done_cell_id,
                    ..
                } if done_cell_id == cell_id => {
                    // Check RuntimeState for the result
                    if let Ok(state) = handle.get_runtime_state() {
                        if let Some(eid) = &execution_id {
                            if let Some(entry) = state.executions.get(eid) {
                                final_status = entry.status.clone();
                                success = entry.success.unwrap_or(false);
                            }
                        }
                    }
                    // If we didn't get status from RuntimeState, infer from outputs
                    if final_status == "running" {
                        final_status = "idle".to_string();
                        success = true;
                    }
                    break;
                }
                _ => {
                    // Other broadcast — continue waiting
                }
            },
            Ok(None) => {
                // Broadcast stream ended — connection dropped
                final_status = "error".to_string();
                break;
            }
            Err(_) => {
                // Timeout
                break;
            }
        }
    }

    // Step 4: Collect outputs from CRDT
    let cell = handle.get_cell(cell_id);
    let execution_count = handle.get_cell_execution_count(cell_id);

    let outputs = if let Some(cell_snapshot) = &cell {
        output_resolver::resolve_cell_outputs(
            &cell_snapshot.outputs,
            blob_base_url,
            blob_store_path,
        )
        .await
    } else {
        Vec::new()
    };

    // Determine status from outputs if we didn't get it from RuntimeState
    if final_status == "idle" && outputs.iter().any(|o| o.output_type == "error") {
        final_status = "error".to_string();
        success = false;
    }

    ExecutionResult {
        cell_id: cell_id.to_string(),
        outputs,
        execution_count,
        status: final_status,
        success,
    }
}
