//! Execution pipeline: submit cell → poll RuntimeStateDoc → collect outputs.
//!
//! This module handles the async execution lifecycle for `execute_cell` and
//! tools that use `and_run`. It polls the RuntimeStateDoc (the daemon-owned
//! Automerge CRDT) for execution lifecycle state, using the CRDT as the
//! source of truth instead of relying on broadcast hints.

use std::collections::{HashMap, HashSet};
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
    let mut output_manifests: Vec<serde_json::Value> = Vec::new();
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
                        output_manifests = exec.outputs.clone();
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
        if (final_status == "done" || final_status == "error") && output_manifests.is_empty() {
            let output_deadline =
                Instant::now() + Duration::from_millis(500).min(deadline - Instant::now());
            while Instant::now() < output_deadline {
                if let Ok(state) = handle.get_runtime_state() {
                    if let Some(exec) = state.executions.get(eid.as_str()) {
                        if !exec.outputs.is_empty() {
                            output_manifests = exec.outputs.clone();
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

    // Get execution_count from RuntimeStateDoc (the source of truth).
    // The NotebookDoc cell's execution_count field is stale since execution
    // state was moved to RuntimeStateDoc.
    let execution_count = if let Some(ref eid) = execution_id {
        handle
            .get_runtime_state()
            .ok()
            .and_then(|state| {
                state
                    .executions
                    .get(eid.as_str())
                    .and_then(|e| e.execution_count)
            })
            .map(|c| c.to_string())
    } else {
        // Fallback: find most recent execution for this cell with an execution_count
        let ec = crate::tools::cell_read::get_cell_execution_count_from_runtime(handle, cell_id);
        if ec.is_empty() {
            None
        } else {
            Some(ec)
        }
    };

    let comms = handle.get_runtime_state().ok().map(|rs| rs.comms);
    let outputs = if !output_manifests.is_empty() {
        output_resolver::resolve_cell_outputs(
            &output_manifests,
            blob_base_url,
            blob_store_path,
            comms.as_ref(),
        )
        .await
    } else if let Some(cell_snapshot) = handle.get_cell(cell_id) {
        output_resolver::resolve_cell_outputs(
            &cell_snapshot.outputs,
            blob_base_url,
            blob_store_path,
            comms.as_ref(),
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

/// Result of running all cells.
pub struct RunAllResult {
    /// Whether the deadline was hit before all cells finished.
    pub timed_out: bool,
    /// Overall status: "completed", "error", or "timed_out".
    pub status: String,
    /// Map of cell_id → execution_id for this run's queued cells.
    /// Used to scope status lookups to this specific run.
    pub cell_execution_ids: HashMap<String, String>,
}

/// Run all cells and wait for completion.
///
/// 1. Calls `confirm_sync()` to ensure the daemon has the latest cell sources.
/// 2. Sends `RunAllCells` request.
/// 3. Polls RuntimeStateDoc until all queued execution IDs reach terminal status.
///
/// Returns a lightweight `RunAllResult` with overall status. The caller should
/// read the full notebook state after this returns to build the summary view.
pub async fn run_all_and_wait(handle: &DocHandle, timeout: Duration) -> RunAllResult {
    // Step 1: Ensure daemon has our latest edits
    if let Err(e) = handle.confirm_sync().await {
        warn!("confirm_sync failed before run_all_cells: {e}");
    }

    // Step 2: Submit run-all request
    let response = handle.send_request(NotebookRequest::RunAllCells {}).await;

    let cell_execution_ids: HashMap<String, String> = match response {
        Ok(NotebookResponse::AllCellsQueued { queued }) => queued
            .into_iter()
            .map(|q| (q.cell_id, q.execution_id))
            .collect(),
        _ => {
            return RunAllResult {
                timed_out: false,
                status: "error".to_string(),
                cell_execution_ids: HashMap::new(),
            };
        }
    };

    if cell_execution_ids.is_empty() {
        return RunAllResult {
            timed_out: false,
            status: "completed".to_string(),
            cell_execution_ids: HashMap::new(),
        };
    }

    let execution_ids: HashSet<&str> = cell_execution_ids.values().map(|s| s.as_str()).collect();

    // Step 3: Poll RuntimeStateDoc for all execution IDs to reach terminal status.
    let deadline = Instant::now() + timeout;
    let mut all_terminal = false;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }

        if let Ok(state) = handle.get_runtime_state() {
            all_terminal = execution_ids.iter().all(|eid| {
                state
                    .executions
                    .get(*eid)
                    .is_some_and(|exec| exec.status == "done" || exec.status == "error")
            });
            if all_terminal {
                break;
            }
        }

        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Step 4: Derive overall status
    let timed_out = !all_terminal;
    let has_error = handle.get_runtime_state().ok().is_some_and(|state| {
        execution_ids.iter().any(|eid| {
            state
                .executions
                .get(*eid)
                .is_some_and(|exec| exec.status == "error")
        })
    });

    let status = if timed_out {
        "timed_out"
    } else if has_error {
        "error"
    } else {
        "completed"
    }
    .to_string();

    RunAllResult {
        timed_out,
        status,
        cell_execution_ids,
    }
}
