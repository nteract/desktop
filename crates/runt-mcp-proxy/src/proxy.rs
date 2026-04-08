//! Core MCP proxy — spawns a child, forwards tools/resources, handles restarts.
//!
//! The `McpProxy` struct manages the child process lifecycle and provides the
//! core forwarding logic. It can be used standalone (by `mcpb-runt`) or wrapped
//! by `mcp-supervisor` which adds dev-specific tools and file watching.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, ReadResourceRequestParams, ReadResourceResult,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use tokio::sync::{mpsc, Notify, RwLock};
use tracing::{error, info, warn};

use crate::child::{self, RunningChild};
use crate::circuit_breaker::CircuitBreaker;
use crate::session;
use crate::tools::{self, ToolDivergence};
use crate::version::{self, ReconnectionEvent};

/// Configuration for the MCP proxy.
pub struct ProxyConfig {
    /// Command to spawn the child (path to `runt` or `runt-nightly`).
    pub child_command: PathBuf,
    /// Arguments to the child (e.g., `["mcp"]`).
    pub child_args: Vec<String>,
    /// Environment variables for the child process.
    pub child_env: HashMap<String, String>,
    /// Server name presented to the MCP client (e.g., "nteract", "nteract-dev").
    pub server_name: String,
    /// Directory for tool cache (optional, enables optimistic tool serving).
    pub cache_dir: Option<PathBuf>,
    /// Path to daemon info file (optional, enables version tracking).
    pub daemon_info_path: Option<PathBuf>,
}

/// Shared mutable state for the proxy.
pub struct ProxyState {
    /// rmcp client connected to the child process.
    pub child_client: Option<RunningChild>,
    /// Number of times the child has been restarted.
    pub restart_count: u32,
    /// Circuit breaker for crash detection.
    pub circuit_breaker: CircuitBreaker,
    /// Cached child tool definitions, loaded from disk at startup.
    pub cached_tools: Option<Vec<Tool>>,
    /// Last active notebook ID for auto-rejoin after restart.
    pub last_notebook_id: Option<String>,
    /// Upstream MCP client name (forwarded to child).
    pub upstream_name: String,
    /// Upstream MCP client title (forwarded to child).
    pub upstream_title: Option<String>,
    /// Last known daemon version (for detecting upgrades).
    pub last_daemon_version: Option<String>,
    /// Pending reconnection message to prepend to the next tool call.
    pub reconnection_message: Option<String>,
    /// Channel to notify that the tool list has changed.
    pub tool_list_changed_tx: Option<mpsc::Sender<()>>,
    /// Whether the proxy should exit (set on incompatible tool divergence).
    pub should_exit: bool,
}

/// The MCP proxy — manages child process lifecycle and forwards MCP calls.
#[derive(Clone)]
pub struct McpProxy {
    pub state: Arc<RwLock<ProxyState>>,
    pub config: Arc<ProxyConfig>,
    /// Signaled when the child client is first connected.
    pub child_ready: Arc<Notify>,
}

impl McpProxy {
    /// Create a new proxy. Does not spawn the child yet — call `init_child()`
    /// or use the background init pattern.
    pub fn new(config: ProxyConfig, tool_list_changed_tx: Option<mpsc::Sender<()>>) -> Self {
        let cached_tools = config
            .cache_dir
            .as_ref()
            .and_then(|dir| tools::load_cached_tools(dir));

        if let Some(ref cached) = cached_tools {
            info!("Loaded {} cached child tools from disk", cached.len());
        }

        let daemon_version = config
            .daemon_info_path
            .as_ref()
            .and_then(version::read_daemon_version);

        Self {
            state: Arc::new(RwLock::new(ProxyState {
                child_client: None,
                restart_count: 0,
                circuit_breaker: CircuitBreaker::new(),
                cached_tools,
                last_notebook_id: None,
                upstream_name: "unknown".to_string(),
                upstream_title: None,
                last_daemon_version: daemon_version,
                reconnection_message: None,
                tool_list_changed_tx,
                should_exit: false,
            })),
            config: Arc::new(config),
            child_ready: Arc::new(Notify::new()),
        }
    }

    /// Set the upstream client identity (from MCP initialize handshake).
    pub async fn set_upstream_identity(&self, name: String, title: Option<String>) {
        let mut state = self.state.write().await;
        state.upstream_name = name;
        state.upstream_title = title;
    }

