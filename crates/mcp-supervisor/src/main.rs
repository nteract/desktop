//! MCP Supervisor — a stable MCP server that proxies to the nteract MCP server.
//!
//! Architecture:
//! - Acts as an MCP **server** facing the client (Zed, Claude, etc.) via stdin/stdout
//! - Spawns the nteract MCP server as a child process
//! - Acts as an MCP **client** to the child process
//! - Proxies tools/prompts/resources between client and child
//! - Injects `supervisor_*` meta-tools for self-management
//! - Auto-restarts the child on crash
//! - Manages the dev daemon lifecycle
//!
//! Usage:
//!   cargo run -p mcp-supervisor
//!   # or via xtask:
//!   cargo xtask mcp

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListToolsResult,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::schemars;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use rmcp::{ClientHandler, ErrorData as McpError, ServerHandler, ServiceExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;
use tokio::sync::RwLock;
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// Daemon management
// ---------------------------------------------------------------------------

/// Check if the dev daemon is running and get its socket path.
fn daemon_status(project_root: &Path) -> Option<DaemonInfo> {
    let runt = if cfg!(windows) {
        project_root.join("target/debug/runt.exe")
    } else {
        project_root.join("target/debug/runt")
    };

    if !runt.exists() {
        return None;
    }

    let mut cmd = std::process::Command::new(&runt);
    cmd.args(["daemon", "status", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("RUNTIMED_DEV", "1");

    if let Some(workspace) = runt_workspace::get_workspace_path() {
        cmd.env("RUNTIMED_WORKSPACE_PATH", &workspace);
    }

    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }

    serde_json::from_slice(&output.stdout).ok()
}

#[derive(Debug, Deserialize)]
struct DaemonInfo {
    socket_path: String,
    running: bool,
}

/// Start the dev daemon as a background process. Returns the child handle.
fn start_daemon(project_root: &Path) -> Option<std::process::Child> {
    let runtimed = if cfg!(windows) {
        project_root.join("target/debug/runtimed.exe")
    } else {
        project_root.join("target/debug/runtimed")
    };

    if !runtimed.exists() {
        error!("runtimed binary not found at {}", runtimed.display());
        return None;
    }

    let mut cmd = std::process::Command::new(&runtimed);
    cmd.args(["--dev", "run"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("RUNTIMED_DEV", "1");

    if let Some(workspace) = runt_workspace::get_workspace_path() {
        cmd.env("RUNTIMED_WORKSPACE_PATH", &workspace);
    }

    match cmd.spawn() {
        Ok(child) => {
            info!("Started dev daemon (PID {})", child.id());
            Some(child)
        }
        Err(e) => {
            error!("Failed to start daemon: {e}");
            None
        }
    }
}

/// Wait for the daemon to become ready (up to `timeout`).
fn wait_for_daemon(project_root: &Path, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if let Some(info) = daemon_status(project_root) {
            if info.running {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Ensure maturin develop has been run (check if runtimed is importable).
fn ensure_maturin_develop(project_root: &Path) -> bool {
    let status = std::process::Command::new("uv")
        .args([
            "run",
            "--no-sync",
            "--directory",
            &project_root.join("python").to_string_lossy(),
            "python",
            "-c",
            "import runtimed",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    match status {
        Ok(s) if s.success() => true,
        _ => {
            info!("runtimed not importable, running maturin develop...");
            run_maturin_develop(project_root)
        }
    }
}

fn run_maturin_develop(project_root: &Path) -> bool {
    let status = std::process::Command::new("uv")
        .args([
            "run",
            "--directory",
            &project_root.join("python/runtimed").to_string_lossy(),
            "maturin",
            "develop",
        ])
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(s) if s.success() => {
            info!("maturin develop succeeded");
            true
        }
        Ok(s) => {
            error!("maturin develop failed with {s}");
            false
        }
        Err(e) => {
            error!("Failed to run maturin develop: {e}");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Child process (nteract MCP server)
// ---------------------------------------------------------------------------

/// Spawn the nteract MCP server as a child process and return an rmcp client.
async fn spawn_nteract_child(
    project_root: &Path,
    socket_path: &str,
) -> Result<rmcp::service::RunningService<RoleNteractClient, NteractClientHandler>, String> {
    let python_dir = project_root.join("python");

    // Augment PATH for uv discovery (same issue as the wrapper script)
    let mut path = std::env::var("PATH").unwrap_or_default();
    if let Ok(home) = std::env::var("HOME") {
        path = format!("{home}/.local/bin:{home}/.cargo/bin:/opt/homebrew/bin:{path}");
    }

    let transport = TokioChildProcess::new(Command::new("uv").configure(|cmd| {
        cmd.args([
            "run",
            "--no-sync",
            "--directory",
            &python_dir.to_string_lossy(),
            "nteract",
        ])
        .env("RUNTIMED_DEV", "1")
        .env("RUNTIMED_SOCKET_PATH", socket_path)
        .env("PATH", &path);
    }))
    .map_err(|e| format!("Failed to spawn nteract child: {e}"))?;

    let client = NteractClientHandler
        .serve(transport)
        .await
        .map_err(|e| format!("Failed to initialize nteract MCP client: {e}"))?;

    Ok(client)
}

type RoleNteractClient = rmcp::service::RoleClient;

/// Minimal client handler — we just need to forward requests, not handle
/// any client-side callbacks from the nteract server.
#[derive(Clone)]
struct NteractClientHandler;

impl ClientHandler for NteractClientHandler {
    fn get_info(&self) -> rmcp::model::ClientInfo {
        rmcp::model::ClientInfo {
            client_info: Implementation {
                name: "mcp-supervisor".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }
}

// ---------------------------------------------------------------------------
// Supervisor server (faces the MCP client)
// ---------------------------------------------------------------------------

/// Shared state for the supervisor.
struct SupervisorState {
    /// rmcp client connected to the nteract child process.
    child_client: Option<rmcp::service::RunningService<RoleNteractClient, NteractClientHandler>>,
    /// Project root path.
    project_root: PathBuf,
    /// Daemon socket path.
    socket_path: String,
    /// Number of times the child has been restarted.
    restart_count: u32,
    /// Timestamps of recent crashes for circuit breaker.
    recent_crashes: Vec<Instant>,
    /// Last error message from child.
    last_error: Option<String>,
    /// Whether we started the daemon (so we know to clean it up).
    daemon_child: Option<std::process::Child>,
}

impl SupervisorState {
    /// Check circuit breaker: too many crashes in a short window.
    fn should_retry(&mut self) -> bool {
        let now = Instant::now();
        self.recent_crashes
            .retain(|t| now.duration_since(*t) < Duration::from_secs(30));
        if self.recent_crashes.len() >= 5 {
            error!(
                "Circuit breaker: {} crashes in 30s, stopping auto-restart",
                self.recent_crashes.len()
            );
            return false;
        }
        self.recent_crashes.push(now);
        true
    }
}

#[derive(Clone)]
struct Supervisor {
    state: Arc<RwLock<SupervisorState>>,
}

impl Supervisor {
    fn new(
        project_root: PathBuf,
        socket_path: String,
        child_client: rmcp::service::RunningService<RoleNteractClient, NteractClientHandler>,
        daemon_child: Option<std::process::Child>,
    ) -> Self {
        Self {
            state: Arc::new(RwLock::new(SupervisorState {
                child_client: Some(child_client),
                project_root,
                socket_path,
                restart_count: 0,
                recent_crashes: Vec::new(),
                last_error: None,
                daemon_child,
            })),
        }
    }

    /// Attempt to restart the nteract child process.
    async fn restart_child(&self) -> Result<(), String> {
        let mut state = self.state.write().await;

        // Drop old client
        if let Some(old) = state.child_client.take() {
            let _ = old.cancel().await;
        }

        info!(
            "Restarting nteract MCP server (restart #{})",
            state.restart_count + 1
        );

        // Check circuit breaker
        if !state.should_retry() {
            let msg = "Too many crashes, auto-restart disabled. Call supervisor_restart to retry.";
            state.last_error = Some(msg.to_string());
            return Err(msg.to_string());
        }

        // Small delay to avoid tight restart loops
        tokio::time::sleep(Duration::from_secs(2)).await;

        match spawn_nteract_child(&state.project_root, &state.socket_path).await {
            Ok(client) => {
                state.child_client = Some(client);
                state.restart_count += 1;
                state.last_error = None;
                info!("nteract MCP server restarted successfully");
                Ok(())
            }
            Err(e) => {
                state.last_error = Some(e.clone());
                error!("Failed to restart nteract child: {e}");
                Err(e)
            }
        }
    }

    /// Forward a tool call to the child, auto-restarting on disconnect.
    async fn forward_tool_call(
        &self,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, McpError> {
        // First attempt
        match self.try_forward_tool_call(&params).await {
            Ok(result) => return Ok(result),
            Err(e) => {
                warn!("Tool call failed, attempting restart: {e}");
            }
        }

        // Restart and retry once
        if let Err(e) = self.restart_child().await {
            return Err(McpError::internal_error(
                format!("Child restart failed: {e}"),
                None,
            ));
        }

        // Second attempt after restart
        self.try_forward_tool_call(&params).await
    }

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

    /// Build the supervisor_status result.
    async fn status(&self) -> SupervisorStatus {
        let state = self.state.read().await;
        SupervisorStatus {
            child_running: state.child_client.is_some(),
            restart_count: state.restart_count,
            last_error: state.last_error.clone(),
            socket_path: state.socket_path.clone(),
            project_root: state.project_root.to_string_lossy().to_string(),
            daemon_managed: state.daemon_child.is_some(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct SupervisorStatus {
    /// Whether the nteract MCP child process is running.
    child_running: bool,
    /// Number of times the child has been restarted.
    restart_count: u32,
    /// Last error from the child process, if any.
    last_error: Option<String>,
    /// Daemon socket path.
    socket_path: String,
    /// Project root directory.
    project_root: String,
    /// Whether the supervisor started (and manages) the daemon.
    daemon_managed: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[allow(dead_code)] // `target` is read via serde deserialization, not directly
struct SupervisorRestartParams {
    /// What to restart: "child" (default) or "daemon" (restarts both).
    #[serde(default = "default_restart_target")]
    target: String,
}

fn default_restart_target() -> String {
    "child".to_string()
}

// The supervisor_status tool schema — no input params needed.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EmptyParams {}

/// MCP ServerHandler that proxies to the nteract child + injects supervisor tools.
impl ServerHandler for Supervisor {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: Default::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "mcp-supervisor".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
            instructions: Some(
                "MCP supervisor proxying to the nteract notebook server. \
                 Includes supervisor_status and supervisor_restart tools \
                 for managing the server lifecycle."
                    .into(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let mut tools = Vec::new();

        // Supervisor's own tools
        tools.push(Tool::new(
            "supervisor_status",
            "Get the status of the MCP supervisor, child process, and daemon.",
            serde_json::to_value(schemars::schema_for!(EmptyParams))
                .unwrap()
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ));
        tools.push(Tool::new(
            "supervisor_restart",
            "Restart the nteract MCP server child process, or the daemon. \
             Use target='child' (default) or target='daemon' (restarts both).",
            serde_json::to_value(schemars::schema_for!(SupervisorRestartParams))
                .unwrap()
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ));

        // Forward to child for its tools
        let state = self.state.read().await;
        if let Some(ref client) = state.child_client {
            match client.list_tools(None).await {
                Ok(child_tools) => {
                    tools.extend(child_tools.tools);
                }
                Err(e) => {
                    warn!("Failed to list child tools: {e}");
                    // Still return supervisor tools even if child is down
                }
            }
        }

        Ok(ListToolsResult {
            tools,
            next_cursor: None,
            meta: None,
        })
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        match request.name.as_ref() {
            "supervisor_status" => {
                let status = self.status().await;
                let json = serde_json::to_string_pretty(&status)
                    .unwrap_or_else(|e| format!("Failed to serialize status: {e}"));
                Ok(CallToolResult::success(vec![Content::text(json)]))
            }
            "supervisor_restart" => {
                let target = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("target"))
                    .and_then(Value::as_str)
                    .unwrap_or("child");

                match target {
                    "daemon" => {
                        // Restart daemon, then child
                        let mut state = self.state.write().await;
                        if let Some(ref mut child) = state.daemon_child {
                            info!("Stopping managed daemon...");
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        let project_root = state.project_root.clone();
                        state.daemon_child = start_daemon(&project_root);
                        drop(state);

                        if !wait_for_daemon(&project_root, Duration::from_secs(30)) {
                            return Ok(CallToolResult::success(vec![Content::text(
                                "Daemon restart failed — daemon did not become ready within 30s",
                            )]));
                        }

                        match self.restart_child().await {
                            Ok(()) => Ok(CallToolResult::success(vec![Content::text(
                                "Daemon and MCP server restarted successfully",
                            )])),
                            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                                "Daemon restarted but MCP server failed: {e}"
                            ))])),
                        }
                    }
                    _ => {
                        // Restart child only
                        // Clear the circuit breaker on manual restart
                        {
                            let mut state = self.state.write().await;
                            state.recent_crashes.clear();
                        }
                        match self.restart_child().await {
                            Ok(()) => Ok(CallToolResult::success(vec![Content::text(
                                "nteract MCP server restarted successfully",
                            )])),
                            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                                "Restart failed: {e}"
                            ))])),
                        }
                    }
                }
            }
            // Everything else → forward to child
            _ => self.forward_tool_call(request).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

fn resolve_project_root() -> PathBuf {
    // Walk up from current dir looking for Cargo.toml with [workspace]
    let mut dir = std::env::current_dir().expect("Failed to get current directory");
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            if let Ok(contents) = std::fs::read_to_string(&cargo_toml) {
                if contents.contains("[workspace]") {
                    return dir;
                }
            }
        }
        if !dir.pop() {
            // Fallback to current dir
            return std::env::current_dir().expect("Failed to get current directory");
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Log to stderr (MCP uses stdin/stdout for transport)
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let project_root = resolve_project_root();
    info!("Project root: {}", project_root.display());

    // Step 1: Ensure daemon is running
    let mut daemon_child = None;
    let socket_path = match daemon_status(&project_root) {
        Some(info) if info.running => {
            info!("Dev daemon already running at {}", info.socket_path);
            info.socket_path
        }
        Some(info) => {
            info!("Daemon not running, starting it...");
            daemon_child = start_daemon(&project_root);
            if !wait_for_daemon(&project_root, Duration::from_secs(30)) {
                error!("Daemon failed to start within 30s");
                // Continue anyway — the socket path is still valid for config
            }
            info.socket_path
        }
        None => {
            // Can't even get status — try to build runt CLI first
            warn!("runt CLI not found, building...");
            let status = std::process::Command::new("cargo")
                .args(["build", "-p", "runt-cli"])
                .current_dir(&project_root)
                .status()?;
            if !status.success() {
                error!("Failed to build runt CLI");
                std::process::exit(1);
            }

            match daemon_status(&project_root) {
                Some(info) => {
                    if !info.running {
                        daemon_child = start_daemon(&project_root);
                        wait_for_daemon(&project_root, Duration::from_secs(30));
                    }
                    info.socket_path
                }
                None => {
                    error!("Cannot determine daemon socket path");
                    std::process::exit(1);
                }
            }
        }
    };

    // Step 2: Ensure maturin develop has been run
    if !ensure_maturin_develop(&project_root) {
        error!("Failed to build Python bindings — nteract MCP server may not work");
        // Continue anyway, let the child process fail with a clear error
    }

    // Step 3: Spawn nteract child
    info!("Spawning nteract MCP server...");
    let child_client = spawn_nteract_child(&project_root, &socket_path)
        .await
        .map_err(|e| {
            error!("{e}");
            e
        })?;
    info!("nteract MCP server connected");

    // Step 4: Start supervisor server on stdin/stdout
    let supervisor = Supervisor::new(project_root, socket_path, child_client, daemon_child);

    let transport = rmcp::transport::io::stdio();
    let server = supervisor.serve(transport).await?;

    // Clone the state Arc before waiting() consumes the server
    let state_for_cleanup = server.service().state.clone();

    info!("MCP supervisor running, waiting for client disconnect...");
    let reason = server.waiting().await?;
    info!("Supervisor shutting down: {reason:?}");

    // Cleanup: stop daemon if we started it
    {
        let mut state = state_for_cleanup.write().await;
        if let Some(ref mut child) = state.daemon_child {
            info!("Stopping managed daemon (PID {:?})...", child.id());
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    Ok(())
}
