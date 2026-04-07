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
    /// Action after adding: "none" (just record, default), "sync" (hot-install, UV only),
    /// or "restart" (restart kernel with new deps).
    #[serde(default)]
    pub after: Option<String>,
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

/// Detect the active package manager for a notebook from its metadata or env_source.
/// Each notebook has exactly one env manager type.
///
/// Priority: metadata section existence (authoritative) → env_source (runtime) → default.
/// Metadata wins because the user explicitly chose the package manager (via
/// `create_notebook(package_manager=...)` or the UI), while env_source reflects
/// what the daemon happened to auto-launch with (which may be the system default,
/// not the notebook's intent).
pub(crate) fn detect_package_manager(handle: &notebook_sync::handle::DocHandle) -> String {
    // Priority 1: metadata declares which package manager section exists.
    // Check section existence, not just non-empty deps — an empty pixi section
    // means "this is a pixi notebook with no deps yet".
    if let Some(meta) = handle.get_notebook_metadata() {
        if meta.runt.pixi.is_some() {
            return "pixi".to_string();
        }
        if meta.runt.conda.is_some() {
            return "conda".to_string();
        }
        if meta.runt.uv.is_some() {
            return "uv".to_string();
        }
    }
    // Priority 2: env_source from running kernel (fallback for notebooks
    // with no runt metadata yet)
    if let Ok(state) = handle.get_runtime_state() {
        let src = &state.kernel.env_source;
        if src.starts_with("conda:") {
            return "conda".to_string();
        }
        if src.starts_with("pixi:") {
            return "pixi".to_string();
        }
        if src.starts_with("uv:") {
            return "uv".to_string();
        }
    }
    // Default
    "uv".to_string()
}

/// Ensure the notebook metadata has the correct package manager section.
///
/// The daemon creates metadata based on `default_python_env`, which may
/// differ from the MCP client's requested `package_manager`. This function
/// reads the current metadata, and if the requested manager's section is
/// missing, creates it (and clears competing sections so
/// `detect_package_manager` picks the right one).
/// Returns `true` if the metadata was changed.
pub(crate) fn ensure_package_manager_metadata(
    handle: &notebook_sync::handle::DocHandle,
    manager: &str,
) -> bool {
    let current = handle.get_notebook_metadata();

    // Check if the metadata already has the right exclusive section.
    // needs_fix is true when: (a) the requested section is missing, OR
    // (b) competing sections exist (e.g. pixi requested but uv section present).
    let needs_fix = match manager {
        "pixi" => current
            .as_ref()
            .is_none_or(|m| m.runt.pixi.is_none() || m.runt.uv.is_some() || m.runt.conda.is_some()),
        "conda" => current
            .as_ref()
            .is_none_or(|m| m.runt.conda.is_none() || m.runt.uv.is_some() || m.runt.pixi.is_some()),
        "uv" => current.as_ref().is_some_and(|m| {
            m.runt.uv.is_none() || m.runt.pixi.is_some() || m.runt.conda.is_some()
        }),
        _ => return false, // Unknown manager — no-op
    };

    if !needs_fix {
        return false;
    }

    // Update the metadata snapshot to have exactly one package manager section.
    // Clear competing sections so detect_package_manager picks the right one.
    let mut snapshot = current.unwrap_or_default();
    match manager {
        "pixi" => {
            if snapshot.runt.pixi.is_none() {
                snapshot.runt.pixi = Some(notebook_doc::metadata::PixiInlineMetadata {
                    dependencies: Vec::new(),
                    pypi_dependencies: Vec::new(),
                    channels: vec!["conda-forge".to_string()],
                    python: None,
                });
            }
            snapshot.runt.uv = None;
            snapshot.runt.conda = None;
        }
        "conda" => {
            if snapshot.runt.conda.is_none() {
                snapshot.runt.conda = Some(notebook_doc::metadata::CondaInlineMetadata {
                    dependencies: Vec::new(),
                    channels: vec!["conda-forge".to_string()],
                    python: None,
                });
            }
            snapshot.runt.uv = None;
            snapshot.runt.pixi = None;
        }
        _ => {
            // uv
            if snapshot.runt.uv.is_none() {
                snapshot.runt.uv = Some(notebook_doc::metadata::UvInlineMetadata {
                    dependencies: Vec::new(),
                    requires_python: None,
                    prerelease: None,
                });
            }
        }
    }
    if let Err(e) = handle.set_metadata_snapshot(&snapshot) {
        tracing::warn!("Failed to fix package manager metadata: {e}");
        return false;
    }
    true
}

