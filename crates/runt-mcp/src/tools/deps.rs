//! Dependency management tools.

use std::borrow::Cow;

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ErrorData as McpError;
use schemars::{json_schema, JsonSchema, Schema, SchemaGenerator};
use serde::{Deserialize, Deserializer};

use notebook_protocol::protocol::{NotebookRequest, NotebookResponse};
use notebook_sync::handle::DocHandle;

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
    /// Action after removing: "none" (just record, default) or "restart"
    /// (restart kernel so the package is actually uninstalled).
    /// Hot-uninstall ("sync") is not supported — removals always require
    /// a kernel restart to take effect.
    #[serde(default)]
    pub after: Option<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetDependenciesParams {}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ApproveTrustParams {
    /// Optional dependency fingerprint from a prior get_dependencies call.
    /// If supplied and dependencies changed since review, approval is rejected.
    #[serde(default)]
    pub dependency_fingerprint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyApply {
    None,
    Sync,
    Restart,
    Other(String),
}

impl DependencyApply {
    fn as_str(&self) -> &str {
        match self {
            Self::None => "none",
            Self::Sync => "sync",
            Self::Restart => "restart",
            Self::Other(value) => value.as_str(),
        }
    }
}

impl<'de> Deserialize<'de> for DependencyApply {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(match value.as_str() {
            "none" => Self::None,
            "sync" => Self::Sync,
            "restart" => Self::Restart,
            _ => Self::Other(value),
        })
    }
}

impl JsonSchema for DependencyApply {
    fn inline_schema() -> bool {
        true
    }

    fn schema_name() -> Cow<'static, str> {
        "DependencyApply".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "Dependency apply mode. Known values are 'none', 'sync', and 'restart'. Unknown strings are rejected with a targeted error.",
            "examples": ["none", "sync", "restart"]
        })
    }
}

#[allow(dead_code)]
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct ManageDependenciesParams {
    /// Packages to add to the notebook's current dependency manager.
    #[serde(default)]
    pub add: Vec<String>,
    /// Packages to remove from the notebook's current dependency manager.
    #[serde(default)]
    pub remove: Vec<String>,
    /// Approve and sign the resulting dependency metadata. With add/remove this
    /// trusts the post-change dependency set. Without add/remove this approves
    /// the current dependency set.
    #[serde(default)]
    pub trust: bool,
    /// Optional fingerprint from a prior manage_dependencies or get_dependencies
    /// response. With add/remove, this must match the pre-change dependency
    /// fingerprint. Without add/remove, this must match the approved fingerprint.
    #[serde(default)]
    pub dependency_fingerprint: Option<String>,
    /// Action after dependency edits: "none" (default), "sync" (hot-install,
    /// UV only), or "restart" (restart kernel with new deps).
    #[serde(default)]
    pub apply: Option<DependencyApply>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SyncEnvironmentParams {}

fn parse_manage_dependencies_params(
    request: &CallToolRequestParams,
) -> Result<ManageDependenciesParams, McpError> {
    let value = request
        .arguments
        .clone()
        .map(serde_json::Value::Object)
        .unwrap_or_else(|| serde_json::json!({}));
    serde_json::from_value(value)
        .map_err(|e| McpError::invalid_params(format!("Invalid parameters: {e}"), None))
}

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
        PackageManager::Uv | PackageManager::Unknown(_) => {
            notebook_doc::metadata::validate_package_specifier(package)?;
            handle
                .add_uv_dependency(package)
                .map_err(|e| format!("Failed to add uv dependency: {e}"))
        }
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

fn dependency_fingerprint_for_handle(handle: &DocHandle) -> Option<String> {
    handle
        .get_notebook_metadata()
        .map(|snapshot| snapshot.dependency_fingerprint())
}

fn observed_heads_for_handle(handle: &DocHandle) -> Option<Vec<String>> {
    handle
        .with_doc(|doc| {
            doc.get_heads()
                .iter()
                .map(|head| head.to_string())
                .collect()
        })
        .ok()
}

async fn approve_current_trust(
    handle: &DocHandle,
    observed_heads: Option<Vec<String>>,
    action: &str,
) -> Result<(), String> {
    if let Err(e) = handle.confirm_sync().await {
        tracing::warn!("confirm_sync failed before {action}: {e}");
    }

    match handle
        .send_request(NotebookRequest::ApproveTrust { observed_heads })
        .await
    {
        Ok(NotebookResponse::Ok {}) => Ok(()),
        Ok(NotebookResponse::GuardRejected { reason }) => Err(reason),
        Ok(NotebookResponse::Error { error }) => Err(error),
        Ok(other) => Err(format!(
            "Unexpected approve_trust response during {action}: {other:?}"
        )),
        Err(e) => Err(format!("Failed to approve dependency metadata: {e}")),
    }
}

fn dependency_mode(env_source: Option<&str>) -> &'static str {
    use notebook_protocol::connection::EnvSource;
    match env_source.map(EnvSource::parse) {
        Some(EnvSource::PixiToml | EnvSource::Pyproject | EnvSource::EnvYml) => "project",
        _ => "inline",
    }
}

