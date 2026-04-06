//! Execution tools: execute_cell, run_all_cells.

use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::execution;
use crate::formatting;
use crate::NteractMcp;

use super::{arg_str, tool_error, tool_success};

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ExecuteCellParams {
    /// The cell ID to execute.
    pub cell_id: String,
    /// Max seconds to wait; returns partial results if exceeded.
    #[serde(default)]
    pub timeout_secs: Option<f64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunAllCellsParams {
    /// Max seconds to wait for all cells to finish. Default: 300.
    #[serde(default)]
    pub timeout_secs: Option<f64>,
}

/// Execute a cell and return results (with structured content for MCP Apps).
pub async fn execute_cell(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let cell_id = arg_str(request, "cell_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: cell_id", None))?;

    let handle = require_handle!(server);

    let timeout_secs = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("timeout_secs"))
        .and_then(|v| v.as_f64())
        .unwrap_or(30.0);

    // Verify cell exists
    if handle.get_cell(cell_id).is_none() {
        return tool_error(&format!("Cell not found: {cell_id}"));
    }

    let peer_label = server.get_peer_label().await;
    crate::presence::emit_focus(&handle, cell_id, &peer_label).await;

    let result = execution::execute_and_wait(
        &handle,
        cell_id,
        Duration::from_secs_f64(timeout_secs),
        &server.blob_base_url,
        &server.blob_store_path,
    )
    .await;

    super::build_execution_result(&result, &handle, server).await
}

/// Execute all code cells in order and wait for completion.
pub async fn run_all_cells(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let handle = require_handle!(server);

    let timeout_secs = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("timeout_secs"))
        .and_then(|v| v.as_f64())
        .unwrap_or(300.0);

    let result = execution::run_all_and_wait(&handle, Duration::from_secs_f64(timeout_secs)).await;

    // Build the notebook summary view (same format as get_all_cells summary).
    // Status and execution_count are scoped to THIS run's execution IDs to
    // avoid mixing in historical state from prior runs.
    let cells = handle.get_cells();
    let runtime_state = handle.get_runtime_state().ok();

    // Look up this run's execution state for a given cell.
    let run_exec = |cell_id: &str| -> Option<&notebook_doc::runtime_state::ExecutionState> {
        let eid = result.cell_execution_ids.get(cell_id)?;
        runtime_state.as_ref()?.executions.get(eid.as_str())
    };

    // Count code cells by status for the header.
    let mut succeeded = 0usize;
    let mut errored = 0usize;
    let mut cancelled = 0usize;
    let mut running = 0usize;
    let mut queued = 0usize;

    for cell in &cells {
        if cell.cell_type != "code" {
            continue;
        }
        if let Some(exec) = run_exec(&cell.id) {
            match exec.status.as_str() {
                "done" => succeeded += 1,
                "error" => {
                    // Cancelled = error status but never actually ran (no execution_count)
                    if exec.execution_count.is_none() {
                        cancelled += 1;
                    } else {
                        errored += 1;
                    }
                }
                "running" => running += 1,
                "queued" => queued += 1,
                _ => {}
            }
        }
    }

    // Build status header line.
    let header = match result.status.as_str() {
        "timed_out" => {
            let done = succeeded + errored;
            let total = done + cancelled + running + queued;
            let mut parts = vec![format!("{done} completed")];
            if running > 0 {
                parts.push(format!("{running} running"));
            }
            if queued > 0 {
                parts.push(format!("{queued} queued"));
            }
            format!("Execution timed out ({total} cells: {})", parts.join(", "))
        }
        "error" => {
            let mut parts = Vec::new();
            if succeeded > 0 {
                parts.push(format!("{succeeded} succeeded"));
            }
            if errored > 0 {
                parts.push(format!("{errored} errored"));
            }
            if cancelled > 0 {
                parts.push(format!("{cancelled} cancelled"));
            }
            format!("Execution error ({})", parts.join(", "))
        }
        _ => {
            format!("Execution completed ({succeeded} succeeded)")
        }
    };

    // Format each cell using the standard summary format.
    // For code cells in this run, use the run-scoped execution state.
    // For non-code cells or cells not in this run, show no status.
    let mut lines = vec![header, String::new()];
    for (i, cell) in cells.iter().enumerate() {
        let (display_status, ec_str);

        if let Some(exec) = run_exec(&cell.id) {
            // Remap "error" with no execution_count to "cancelled" for display.
            display_status = Some(match exec.status.as_str() {
                "error" if exec.execution_count.is_none() => "cancelled",
                other => other,
            });
            ec_str = exec.execution_count.map(|c| c.to_string());
        } else {
            display_status = None;
            ec_str = None;
        }

        let line = formatting::format_cell_summary(
            i,
            &cell.id,
            &cell.cell_type,
            &cell.source,
            ec_str.as_deref(),
            display_status,
            60,
        );
        lines.push(line);
    }

    tool_success(&lines.join("\n"))
}
