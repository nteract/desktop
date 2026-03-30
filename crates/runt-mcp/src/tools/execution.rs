//! Execution tools: execute_cell, run_all_cells.

use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use notebook_protocol::protocol::NotebookRequest;

use crate::execution;
use crate::formatting;
use crate::structured;
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
pub struct RunAllCellsParams {}

/// Execute a cell and return results (with structured content for MCP Apps).
pub async fn execute_cell(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let cell_id = arg_str(request, "cell_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: cell_id", None))?;

    let session = server.session.read().await;
    let session = match session.as_ref() {
        Some(s) => s,
        None => {
            return tool_error(
                "No active notebook session. Call join_notebook or open_notebook first.",
            )
        }
    };

    let timeout_secs = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("timeout_secs"))
        .and_then(|v| v.as_f64())
        .unwrap_or(30.0);

    let handle = &session.handle;

    // Verify cell exists
    if handle.get_cell(cell_id).is_none() {
        return tool_error(&format!("Cell not found: {cell_id}"));
    }

    crate::presence::emit_focus(handle, cell_id).await;

    let result = execution::execute_and_wait(
        handle,
        cell_id,
        Duration::from_secs_f64(timeout_secs),
        &server.blob_base_url,
        &server.blob_store_path,
    )
    .await;

    // Format text content
    let header = formatting::format_cell_header(
        &result.cell_id,
        "code",
        result.execution_count.as_deref(),
        Some(&result.status),
    );
    let output_text = formatting::format_outputs_text(&result.outputs);
    let text = if !output_text.is_empty() {
        format!("{header}\n\n{output_text}")
    } else {
        header
    };

    let items = vec![Content::text(text)];

    // Build structured content for MCP Apps widget using the protocol's
    // structured_content field instead of a text-based fallback.
    let cell_snapshot = handle.get_cell(&result.cell_id);
    let structured_content = if let Some(snap) = cell_snapshot {
        let outputs = runtimed_client::output_resolver::resolve_cell_outputs(
            &snap.outputs,
            &server.blob_base_url,
            &server.blob_store_path,
        )
        .await;
        let resolved = runtimed_client::resolved_output::ResolvedCell {
            id: snap.id,
            cell_type: snap.cell_type,
            position: snap.position,
            source: snap.source,
            execution_count: snap.execution_count.parse().ok(),
            outputs,
            metadata_json: serde_json::to_string(&snap.metadata).unwrap_or_default(),
        };
        Some(structured::cell_structured_content(
            &resolved,
            &result.status,
        ))
    } else {
        None
    };

    let mut call_result = CallToolResult::success(items);
    call_result.structured_content = structured_content;
    Ok(call_result)
}

/// Queue all code cells for execution.
pub async fn run_all_cells(
    server: &NteractMcp,
    _request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let session = server.session.read().await;
    let session = match session.as_ref() {
        Some(s) => s,
        None => {
            return tool_error(
                "No active notebook session. Call join_notebook or open_notebook first.",
            )
        }
    };

    let handle = &session.handle;

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
