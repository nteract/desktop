//! Kernel management tools: interrupt_kernel, restart_kernel.

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ErrorData as McpError;

use notebook_protocol::protocol::{NotebookRequest, NotebookResponse};
use notebook_sync::SyncError;

use crate::NteractMcp;

use super::{tool_error, tool_success};

/// Interrupt the currently executing cell.
pub async fn interrupt_kernel(
    server: &NteractMcp,
    _request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let handle = require_handle!(server);

    match handle
        .send_request(NotebookRequest::InterruptExecution {})
        .await
    {
        Ok(_) => {
            let result = serde_json::json!({ "interrupted": true });
            tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
        }
        Err(e) => tool_error(&format!("Failed to interrupt kernel: {e}")),
    }
}

/// Restart the kernel, clearing all state.
pub async fn restart_kernel(
    server: &NteractMcp,
    _request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
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

    // Capture kernel_type and env_source from the *current* RuntimeState
    // before shutdown. After a daemon restart the fresh RuntimeStateDoc has
    // kernel.name = "" and env_source = "", so reading it post-reconnect
    // would silently regress to "python" / "auto:uv".
    let (pre_shutdown_kernel_type, pre_shutdown_env_source) = {
        let state = handle.get_runtime_state().ok();
        let kernel_type = state
            .as_ref()
            .and_then(|s| {
                let name = &s.kernel.name;
                if name.is_empty() {
                    None
                } else {
                    Some(name.clone())
                }
            })
            .unwrap_or_else(|| "python".to_string());
        let env_source = state
            .as_ref()
            .map(|s| s.kernel.env_source.clone())
            .filter(|s| !s.is_empty());
        (kernel_type, env_source)
    };

    // Step 1: Shutdown existing kernel
    match handle
        .send_request(NotebookRequest::ShutdownKernel {})
        .await
    {
        Ok(_) | Err(_) => {
            // Even if shutdown fails (no kernel), proceed to launch
        }
    }

    // Brief pause for shutdown to complete
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Step 2: Get a fresh handle (the original may have been invalidated by
    // a daemon restart during the shutdown sequence). If the session was
    // replaced by daemon_watch's rejoin, we pick up the new one.
    let handle = {
        let guard = server.session.read().await;
        match guard.as_ref() {
            Some(s) => s.handle.clone(),
            None => {
                // Session dropped — wait for daemon_watch to rejoin.
                drop(guard);
                tokio::time::sleep(std::time::Duration::from_secs(8)).await;
                let guard = server.session.read().await;
                match guard.as_ref() {
                    Some(s) => s.handle.clone(),
                    None => {
                        return tool_error(
                            "Lost connection to daemon during kernel restart. \
                             The session will auto-reconnect — retry in a few seconds.",
                        )
                    }
                }
            }
        }
    };

    // Ensure daemon has latest metadata (deps may have changed since last sync)
    if let Err(e) = handle.confirm_sync().await {
        tracing::warn!("confirm_sync failed before restart_kernel launch: {e}");
    }

    // Step 3: Determine env_source. Prefer the pre-shutdown env_source
    // captured above — it's authoritative for the kernel that was running.
    // After shutdown (or after a daemon restart that clears RuntimeStateDoc)
    // the handle's runtime state may be empty, and detect_package_manager
    // would fall through to the Uv default, losing conda/pixi context.
    let kernel_type = pre_shutdown_kernel_type;
    let env_source = {
        use notebook_protocol::connection::{EnvSource, PackageManager};
        if let Some(ref prev) = pre_shutdown_env_source {
            // Derive the scoped auto-detect from the previous env_source,
            // preserving the package manager family. The daemon's auto:*
            // variants re-resolve through the normal launch priority
            // (project files → inline deps → captured envs → prewarmed).
            match EnvSource::parse(prev) {
                EnvSource::Prewarmed(PackageManager::Conda) => "auto:conda".to_string(),
                EnvSource::Prewarmed(PackageManager::Pixi) => "auto:pixi".to_string(),
                EnvSource::Prewarmed(PackageManager::Uv) => "auto:uv".to_string(),
                EnvSource::Deno => "deno".to_string(),
                other => {
                    // For inline, pep723, project-file etc. use the scoped
                    // auto variant matching the package manager, or fall back
                    // to the raw value.
                    if let Some(pm) = other.package_manager() {
                        match pm {
                            PackageManager::Conda => "auto:conda".to_string(),
                            PackageManager::Pixi => "auto:pixi".to_string(),
                            _ => "auto:uv".to_string(),
                        }
                    } else {
                        other.as_str().to_string()
                    }
                }
            }
        } else {
            // No pre-shutdown env_source (daemon may have been fresh).
            // Fall back to metadata-based detection.
            let detected_manager = super::deps::detect_package_manager(&handle);
            match detected_manager.as_str() {
                "pixi" => "auto:pixi".to_string(),
                "conda" => "auto:conda".to_string(),
                _ => "auto:uv".to_string(),
            }
        }
    };

    // Step 4: Launch kernel
    let notebook_path = if notebook_id.contains('/') || notebook_id.contains('\\') {
        Some(notebook_id)
    } else {
        None
    };

    let launch_result = handle
        .send_request(NotebookRequest::LaunchKernel {
            kernel_type: kernel_type.clone(),
            env_source: notebook_protocol::connection::LaunchSpec::parse(&env_source),
            notebook_path: notebook_path.clone(),
        })
        .await;

    // If LaunchKernel failed with a disconnection, the daemon may have
    // restarted. Wait for the health monitor to reconnect and retry once.
    let launch_result = match launch_result {
        Err(SyncError::Disconnected) => {
            tracing::warn!("LaunchKernel disconnected during restart, waiting for reconnection");
            tokio::time::sleep(std::time::Duration::from_secs(8)).await;
            let guard = server.session.read().await;
            match guard.as_ref() {
                Some(s) => {
                    let fresh_handle = s.handle.clone();
                    drop(guard);
                    if let Err(e) = fresh_handle.confirm_sync().await {
                        tracing::warn!(
                            "confirm_sync failed before restart_kernel retry launch: {e}"
                        );
                    }
                    fresh_handle
                        .send_request(NotebookRequest::LaunchKernel {
                            kernel_type: kernel_type.clone(),
                            env_source: notebook_protocol::connection::LaunchSpec::parse(
                                &env_source,
                            ),
                            notebook_path,
                        })
                        .await
                }
                None => Err(SyncError::Disconnected),
            }
        }
        other => other,
    };

    match launch_result {
        Ok(NotebookResponse::KernelLaunched { .. })
        | Ok(NotebookResponse::KernelAlreadyRunning { .. }) => {
            // Poll RuntimeState for kernel to become ready.
            // Re-read the session handle each iteration in case it was
            // replaced by the health monitor during reconnection.
            let start = std::time::Instant::now();
            let timeout = std::time::Duration::from_secs(120);
            loop {
                if start.elapsed() >= timeout {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                let current_handle = {
                    let guard = server.session.read().await;
                    guard.as_ref().map(|s| s.handle.clone())
                };
                let Some(h) = current_handle else {
                    continue;
                };
                if let Ok(state) = h.get_runtime_state() {
                    if matches!(
                        state.kernel.lifecycle,
                        runtime_doc::RuntimeLifecycle::Running(_)
                    ) {
                        break;
                    }
                    if matches!(state.kernel.lifecycle, runtime_doc::RuntimeLifecycle::Error) {
                        return tool_error("Kernel failed to start");
                    }
                }
            }

            let result = serde_json::json!({
                "restarted": true,
                "env_source": env_source,
            });
            tool_success(&serde_json::to_string_pretty(&result).unwrap_or_default())
        }
        Ok(NotebookResponse::Error { error }) => {
            tool_error(&format!("Failed to restart kernel: {error}"))
        }
        Ok(NotebookResponse::GuardRejected { reason }) => tool_error(&format!(
            "Kernel restart blocked by notebook trust: {reason}"
        )),
        Ok(_) => tool_success(&serde_json::json!({ "restarted": true }).to_string()),
        Err(e) => tool_error(&format!("Failed to restart kernel: {e}")),
    }
}