    /// Spawn the child process and connect. Called during initialization.
    pub async fn init_child(&self) -> Result<(), String> {
        let (upstream_name, upstream_title) = {
            let state = self.state.read().await;
            (state.upstream_name.clone(), state.upstream_title.clone())
        };

        let client = child::spawn_child(
            &self.config.child_command,
            &self.config.child_args,
            &self.config.child_env,
            &upstream_name,
            upstream_title.as_deref(),
        )
        .await?;

        let mut state = self.state.write().await;
        state.child_client = Some(client);

        // Refresh tool cache from the new child
        self.refresh_tool_cache_locked(&mut state).await;

        // Record current daemon version
        if let Some(ref info_path) = self.config.daemon_info_path {
            state.last_daemon_version = version::read_daemon_version(info_path);
        }

        drop(state);
        self.child_ready.notify_waiters();
        info!("Child process initialized successfully");

        Ok(())
    }

    /// Restart the child process after it dies.
    ///
    /// Handles circuit breaker, version detection, session rejoin, and
    /// tool divergence detection.
    pub async fn restart_child(&self) -> Result<(), String> {
        // Phase 1: Drop old client, check circuit breaker
        let child_was_dead = {
            let mut state = self.state.write().await;
            let was_dead = state.child_client.is_none();

            if let Some(old) = state.child_client.take() {
                let _ = old.cancel().await;
            }

            info!(
                "Restarting child (restart #{}{})",
                state.restart_count + 1,
                if was_dead { ", child had exited" } else { "" }
            );

            // Skip circuit breaker if child exited on its own (daemon upgrade)
            if !was_dead && !state.circuit_breaker.record_crash() {
                let msg = "The nteract MCP server failed after repeated restarts. \
                           Try restarting the nteract app, or restart your Claude session \
                           so a fresh MCP connection is established. You may also need to \
                           reinstall the nteract extension.";
                state.reconnection_message = Some(msg.to_string());
                return Err(msg.to_string());
            }

            was_dead
        };

        // Phase 2: Backoff (skip for daemon upgrade exits)
        if !child_was_dead {
            tokio::time::sleep(Duration::from_secs(2)).await;
        }

        // Snapshot the daemon version before restart
        let old_version = {
            let state = self.state.read().await;
            state.last_daemon_version.clone()
        };

        // Phase 3: Spawn new child
        let (upstream_name, upstream_title) = {
            let state = self.state.read().await;
            (state.upstream_name.clone(), state.upstream_title.clone())
        };

        match child::spawn_child(
            &self.config.child_command,
            &self.config.child_args,
            &self.config.child_env,
            &upstream_name,
            upstream_title.as_deref(),
        )
        .await
        {
            Ok(client) => {
                let mut state = self.state.write().await;
                state.child_client = Some(client);
                state.restart_count += 1;

                // Refresh tool cache and check for divergence
                let old_tools = state.cached_tools.clone();
                self.refresh_tool_cache_locked(&mut state).await;

                if let (Some(ref old), Some(ref new)) = (&old_tools, &state.cached_tools) {
                    match tools::detect_divergence(old, new) {
                        ToolDivergence::Same => {}
                        ToolDivergence::Superset { ref added } => {
                            info!("New tools available after restart: {added:?}");
                            if let Some(ref tx) = state.tool_list_changed_tx {
                                let _ = tx.send(()).await;
                            }
                        }
                        ToolDivergence::Incompatible {
                            ref removed,
                            ref added,
                        } => {
                            warn!(
                                "Tool list incompatible after restart — removed: {removed:?}, added: {added:?}. \
                                 Exiting so the MCP client can restart with the new tool set."
                            );
                            state.should_exit = true;
                            // Don't return error — let the proxy exit cleanly
                        }
                    }
                }

                // Detect version change
                if let Some(ref info_path) = self.config.daemon_info_path {
                    let new_version = version::read_daemon_version(info_path);
                    state.last_daemon_version = new_version;
                }

                let reconnection_event = match version::detect_version_change(
                    self.config
                        .daemon_info_path
                        .as_ref()
                        .unwrap_or(&PathBuf::new()),
                    old_version.as_deref(),
                ) {
                    Some(event) => Some(event),
                    None => Some(ReconnectionEvent::ChildRestart {
                        session_rejoined: false,
                    }),
                };

                // Auto-rejoin session
                let mut session_rejoined = false;
                if let Some(ref notebook_id) = state.last_notebook_id.clone() {
                    if let Some(ref client) = state.child_client {
                        session_rejoined = session::auto_rejoin(client, notebook_id).await;
                        if !session_rejoined {
                            state.last_notebook_id = None;
                        }
                    }
                }

                // Update the reconnection event with session status
                if let Some(event) = reconnection_event {
                    let msg = match event {
                        ReconnectionEvent::DaemonUpgrade {
                            old_version,
                            new_version,
                            ..
                        } => ReconnectionEvent::DaemonUpgrade {
                            old_version,
                            new_version,
                            session_rejoined,
                        }
                        .message(),
                        ReconnectionEvent::ChildRestart { .. } => {
                            ReconnectionEvent::ChildRestart { session_rejoined }.message()
                        }
                    };
                    state.reconnection_message = Some(msg);
                }

                self.child_ready.notify_waiters();
                info!("Child restarted successfully");
                Ok(())
            }
            Err(e) => {
                let mut state = self.state.write().await;
                state.reconnection_message = Some(format!("Child restart failed: {e}"));
                error!("Failed to restart child: {e}");
                Err(e)
            }
        }
    }

