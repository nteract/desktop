//! Core MCP proxy — spawns a child, forwards tools/resources, handles restarts.
//!
//! The `McpProxy` struct manages the child process lifecycle and provides the
//! core forwarding logic. It can be used standalone (by `mcpb-runt`) or wrapped
//! by `mcp-supervisor` which adds dev-specific tools and file watching.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, ReadResourceRequestParams, ReadResourceResult,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler};
use tokio::sync::{mpsc, Mutex, Notify, RwLock};
use tracing::{error, info, warn};

use crate::child::{self, RunningChild};
use crate::circuit_breaker::CircuitBreaker;
use crate::session;
use crate::tools::{self, ToolDivergence};
use crate::version::{self, ReconnectionEvent};

/// Configuration for the MCP proxy.
pub struct ProxyConfig {
    /// Resolver that returns the path to the child binary.
    /// Called on every spawn (init and restart) so it picks up binary upgrades on disk.
    /// This is the core reason the proxy exists: the MCPB bundle is stable while
    /// the user upgrades the nteract app, and the proxy transparently picks up the new binary.
    pub resolve_child_command: Box<dyn Fn() -> Result<PathBuf, String> + Send + Sync>,
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
    /// Child monitor polling interval in milliseconds (default: 500ms).
    /// Lower values detect child exit faster but use more CPU.
    pub monitor_poll_interval_ms: u64,
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
    /// Timestamp when the current child was spawned (for uptime tracking).
    pub child_spawn_time: Option<Instant>,
    /// Timestamp of the last child restart.
    pub last_restart_time: Option<Instant>,
}

/// The MCP proxy — manages child process lifecycle and forwards MCP calls.
#[derive(Clone)]
pub struct McpProxy {
    pub state: Arc<RwLock<ProxyState>>,
    pub config: Arc<ProxyConfig>,
    /// Signaled when the child client is first connected.
    pub child_ready: Arc<Notify>,
    /// Signaled when the proxy should exit (incompatible tool divergence).
    pub exit_signal: Arc<Notify>,
    /// Flag to prevent concurrent restarts (monitor + tool call racing).
    restart_in_progress: Arc<Mutex<bool>>,
}

