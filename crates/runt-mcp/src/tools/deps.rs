//! Dependency management tools: add_dependency, remove_dependency, get_dependencies, sync_environment.

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use notebook_protocol::protocol::{NotebookRequest, NotebookResponse};

use crate::NteractMcp;

use super::{arg_str, tool_error, tool_success};

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddDependencyParams {
    /// Package to add (e.g. "pandas>=2.0").
    pub package: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveDependencyParams {
    /// Package to remove.
    pub package: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetDependenciesParams {}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SyncEnvironmentParams {}

/// Add a package dependency.
pub async fn add_dependency(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let package = arg_str(request, "package")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: package", None))?;

    let session = server.session.read().await;
    let session = match session.as_ref() {
        Some(s) => s,
        None => {
            return tool_error(
                "No active notebook session. Call join_notebook or open_notebook first.",
            )
        }
    };

    session
        .handle
        .add_uv_dependency(package)
        .map_err(|e| McpError::internal_error(format!("Failed to add dependency: {e}"), None))?;

    // Read back current dependencies
    let deps = get_deps_list(&session.handle);

    let result = serde_json::json!({
        "dependencies": deps,
        "added": package,
    });
    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}

/// Remove a package dependency.
pub async fn remove_dependency(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let package = arg_str(request, "package")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: package", None))?;

    let session = server.session.read().await;
    let session = match session.as_ref() {
        Some(s) => s,
        None => {
            return tool_error(
                "No active notebook session. Call join_notebook or open_notebook first.",
            )
        }
    };

    session
        .handle
        .remove_uv_dependency(package)
        .map_err(|e| McpError::internal_error(format!("Failed to remove dependency: {e}"), None))?;

    let deps = get_deps_list(&session.handle);

    let result = serde_json::json!({
        "dependencies": deps,
        "removed": package,
    });
    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}

/// Get the notebook's current package dependencies.
pub async fn get_dependencies(
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

    let deps = get_deps_list(&session.handle);

    let result = serde_json::json!({ "dependencies": deps });
    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}

/// Hot-install new dependencies without restarting.
pub async fn sync_environment(
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

    // Ensure daemon has latest metadata
    if let Err(e) = handle.confirm_sync().await {
        tracing::warn!("confirm_sync failed before sync_environment: {e}");
    }

    match handle
        .send_request(NotebookRequest::SyncEnvironment {})
        .await
    {
        Ok(NotebookResponse::SyncEnvironmentComplete {
            synced_packages, ..
        }) => {
            let result = serde_json::json!({
                "success": true,
                "synced_packages": synced_packages,
            });
            tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
        }
        Ok(NotebookResponse::SyncEnvironmentStarted { packages }) => {
            // Sync started but hasn't completed yet — return started status
            let result = serde_json::json!({
                "success": true,
                "synced_packages": packages,
            });
            tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
        }
        Ok(NotebookResponse::SyncEnvironmentFailed {
            error,
            needs_restart,
        }) => {
            let result = serde_json::json!({
                "success": false,
                "error": error,
                "needs_restart": needs_restart,
            });
            tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
        }
        Ok(NotebookResponse::Error { error }) => {
            let result = serde_json::json!({
                "success": false,
                "error": error,
                "needs_restart": true,
            });
            tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
        }
        Ok(_) => tool_success(&serde_json::json!({ "success": true }).to_string()),
        Err(e) => tool_error(&format!("Failed to sync environment: {e}")),
    }
}

/// Read UV dependencies from notebook metadata.
fn get_deps_list(handle: &notebook_sync::handle::DocHandle) -> Vec<String> {
    handle
        .get_notebook_metadata()
        .map(|m| m.uv_dependencies().to_vec())
        .unwrap_or_default()
}