    /// Forward a tool call to the child, restarting if the child has disconnected.
    ///
    /// Prepends any pending reconnection message to the result.
    pub async fn forward_tool_call(
        &self,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, McpError> {
        // First attempt
        match self.try_forward_tool_call(&params).await {
            Ok(mut result) => {
                self.track_session(&params, &result).await;
                self.prepend_reconnection_message(&mut result).await;
                return Ok(result);
            }
            Err(e) => {
                let state = self.state.read().await;
                if state.child_client.is_some() {
                    warn!("Tool call failed but child still connected, not restarting: {e}");
                    return Err(e);
                }
                warn!("Tool call failed and child disconnected, attempting restart: {e}");
            }
        }

        // Child is gone — restart and retry once
        if let Err(e) = self.restart_child().await {
            return Err(McpError::internal_error(
                format!("Child restart failed: {e}"),
                None,
            ));
        }

        // Check if we should exit due to tool divergence
        {
            let state = self.state.read().await;
            if state.should_exit {
                return Err(McpError::internal_error(
                    "Tool list changed incompatibly after daemon upgrade. \
                     The MCP server will exit so your client can reconnect with the updated tools. \
                     You may need to reinstall the nteract extension.",
                    None,
                ));
            }
        }

        // Second attempt after restart
        let mut result = self.try_forward_tool_call(&params).await?;
        self.track_session(&params, &result).await;
        self.prepend_reconnection_message(&mut result).await;
        Ok(result)
    }

    /// Forward a resource read to the child, restarting if disconnected.
    pub async fn forward_read_resource(
        &self,
        params: ReadResourceRequestParams,
    ) -> Result<ReadResourceResult, McpError> {
        match self.try_forward_read_resource(&params).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                let state = self.state.read().await;
                if state.child_client.is_some() {
                    return Err(e);
                }
                warn!("Resource read failed and child disconnected, attempting restart: {e}");
            }
        }

        if let Err(e) = self.restart_child().await {
            return Err(McpError::internal_error(
                format!("Child restart failed: {e}"),
                None,
            ));
        }