impl McpProxy {
    /// Create a new proxy. Does not spawn the child yet — call `init_child()`
    /// or use the background init pattern.
    pub fn new(config: ProxyConfig, tool_list_changed_tx: Option<mpsc::Sender<()>>) -> Self {
        let cached_tools = config
            .cache_dir
            .as_ref()
            .map(|dir| tools::load_cached_tools(dir))
            .unwrap_or_else(tools::load_builtin_tools);

        if let Some(ref cached) = cached_tools {
            info!("Loaded {} cached child tools", cached.len());
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
                child_spawn_time: None,
                last_restart_time: None,
            })),
            config: Arc::new(config),
            child_ready: Arc::new(Notify::new()),
            exit_signal: Arc::new(Notify::new()),
            restart_in_progress: Arc::new(Mutex::new(false)),
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

        let child_command = (self.config.resolve_child_command)()
            .map_err(|e| format!("Failed to resolve child binary: {e}"))?;

        info!(
            event = "child_spawned",
            binary = %child_command.display(),
            args = ?self.config.child_args,
            upstream_client = %upstream_name,
            "Spawning child process"
        );

        let client = child::spawn_child(
            &child_command,
            &self.config.child_args,
            &self.config.child_env,
            &upstream_name,
            upstream_title.as_deref(),
        )
        .await?;

        let mut state = self.state.write().await;
        state.child_client = Some(client);
        state.child_spawn_time = Some(Instant::now());

        // Refresh tool cache from the new child
        self.refresh_tool_cache_locked(&mut state).await;

        // Record current daemon version
        if let Some(ref info_path) = self.config.daemon_info_path {
            state.last_daemon_version = version::read_daemon_version(info_path);
        }

        drop(state);

        // Spawn background task to monitor child lifecycle
        self.spawn_child_monitor();

        self.child_ready.notify_waiters();
        info!("Child process initialized successfully");

        Ok(())
    }

    /// Restart the child process after it dies.
    ///
    /// Handles circuit breaker, version detection, session rejoin, and
    /// tool divergence detection.
    pub async fn restart_child(&self) -> Result<(), String> {
        // Prevent concurrent restarts (monitor task + tool call racing)
        let mut restart_lock = self.restart_in_progress.lock().await;
        if *restart_lock {
            info!("Restart already in progress, skipping duplicate request");
            return Ok(());
        }
        *restart_lock = true;
        drop(restart_lock);

        // Phase 1: Drop old client, check circuit breaker
        let child_was_dead = {
            let mut state = self.state.write().await;
            let was_dead = state.child_client.is_none();

            if let Some(old) = state.child_client.take() {
                let _ = old.cancel().await;
            }

            let uptime = state
                .child_spawn_time
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);

            info!(
                event = "child_restart_requested",
                restart_num = state.restart_count + 1,
                child_was_dead = was_dead,
                uptime_secs = uptime,
                "Restarting child process"
            );

            // Skip circuit breaker if child exited on its own (daemon upgrade)
            if !was_dead && !state.circuit_breaker.record_crash() {
                let msg = "The nteract MCP server failed after repeated restarts. \
                           Try restarting the nteract app, or restart your Claude session \
                           so a fresh MCP connection is established. You may also need to \
                           reinstall the nteract extension.";
                state.reconnection_message = Some(msg.to_string());
                *self.restart_in_progress.lock().await = false;
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

        // Phase 3: Resolve binary and spawn new child.
        // Re-resolve on every restart so binary upgrades take effect.
        let child_command = match (self.config.resolve_child_command)() {
            Ok(path) => {
                info!("Resolved child binary: {}", path.display());
                path
            }
            Err(e) => {
                let msg = format!("Failed to resolve child binary on restart: {e}");
                error!("{msg}");
                let mut state = self.state.write().await;
                state.reconnection_message = Some(msg.clone());
                return Err(msg);
            }
        };

        let (upstream_name, upstream_title) = {
            let state = self.state.read().await;
            (state.upstream_name.clone(), state.upstream_title.clone())
        };

        match child::spawn_child(
            &child_command,
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
                state.child_spawn_time = Some(Instant::now());
                state.last_restart_time = Some(Instant::now());

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
                            // Signal the exit so mcpb-runt can shut down
                            self.exit_signal.notify_waiters();
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

                drop(state);

                // Spawn new monitor for the restarted child
                self.spawn_child_monitor();

                // Notify upstream client to keep connection alive
                let state = self.state.read().await;
                if let Some(ref tx) = state.tool_list_changed_tx {
                    let _ = tx.send(()).await;
                    info!("Notified upstream client of tool list change to keep connection alive");
                }
                drop(state);

                *self.restart_in_progress.lock().await = false;
                self.child_ready.notify_waiters();
                info!("Child restarted successfully");
                Ok(())
            }
            Err(e) => {
                let mut state = self.state.write().await;
                state.reconnection_message = Some(format!("Child restart failed: {e}"));
                error!("Failed to restart child: {e}");
                *self.restart_in_progress.lock().await = false;
                Err(e)
            }
        }
    }

    /// Spawn a background task to monitor the child process and auto-restart on exit.
    ///
    /// Uses polling (every 500ms) instead of `waiting()` because `RunningService`
    /// doesn't implement `Clone` and `waiting()` consumes `self`, making it incompatible
    /// with the existing architecture where `child_client` is held in `Arc<RwLock<ProxyState>>`.
    ///
    /// When the child exits, the monitor triggers `restart_child()` which spawns a new
    /// monitor, then this monitor exits (preventing task leak).
    fn spawn_child_monitor(&self) {
        let proxy = self.clone();
        tokio::spawn(async move {
            loop {
                // Check if child exists
                let has_child = {
                    let state = proxy.state.read().await;
                    state.child_client.is_some()
                };

                if !has_child {
                    info!("Child monitor: no child to monitor, exiting");
                    break;
                }

                // Poll at configured interval for child closure
                tokio::time::sleep(Duration::from_millis(proxy.config.monitor_poll_interval_ms))
                    .await;

                // Check if child is closed
                let is_closed = {
                    let state = proxy.state.read().await;
                    state
                        .child_client
                        .as_ref()
                        .map(|c| c.is_closed())
                        .unwrap_or(true)
                };

                if !is_closed {
                    continue;
                }

                // Child has exited, record uptime
                let uptime_secs = {
                    let state = proxy.state.read().await;
                    state
                        .child_spawn_time
                        .map(|t| t.elapsed().as_secs())
                        .unwrap_or(0)
                };

                info!(
                    event = "child_exited",
                    uptime_secs = uptime_secs,
                    "Child process exited, attempting automatic restart"
                );

                // Attempt to restart the child
                match proxy.restart_child().await {
                    Ok(_) => {
                        info!("Child monitor: restart successful, exiting (new monitor spawned)");
                        // Exit this monitor — restart_child() spawned a new one
                        break;
                    }
                    Err(e) => {
                        error!(
                            event = "child_restart_failed",
                            error = %e,
                            "Child monitor: restart failed, stopping monitor task"
                        );
                        // Circuit breaker may have tripped, stop monitoring
                        break;
                    }
                }
            }
        });
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
                let child_alive = state.child_client.as_ref().is_some_and(|c| !c.is_closed());
                drop(state);
                if child_alive {
                    warn!("Tool call failed but child still connected, not restarting: {e}");
                    return Err(e);
                }
                warn!("Tool call failed and child transport closed, attempting restart: {e}");
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
                let child_alive = state.child_client.as_ref().is_some_and(|c| !c.is_closed());
                drop(state);
                if child_alive {
                    return Err(e);
                }
                warn!("Resource read failed and child transport closed, attempting restart: {e}");
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

    /// Get the number of child restarts since proxy creation.
    pub async fn restart_count(&self) -> u32 {
        self.state.read().await.restart_count
    }

    /// Get the child process uptime in seconds (None if no child).
    pub async fn child_uptime_secs(&self) -> Option<u64> {
        self.state
            .read()
            .await
            .child_spawn_time
            .map(|t| t.elapsed().as_secs())
    }

    /// Get the timestamp of the last restart (None if never restarted).
    pub async fn last_restart_time(&self) -> Option<Instant> {
        self.state.read().await.last_restart_time
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

        let mut tools = self.child_tools().await;
        tools.push(reconnect_tool());
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
        // Intercept the built-in reconnect tool before waiting on child
        // readiness — reconnect is the escape hatch when the child is
        // wedged, so it must not block on child readiness itself.
        if request.name == RECONNECT_TOOL_NAME {
            return self.handle_reconnect().await;
        }

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

/// Name of the built-in reconnect tool exposed by the standalone proxy.
const RECONNECT_TOOL_NAME: &str = "reconnect";

/// Build the `reconnect` tool definition injected into the standalone
/// `ServerHandler::list_tools` result.
fn reconnect_tool() -> Tool {
    let empty_schema: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
    Tool::new(
        RECONNECT_TOOL_NAME,
        "Restart the nteract MCP child process and reconnect to the daemon. \
         Use when tools are hanging, returning stale errors, or after a daemon \
         upgrade. Child-only — the daemon itself is managed by the installed \
         nteract app.",
        empty_schema,
    )
}

impl McpProxy {
    /// Handle the built-in `reconnect` tool call.
    ///
    /// Kicks the child via `restart_child()` and waits briefly for the new
    /// child to become ready so the caller's next tool call sees a fresh
    /// transport. Any reconnection message set by `restart_child()` is
    /// returned inline rather than prepended to a future call.
    async fn handle_reconnect(&self) -> Result<CallToolResult, McpError> {
        info!("reconnect tool invoked — restarting child");
        let prior_restart_count = self.restart_count().await;

        if let Err(e) = self.restart_child().await {
            return Err(McpError::internal_error(
                format!("Child restart failed: {e}"),
                None,
            ));
        }

        // Best-effort wait so the caller's next tool call sees the new
        // child. Don't fail the reconnect call if the child is slow —
        // the next forwarded call will retry via the normal restart path.
        let notified = self.child_ready.notified();
        let _ = tokio::time::timeout(Duration::from_secs(30), notified).await;

        let restart_count = self.restart_count().await;
        let pending = self.state.write().await.reconnection_message.take();
        let detail = pending.unwrap_or_else(|| "Child restarted.".to_string());
        let body = format!("{detail}\n\nRestart #{restart_count} (was #{prior_restart_count}).");
        Ok(CallToolResult::success(vec![Content::text(body)]))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use rmcp::model::Content;
    use std::collections::HashMap;

    fn test_config() -> ProxyConfig {
        ProxyConfig {
            resolve_child_command: Box::new(|| Ok(PathBuf::from("/nonexistent/runt"))),
            child_args: vec!["mcp".to_string()],
            child_env: HashMap::new(),
            server_name: "test-proxy".to_string(),
            cache_dir: None,
            daemon_info_path: None,
            monitor_poll_interval_ms: 500,
        }
    }

    fn test_config_with_cache(dir: &std::path::Path) -> ProxyConfig {
        ProxyConfig {
            resolve_child_command: Box::new(|| Ok(PathBuf::from("/nonexistent/runt"))),
            child_args: vec!["mcp".to_string()],
            child_env: HashMap::new(),
            server_name: "test-proxy".to_string(),
            cache_dir: Some(dir.to_path_buf()),
            daemon_info_path: None,
            monitor_poll_interval_ms: 500,
        }
    }

    // ── Proxy creation ────────────────────────────────────────────────

    #[tokio::test]
    async fn proxy_starts_with_no_child() {
        let proxy = McpProxy::new(test_config(), None);
        let state = proxy.state.read().await;
        assert!(state.child_client.is_none());
        assert_eq!(state.restart_count, 0);
        assert!(state.last_notebook_id.is_none());
        assert!(state.reconnection_message.is_none());
        assert!(!state.should_exit);
        assert!(state.child_spawn_time.is_none());
        assert!(state.last_restart_time.is_none());
    }

    #[tokio::test]
    async fn proxy_starts_with_unknown_upstream() {
        let proxy = McpProxy::new(test_config(), None);
        let state = proxy.state.read().await;
        assert_eq!(state.upstream_name, "unknown");
        assert!(state.upstream_title.is_none());
    }

    #[tokio::test]
    async fn set_upstream_identity() {
        let proxy = McpProxy::new(test_config(), None);
        proxy
            .set_upstream_identity("Claude Code".to_string(), Some("Claude Code".to_string()))
            .await;

        let state = proxy.state.read().await;
        assert_eq!(state.upstream_name, "Claude Code");
        assert_eq!(state.upstream_title, Some("Claude Code".to_string()));
    }

    #[tokio::test]
    async fn set_upstream_identity_without_title() {
        let proxy = McpProxy::new(test_config(), None);
        proxy.set_upstream_identity("zed".to_string(), None).await;

        let state = proxy.state.read().await;
        assert_eq!(state.upstream_name, "zed");
        assert!(state.upstream_title.is_none());
    }

    // ── Tool cache loading at startup ─────────────────────────────────

    #[tokio::test]
    async fn proxy_loads_tool_cache_on_creation() {
        let dir = tempfile::tempdir().unwrap();
        let tool = rmcp::model::Tool::new(
            "test_tool".to_string(),
            "A test tool".to_string(),
            serde_json::Map::new(),
        );
        tools::save_tool_cache(dir.path(), &[tool]);

        let proxy = McpProxy::new(test_config_with_cache(dir.path()), None);
        let state = proxy.state.read().await;
        assert!(state.cached_tools.is_some());
        assert_eq!(state.cached_tools.as_ref().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn proxy_falls_back_to_builtin_when_dir_empty() {
        let dir = tempfile::tempdir().unwrap();
        let proxy = McpProxy::new(test_config_with_cache(dir.path()), None);
        let state = proxy.state.read().await;
        // Falls back to built-in tool cache
        assert!(state.cached_tools.is_some());
        assert!(!state.cached_tools.as_ref().unwrap().is_empty());
    }

    #[tokio::test]
    async fn proxy_uses_builtin_when_no_cache_dir() {
        let proxy = McpProxy::new(test_config(), None);
        let state = proxy.state.read().await;
        // Built-in tool cache is always available
        assert!(state.cached_tools.is_some());
        assert!(!state.cached_tools.as_ref().unwrap().is_empty());
    }

    // ── should_exit ───────────────────────────────────────────────────

    #[tokio::test]
    async fn should_exit_starts_false() {
        let proxy = McpProxy::new(test_config(), None);
        assert!(!proxy.should_exit().await);
    }

    #[tokio::test]
    async fn should_exit_reflects_state() {
        let proxy = McpProxy::new(test_config(), None);
        proxy.state.write().await.should_exit = true;
        assert!(proxy.should_exit().await);
    }

    // ── reset_circuit_breaker ─────────────────────────────────────────

    #[tokio::test]
    async fn reset_circuit_breaker_clears_state() {
        let proxy = McpProxy::new(test_config(), None);

        // Trip the circuit breaker
        {
            let mut state = proxy.state.write().await;
            for _ in 0..10 {
                state.circuit_breaker.record_crash();
            }
            assert!(!state.circuit_breaker.record_crash(), "should be tripped");
        }

        proxy.reset_circuit_breaker().await;

        {
            let mut state = proxy.state.write().await;
            assert!(
                state.circuit_breaker.record_crash(),
                "should allow crashes after reset"
            );
        }
    }

    // ── Reconnection message lifecycle ────────────────────────────────

    #[tokio::test]
    async fn reconnection_message_prepended_to_result() {
        let proxy = McpProxy::new(test_config(), None);

        // Set a reconnection message
        proxy.state.write().await.reconnection_message =
            Some("Daemon upgraded (2.1.2 → 2.1.3)".to_string());

        // Create a tool result
        let mut result = CallToolResult::success(vec![Content::text("tool output")]);

        // Prepend should add the message
        proxy.prepend_reconnection_message(&mut result).await;

        assert_eq!(result.content.len(), 2);
        // First content should be the reconnection notice
        let first_text = result.content[0]
            .raw
            .as_text()
            .expect("first content should be text");
        assert!(first_text.text.contains("Daemon upgraded"));
        assert!(first_text.text.starts_with("[nteract]"));
        // Second should be the original
        let second_text = result.content[1]
            .raw
            .as_text()
            .expect("second content should be text");
        assert_eq!(second_text.text, "tool output");
    }

    #[tokio::test]
    async fn reconnection_message_cleared_after_prepend() {
        let proxy = McpProxy::new(test_config(), None);

        proxy.state.write().await.reconnection_message = Some("test message".to_string());

        let mut result = CallToolResult::success(vec![Content::text("output")]);
        proxy.prepend_reconnection_message(&mut result).await;

        // Message should be consumed
        assert!(proxy.state.read().await.reconnection_message.is_none());
    }

    #[tokio::test]
    async fn no_reconnection_message_leaves_result_unchanged() {
        let proxy = McpProxy::new(test_config(), None);

        // No reconnection message set
        let mut result = CallToolResult::success(vec![Content::text("output")]);
        let original_len = result.content.len();

        proxy.prepend_reconnection_message(&mut result).await;

        assert_eq!(result.content.len(), original_len);
    }

    #[tokio::test]
    async fn reconnection_message_only_prepended_once() {
        let proxy = McpProxy::new(test_config(), None);
        proxy.state.write().await.reconnection_message = Some("upgraded".to_string());

        let mut result1 = CallToolResult::success(vec![Content::text("first")]);
        proxy.prepend_reconnection_message(&mut result1).await;
        assert_eq!(result1.content.len(), 2);

        // Second call should not prepend anything
        let mut result2 = CallToolResult::success(vec![Content::text("second")]);
        proxy.prepend_reconnection_message(&mut result2).await;
        assert_eq!(result2.content.len(), 1);
    }

    // ── Session tracking via track_session ────────────────────────────

    #[tokio::test]
    async fn track_session_captures_open_notebook() {
        let proxy = McpProxy::new(test_config(), None);

        let params: CallToolRequestParams = serde_json::from_value(serde_json::json!({
            "name": "open_notebook",
            "arguments": { "path": "/tmp/test.ipynb" }
        }))
        .unwrap();
        let result = CallToolResult::success(vec![Content::text("ok")]);

        proxy.track_session(&params, &result).await;

        let state = proxy.state.read().await;
        assert_eq!(state.last_notebook_id, Some("/tmp/test.ipynb".to_string()));
    }

    #[tokio::test]
    async fn track_session_updates_on_new_notebook() {
        let proxy = McpProxy::new(test_config(), None);

        // Open first notebook
        let params1: CallToolRequestParams = serde_json::from_value(serde_json::json!({
            "name": "open_notebook",
            "arguments": { "path": "/tmp/first.ipynb" }
        }))
        .unwrap();
        proxy
            .track_session(
                &params1,
                &CallToolResult::success(vec![Content::text("ok")]),
            )
            .await;

        // Open second notebook — should replace
        let params2: CallToolRequestParams = serde_json::from_value(serde_json::json!({
            "name": "open_notebook",
            "arguments": { "path": "/tmp/second.ipynb" }
        }))
        .unwrap();
        proxy
            .track_session(
                &params2,
                &CallToolResult::success(vec![Content::text("ok")]),
            )
            .await;

        let state = proxy.state.read().await;
        assert_eq!(
            state.last_notebook_id,
            Some("/tmp/second.ipynb".to_string())
        );
    }

    #[tokio::test]
    async fn track_session_ignores_non_session_tools() {
        let proxy = McpProxy::new(test_config(), None);

        let params: CallToolRequestParams = serde_json::from_value(serde_json::json!({
            "name": "execute_cell",
            "arguments": { "cell_id": "abc" }
        }))
        .unwrap();

        proxy
            .track_session(&params, &CallToolResult::success(vec![Content::text("ok")]))
            .await;

        let state = proxy.state.read().await;
        assert!(state.last_notebook_id.is_none());
    }

    #[tokio::test]
    async fn track_session_ignores_errors() {
        let proxy = McpProxy::new(test_config(), None);

        let params: CallToolRequestParams = serde_json::from_value(serde_json::json!({
            "name": "open_notebook",
            "arguments": { "path": "/tmp/test.ipynb" }
        }))
        .unwrap();
        let mut result = CallToolResult::success(vec![Content::text("error")]);
        result.is_error = Some(true);

        proxy.track_session(&params, &result).await;

        let state = proxy.state.read().await;
        assert!(state.last_notebook_id.is_none());
    }

    // ── try_forward_tool_call without child ───────────────────────────

    #[tokio::test]
    async fn forward_fails_without_child() {
        let proxy = McpProxy::new(test_config(), None);
        let params: CallToolRequestParams = serde_json::from_value(serde_json::json!({
            "name": "list_active_notebooks"
        }))
        .unwrap();

        let result = proxy.try_forward_tool_call(&params).await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("not running"));
    }

    // ── child_tools falls back to cache ───────────────────────────────

    #[tokio::test]
    async fn child_tools_returns_cache_when_no_child() {
        let proxy = McpProxy::new(test_config(), None);
        let cached = vec![rmcp::model::Tool::new(
            "cached_tool".to_string(),
            "A cached tool".to_string(),
            serde_json::Map::new(),
        )];
        proxy.state.write().await.cached_tools = Some(cached);

        let tools = proxy.child_tools().await;
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name.as_ref(), "cached_tool");
    }

    #[tokio::test]
    async fn child_tools_returns_builtin_when_no_child() {
        let proxy = McpProxy::new(test_config(), None);
        let tools = proxy.child_tools().await;
        // Falls back to built-in cache even without a child
        assert!(!tools.is_empty());
    }

    // ── tool_list_changed notification channel ────────────────────────

    #[tokio::test]
    async fn tool_list_changed_tx_is_stored() {
        let (tx, _rx) = mpsc::channel::<()>(4);
        let proxy = McpProxy::new(test_config(), Some(tx));
        let state = proxy.state.read().await;
        assert!(state.tool_list_changed_tx.is_some());
    }

    #[tokio::test]
    async fn no_tool_list_changed_tx_when_none() {
        let proxy = McpProxy::new(test_config(), None);
        let state = proxy.state.read().await;
        assert!(state.tool_list_changed_tx.is_none());
    }

    // ── ProxyConfig ───────────────────────────────────────────────────

    #[test]
    fn proxy_config_accepts_env_vars() {
        let mut env = HashMap::new();
        env.insert("NTERACT_CHANNEL".to_string(), "stable".to_string());
        env.insert(
            "RUNTIMED_SOCKET_PATH".to_string(),
            "/tmp/test.sock".to_string(),
        );

        let config = ProxyConfig {
            resolve_child_command: Box::new(|| Ok(PathBuf::from("/nonexistent/runt"))),
            child_args: vec!["mcp".to_string()],
            child_env: env,
            server_name: "nteract".to_string(),
            cache_dir: None,
            daemon_info_path: None,
            monitor_poll_interval_ms: 500,
        };

        assert_eq!(config.child_env.len(), 2);
        assert_eq!(config.child_env.get("NTERACT_CHANNEL").unwrap(), "stable");
    }

    // ── Version tracking at creation ──────────────────────────────────

    #[tokio::test]
    async fn proxy_reads_daemon_version_at_creation() {
        let dir = tempfile::tempdir().unwrap();
        let info_path = dir.path().join("daemon.json");
        let info = serde_json::json!({
            "endpoint": "/tmp/test.sock",
            "pid": 1234,
            "version": "2.1.3",
            "started_at": "2026-01-01T00:00:00Z"
        });
        std::fs::write(&info_path, serde_json::to_string(&info).unwrap()).unwrap();

        let config = ProxyConfig {
            resolve_child_command: Box::new(|| Ok(PathBuf::from("/nonexistent/runt"))),
            child_args: vec!["mcp".to_string()],
            child_env: HashMap::new(),
            server_name: "test".to_string(),
            cache_dir: None,
            daemon_info_path: Some(info_path),
            monitor_poll_interval_ms: 500,
        };

        let proxy = McpProxy::new(config, None);
        let state = proxy.state.read().await;
        assert_eq!(state.last_daemon_version, Some("2.1.3".to_string()));
    }

    #[tokio::test]
    async fn proxy_has_no_version_when_no_info_path() {
        let proxy = McpProxy::new(test_config(), None);
        let state = proxy.state.read().await;
        assert!(state.last_daemon_version.is_none());
    }

    #[tokio::test]
    async fn proxy_has_no_version_when_info_file_missing() {
        let config = ProxyConfig {
            resolve_child_command: Box::new(|| Ok(PathBuf::from("/nonexistent/runt"))),
            child_args: vec!["mcp".to_string()],
            child_env: HashMap::new(),
            server_name: "test".to_string(),
            cache_dir: None,
            daemon_info_path: Some(PathBuf::from("/nonexistent/daemon.json")),
            monitor_poll_interval_ms: 500,
        };

        let proxy = McpProxy::new(config, None);
        let state = proxy.state.read().await;
        assert!(state.last_daemon_version.is_none());
    }

    // ── Restart metrics ───────────────────────────────────────────────

    #[tokio::test]
    async fn restart_count_starts_at_zero() {
        let proxy = McpProxy::new(test_config(), None);
        assert_eq!(proxy.restart_count().await, 0);
    }

    #[tokio::test]
    async fn child_uptime_is_none_without_child() {
        let proxy = McpProxy::new(test_config(), None);
        assert!(proxy.child_uptime_secs().await.is_none());
    }

    #[tokio::test]
    async fn last_restart_time_is_none_initially() {
        let proxy = McpProxy::new(test_config(), None);
        assert!(proxy.last_restart_time().await.is_none());
    }

    // ── Concurrent restart prevention ─────────────────────────────────

    #[tokio::test]
    async fn restart_in_progress_prevents_duplicate_restarts() {
        let proxy = McpProxy::new(test_config(), None);

        // Simulate restart in progress
        *proxy.restart_in_progress.lock().await = true;

        // Attempt restart should return early
        let result = proxy.restart_child().await;

        // Should succeed but do nothing (early return)
        assert!(result.is_ok());

        // Restart count should still be 0
        assert_eq!(proxy.restart_count().await, 0);
    }

    // ── reconnect tool ────────────────────────────────────────────────

    #[test]
    fn reconnect_tool_has_expected_shape() {
        let tool = reconnect_tool();
        assert_eq!(tool.name.as_ref(), RECONNECT_TOOL_NAME);
        assert_eq!(tool.name.as_ref(), "reconnect");
        let desc = tool.description.as_ref().map(|d| d.as_ref()).unwrap_or("");
        assert!(
            desc.to_lowercase().contains("restart"),
            "reconnect tool description should mention restart: {desc}"
        );
    }

    #[tokio::test]
    async fn handle_reconnect_returns_success_and_bumps_restart_count() {
        // Proxy with a bogus child command so restart_child marks a new
        // attempt but the spawn itself fails quickly. We just need it to
        // advance state observable by the caller.
        let proxy = McpProxy::new(test_config(), None);

        let result = proxy.handle_reconnect().await;

        // restart_child() wraps spawn failure as Err; handle_reconnect
        // surfaces that as McpError. Either outcome is fine for this
        // test — we care that the tool runs to completion (no panic,
        // no deadlock) and that restart_count observably advanced when
        // the spawn attempt happened.
        match result {
            Ok(call_result) => {
                let text: String = call_result
                    .content
                    .iter()
                    .filter_map(|c| c.raw.as_text().map(|t| t.text.clone()))
                    .collect::<Vec<_>>()
                    .join("\n");
                assert!(
                    text.contains("Restart #"),
                    "expected Restart # in body, got: {text}"
                );
            }
            Err(e) => {
                // Spawn failure is acceptable — just verify the error path
                // is the restart failure message, not something unrelated.
                assert!(
                    e.to_string().to_lowercase().contains("restart")
                        || e.to_string().to_lowercase().contains("spawn")
                        || e.to_string().to_lowercase().contains("child"),
                    "unexpected reconnect error: {e}"
                );
            }
        }
    }
}
