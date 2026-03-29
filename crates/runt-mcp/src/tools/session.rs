//! Session management tools: list, join, open notebooks.

use rmcp::model::{CallToolRequestParams, CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use runtimed_client::client::PoolClient;
use runtimed_client::daemon_paths;

use crate::session::NotebookSession;
use crate::NteractMcp;

use super::{arg_str, tool_error, tool_success};

#[derive(Debug, Deserialize, JsonSchema)]
pub struct JoinNotebookParams {
    /// The notebook ID from list_active_notebooks.
    pub notebook_id: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenNotebookParams {
    /// Path to the notebook file on disk.
    pub path: String,
}

/// List all active notebook sessions.
pub async fn list_active_notebooks(server: &NteractMcp) -> Result<CallToolResult, McpError> {
    let client = PoolClient::new(server.socket_path.clone());
    match client.list_rooms().await {
        Ok(rooms) => {
            let json = serde_json::to_string_pretty(&rooms).unwrap_or_else(|_| "[]".to_string());
            tool_success(&json)
        }
        Err(e) => tool_error(&format!(
            "Failed to list notebooks. Is the daemon running? Error: {}",
            e
        )),
    }
}

/// Join an existing notebook session.
pub async fn join_notebook(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let notebook_id = arg_str(request, "notebook_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: notebook_id", None))?;

    let notebook_id = daemon_paths::resolve_notebook_path(notebook_id);

    match notebook_sync::connect::connect(
        server.socket_path.clone(),
        notebook_id.clone(),
        "runt-mcp",
    )
    .await
    {
        Ok(result) => {
            let info = format!(
                "Joined notebook: {} ({} cells)",
                result.handle.notebook_id(),
                result.handle.get_cell_ids().len()
            );

            let session = NotebookSession {
                handle: result.handle,
                notebook_id,
            };
            *server.session.write().await = Some(session);

            tool_success(&info)
        }
        Err(e) => tool_error(&format!("Failed to join notebook: {}", e)),
    }
}

/// Open a notebook file from disk.
pub async fn open_notebook(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let path = arg_str(request, "path")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: path", None))?;

    let notebook_id = daemon_paths::resolve_notebook_path(path);

    match notebook_sync::connect::connect(
        server.socket_path.clone(),
        notebook_id.clone(),
        "runt-mcp",
    )
    .await
    {
        Ok(result) => {
            let cell_count = result.handle.get_cell_ids().len();
            let info = serde_json::json!({
                "notebook_id": result.handle.notebook_id(),
                "cell_count": cell_count,
                "status": "connected"
            });

            let session = NotebookSession {
                handle: result.handle,
                notebook_id,
            };
            *server.session.write().await = Some(session);

            Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&info).unwrap_or_default(),
            )]))
        }
        Err(e) => tool_error(&format!("Failed to open notebook '{}': {}", path, e)),
    }
}
