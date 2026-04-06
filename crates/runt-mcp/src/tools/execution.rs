//! Execution tools: execute_cell, run_all_cells.

use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::execution;
use crate::formatting;
use crate::NteractMcp;

use super::cell_read::{build_cell_execution_count_map, build_cell_status_map};
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
    let cells = handle.get_cells();
    let cell_status_map = build_cell_status_map(&handle);
    let cell_ec_map = build_cell_execution_count_map(&handle);

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
        let status = cell_status_map.get(&cell.id).map(String::as_str);
        let ec = cell_ec_map.get(&cell.id);
        match status {
            Some("done") => succeeded += 1,
            Some("error") => {
                // Cancelled = error status but never actually ran (no execution_count)
                if ec.is_none() {
                    cancelled += 1;
                } else {
                    errored += 1;
                }
            }
            Some("running") => running += 1,
            Some("queued") => queued += 1,
            _ => {}
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
    let mut lines = vec![header, String::new()];
    for (i, cell) in cells.iter().enumerate() {
        let ec = cell_ec_map.get(&cell.id).map(String::as_str);

        // Remap "error" with no execution_count to "cancelled" for display.
        let raw_status = cell_status_map.get(&cell.id).map(String::as_str);
        let display_status = match raw_status {
            Some("error") if ec.is_none() => Some("cancelled"),
            other => other,
        };

        let line = formatting::format_cell_summary(
            i,
            &cell.id,
            &cell.cell_type,
            &cell.source,
            ec,
            display_status,
            60,
        );
        lines.push(line);
    }

    tool_success(&lines.join("\n"))
}