/// Add a dependency using the appropriate package manager, return error string on failure.
pub(crate) fn add_dep_for_manager(
    handle: &notebook_sync::handle::DocHandle,
    package: &str,
    manager: &str,
) -> Result<(), String> {
    match manager {
        "conda" => handle
            .add_conda_dependency(package)
            .map_err(|e| format!("Failed to add conda dependency: {e}")),
        "pixi" => handle
            .add_pixi_dependency(package)
            .map_err(|e| format!("Failed to add pixi dependency: {e}")),
        _ => handle
            .add_uv_dependency(package)
            .map_err(|e| format!("Failed to add uv dependency: {e}")),
    }
}

/// Remove a dependency using the appropriate package manager.
fn remove_dep_for_manager(
    handle: &notebook_sync::handle::DocHandle,
    package: &str,
    manager: &str,
) -> Result<bool, String> {
    match manager {
        "conda" => handle
            .remove_conda_dependency(package)
            .map_err(|e| format!("Failed to remove conda dependency: {e}")),
        "pixi" => handle
            .remove_pixi_dependency(package)
            .map_err(|e| format!("Failed to remove pixi dependency: {e}")),
        _ => handle
            .remove_uv_dependency(package)
            .map_err(|e| format!("Failed to remove uv dependency: {e}")),
    }
}

/// Add a package dependency. Auto-detects the notebook's package manager (uv, conda, or pixi).
pub async fn add_dependency(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let package = arg_str(request, "package")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: package", None))?;
    let after = arg_str(request, "after").unwrap_or("none");

    let (handle, notebook_id) = {
        let guard = server.session.read().await;
        match guard.as_ref() {
            Some(s) => (s.handle.clone(), s.notebook_id.clone()),
            None => {
                return tool_error(
                    "No active notebook session. Call open_notebook or create_notebook first.",
                )
            }
        }
    };

    let manager = detect_package_manager(&handle);

    add_dep_for_manager(&handle, package, &manager)
        .map_err(|e| McpError::internal_error(e, None))?;

    // Ensure daemon has the metadata change before any follow-up action
    if let Err(e) = handle.confirm_sync().await {
        tracing::warn!("confirm_sync failed after add_dependency: {e}");
    }

    // Read back current dependencies
    let deps = get_deps_for_manager(&handle, &manager);

    let mut result = serde_json::json!({
        "dependencies": deps,
        "added": package,
        "package_manager": manager,
    });

    match after {
        "sync" => {
            // Attempt hot-install (UV only; conda/pixi will report needs_restart)
            match handle
                .send_request(NotebookRequest::SyncEnvironment {})
                .await
            {
                Ok(NotebookResponse::SyncEnvironmentComplete {
                    synced_packages, ..
                }) => {
                    result["sync"] = serde_json::json!({
                        "success": true,
                        "synced_packages": synced_packages,
                    });
                }
                Ok(NotebookResponse::SyncEnvironmentStarted { packages }) => {
                    result["sync"] = serde_json::json!({
                        "success": true,
                        "synced_packages": packages,
                    });
                }
                Ok(NotebookResponse::SyncEnvironmentFailed {
                    error,
                    needs_restart,
                }) => {
                    result["sync"] = serde_json::json!({
                        "success": false,
                        "error": error,
                        "needs_restart": needs_restart,
                    });
                }
                Ok(NotebookResponse::Error { error }) => {
                    result["sync"] = serde_json::json!({
                        "success": false,
                        "error": error,
                        "needs_restart": true,
                    });
                }
                Ok(_) => {
                    result["sync"] = serde_json::json!({ "success": true });
                }
                Err(e) => {
                    result["sync"] = serde_json::json!({
                        "success": false,
                        "error": format!("Failed to sync: {e}"),
                        "needs_restart": true,
                    });
                }
            }
        }
        "restart" => {
            // Shutdown + relaunch with scoped auto-detect to preserve the
            // package manager family (auto:uv, auto:conda, auto:pixi)
            let restart_env_source = match handle
                .get_runtime_state()
                .ok()
                .map(|s| s.kernel.env_source.clone())
                .as_deref()
            {
                Some("uv:prewarmed") => "auto:uv".to_string(),
                Some("conda:prewarmed") => "auto:conda".to_string(),
                Some("pixi:prewarmed") => "auto:pixi".to_string(),
                Some("") | None => "auto".to_string(),
                Some(s) => s.to_string(),
            };
            // Derive notebook_path for project-file-backed envs (uv:pyproject, pixi:toml, etc.)
            let notebook_path = if notebook_id.contains('/') || notebook_id.contains('\\') {
                Some(notebook_id.clone())
            } else {
                None
            };
            let _ = handle
                .send_request(NotebookRequest::ShutdownKernel {})
                .await;
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            match handle
                .send_request(NotebookRequest::LaunchKernel {
                    kernel_type: "python".to_string(),
                    env_source: restart_env_source,
                    notebook_path,
                })
                .await
            {
                Ok(NotebookResponse::KernelLaunched { env_source, .. }) => {
                    result["restart"] = serde_json::json!({
                        "success": true,
                        "env_source": env_source,
                    });
                }
                Ok(NotebookResponse::Error { error }) => {
                    result["restart"] = serde_json::json!({
                        "success": false,
                        "error": error,
                    });
                }
                Err(e) => {
                    result["restart"] = serde_json::json!({
                        "success": false,
                        "error": format!("Failed to restart: {e}"),
                    });
                }
                _ => {}
            }
        }
        _ => {} // "none" — just record the dep
    }

    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}

