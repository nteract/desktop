//! Execution pipeline: submit cell → poll RuntimeStateDoc → collect outputs.
//!
//! This module handles the async execution lifecycle for `execute_cell` and
//! tools that use `and_run`. It polls the RuntimeStateDoc (the daemon-owned
//! Automerge CRDT) for execution lifecycle state, using the CRDT as the
//! source of truth instead of relying on broadcast hints.

use std::time::{Duration, Instant};

use notebook_protocol::protocol::{NotebookRequest, NotebookResponse};
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
    /// Final status: "done", "error", "running" (if timed out).
    pub status: String,
    /// Whether the execution completed successfully.
    pub success: bool,
}

/// Execute a cell and wait for completion.
///
/// 1. Calls `confirm_sync()` to ensure the daemon has the latest cell source.
/// 2. Sends `ExecuteCell` request.
/// 3. Polls RuntimeStateDoc until the execution reaches terminal status.
/// 4. Collects and resolves outputs from the CRDT.
///
/// The daemon writes `set_execution_done` AFTER all outputs are written,
/// so once the synced execution status is `"done"` or `"error"`, outputs
/// are guaranteed to be present.
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
    let response = handle.send_request(request).await;

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

    // Step 3: Poll RuntimeStateDoc for terminal execution status.
    // The CRDT is the source of truth — no broadcast dependency.
    let mut final_status = "running".to_string();
    let mut success = false;
    let mut output_hashes: Vec<String> = Vec::new();
    let deadline = Instant::now() + timeout;

    if let Some(ref eid) = execution_id {
        // Phase 1: Wait for execution to reach terminal status.
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }

            if let Ok(state) = handle.get_runtime_state() {
                if let Some(exec) = state.executions.get(eid.as_str()) {
                    if exec.status == "done" || exec.status == "error" {
                        final_status = exec.status.clone();
                        success = exec.success.unwrap_or(false);
                        output_hashes = exec.outputs.clone();
                        break;
                    }
                }
            }

            // Yield to the sync task so it can process incoming
            // RuntimeStateDoc frames from the daemon.
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        // Phase 2: If status is terminal but outputs are empty, poll briefly
        // for output sync to catch up. The daemon writes outputs before
        // set_execution_done, but they may arrive in separate sync frames.
        // Cap at 500ms to avoid hanging on genuinely output-free executions.
        if (final_status == "done" || final_status == "error") && output_hashes.is_empty() {
            let output_deadline =
                Instant::now() + Duration::from_millis(500).min(deadline - Instant::now());
            while Instant::now() < output_deadline {
                if let Ok(state) = handle.get_runtime_state() {
                    if let Some(exec) = state.executions.get(eid.as_str()) {
                        if !exec.outputs.is_empty() {
                            output_hashes = exec.outputs.clone();
                            break;
                        }
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }

    // Step 4: Collect outputs from CRDT.
    // Prefer output hashes from RuntimeStateDoc (already synced above).
    // Fall back to handle.get_cell() which reads via execution_id facade.
    let execution_count = handle.get_cell_execution_count(cell_id);

    let outputs = if !output_hashes.is_empty() {
        output_resolver::resolve_cell_outputs(&output_hashes, blob_base_url, blob_store_path).await
    } else if let Some(cell_snapshot) = handle.get_cell(cell_id) {
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
