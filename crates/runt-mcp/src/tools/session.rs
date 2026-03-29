//! Session management tools: list, join, open notebooks.

use std::path::PathBuf;

use rmcp::model::{CallToolRequestParams, CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use runtimed_client::client::PoolClient;

use crate::session::NotebookSession;
use crate::NteractMcp;

use super::{arg_str, tool_error, tool_success};

#[allow(dead_code)] // Fields used by schemars for tool input schema generation
#[derive(Debug, Deserialize, JsonSchema)]
pub struct JoinNotebookParams {
    /// The notebook ID from list_active_notebooks.
    pub notebook_id: String,
}

#[allow(dead_code)] // Fields used by schemars for tool input schema generation
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
///
/// Accepts the notebook_id exactly as returned by list_active_notebooks.
/// Does NOT rewrite the ID — UUIDs, file paths, and opaque room names
/// are all passed through unchanged.
pub async fn join_notebook(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let notebook_id = arg_str(request, "notebook_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: notebook_id", None))?;

    // Pass the notebook_id through unchanged — it may be a UUID, file path,
    // or opaque room name. resolve_notebook_path would corrupt non-path IDs.
    let notebook_id = notebook_id.to_string();

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
///
/// Uses the OpenNotebook handshake so the daemon loads the .ipynb from disk,
/// creates a file-backed room, and returns the notebook_id.
pub async fn open_notebook(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let path = arg_str(request, "path")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: path", None))?;

    // Resolve to absolute path for the daemon
    let abs_path = if std::path::Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(path)
    };

    // Use connect_open which sends the OpenNotebook handshake —
    // the daemon loads the .ipynb from disk and creates a file-backed room.
    match notebook_sync::connect::connect_open(
        server.socket_path.clone(),
        abs_path.clone(),
        "runt-mcp",
    )
    .await
    {
        Ok(result) => {
            let cell_count = result.handle.get_cell_ids().len();
            let notebook_id = result.handle.notebook_id().to_string();
            let info = serde_json::json!({
                "notebook_id": notebook_id,
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