/// Remove a package dependency. Auto-detects the notebook's package manager.
pub async fn remove_dependency(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let package = arg_str(request, "package")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: package", None))?;

    let handle = require_handle!(server);

    let manager = detect_package_manager(&handle);

    let removed = remove_dep_for_manager(&handle, package, &manager)
        .map_err(|e| McpError::internal_error(e, None))?;

    // Ensure daemon has the metadata change
    if let Err(e) = handle.confirm_sync().await {
        tracing::warn!("confirm_sync failed after remove_dependency: {e}");
    }

    let deps = get_deps_for_manager(&handle, &manager);

    let result = serde_json::json!({
        "dependencies": deps,
        "removed": package,
        "was_present": removed,
        "package_manager": manager,
    });
    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}

/// Get the notebook's current package dependencies.
pub async fn get_dependencies(
    server: &NteractMcp,
    _request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let handle = require_handle!(server);

    let manager = detect_package_manager(&handle);
    let deps = get_deps_for_manager(&handle, &manager);

    // Include prewarmed packages from RuntimeStateDoc when available
    let prewarmed = handle
        .get_runtime_state()
        .ok()
        .map(|s| s.env.prewarmed_packages)
        .unwrap_or_default();

    let mut result = serde_json::json!({
        "dependencies": deps,
        "package_manager": manager,
    });
    if !prewarmed.is_empty() {
        result["available_packages"] = serde_json::json!(prewarmed);
    }
    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}

/// Hot-install new dependencies without restarting.
pub async fn sync_environment(
    server: &NteractMcp,
    _request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let handle = require_handle!(server);

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

/// Read dependencies for the detected package manager.
fn get_deps_for_manager(handle: &notebook_sync::handle::DocHandle, manager: &str) -> Vec<String> {
    handle
        .get_notebook_metadata()
        .map(|m| match manager {
            "conda" => m.conda_dependencies().to_vec(),
            "pixi" => m.pixi_dependencies().to_vec(),
            _ => m.uv_dependencies().to_vec(),
        })
        .unwrap_or_default()
}
