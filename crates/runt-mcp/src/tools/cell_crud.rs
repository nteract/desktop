//! Cell CRUD tools: create_cell, set_cell, delete_cell, move_cell, clear_outputs.

use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use notebook_protocol::protocol::NotebookRequest;

use crate::execution;
use crate::NteractMcp;

use super::{arg_bool, arg_str, arg_string_array, tool_error, tool_success};

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateCellParams {
    /// Cell source code or markdown content.
    #[serde(default)]
    pub source: Option<String>,
    /// Cell type: "code", "markdown", or "raw".
    #[serde(default)]
    pub cell_type: Option<String>,
    /// Position to insert (0-based index). None appends at end.
    #[serde(default)]
    pub index: Option<i64>,
    /// Execute the cell immediately after creation.
    #[serde(default)]
    pub and_run: Option<bool>,
    /// Max seconds to wait for execution.
    #[serde(default)]
    pub timeout_secs: Option<f64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetCellParams {
    /// The cell ID to update.
    pub cell_id: String,
    /// New source code (None to leave unchanged).
    #[serde(default)]
    pub source: Option<String>,
    /// New cell type (None to leave unchanged).
    #[serde(default)]
    pub cell_type: Option<String>,
    /// Execute the cell after changes (code cells only).
    #[serde(default)]
    pub and_run: Option<bool>,
    /// Max seconds to wait for execution.
    #[serde(default)]
    pub timeout_secs: Option<f64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct DeleteCellParams {
    /// The cell ID to delete.
    pub cell_id: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct MoveCellParams {
    /// The cell ID to move.
    pub cell_id: String,
    /// Move after this cell, or null for start.
    #[serde(default)]
    pub after_cell_id: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClearOutputsParams {
    /// Cell IDs to clear outputs for. If omitted or empty, clears outputs for ALL cells (destructive).
    pub cell_ids: Option<Vec<String>>,
}

/// Create a new cell, optionally executing it.
pub async fn create_cell(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let source = arg_str(request, "source").unwrap_or("");
    let cell_type = arg_str(request, "cell_type").unwrap_or("code");
    let index = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("index"))
        .and_then(|v| v.as_i64());
    let and_run = arg_bool(request, "and_run").unwrap_or(false);
    let timeout_secs = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("timeout_secs"))
        .and_then(|v| v.as_f64())
        .unwrap_or(30.0);

    // Clone handle, then drop the session lock so other tools
    // (interrupt_kernel, etc.) aren't blocked during execution.
    let (handle, cell_id) = {
        let handle = require_handle!(server);
        let cell_id = format!("cell-{}", uuid::Uuid::new_v4());

        // Determine after_cell_id based on index
        let after_cell_id = if let Some(idx) = index {
            let cell_ids = handle.get_cell_ids();
            if idx <= 0 {
                None // Insert at beginning
            } else {
                let idx = (idx as usize).min(cell_ids.len());
                if idx > 0 {
                    Some(cell_ids[idx - 1].clone())
                } else {
                    None
                }
            }
        } else {
            // Append at end
            handle.last_cell_id()
        };

        handle
            .add_cell_with_source(&cell_id, cell_type, after_cell_id.as_deref(), source)
            .map_err(|e| McpError::internal_error(format!("Failed to create cell: {e}"), None))?;

        (handle, cell_id)
    };

    // Sync so the daemon (and peers) know about the new cell before we send presence
    let _ = handle.confirm_sync().await;

    // Cursor at end of source (shows "finished typing")
    let peer_label = server.get_peer_label().await;
    let (end_line, end_col) = crate::presence::offset_to_line_col(source, source.len());
    crate::presence::emit_cursor(&handle, &cell_id, end_line, end_col, &peer_label).await;

    if and_run && cell_type == "code" {
        let result = execution::execute_and_wait(
            &handle,
            &cell_id,
            Duration::from_secs_f64(timeout_secs),
            &server.blob_base_url,
            &server.blob_store_path,
        )
        .await;

        return super::build_execution_result(&result, &handle, server).await;
    }

    tool_success(&format!("Created cell: {cell_id}"))
}

/// Update a cell's source and/or type.
pub async fn set_cell(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let cell_id = arg_str(request, "cell_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: cell_id", None))?;

    let source = arg_str(request, "source");
    let cell_type = arg_str(request, "cell_type");
    let and_run = arg_bool(request, "and_run").unwrap_or(false);
    let timeout_secs = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("timeout_secs"))
        .and_then(|v| v.as_f64())
        .unwrap_or(30.0);

    let handle = require_handle!(server);

    // Verify cell exists
    if handle.get_cell(cell_id).is_none() {
        return tool_error(&format!("Cell not found: {cell_id}"));
    }

    if source.is_none() && cell_type.is_none() {
        return tool_success(&format!(
            "Cell \"{cell_id}\" unchanged (no updates specified)"
        ));
    }

    if let Some(src) = source {
        handle
            .update_source(cell_id, src)
            .map_err(|e| McpError::internal_error(format!("Failed to update source: {e}"), None))?;

        // Sync so peers see the edit before the cursor
        let _ = handle.confirm_sync().await;

        // Cursor at end of new source
        let peer_label = server.get_peer_label().await;
        let (end_line, end_col) = crate::presence::offset_to_line_col(src, src.len());
        crate::presence::emit_cursor(&handle, cell_id, end_line, end_col, &peer_label).await;
    }
    if let Some(ct) = cell_type {
        handle
            .set_cell_type(cell_id, ct)
            .map_err(|e| McpError::internal_error(format!("Failed to set cell type: {e}"), None))?;
    }

    let current_type = handle.get_cell_type(cell_id).unwrap_or_default();
    if and_run && current_type == "code" {
        let result = execution::execute_and_wait(
            &handle,
            cell_id,
            Duration::from_secs_f64(timeout_secs),
            &server.blob_base_url,
            &server.blob_store_path,
        )
        .await;

        return super::build_execution_result(&result, &handle, server).await;
    }

    tool_success(&format!("Cell \"{cell_id}\" updated"))
}

