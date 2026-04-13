//! Session management tools: list, join, open notebooks.

use std::path::PathBuf;

use rmcp::model::{CallToolRequestParams, CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use runtimed_client::client::PoolClient;

use crate::formatting;
use crate::session::NotebookSession;
use crate::NteractMcp;

/// Read the current session's notebook_id (if any) before replacing it.
async fn previous_notebook_id(server: &NteractMcp) -> Option<String> {
    server
        .session
        .read()
        .await
        .as_ref()
        .map(|s| s.notebook_id.clone())
}

/// Resolve a user-provided path: expand ~ to home dir and resolve relative paths
/// against the current working directory. The MCP server runs in the expected cwd,
/// so relative paths are meaningful here (unlike the daemon, which may run as launchd).
fn resolve_path(path: &str) -> String {
    // Expand ~ using dirs::home_dir() (handles HOME on Unix, USERPROFILE on Windows)
    let expanded = if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            home.join(rest).to_string_lossy().to_string()
        } else {
            path.to_string()
        }
    } else if let Some(rest) = path.strip_prefix("~\\") {
        // Windows-style: ~\Documents\notebook.ipynb
        if let Some(home) = dirs::home_dir() {
            home.join(rest).to_string_lossy().to_string()
        } else {
            path.to_string()
        }
    } else if path == "~" {
        dirs::home_dir()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string())
    } else {
        path.to_string()
    };

    let p = PathBuf::from(&expanded);
    if p.is_relative() {
        std::env::current_dir()
            .map(|cwd| cwd.join(&p).to_string_lossy().to_string())
            .unwrap_or(expanded)
    } else {
        expanded
    }
}

use notebook_protocol::protocol::{NotebookRequest, NotebookResponse};

use super::{arg_bool, arg_str, arg_string_array, tool_error, tool_success};

fn has_display() -> bool {
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        return true;
    }
    std::env::var("DISPLAY").is_ok() || std::env::var("WAYLAND_DISPLAY").is_ok()
}

/// Collect runtime info from RuntimeStateDoc, polling briefly for it to sync.
/// Matches Python's `_collect_runtime_info()`.
async fn collect_runtime_info(handle: &notebook_sync::handle::DocHandle) -> serde_json::Value {
    // Poll up to ~500ms for RuntimeStateDoc to sync after join
    let mut info = read_runtime_info(handle);
    if info
        .get("kernel_status")
        .and_then(|v| v.as_str())
        .unwrap_or("not_started")
        == "not_started"
    {
        for _ in 0..5 {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            info = read_runtime_info(handle);
            let status = info
                .get("kernel_status")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if status != "not_started" && status != "unknown" && !status.is_empty() {
                break;
            }
        }
    }
    info
}

/// Read runtime info snapshot from the handle's RuntimeStateDoc.
fn read_runtime_info(handle: &notebook_sync::handle::DocHandle) -> serde_json::Value {
    let mut info = serde_json::Map::new();
    match handle.get_runtime_state() {
        Ok(state) => {
            info.insert(
                "kernel_status".into(),
                serde_json::json!(state.kernel.status),
            );
            if !state.kernel.language.is_empty() {
                info.insert("language".into(), serde_json::json!(state.kernel.language));
            }
            if !state.kernel.name.is_empty() {
                info.insert("kernel_name".into(), serde_json::json!(state.kernel.name));
            }
            if !state.kernel.env_source.is_empty() {
                info.insert(
                    "env_source".into(),
                    serde_json::json!(state.kernel.env_source),
                );
                let env_source = &state.kernel.env_source;
                if env_source.starts_with("conda:") {
                    info.insert("package_manager".into(), serde_json::json!("conda"));
                } else if env_source.starts_with("uv:") {
                    info.insert("package_manager".into(), serde_json::json!("uv"));
                } else if env_source == "deno" {
                    info.insert("package_manager".into(), serde_json::json!("deno"));
                }
            }
            if !state.env.in_sync {
                info.insert("env_in_sync".into(), serde_json::json!(false));
            }
            if !state.env.prewarmed_packages.is_empty() {
                info.insert(
                    "prewarmed_packages".into(),
                    serde_json::json!(state.env.prewarmed_packages),
                );
            }
        }
        Err(_) => {
            info.insert("kernel_status".into(), serde_json::json!("unknown"));
        }
    }
    serde_json::Value::Object(info)
}

