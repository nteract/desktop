//! Execution tools: execute_cell, run_all_cells.

use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use notebook_protocol::protocol::NotebookRequest;

use crate::execution;
use crate::NteractMcp;

use super::{arg_str, require_handle, tool_error, tool_success};

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
pub struct RunAllCellsParams {}

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

/// Queue all code cells for execution.
pub async fn run_all_cells(
    server: &NteractMcp,
    _request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let handle = require_handle!(server);

    // Ensure daemon has latest source
    if let Err(e) = handle.confirm_sync().await {
        tracing::warn!("confirm_sync failed before run_all_cells: {e}");
    }

    match handle.send_request(NotebookRequest::RunAllCells {}).await {
        Ok(_) => {
            let count = handle
                .get_cells()
                .iter()
                .filter(|c| c.cell_type == "code")
                .count();
            let result = serde_json::json!({ "status": "queued", "count": count });
            tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
        }
        Err(e) => tool_error(&format!("Failed to run all cells: {e}")),
    }
}