/// Delete a cell by ID.
pub async fn delete_cell(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let cell_id = arg_str(request, "cell_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: cell_id", None))?;

    let handle = require_handle!(server);

    let peer_label = server.get_peer_label().await;
    crate::presence::emit_focus(&handle, cell_id, &peer_label).await;

    let deleted = handle
        .delete_cell(cell_id)
        .map_err(|e| McpError::internal_error(format!("Failed to delete cell: {e}"), None))?;

    if deleted {
        let result = serde_json::json!({ "cell_id": cell_id, "deleted": true });
        tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
    } else {
        tool_error(&format!("Cell not found: {cell_id}"))
    }
}

/// Move a cell to a new position.
pub async fn move_cell(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let cell_id = arg_str(request, "cell_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: cell_id", None))?;

    let handle = require_handle!(server);

    let after_cell_id = arg_str(request, "after_cell_id");

    handle
        .move_cell(cell_id, after_cell_id)
        .map_err(|e| McpError::internal_error(format!("Failed to move cell: {e}"), None))?;

    let peer_label = server.get_peer_label().await;
    crate::presence::emit_focus(&handle, cell_id, &peer_label).await;

    let result = serde_json::json!({
        "cell_id": cell_id,
        "after_cell_id": after_cell_id,
        "moved": true,
    });
    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}

/// Clear a cell's outputs.
pub async fn clear_outputs(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let explicit_ids: Option<Vec<String>> = arg_string_array(request, "cell_ids");

    let handle = require_handle!(server);

    // If cell_ids provided, clear those (empty list = no-op); otherwise clear all.
    let cell_ids: Vec<String> = match explicit_ids {
        Some(ids) => ids,
        None => handle.get_cell_ids(),
    };

    // Validate that all requested cell IDs exist in the notebook.
    if !cell_ids.is_empty() {
        let all_ids = handle.get_cell_ids();
        let unknown: Vec<&str> = cell_ids
            .iter()
            .filter(|id| !all_ids.contains(id))
            .map(|s| s.as_str())
            .collect();
        if !unknown.is_empty() {
            return tool_error(&format!("Unknown cell IDs: {}", unknown.join(", ")));
        }
    }

    let peer_label = server.get_peer_label().await;
    let mut cleared = Vec::new();
    let mut failed = Vec::new();

    for id in &cell_ids {
        crate::presence::emit_focus(&handle, id, &peer_label).await;

        match handle
            .send_request(NotebookRequest::ClearOutputs {
                cell_id: id.clone(),
            })
            .await
        {
            Ok(_) => cleared.push(id.as_str()),
            Err(e) => failed.push(format!("{id}: {e}")),
        }
    }

    if !failed.is_empty() {
        return tool_error(&format!(
            "Cleared {}/{} cells. Failures: {}",
            cleared.len(),
            cell_ids.len(),
            failed.join(", ")
        ));
    }

    let result = serde_json::json!({ "cleared": cleared.len() });
    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}
