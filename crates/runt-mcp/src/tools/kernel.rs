//! Kernel management tools: interrupt_kernel, restart_kernel.

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ErrorData as McpError;

use notebook_protocol::protocol::{NotebookRequest, NotebookResponse};

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
                return tool_error(
                    "No active notebook session. Call open_notebook or create_notebook first.",
                )
            }
        }
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

    // Ensure daemon has latest metadata (deps may have changed since last sync)
    if let Err(e) = handle.confirm_sync().await {
        tracing::warn!("confirm_sync failed before restart_kernel launch: {e}");
    }

    // Step 2: Determine kernel type and env_source.
    // Use metadata-based detection (not RuntimeState env_source) to scope the
    // auto-detect. This ensures the correct package manager pool is used even
    // if the previous kernel was launched with a different env (e.g., UV default
    // when the notebook metadata says pixi). See #1605.
    let (kernel_type, env_source) = {
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
        // Scope auto-detect based on notebook metadata, not stale env_source
        let detected_manager = super::deps::detect_package_manager(&handle);
        let env_source = match detected_manager.as_str() {
            "pixi" => "auto:pixi".to_string(),
            "conda" => "auto:conda".to_string(),
            _ => "auto:uv".to_string(),
        };
        (kernel_type, env_source)
    };

    // Step 3: Launch kernel

    let notebook_path = if notebook_id.contains('/') || notebook_id.contains('\\') {
        Some(notebook_id)
    } else {
        None
    };

    match handle
        .send_request(NotebookRequest::LaunchKernel {
            kernel_type: kernel_type.clone(),
            env_source: env_source.clone(),
            notebook_path,
        })
        .await
    {
        Ok(NotebookResponse::KernelLaunched { .. })
        | Ok(NotebookResponse::KernelAlreadyRunning { .. }) => {
            // Poll RuntimeState for kernel to become ready
            let start = std::time::Instant::now();
            let timeout = std::time::Duration::from_secs(120);
            loop {
                if start.elapsed() >= timeout {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                if let Ok(state) = handle.get_runtime_state() {
                    if state.kernel.status == "idle" || state.kernel.status == "busy" {
                        break;
                    }
                    if state.kernel.status == "error" {
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
        Ok(_) => tool_success(&serde_json::json!({ "restarted": true }).to_string()),
        Err(e) => tool_error(&format!("Failed to restart kernel: {e}")),
    }
}
