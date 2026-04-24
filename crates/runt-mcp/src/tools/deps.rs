//! Dependency management tools: add_dependency, remove_dependency, get_dependencies, sync_environment.

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use notebook_protocol::protocol::{NotebookRequest, NotebookResponse};

use crate::NteractMcp;

use super::{arg_str, tool_error, tool_success};

/// Parse a `package` parameter that may be a single package spec or a
/// list-like string agents sometimes produce.
///
/// Accepted forms:
///  - `"pandas>=2.0"` → `["pandas>=2.0"]`
///  - `"[\"pandas\",\"numpy\"]"` (JSON array) → `["pandas", "numpy"]`
///  - `"['pandas','numpy']"` (Python repr) → `["pandas", "numpy"]`
///
/// Returns a non-empty Vec; falls back to the raw string as-is if no list
/// pattern is detected (the daemon will report the error naturally for
/// invalid package names).
fn parse_package_param(raw: &str) -> Vec<String> {
    let trimmed = raw.trim();

    if trimmed.starts_with('[') && trimmed.ends_with(']') {
        // Try JSON first, then Python-repr (single quotes → double quotes).
        if let Ok(parsed) = serde_json::from_str::<Vec<String>>(trimmed) {
            if !parsed.is_empty() {
                tracing::warn!(
                    "[mcp] add_dependency `package` param contained a JSON list; \
                     splitting into {} individual packages (#2084)",
                    parsed.len()
                );
                return parsed;
            }
        }
        let json_ified = trimmed.replace('\'', "\"");
        if let Ok(parsed) = serde_json::from_str::<Vec<String>>(&json_ified) {
            if !parsed.is_empty() {
                tracing::warn!(
                    "[mcp] add_dependency `package` param contained a Python-repr list; \
                     splitting into {} individual packages (#2084)",
                    parsed.len()
                );
                return parsed;
            }
        }
    }

    // Single package spec (normal case).
    vec![trimmed.to_string()]
}

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
pub(crate) fn detect_package_manager(
    handle: &notebook_sync::handle::DocHandle,
) -> notebook_protocol::connection::PackageManager {
    use notebook_protocol::connection::PackageManager;
    // Priority 1: metadata declares which package manager section exists.
    // Check section existence, not just non-empty deps — an empty pixi section
    // means "this is a pixi notebook with no deps yet".
    if let Some(meta) = handle.get_notebook_metadata() {
        if meta.runt.pixi.is_some() {
            return PackageManager::Pixi;
        }
        if meta.runt.conda.is_some() {
            return PackageManager::Conda;
        }
        if meta.runt.uv.is_some() {
            return PackageManager::Uv;
        }
    }
    // Priority 2: env_source from running kernel (fallback for notebooks
    // with no runt metadata yet).
    if let Ok(state) = handle.get_runtime_state() {
        if let Some(pm) = notebook_protocol::connection::EnvSource::parse(&state.kernel.env_source)
            .package_manager()
        {
            return pm;
        }
    }
    // Default
    PackageManager::Uv
}

/// Add a dependency using the appropriate package manager, return error string on failure.
///
/// `Unknown` package managers fall back to Uv (same default as
/// `detect_package_manager`) — consistent with the historical behavior.
pub(crate) fn add_dep_for_manager(
    handle: &notebook_sync::handle::DocHandle,
    package: &str,
    manager: &notebook_protocol::connection::PackageManager,
) -> Result<(), String> {
    use notebook_protocol::connection::PackageManager;
    match manager {
        PackageManager::Conda => {
            // Reject PEP 508 extras (`pkg[extra]`) before they land in
            // the doc — conda matchspecs crash rattler with `invalid
            // bracket` and take the kernel with them. See #2119.
            notebook_doc::metadata::validate_conda_package_specifier(package)?;
            handle
                .add_conda_dependency(package)
                .map_err(|e| format!("Failed to add conda dependency: {e}"))
        }
        PackageManager::Pixi => {
            notebook_doc::metadata::validate_conda_package_specifier(package)?;
            handle
                .add_pixi_dependency(package)
                .map_err(|e| format!("Failed to add pixi dependency: {e}"))
        }
        PackageManager::Uv | PackageManager::Unknown(_) => handle
            .add_uv_dependency(package)
            .map_err(|e| format!("Failed to add uv dependency: {e}")),
    }
}

/// Remove a dependency using the appropriate package manager.
///
/// `Unknown` package managers fall back to Uv (same default as `add`).
fn remove_dep_for_manager(
    handle: &notebook_sync::handle::DocHandle,
    package: &str,
    manager: &notebook_protocol::connection::PackageManager,
) -> Result<bool, String> {
    use notebook_protocol::connection::PackageManager;
    match manager {
        PackageManager::Conda => handle
            .remove_conda_dependency(package)
            .map_err(|e| format!("Failed to remove conda dependency: {e}")),
        PackageManager::Pixi => handle
            .remove_pixi_dependency(package)
            .map_err(|e| format!("Failed to remove pixi dependency: {e}")),
        PackageManager::Uv | PackageManager::Unknown(_) => handle
            .remove_uv_dependency(package)
            .map_err(|e| format!("Failed to remove uv dependency: {e}")),
    }
}