        self.try_forward_read_resource(&params).await
    }

    /// Get the current child tools (live from child, or cached).
    pub async fn child_tools(&self) -> Vec<Tool> {
        let state = self.state.read().await;
        if let Some(ref client) = state.child_client {
            match client.list_tools(None).await {
                Ok(result) => return result.tools,
                Err(e) => {
                    warn!("Failed to list child tools: {e}");
                }
            }
        }
        // Fall back to cache
        state.cached_tools.clone().unwrap_or_default()
    }

    /// List child resources (pass-through).
    pub async fn child_resources(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
    ) -> ListResourcesResult {
        let state = self.state.read().await;
        if let Some(ref client) = state.child_client {
            match client.list_resources(request).await {
                Ok(result) => return result,
                Err(e) => warn!("Failed to list child resources: {e}"),
            }
        }
        ListResourcesResult::default()
    }

    /// List child resource templates (pass-through).
    pub async fn child_resource_templates(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
    ) -> ListResourceTemplatesResult {
        let state = self.state.read().await;
        if let Some(ref client) = state.child_client {
            match client.list_resource_templates(request).await {
                Ok(result) => return result,
                Err(e) => warn!("Failed to list child resource templates: {e}"),
            }
        }
        ListResourceTemplatesResult::default()
    }

    /// Check whether the proxy should exit (due to incompatible tool divergence).
    pub async fn should_exit(&self) -> bool {
        self.state.read().await.should_exit
    }

    /// Reset the circuit breaker (used by supervisor after manual restart or file change).
    pub async fn reset_circuit_breaker(&self) {
        self.state.write().await.circuit_breaker.reset();
    }

    // ── Internal helpers ──────────────────────────────────────────────

    async fn try_forward_tool_call(
        &self,
        params: &CallToolRequestParams,
    ) -> Result<CallToolResult, McpError> {
        let state = self.state.read().await;
        let client = state
            .child_client
            .as_ref()
            .ok_or_else(|| McpError::internal_error("nteract MCP server not running", None))?;

        client
            .call_tool(params.clone())
            .await
            .map_err(|e| McpError::internal_error(format!("Child tool call failed: {e}"), None))
    }

    async fn try_forward_read_resource(
        &self,
        params: &ReadResourceRequestParams,
    ) -> Result<ReadResourceResult, McpError> {
        let state = self.state.read().await;
        let client = state
            .child_client
            .as_ref()
            .ok_or_else(|| McpError::internal_error("nteract MCP server not running", None))?;

        client
            .read_resource(params.clone())
            .await
            .map_err(|e| McpError::internal_error(format!("Child resource read failed: {e}"), None))
    }

    async fn track_session(&self, params: &CallToolRequestParams, result: &CallToolResult) {
        if let Some(id) = session::extract_session_id(params, result) {
            info!("Tracking active notebook session: {id}");
            self.state.write().await.last_notebook_id = Some(id);
        }
    }

    async fn prepend_reconnection_message(&self, result: &mut CallToolResult) {
        let mut state = self.state.write().await;
        if let Some(msg) = state.reconnection_message.take() {
            // Prepend the reconnection notice before the actual tool result
            result
                .content
                .insert(0, Content::text(format!("[nteract] {msg}\n")));
        }
    }

    async fn refresh_tool_cache_locked(&self, state: &mut ProxyState) {
        if let Some(ref client) = state.child_client {
            match client.list_tools(None).await {
                Ok(child_tools) => {
                    if let Some(ref cache_dir) = self.config.cache_dir {
                        tools::save_tool_cache(cache_dir, &child_tools.tools);
                    }
                    state.cached_tools = Some(child_tools.tools);
                }
                Err(e) => {
                    warn!("Failed to refresh tool cache: {e}");
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ServerHandler — used when McpProxy is the top-level MCP server (mcpb-runt)
// ---------------------------------------------------------------------------

/// Standalone ServerHandler for `McpProxy`. Used by `mcpb-runt` where the proxy
/// IS the MCP server. The supervisor wraps this with its own ServerHandler that
/// adds supervisor_* tools.
impl ServerHandler for McpProxy {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .enable_resources_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new(
            &self.config.server_name,
            env!("CARGO_PKG_VERSION"),
        ))
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        // Wait briefly for child if not ready
        let notified = self.child_ready.notified();
        let needs_wait = self.state.read().await.child_client.is_none();
        if needs_wait {
            info!("list_tools called before child ready, waiting...");
            let _ = tokio::time::timeout(Duration::from_secs(30), notified).await;
        }

        let tools = self.child_tools().await;
        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn list_resources(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        Ok(self.child_resources(request).await)
    }

    async fn list_resource_templates(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        Ok(self.child_resource_templates(request).await)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResult, McpError> {
        self.forward_read_resource(request).await
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // Wait for child if not ready
        let notified = self.child_ready.notified();
        let needs_wait = self.state.read().await.child_client.is_none();
        if needs_wait {
            info!(
                "Tool '{}' called before child ready, waiting...",
                request.name
            );
            let _ = tokio::time::timeout(Duration::from_secs(60), notified).await;
        }

        self.forward_tool_call(request).await
    }
}
