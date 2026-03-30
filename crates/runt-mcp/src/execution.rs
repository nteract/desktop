//! Execution pipeline: submit cell → poll RuntimeState → collect outputs.
//!
//! This module handles the async execution lifecycle for `execute_cell` and
//! tools that use `and_run`. It polls the daemon's RuntimeStateDoc to track
//! execution status and collects outputs from the CRDT once complete.

use std::time::{Duration, Instant};

use notebook_protocol::protocol::NotebookRequest;
use notebook_sync::handle::DocHandle;
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
/// 2. Sends `ExecuteCell` request to the daemon.
/// 3. Polls `RuntimeStateDoc` until the execution completes or times out.
/// 4. Collects and resolves outputs from the CRDT.
pub async fn execute_and_wait(
    handle: &DocHandle,
    cell_id: &str,
    timeout: Duration,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<std::path::PathBuf>,
) -> ExecutionResult {
    // Step 1: Ensure daemon has our latest edits
    if let Err(e) = handle.confirm_sync().await {
        warn!("confirm_sync failed before execution: {e}");
    }

    // Step 2: Submit execution request
    let request = NotebookRequest::ExecuteCell {
        cell_id: cell_id.to_string(),
    };
    if let Err(_e) = handle.send_request(request).await {
        return ExecutionResult {
            cell_id: cell_id.to_string(),
            outputs: Vec::new(),
            execution_count: None,
            status: "error".to_string(),
            success: false,
        };
    }

    // Step 3: Poll RuntimeStateDoc for completion.
    //
    // We track whether we've ever seen the cell in the queue. The
    // RuntimeStateDoc can lag behind the ExecuteCell request, so the cell
    // may not appear in the queue on the first poll. Without this guard
    // we'd immediately declare "done" before execution even starts.
    let start = Instant::now();
    let poll_interval = Duration::from_millis(100);
    let mut final_status = "running".to_string();
    let mut success = false;
    let mut seen_in_queue = false;

    loop {
        if start.elapsed() >= timeout {
            // Timeout — return partial results
            break;
        }

        tokio::time::sleep(poll_interval).await;

        // Read execution state from RuntimeStateDoc
        if let Ok(state) = handle.get_runtime_state() {
            let is_executing = state
                .queue
                .executing
                .as_ref()
                .is_some_and(|e| e.cell_id == cell_id);
            let is_queued = state.queue.queued.iter().any(|e| e.cell_id == cell_id);

            if is_executing || is_queued {
                seen_in_queue = true;
            }

            if seen_in_queue && !is_executing && !is_queued {
                // Was in queue, now done — check executions map for the result
                let exec_entry = state.executions.values().find(|e| e.cell_id == cell_id);

                if let Some(entry) = exec_entry {
                    final_status = entry.status.clone();
                    success = entry.success.unwrap_or(false);
                } else {
                    // No execution entry — might have completed before we polled
                    final_status = "idle".to_string();
                    success = true;
                }
                break;
            }
        }
    }

    // Step 4: Collect outputs from CRDT
    let cell = handle.get_cell(cell_id);
    let execution_count = handle.get_cell_execution_count(cell_id);

    let outputs = if let Some(cell_snapshot) = &cell {
        // Resolve outputs (raw JSON strings from CRDT → resolved Output structs)
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