/// Add a package dependency. Auto-detects the notebook's package manager (uv, conda, or pixi).
///
/// Tolerates agents passing a list-like string (e.g. `"['pandas','numpy']"` or
/// `'["pandas","numpy"]'`) as the `package` parameter — splits into individual
/// packages and adds each one.  See #2084.
pub async fn add_dependency(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let raw_package = arg_str(request, "package")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: package", None))?;
    let after = arg_str(request, "after").unwrap_or("none");

    // Detect list-like strings agents sometimes pass and split them.
    let packages = parse_package_param(raw_package);

    let (handle, notebook_id) =
        {
            let guard = server.session.read().await;
            match guard.as_ref() {
                Some(s) => (s.handle.clone(), s.notebook_id.clone()),
                None => return tool_error(
                    "No active notebook session. Call connect_notebook or create_notebook first.",
                ),
            }
        };

    let manager = detect_package_manager(&handle);

    for package in &packages {
        add_dep_for_manager(&handle, package, &manager)
            .map_err(|e| McpError::internal_error(e, None))?;
    }
    // For the response, use the first package as `package` for backward compat
    let package = packages.first().map(|s| s.as_str()).unwrap_or(raw_package);

    // Ensure daemon has the metadata change before any follow-up action
    if let Err(e) = handle.confirm_sync().await {
        tracing::warn!("confirm_sync failed after add_dependency: {e}");
    }

    // Read back current dependencies
    let deps = get_deps_for_manager(&handle, &manager);

    let mut result = serde_json::json!({
        "dependencies": deps,
        "added": package,
        "package_manager": manager.as_str(),
    });
    if packages.len() > 1 {
        result["added_packages"] = serde_json::json!(packages);
    }

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
            // package manager family (auto:uv, auto:conda, auto:pixi).
            use notebook_protocol::connection::{EnvSource, PackageManager};
            let prev_env = handle
                .get_runtime_state()
                .ok()
                .map(|s| s.kernel.env_source.clone())
                .unwrap_or_default();
            let restart_env_source = if prev_env.is_empty() {
                "auto".to_string()
            } else {
                match EnvSource::parse(&prev_env) {
                    EnvSource::Prewarmed(PackageManager::Uv) => "auto:uv".to_string(),
                    EnvSource::Prewarmed(PackageManager::Conda) => "auto:conda".to_string(),
                    EnvSource::Prewarmed(PackageManager::Pixi) => "auto:pixi".to_string(),
                    other => other.as_str().to_string(),
                }
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
        "package_manager": manager.as_str(),
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

    // Indicate whether deps come from a project file or notebook metadata
    let env_source = handle
        .get_runtime_state()
        .ok()
        .map(|s| s.kernel.env_source.clone());
    use notebook_protocol::connection::EnvSource;
    let mode = match env_source.as_deref().map(EnvSource::parse) {
        Some(EnvSource::PixiToml | EnvSource::Pyproject | EnvSource::EnvYml) => "project",
        _ => "inline",
    };

    let mut result = serde_json::json!({
        "dependencies": deps,
        "package_manager": manager.as_str(),
        "mode": mode,
    });
    if let Some(ref source) = env_source {
        result["env_source"] = serde_json::json!(source);
    }
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

/// Read dependencies for the detected package manager (pub for session.rs).
pub(crate) fn get_deps_for_manager_pub(
    handle: &notebook_sync::handle::DocHandle,
    manager: &notebook_protocol::connection::PackageManager,
) -> Vec<String> {
    get_deps_for_manager(handle, manager)
}

/// Read dependencies for the detected package manager.
fn get_deps_for_manager(
    handle: &notebook_sync::handle::DocHandle,
    manager: &notebook_protocol::connection::PackageManager,
) -> Vec<String> {
    use notebook_protocol::connection::PackageManager;
    handle
        .get_notebook_metadata()
        .map(|m| match manager {
            PackageManager::Conda => m.conda_dependencies().to_vec(),
            PackageManager::Pixi => m.pixi_dependencies().to_vec(),
            PackageManager::Uv | PackageManager::Unknown(_) => m.uv_dependencies().to_vec(),
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_single_package() {
        assert_eq!(parse_package_param("pandas>=2.0"), vec!["pandas>=2.0"]);
    }

    #[test]
    fn parse_json_array_string() {
        assert_eq!(
            parse_package_param(r#"["pandas","numpy"]"#),
            vec!["pandas", "numpy"]
        );
    }

    #[test]
    fn parse_python_repr_string() {
        assert_eq!(
            parse_package_param("['pandas','numpy']"),
            vec!["pandas", "numpy"]
        );
    }

    #[test]
    fn parse_python_repr_with_version_specs() {
        assert_eq!(
            parse_package_param("['pandas>=2.0', 'numpy']"),
            vec!["pandas>=2.0", "numpy"]
        );
    }

    #[test]
    fn parse_empty_brackets_falls_through() {
        // Empty list → fall back to raw string (will error naturally)
        assert_eq!(parse_package_param("[]"), vec!["[]"]);
    }

    #[test]
    fn parse_whitespace_trimmed() {
        assert_eq!(parse_package_param("  ['pandas']  "), vec!["pandas"]);
    }
}
