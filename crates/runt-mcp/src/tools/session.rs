//! Session management tools: list, join, open notebooks.

use std::path::PathBuf;

use rmcp::model::{CallToolRequestParams, CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use runtimed_client::client::PoolClient;

use crate::session::NotebookSession;
use crate::NteractMcp;

use notebook_protocol::protocol::{NotebookRequest, NotebookResponse};

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

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateNotebookParams {
    /// Runtime type: "python" or "deno".
    #[serde(default)]
    pub runtime: Option<String>,
    /// Working directory for the kernel.
    #[serde(default)]
    pub working_dir: Option<String>,
    /// Python packages to pre-install.
    #[serde(default)]
    pub dependencies: Option<Vec<String>>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SaveNotebookParams {
    /// Path to save the notebook to. If None, saves to original location.
    #[serde(default)]
    pub path: Option<String>,
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
        std::env::current_dir().unwrap_or_default().join(path)
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

/// Create a new notebook with optional dependencies.
pub async fn create_notebook(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let runtime = arg_str(request, "runtime").unwrap_or("python");
    let working_dir = arg_str(request, "working_dir").map(std::path::PathBuf::from);

    match notebook_sync::connect::connect_create(
        server.socket_path.clone(),
        runtime,
        working_dir,
        "runt-mcp",
    )
    .await
    {
        Ok(result) => {
            let notebook_id = result.handle.notebook_id().to_string();
            // Add dependencies if specified
            let deps: Vec<String> = request
                .arguments
                .as_ref()
                .and_then(|a| a.get("dependencies"))
                .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                .unwrap_or_default();

            if runtime == "python" {
                for dep in &deps {
                    let _ = result.handle.add_uv_dependency(dep);
                }
            }

            let session = NotebookSession {
                handle: result.handle,
                notebook_id: notebook_id.clone(),
            };
            *server.session.write().await = Some(session);

            // If dependencies were added, restart kernel to pick them up
            if !deps.is_empty() && runtime == "python" {
                let session = server.session.read().await;
                if let Some(s) = session.as_ref() {
                    // Shutdown and relaunch
                    let _ = s
                        .handle
                        .send_request(NotebookRequest::ShutdownKernel {})
                        .await;
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    let _ = s
                        .handle
                        .send_request(NotebookRequest::LaunchKernel {
                            kernel_type: runtime.to_string(),
                            env_source: "uv:inline".to_string(),
                            notebook_path: None,
                        })
                        .await;
                }
            }

            let info = serde_json::json!({
                "notebook_id": notebook_id,
                "runtime": { "language": runtime },
                "dependencies": deps,
            });

            Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&info).unwrap_or_default(),
            )]))
        }
        Err(e) => tool_error(&format!("Failed to create notebook: {}", e)),
    }
}

/// Save notebook to disk.
pub async fn save_notebook(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let path = arg_str(request, "path").map(|s| s.to_string());

    // Clone the handle and notebook_id so we can drop the read guard
    let (handle, notebook_id) = {
        let session = server.session.read().await;
        match session.as_ref() {
            Some(s) => (s.handle.clone(), s.notebook_id.clone()),
            None => {
                return tool_error(
                    "No active notebook session. Call join_notebook or open_notebook first.",
                )
            }
        }
    };

    // Ensure daemon has latest
    if let Err(e) = handle.confirm_sync().await {
        tracing::warn!("confirm_sync failed before save: {e}");
    }

    match handle
        .send_request(NotebookRequest::SaveNotebook {
            format_cells: false,
            path: path.clone(),
        })
        .await
    {
        Ok(NotebookResponse::NotebookSaved {
            path: saved_path,
            new_notebook_id,
        }) => {
            let mut result = serde_json::json!({
                "path": saved_path,
                "notebook_id": new_notebook_id.as_deref().unwrap_or(&notebook_id),
            });

            // If room was re-keyed, update our session
            if let Some(new_id) = &new_notebook_id {
                let mut write = server.session.write().await;
                if let Some(ref mut s) = *write {
                    let old_id = s.notebook_id.clone();
                    s.notebook_id = new_id.clone();
                    result["previous_notebook_id"] = serde_json::json!(old_id);
                }
            }

            Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&result).unwrap_or_default(),
            )]))
        }
        Ok(NotebookResponse::Error { error }) => {
            if path.is_none() && (error.contains("Read-only") || error.contains("Failed to write"))
            {
                tool_error(
                    "No path specified. For notebooks created with create_notebook(), \
                     you must provide a path (e.g., save_notebook(path='/path/to/file.ipynb'))",
                )
            } else {
                tool_error(&format!("Failed to save notebook: {error}"))
            }
        }
        Ok(resp) => tool_error(&format!("Unexpected response: {resp:?}")),
        Err(e) => tool_error(&format!("Failed to save notebook: {e}")),
    }
}