fn dependency_state_json(
    handle: &DocHandle,
    manager: &notebook_protocol::connection::PackageManager,
) -> serde_json::Value {
    let deps = get_deps_for_manager(handle, manager);
    let dependency_fingerprint = dependency_fingerprint_for_handle(handle);
    let runtime_state = handle.get_runtime_state().ok();
    let env_source = runtime_state
        .as_ref()
        .map(|s| s.kernel.env_source.clone())
        .filter(|s| !s.is_empty());
    let prewarmed = runtime_state
        .as_ref()
        .map(|s| s.env.prewarmed_packages.clone())
        .unwrap_or_default();

    let mut result = serde_json::json!({
        "dependencies": deps,
        "package_manager": manager.as_str(),
        "mode": dependency_mode(env_source.as_deref()),
        "dependency_fingerprint": dependency_fingerprint,
    });
    if let Some(source) = env_source {
        result["env_source"] = serde_json::json!(source);
    }
    if let Some(trust) = runtime_state.map(|s| s.trust) {
        result["trust"] = serde_json::json!({
            "status": trust.status,
            "needs_approval": trust.needs_approval,
        });
    }
    if !prewarmed.is_empty() {
        result["available_packages"] = serde_json::json!(prewarmed);
    }
    result
}

async fn apply_dependency_changes(
    handle: &DocHandle,
    notebook_id: &str,
    apply: &str,
    result: &mut serde_json::Value,
) {
    match apply {
        "sync" => {
            match handle
                .send_request(NotebookRequest::SyncEnvironment { guard: None })
                .await
            {
                Ok(NotebookResponse::SyncEnvironmentComplete {
                    synced_packages, ..
                }) => {
                    result["apply"] = serde_json::json!({
                        "mode": "sync",
                        "success": true,
                        "synced_packages": synced_packages,
                    });
                }
                Ok(NotebookResponse::GuardRejected { reason }) => {
                    result["apply"] = serde_json::json!({
                        "mode": "sync",
                        "success": false,
                        "error": reason,
                        "needs_restart": false,
                    });
                }
                Ok(NotebookResponse::SyncEnvironmentFailed {
                    error,
                    needs_restart,
                }) => {
                    result["apply"] = serde_json::json!({
                        "mode": "sync",
                        "success": false,
                        "error": error,
                        "needs_restart": needs_restart,
                    });
                }
                Ok(NotebookResponse::Error { error }) => {
                    result["apply"] = serde_json::json!({
                        "mode": "sync",
                        "success": false,
                        "error": error,
                        "needs_restart": true,
                    });
                }
                Ok(_) => {
                    result["apply"] = serde_json::json!({
                        "mode": "sync",
                        "success": true,
                    });
                }
                Err(e) => {
                    result["apply"] = serde_json::json!({
                        "mode": "sync",
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
            let notebook_path = if notebook_id.contains('/') || notebook_id.contains('\\') {
                Some(notebook_id.to_string())
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
                    env_source: notebook_protocol::connection::LaunchSpec::parse(
                        &restart_env_source,
                    ),
                    notebook_path,
                })
                .await
            {
                Ok(NotebookResponse::KernelLaunched { env_source, .. }) => {
                    result["apply"] = serde_json::json!({
                        "mode": "restart",
                        "success": true,
                        "env_source": env_source,
                    });
                }
                Ok(NotebookResponse::Error { error }) => {
                    result["apply"] = serde_json::json!({
                        "mode": "restart",
                        "success": false,
                        "error": error,
                    });
                }
                Ok(NotebookResponse::GuardRejected { reason }) => {
                    result["apply"] = serde_json::json!({
                        "mode": "restart",
                        "success": false,
                        "error": reason,
                    });
                }
                Err(e) => {
                    result["apply"] = serde_json::json!({
                        "mode": "restart",
                        "success": false,
                        "error": format!("Failed to restart: {e}"),
                    });
                }
                _ => {}
            }
        }
        _ => {}
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

    let (handle, notebook_id) = {
        let guard = server.session.read().await;
        match guard.as_ref() {
            Some(s) => (s.handle.clone(), s.notebook_id.clone()),
            None => {
                drop(guard);
                return super::no_session_error(server).await;
            }
        }
    };

    let manager = detect_package_manager(&handle);

    for package in &packages {
        if let Err(e) = add_dep_for_manager(&handle, package, &manager) {
            return tool_error(&e);
        }
    }
    // For the response, use the first package as `package` for backward compat
    let package = packages.first().map(|s| s.as_str()).unwrap_or(raw_package);

    if let Err(e) = approve_current_trust(&handle, None, "add_dependency").await {
        return tool_error(&e);
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
        "sync" | "restart" => {
            apply_dependency_changes(&handle, &notebook_id, after, &mut result).await;
            if let Some(apply) = result.as_object_mut().and_then(|obj| obj.remove("apply")) {
                result[after] = apply;
            }
        }
        _ => {} // "none" — just record the dep
    }

    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}

/// Apply dependency edits, optional trust approval, and optional environment action.
///
/// With no parameters, this returns the current dependency state. With add/remove,
/// the optional `dependency_fingerprint` is treated as a reviewed pre-change
/// fingerprint. With `trust: true`, the daemon signs the resulting dependency set.
pub async fn manage_dependencies(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let params = parse_manage_dependencies_params(request)?;
    let apply = params.apply.clone().unwrap_or(DependencyApply::None);
    if let DependencyApply::Other(value) = &apply {
        return tool_error(&format!(
            "Invalid apply value {value:?}. Expected one of: none, sync, restart."
        ));
    }
    let apply_str = apply.as_str();

    let (handle, notebook_id) = {
        let guard = server.session.read().await;
        match guard.as_ref() {
            Some(s) => (s.handle.clone(), s.notebook_id.clone()),
            None => {
                drop(guard);
                return super::no_session_error(server).await;
            }
        }
    };
    let manager = detect_package_manager(&handle);
    let before_fingerprint = dependency_fingerprint_for_handle(&handle);
    let has_edits = !params.add.is_empty() || !params.remove.is_empty();

    if !has_edits && !params.trust && apply == DependencyApply::None {
        let result = dependency_state_json(&handle, &manager);
        return tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default());
    }

    if has_edits {
        if let Some(expected) = params.dependency_fingerprint.as_deref() {
            if before_fingerprint.as_deref() != Some(expected) {
                return tool_error(
                    "Dependencies changed since review. Inspect the current dependencies before applying edits.",
                );
            }
        }
    }

    let before = dependency_state_json(&handle, &manager);

    for package in &params.add {
        if let Err(e) = add_dep_for_manager(&handle, package, &manager) {
            return tool_error(&e);
        }
    }

    let mut removed = Vec::new();
    let mut not_found = Vec::new();
    for package in &params.remove {
        match remove_dep_for_manager(&handle, package, &manager) {
            Ok(true) => removed.push(package.clone()),
            Ok(false) => not_found.push(package.clone()),
            Err(e) => return tool_error(&e),
        }
    }

    let mut trust_approved = false;
    let approved_fingerprint = if params.trust {
        if has_edits {
            dependency_fingerprint_for_handle(&handle)
        } else {
            if let Some(expected) = params.dependency_fingerprint.as_deref() {
                if before_fingerprint.as_deref() != Some(expected) {
                    return tool_error(
                        "Dependencies changed since review. Inspect the current dependencies before approving.",
                    );
                }
            }
            before_fingerprint.clone()
        }
    } else {
        None
    };

    if params.trust {
        let observed_heads = if has_edits {
            None
        } else {
            observed_heads_for_handle(&handle)
        };
        if let Err(e) = approve_current_trust(&handle, observed_heads, "manage_dependencies").await
        {
            return tool_error(&e);
        }
        trust_approved = true;
    } else if has_edits {
        if let Err(e) = handle.confirm_sync().await {
            tracing::warn!("confirm_sync failed after manage_dependencies edits: {e}");
        }
    }

    let after = dependency_state_json(&handle, &manager);
    let mut result = serde_json::json!({
        "package_manager": manager.as_str(),
        "before": before,
        "after": after,
        "add": params.add,
        "remove": {
            "requested": params.remove,
            "removed": removed,
            "not_found": not_found,
        },
        "trust": {
            "requested": params.trust,
            "approved": trust_approved,
            "dependency_fingerprint": approved_fingerprint,
        },
    });

    if matches!(apply, DependencyApply::Sync | DependencyApply::Restart) {
        apply_dependency_changes(&handle, &notebook_id, apply_str, &mut result).await;
    }

    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}

/// Remove a package dependency. Auto-detects the notebook's package manager.
///
/// Supports `after: "restart"` to restart the kernel so the package is
/// actually uninstalled from the running environment. Without `after`,
/// the response includes `needs_restart: true` when the dep was present.
pub async fn remove_dependency(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let package = arg_str(request, "package")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: package", None))?;
    let after = arg_str(request, "after").unwrap_or("none");

    // "sync" is not meaningful for removals — the daemon's SyncEnvironment
    // rejects removes and signals needs_restart. Map it to "restart" so the
    // caller gets the right outcome without a confusing intermediate error.
    let after = if after == "sync" { "restart" } else { after };

    let (handle, notebook_id) = {
        let guard = server.session.read().await;
        match guard.as_ref() {
            Some(s) => (s.handle.clone(), s.notebook_id.clone()),
            None => {
                drop(guard);
                return super::no_session_error(server).await;
            }
        }
    };

    let manager = detect_package_manager(&handle);

    let removed = remove_dep_for_manager(&handle, package, &manager)
        .map_err(|e| McpError::internal_error(e, None))?;

    if removed {
        if let Err(e) = approve_current_trust(&handle, None, "remove_dependency").await {
            return tool_error(&e);
        }
    }

    let deps = get_deps_for_manager(&handle, &manager);

    let mut result = serde_json::json!({
        "dependencies": deps,
        "removed": package,
        "was_present": removed,
        "package_manager": manager.as_str(),
    });

    if after == "restart" && removed {
        apply_dependency_changes(&handle, &notebook_id, "restart", &mut result).await;
        if let Some(apply) = result.as_object_mut().and_then(|obj| obj.remove("apply")) {
            result["restart"] = apply;
        }
    } else if removed {
        // The dep was removed from metadata but the running environment
        // still has the package installed. Signal the caller.
        result["needs_restart"] = serde_json::json!(true);
    }

    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}

/// Get the notebook's current package dependencies.
pub async fn get_dependencies(
    server: &NteractMcp,
    _request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let handle = require_handle!(server);

    let manager = detect_package_manager(&handle);
    let result = dependency_state_json(&handle, &manager);
    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}

/// Approve/sign the current dependency metadata for headless clients.
pub async fn approve_trust(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let handle = require_handle!(server);
    let supplied_fingerprint = arg_str(request, "dependency_fingerprint").map(str::to_string);
    let current_fingerprint = dependency_fingerprint_for_handle(&handle);
    let expected_fingerprint = supplied_fingerprint
        .clone()
        .or_else(|| current_fingerprint.clone());
    if let Some(expected) = supplied_fingerprint.as_deref() {
        if current_fingerprint.as_deref() != Some(expected) {
            return tool_error(
                "Dependencies changed since review. Inspect the current dependencies before approving.",
            );
        }
    }
    let observed_heads = if supplied_fingerprint.is_some() {
        observed_heads_for_handle(&handle)
    } else {
        None
    };

    if let Err(e) = approve_current_trust(&handle, observed_heads, "approve_trust").await {
        return tool_error(&e);
    }

    if let Err(e) = handle.confirm_sync().await {
        tracing::warn!("confirm_sync failed after approve_trust: {e}");
    }

    let trust = handle.get_runtime_state().ok().map(|s| s.trust);
    let mut result = serde_json::json!({
        "approved": true,
        "dependency_fingerprint": expected_fingerprint,
        "supplied_fingerprint": supplied_fingerprint,
    });
    if let Some(trust) = trust {
        result["trust"] = serde_json::json!({
            "status": trust.status,
            "needs_approval": trust.needs_approval,
        });
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
        .send_request(NotebookRequest::SyncEnvironment { guard: None })
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
        Ok(NotebookResponse::GuardRejected { reason }) => {
            let result = serde_json::json!({
                "success": false,
                "error": reason,
                "needs_restart": false,
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

    fn make_request(args: serde_json::Value) -> CallToolRequestParams {
        serde_json::from_value(serde_json::json!({
            "name": "manage_dependencies",
            "arguments": args,
        }))
        .unwrap()
    }

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

    #[test]
    fn approve_trust_params_accept_missing_fingerprint() {
        let params: ApproveTrustParams = serde_json::from_value(serde_json::json!({})).unwrap();

        assert_eq!(params.dependency_fingerprint, None);
    }

    #[test]
    fn manage_dependencies_params_default_to_inspect() {
        let request = make_request(serde_json::json!({}));
        let params = parse_manage_dependencies_params(&request).unwrap();
        assert!(params.add.is_empty());
        assert!(params.remove.is_empty());
        assert!(!params.trust);
        assert_eq!(params.apply, None);
        assert_eq!(params.dependency_fingerprint, None);
    }

    #[test]
    fn approve_trust_params_accept_supplied_fingerprint() {
        let params: ApproveTrustParams =
            serde_json::from_value(serde_json::json!({"dependency_fingerprint": "sha256:abc"}))
                .unwrap();

        assert_eq!(
            params.dependency_fingerprint,
            Some("sha256:abc".to_string())
        );
    }

    #[test]
    fn manage_dependencies_params_parse_intent() {
        let request = make_request(serde_json::json!({
            "add": ["pandas", "matplotlib"],
            "remove": ["seaborn"],
            "trust": true,
            "dependency_fingerprint": "{\"uv\":{\"dependencies\":[]}}",
            "apply": "sync",
        }));
        let params = parse_manage_dependencies_params(&request).unwrap();
        assert_eq!(params.add, vec!["pandas", "matplotlib"]);
        assert_eq!(params.remove, vec!["seaborn"]);
        assert!(params.trust);
        assert_eq!(
            params.dependency_fingerprint.as_deref(),
            Some("{\"uv\":{\"dependencies\":[]}}")
        );
        assert_eq!(params.apply, Some(DependencyApply::Sync));
    }

    #[test]
    fn manage_dependencies_params_preserve_unknown_apply() {
        let request = make_request(serde_json::json!({
            "apply": "future-mode",
        }));
        let params = parse_manage_dependencies_params(&request).unwrap();
        assert_eq!(
            params.apply,
            Some(DependencyApply::Other("future-mode".to_string()))
        );
    }
}