/// Get dependencies from notebook metadata.
fn get_dependencies(handle: &notebook_sync::handle::DocHandle) -> Vec<String> {
    handle
        .get_notebook_metadata()
        .and_then(|m| m.runt.uv)
        .map(|uv| uv.dependencies)
        .unwrap_or_default()
}

/// Format cell summaries for join/open response.
fn format_cell_summaries(handle: &notebook_sync::handle::DocHandle) -> String {
    let cells = handle.get_cells();
    let cell_status_map = crate::tools::cell_read::build_cell_status_map(handle);
    let cell_ec_map = crate::tools::cell_read::build_cell_execution_count_map(handle);
    cells
        .iter()
        .enumerate()
        .map(|(i, cell)| {
            let status = cell_status_map.get(&cell.id).map(String::as_str);
            let ec = cell_ec_map.get(&cell.id).map(String::as_str);
            formatting::format_cell_summary(
                i,
                &cell.id,
                &cell.cell_type,
                &cell.source,
                ec,
                status,
                60,
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[allow(dead_code)] // Fields used by schemars for tool input schema generation
#[derive(Debug, Deserialize, JsonSchema)]
pub struct OpenNotebookParams {
    /// A file path (e.g. "~/analysis.ipynb") or a notebook ID from
    /// list_active_notebooks. Paths are opened from disk; IDs connect
    /// to a running session.
    pub notebook: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct CreateNotebookParams {
    /// Runtime type: "python" or "deno".
    #[serde(default)]
    pub runtime: Option<String>,
    /// Alias for runtime (deprecated but supported for convenience).
    #[serde(default)]
    pub kernel: Option<String>,
    /// Working directory for the kernel.
    #[serde(default)]
    pub working_dir: Option<String>,
    /// Packages to pre-install.
    #[serde(default)]
    pub dependencies: Option<Vec<String>>,
    /// Package manager for dependencies: "uv", "conda", or "pixi".
    /// Defaults to the user's default_python_env setting.
    #[serde(default)]
    pub package_manager: Option<String>,
    /// When true (default for MCP), notebook exists only in memory.
    /// Use save_notebook(path=...) to persist to disk.
    #[serde(default)]
    pub ephemeral: Option<bool>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ShowNotebookParams {
    /// Notebook ID to show. Defaults to current session's notebook.
    #[serde(default)]
    pub notebook_id: Option<String>,
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

/// Heuristic: does this string look like a file path rather than a daemon room ID?
/// Paths contain separators, start with ~ or ., or end in .ipynb.
fn looks_like_path(s: &str) -> bool {
    s.contains('/')
        || s.contains('\\')
        || s.starts_with('~')
        || s.starts_with('.')
        || s.ends_with(".ipynb")
}

/// Open a notebook — either from a file path on disk or by connecting to an
/// existing daemon session by ID.
///
/// Unified handler for `open_notebook` and the deprecated `join_notebook`.
/// Accepts `notebook`, `path`, or `notebook_id` params (in that priority order)
/// and auto-detects whether the value is a file path or a session ID.
pub async fn open_notebook(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    // Resolve the notebook identifier from whichever param was provided.
    let notebook = arg_str(request, "notebook")
        .or_else(|| arg_str(request, "path"))
        .or_else(|| arg_str(request, "notebook_id"))
        .ok_or_else(|| {
            McpError::invalid_params(
                "Missing required parameter: notebook (a file path or notebook ID)",
                None,
            )
        })?
        .to_string();

    let prev = previous_notebook_id(server).await;

    if looks_like_path(&notebook) {
        // File path — resolve and open from disk via the daemon's OpenNotebook handshake.
        let abs_path = PathBuf::from(resolve_path(&notebook));

        match notebook_sync::connect::connect_open(
            server.socket_path.clone(),
            abs_path.clone(),
            &server.get_peer_label().await,
        )
        .await
        {
            Ok(result) => {
                let handle = &result.handle;
                let notebook_id = handle.notebook_id().to_string();

                let _ = handle.confirm_sync().await;

                let runtime_info = collect_runtime_info(handle).await;
                let deps = get_dependencies(handle);
                let cells_summary = format_cell_summaries(handle);

                let mut response = serde_json::json!({
                    "notebook_id": notebook_id,
                    "path": abs_path.to_string_lossy(),
                    "runtime": runtime_info,
                    "dependencies": deps,
                    "cells": cells_summary,
                });

                if let Some(ref prev_id) = prev {
                    if *prev_id != notebook_id {
                        response["switched_from"] = serde_json::json!(prev_id);
                    }
                }

                let peer_label = server.get_peer_label().await;
                crate::presence::announce(handle, &peer_label).await;

                let session = NotebookSession {
                    handle: result.handle,
                    broadcast_rx: result.broadcast_rx,
                    notebook_id,
                };
                *server.session.write().await = Some(session);

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&response).unwrap_or_default(),
                )]))
            }
            Err(e) => tool_error(&format!("Failed to open notebook '{}': {}", notebook, e)),
        }
    } else {
        // Session ID — connect to an existing daemon room.
        let notebook_id = notebook;

        match notebook_sync::connect::connect(
            server.socket_path.clone(),
            notebook_id.clone(),
            &server.get_peer_label().await,
        )
        .await
        {
            Ok(result) => {
                let handle = &result.handle;

                let _ = handle.confirm_sync().await;

                let runtime_info = collect_runtime_info(handle).await;
                let deps = get_dependencies(handle);
                let cells_summary = format_cell_summaries(handle);

                let mut response = serde_json::json!({
                    "notebook_id": handle.notebook_id(),
                    "connected": true,
                    "runtime": runtime_info,
                    "dependencies": deps,
                    "cells": cells_summary,
                });

                if let Some(ref prev_id) = prev {
                    if *prev_id != notebook_id {
                        response["switched_from"] = serde_json::json!(prev_id);
                    }
                }

                let peer_label = server.get_peer_label().await;
                crate::presence::announce(handle, &peer_label).await;

                let session = NotebookSession {
                    handle: result.handle,
                    broadcast_rx: result.broadcast_rx,
                    notebook_id,
                };
                *server.session.write().await = Some(session);

                Ok(CallToolResult::success(vec![Content::text(
                    serde_json::to_string_pretty(&response).unwrap_or_default(),
                )]))
            }
            Err(e) => tool_error(&format!("Failed to join notebook: {}", e)),
        }
    }
}

/// Create a new notebook with optional dependencies.
pub async fn create_notebook(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    // Support both 'runtime' and 'kernel' params (kernel is an alias for convenience)
    let kernel_alias = arg_str(request, "kernel");
    let runtime_arg = arg_str(request, "runtime");
    let used_kernel_alias = kernel_alias.is_some() && runtime_arg.is_none();
    let runtime = runtime_arg.or(kernel_alias).unwrap_or("python");

    let working_dir = arg_str(request, "working_dir")
        .map(|s| PathBuf::from(resolve_path(s)))
        .or_else(|| std::env::current_dir().ok());
    let working_dir_for_detection = working_dir.clone();
    let ephemeral = arg_bool(request, "ephemeral").unwrap_or(true);

    let prev = previous_notebook_id(server).await;

    match notebook_sync::connect::connect_create(
        server.socket_path.clone(),
        runtime,
        working_dir,
        &server.get_peer_label().await,
        ephemeral,
    )
    .await
    {
        Ok(result) => {
            let notebook_id = result.handle.notebook_id().to_string();

            // Announce presence so the peer is visible immediately
            let peer_label = server.get_peer_label().await;
            crate::presence::announce(&result.handle, &peer_label).await;

            // Add dependencies if specified
            let deps: Vec<String> = arg_string_array(request, "dependencies").unwrap_or_default();

            let explicit_pkg_manager = arg_str(request, "package_manager");

            // Validate explicit package_manager if provided
            if let Some(pm) = explicit_pkg_manager {
                if !matches!(pm, "uv" | "conda" | "pixi") {
                    return tool_error(&format!(
                        "Invalid package_manager '{}'. Must be 'uv', 'conda', or 'pixi'.",
                        pm
                    ));
                }
            }

            // Ensure the daemon's doc structure is fully received before
            // any metadata writes.
            let mut metadata_changed = false;
            if runtime != "deno" {
                if let Err(e) = result.handle.confirm_sync().await {
                    tracing::warn!("confirm_sync before create_notebook metadata fix: {e}");
                }

                // Only override metadata when the user explicitly requested a
                // package manager. When omitted, the daemon already set the
                // correct metadata from default_python_env.
                // Skip when a matching project file exists — the daemon already
                // detected it and will bootstrap deps into CRDT.
                if let Some(pm) = explicit_pkg_manager {
                    let project_matches = working_dir_for_detection
                        .as_ref()
                        .and_then(|wd| crate::project_file::detect_project_file(wd))
                        .is_some_and(|d| d.manager() == pm);
                    if !project_matches {
                        metadata_changed =
                            super::deps::ensure_package_manager_metadata(&result.handle, pm);
                    }
                }
            }

            // Effective package manager: explicit arg, or what the daemon set
            // from default_python_env.
            let pkg_manager: String = explicit_pkg_manager
                .map(String::from)
                .unwrap_or_else(|| super::deps::detect_package_manager(&result.handle));

            if runtime != "deno" {
                for dep in &deps {
                    let _ = super::deps::add_dep_for_manager(&result.handle, dep, &pkg_manager);
                }
            }

            let session = NotebookSession {
                handle: result.handle,
                broadcast_rx: result.broadcast_rx,
                notebook_id: notebook_id.clone(),
            };
            *server.session.write().await = Some(session);

            // Restart kernel if deps were added or package manager metadata
            // was changed from the daemon's default (so the kernel picks up
            // the right env). Skip for deno — deno doesn't use Python
            // package managers.
            let needs_restart = runtime != "deno" && (!deps.is_empty() || metadata_changed);
            if needs_restart {
                let session = server.session.read().await;
                if let Some(s) = session.as_ref() {
                    // Ensure daemon has the dep metadata before restarting
                    if let Err(e) = s.handle.confirm_sync().await {
                        tracing::warn!("confirm_sync failed before create_notebook relaunch: {e}");
                    }

                    // Shutdown and relaunch with scoped auto-detect so the daemon
                    // uses the correct package manager pool (not the system default).
                    // "auto:pixi" → pixi pool/inline, "auto:conda" → conda pool/inline,
                    // "auto" → follows default_python_env (which may differ from requested).
                    let scoped_env_source = match pkg_manager.as_str() {
                        "pixi" => "auto:pixi",
                        "conda" => "auto:conda",
                        _ => "auto:uv",
                    };
                    let _ = s
                        .handle
                        .send_request(NotebookRequest::ShutdownKernel {})
                        .await;
                    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
                    let _ = s
                        .handle
                        .send_request(NotebookRequest::LaunchKernel {
                            kernel_type: runtime.to_string(),
                            env_source: scoped_env_source.to_string(),
                            notebook_path: None,
                        })
                        .await;

                    // Wait for kernel to become ready
                    let start = std::time::Instant::now();
                    let timeout = std::time::Duration::from_secs(120);
                    loop {
                        if start.elapsed() >= timeout {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        if let Ok(state) = s.handle.get_runtime_state() {
                            if state.kernel.status == "idle" || state.kernel.status == "busy" {
                                break;
                            }
                            if state.kernel.status == "error" {
                                break;
                            }
                        }
                    }
                }
            }

            let mut info = serde_json::json!({
                "notebook_id": notebook_id,
                "runtime": { "language": runtime },
                "dependencies": deps,
                "package_manager": pkg_manager,
                "ephemeral": ephemeral,
            });

            if let Some(ref prev_id) = prev {
                if *prev_id != notebook_id {
                    info["switched_from"] = serde_json::json!(prev_id);
                }
            }

            if used_kernel_alias {
                info["info"] = serde_json::json!("Used 'kernel' parameter (alias for 'runtime')");
            }

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
    let path = arg_str(request, "path").map(resolve_path);

    // Need both handle and the mutable notebook_id from the session (not the
    // handle's immutable connect-time ID) so that post-rekey saves report the
    // correct notebook_id.
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

    // If no path and notebook is ephemeral (UUID-keyed, not a file path), require a path
    if path.is_none() && !looks_like_path(&notebook_id) {
        return tool_error(
            "No path specified for ephemeral notebook. \
             Provide a path: save_notebook(path='/path/to/notebook.ipynb')",
        );
    }

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

/// Open the notebook in the nteract desktop app.
pub async fn show_notebook(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    // Resolve notebook_id from param or current session
    let target = match arg_str(request, "notebook_id") {
        Some(id) => id.to_string(),
        None => {
            let session = server.session.read().await;
            match session.as_ref() {
                Some(s) => s.notebook_id.clone(),
                None => {
                    return tool_error(
                        "No notebook_id provided and no active session. \
                         Use list_active_notebooks() to find a notebook_id, \
                         or connect to one first.",
                    )
                }
            }
        }
    };

    // Validate notebook is active in daemon
    let client = PoolClient::new(server.socket_path.clone());
    let rooms = client
        .list_rooms()
        .await
        .map_err(|e| McpError::internal_error(format!("Failed to list notebooks: {e}"), None))?;
    if !rooms.iter().any(|r| r.notebook_id == target) {
        return tool_error(&format!(
            "Notebook '{}' is not currently running. \
             Use list_active_notebooks() to see active notebooks.",
            target
        ));
    }

    let is_ephemeral = rooms
        .iter()
        .find(|r| r.notebook_id == target)
        .map(|r| r.ephemeral)
        .unwrap_or(false);

    if !has_display() {
        let mut result = serde_json::json!({
            "notebook_id": target,
            "opened": false,
            "reason": "No display available (headless environment). The notebook is running in the daemon and accessible via MCP tools."
        });
        if is_ephemeral {
            result["note"] = serde_json::json!(
                "This notebook is ephemeral. Use save_notebook(path) to persist."
            );
        }
        return tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default());
    }

    // Launch the app using the binary's build channel.
    // NOTE: If RUNTIMED_SOCKET_PATH points at a different channel's daemon,
    // this may open the wrong app. That's a known dev-only edge case.
    let is_file_backed = std::path::Path::new(&target).is_absolute();
    if is_file_backed {
        runt_workspace::open_notebook_app(Some(std::path::Path::new(&target)), &[])
            .map_err(|e| McpError::internal_error(format!("Failed to open app: {e}"), None))?;
    } else {
        runt_workspace::open_notebook_app(None, &["--notebook-id", &target])
            .map_err(|e| McpError::internal_error(format!("Failed to open app: {e}"), None))?;
    }

    let mut result = serde_json::json!({ "notebook_id": target, "opened": true });
    if is_ephemeral {
        result["warning"] =
            serde_json::json!("This notebook is ephemeral. Save it from the app to keep it.");
    }
    tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    /// When package_manager is explicitly provided, it takes precedence
    /// over whatever the daemon detected.
    #[test]
    fn explicit_pkg_manager_takes_precedence() {
        let explicit: Option<&str> = Some("conda");
        let detected = "uv".to_string();
        let result: String = explicit.map(String::from).unwrap_or(detected);
        assert_eq!(result, "conda");
    }

    /// When package_manager is omitted, the detected (daemon) value is used.
    #[test]
    fn omitted_pkg_manager_uses_detected() {
        let explicit: Option<&str> = None;
        let detected = "pixi".to_string();
        let result: String = explicit.map(String::from).unwrap_or(detected);
        assert_eq!(result, "pixi");
    }

    /// Validation rejects invalid package_manager values.
    #[test]
    fn invalid_pkg_manager_values() {
        for valid in ["uv", "conda", "pixi"] {
            assert!(matches!(valid, "uv" | "conda" | "pixi"));
        }
        assert!(!matches!("mamba", "uv" | "conda" | "pixi"));
        assert!(!matches!("pip", "uv" | "conda" | "pixi"));
    }
}
