//! nteract-dev — a stable MCP server that proxies to the nteract MCP server
//! and manages the full dev environment (daemon, vite, notebook app).
//!
//! Architecture:
//! - Acts as an MCP **server** facing the client (Zed, Claude, etc.) via stdin/stdout
//! - Spawns the nteract MCP server as a child process
//! - Acts as an MCP **client** to the child process
//! - Proxies tools/prompts/resources between client and child
//! - Injects `supervisor_*` meta-tools for self-management
//! - Auto-restarts the child on crash
//! - Watches files for changes and hot-reloads the child
//! - Manages the dev daemon lifecycle
//!
//! Usage:
//!   cargo run -p mcp-supervisor
//!   # or via xtask:
//!   cargo xtask mcp

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
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
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info, warn};

// ---------------------------------------------------------------------------
// Daemon management
// ---------------------------------------------------------------------------

/// Global flag for release-mode binaries. Initialized from `RUNTIMED_RELEASE=1`
/// and toggled at runtime by the `supervisor_set_mode` MCP tool.
static RELEASE_MODE: AtomicBool = AtomicBool::new(false);

/// Whether to use release-mode binaries.
fn use_release_binaries() -> bool {
    RELEASE_MODE.load(Ordering::Relaxed)
}

/// Resolve the path to a Cargo-built binary, respecting release mode.
fn cargo_binary(project_root: &Path, name: &str) -> std::path::PathBuf {
    let profile = if use_release_binaries() {
        "release"
    } else {
        "debug"
    };
    let bin_name = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    project_root.join("target").join(profile).join(bin_name)
}

/// Check if the dev daemon is running and get its socket path.
fn daemon_status(project_root: &Path) -> Option<DaemonInfo> {
    let runt = cargo_binary(project_root, "runt");

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
    /// Nested daemon_info from `runt daemon status --json` (present when running).
    #[serde(default)]
    daemon_info: Option<DaemonProcessInfo>,
}

#[derive(Debug, Deserialize)]
struct DaemonProcessInfo {
    /// PID of the running daemon process.
    #[serde(default)]
    pid: Option<u32>,
    /// Version string, e.g. "2.0.2+6147ffa".
    #[serde(default)]
    version: Option<String>,
}

/// Get the version string from the built runtimed binary (e.g. "runtimed 2.0.2+abc1234").
/// Returns just the version part after "runtimed " or the full line if parsing fails.
fn expected_daemon_version(project_root: &Path) -> Option<String> {
    let runtimed = cargo_binary(project_root, "runtimed");
    if !runtimed.exists() {
        return None;
    }
    let output = std::process::Command::new(&runtimed)
        .arg("--version")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let full = String::from_utf8_lossy(&output.stdout).trim().to_string();
    // "runtimed 2.0.2+abc1234" → "2.0.2+abc1234"
    Some(full.strip_prefix("runtimed ").unwrap_or(&full).to_string())
}

/// Stop a running daemon by PID. Works whether or not we started it.
/// Tries `runt daemon stop` first (graceful), falls back to kill(pid).
///
/// Safety: refuses to act if the workspace path can't be resolved, because
/// without it the `runt` CLI would target the system nightly daemon instead
/// of this worktree's dev daemon.
fn stop_daemon_by_pid(project_root: &Path, pid: u32) {
    let workspace = match runt_workspace::get_workspace_path() {
        Some(ws) => ws,
        None => {
            error!(
                "Refusing to stop daemon (PID {pid}): cannot resolve workspace path. \
                 Without RUNTIMED_WORKSPACE_PATH the stop command would target the \
                 system nightly daemon, not this worktree's dev daemon."
            );
            return;
        }
    };

    // Try graceful stop via runt CLI
    let runt = cargo_binary(project_root, "runt");
    if runt.exists() {
        info!(
            "Stopping daemon (PID {pid}) via runt daemon stop (workspace: {})...",
            workspace.display()
        );
        let mut cmd = std::process::Command::new(&runt);
        cmd.args(["daemon", "stop"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env("RUNTIMED_DEV", "1")
            .env("RUNTIMED_WORKSPACE_PATH", &workspace);
        if let Ok(status) = cmd.status() {
            if status.success() {
                // Give it a moment to release the socket
                std::thread::sleep(Duration::from_secs(2));
                return;
            }
        }
    }

    // Fallback: kill by PID
    info!("Falling back to kill(PID {pid})...");
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_secs(2));
}

/// Start the dev daemon as a background process. Returns the child handle.
///
/// Before starting, checks for a stale daemon on the socket and stops it
/// if its version doesn't match the built binary.
fn start_daemon(project_root: &Path) -> Option<std::process::Child> {
    let runtimed = cargo_binary(project_root, "runtimed");

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

/// Build an augmented PATH that includes common user-local tool directories.
///
/// MCP clients (Zed, Claude, etc.) often spawn servers with a minimal PATH
/// that's missing ~/.local/bin (uv), ~/.cargo/bin, ~/.nvm/*, /opt/homebrew/bin, etc.
fn augmented_path() -> String {
    let base = std::env::var("PATH").unwrap_or_default();
    let mut dirs: Vec<String> = Vec::new();

    if let Ok(home) = std::env::var("HOME") {
        dirs.push(format!("{home}/.local/bin"));
        dirs.push(format!("{home}/.cargo/bin"));

        // nvm: include all node version bin dirs (pnpm may only be installed
        // in some versions, not necessarily the latest)
        let nvm_dir = PathBuf::from(&home).join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(&nvm_dir) {
            let mut versions: Vec<PathBuf> = entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| p.join("bin/node").exists())
                .collect();
            versions.sort();
            // Add in reverse order so newer versions take priority
            for version in versions.into_iter().rev() {
                dirs.push(version.join("bin").to_string_lossy().to_string());
            }
        }

        // Standalone pnpm installer location
        dirs.push(format!("{home}/Library/pnpm"));
    }

    dirs.push("/opt/homebrew/bin".into());

    // Use platform-appropriate PATH separator
    let sep = if cfg!(windows) { ";" } else { ":" };
    let prefix = dirs.join(sep);
    format!("{prefix}{sep}{base}")
}

/// Run `cargo build -p runtimed` to rebuild the daemon binary.
///
/// Returns `true` on success, `false` on failure.
fn run_cargo_build_daemon(project_root: &Path) -> bool {
    info!("Building daemon binary (cargo build -p runtimed)...");
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("build")
        .arg("-p")
        .arg("runtimed")
        .current_dir(project_root)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if use_release_binaries() {
        cmd.arg("--release");
    }
    match cmd.status() {
        Ok(status) if status.success() => {
            info!("cargo build -p runtimed succeeded");
            true
        }
        Ok(status) => {
            error!("cargo build -p runtimed failed with {status}");
            false
        }
        Err(e) => {
            error!("Failed to run cargo build: {e}");
            false
        }
    }
}

fn run_maturin_develop(project_root: &Path) -> bool {
    // Route stdout to null — the supervisor uses stdout for MCP transport,
    // so maturin output would corrupt the JSON-RPC stream. Stderr goes to
    // our stderr (which is the supervisor's log stream).
    //
    // We run maturin from python/runtimed (where it's a dev dependency),
    // but set VIRTUAL_ENV to .venv (the workspace root venv) so the .so
    // is installed where the MCP server actually runs from. Without this,
    // maturin installs into python/runtimed/.venv (the test-only venv)
    // and the MCP server never sees the updated bindings.
    let status = std::process::Command::new("uv")
        .args([
            "run",
            "--directory",
            &project_root.join("python/runtimed").to_string_lossy(),
            "maturin",
            "develop",
        ])
        .env(
            "VIRTUAL_ENV",
            project_root.join(".venv").to_string_lossy().as_ref(),
        )
        .stdout(Stdio::null())
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
    let path = augmented_path();

    let transport = TokioChildProcess::new(Command::new("uv").configure(|cmd| {
        cmd.args([
            "run",
            "--no-sync",
            "--directory",
            &project_root.to_string_lossy(),
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
                name: "nteract-dev".into(),
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

/// What kind of file change was detected.
#[derive(Debug, Clone, PartialEq)]
enum ChangeKind {
    /// Only Python files changed — restart child only.
    PythonOnly,
    /// Rust files changed — needs maturin develop + restart.
    RustChanged,
}

/// A managed long-running child process (vite, notebook app, etc.).
struct ManagedProcess {
    /// The child process handle.
    child: std::process::Child,
    /// The port it's listening on, if applicable.
    port: Option<u16>,
}

impl ManagedProcess {
    fn is_alive(&mut self) -> bool {
        // Cross-platform liveness check via try_wait (no extra process spawn)
        match self.child.try_wait() {
            Ok(None) => true,     // Still running
            Ok(Some(_)) => false, // Exited
            Err(_) => false,      // Can't check — assume dead
        }
    }
}

/// Shared state for the supervisor.
struct SupervisorState {
    /// rmcp client connected to the nteract child process.
    child_client: Option<rmcp::service::RunningService<RoleNteractClient, NteractClientHandler>>,
    /// Project root path.
    project_root: PathBuf,
    /// Log directory (.context/ in the project root).
    log_dir: PathBuf,
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
    /// Channel to request a tool list changed notification from the server context.
    /// The file watcher sends on this channel after triggering a restart.
    tool_list_changed_tx: Option<mpsc::Sender<()>>,
    /// Managed long-running processes (vite dev server, notebook app, etc.).
    managed: HashMap<String, ManagedProcess>,
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
        tool_list_changed_tx: mpsc::Sender<()>,
    ) -> Self {
        let log_dir = project_root.join(".context");
        let _ = std::fs::create_dir_all(&log_dir);
        Self {
            state: Arc::new(RwLock::new(SupervisorState {
                child_client: Some(child_client),
                log_dir,
                project_root,
                socket_path,
                restart_count: 0,
                recent_crashes: Vec::new(),
                last_error: None,
                daemon_child,
                tool_list_changed_tx: Some(tool_list_changed_tx),
                managed: HashMap::new(),
            })),
        }
    }

    /// Attempt to restart the nteract child process.
    async fn restart_child(&self) -> Result<(), String> {
        // Phase 1: Drop old client and check circuit breaker (hold lock briefly)
        let (project_root, socket_path) = {
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
                let msg =
                    "Too many crashes, auto-restart disabled. Call supervisor_restart to retry.";
                state.last_error = Some(msg.to_string());
                return Err(msg.to_string());
            }

            (state.project_root.clone(), state.socket_path.clone())
            // Lock dropped here
        };

        // Phase 2: Sleep without holding the lock (other tasks can read state)
        tokio::time::sleep(Duration::from_secs(2)).await;

        // Phase 3: Spawn child without holding the lock
        match spawn_nteract_child(&project_root, &socket_path).await {
            Ok(client) => {
                // Phase 4: Re-acquire lock to store the new client
                let mut state = self.state.write().await;
                state.child_client = Some(client);
                state.restart_count += 1;
                state.last_error = None;
                info!("nteract MCP server restarted successfully");
                Ok(())
            }
            Err(e) => {
                let mut state = self.state.write().await;
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
        let mut state = self.state.write().await;
        // Just check if we have a client handle. A full liveness probe
        // (list_tools) can hang if the child is wedged, which blocks the
        // entire status call. The forward_tool_call path handles detection
        // and auto-restart on actual failures.
        let child_running = state.child_client.is_some();
        let managed_processes: HashMap<String, ManagedProcessStatus> = state
            .managed
            .iter_mut()
            .map(|(name, proc)| {
                (
                    name.clone(),
                    ManagedProcessStatus {
                        pid: proc.child.id(),
                        alive: proc.is_alive(),
                        port: proc.port,
                    },
                )
            })
            .collect();

        // Query daemon version and detect mismatches
        let daemon_version = daemon_status(&state.project_root)
            .and_then(|info| info.daemon_info)
            .and_then(|di| di.version);
        let expected_version = expected_daemon_version(&state.project_root);
        let version_mismatch = match (&daemon_version, &expected_version) {
            (Some(running), Some(expected)) => running != expected,
            _ => false,
        };

        // Check if the daemon_child we're tracking is still our process
        if let Some(ref mut child) = state.daemon_child {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    // Our managed daemon exited (launchd may have replaced it)
                    warn!(
                        "Managed daemon (PID {:?}) has exited — \
                         another process may have taken over the socket",
                        child.id()
                    );
                    state.daemon_child = None;
                }
                Ok(None) => {} // still running
                Err(_) => {
                    state.daemon_child = None;
                }
            }
        }

        SupervisorStatus {
            child_running,
            restart_count: state.restart_count,
            last_error: state.last_error.clone(),
            socket_path: state.socket_path.clone(),
            project_root: state.project_root.to_string_lossy().to_string(),
            daemon_managed: state.daemon_child.is_some(),
            build_mode: if use_release_binaries() {
                "release"
            } else {
                "debug"
            }
            .to_string(),
            daemon_version,
            expected_version,
            version_mismatch,
            managed_processes,
        }
    }

    /// Start the Vite dev server for hot-reload frontend development.
    async fn start_vite(&self) -> Result<u16, String> {
        // Phase 1: Check existing process and extract state (hold lock briefly)
        let (project_root, needs_start) = {
            let mut state = self.state.write().await;

            // Already running?
            if let Some(proc) = state.managed.get_mut("vite") {
                if proc.is_alive() {
                    return proc
                        .port
                        .ok_or_else(|| "Vite is running but port unknown".into());
                }
                // Dead — clean up
                state.managed.remove("vite");
            }

            (state.project_root.clone(), true)
            // Lock dropped here
        };

        if !needs_start {
            unreachable!();
        }

        // Phase 2: Derive port and run pnpm install without holding the lock
        let port = std::env::var("RUNTIMED_VITE_PORT")
            .ok()
            .or_else(|| std::env::var("CONDUCTOR_PORT").ok())
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or_else(|| runt_workspace::vite_port_for_workspace(&project_root));

        // Ensure pnpm install has been run (use spawn_blocking to avoid
        // stalling the tokio runtime during a potentially long install)
        let node_modules = project_root.join("node_modules/.modules.yaml");
        if !node_modules.exists() {
            info!("Running pnpm install...");
            let pr = project_root.clone();
            let status = tokio::task::spawn_blocking(move || {
                std::process::Command::new("pnpm")
                    .args(["install"])
                    .current_dir(&pr)
                    .env("PATH", augmented_path())
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::inherit())
                    .status()
            })
            .await
            .map_err(|e| format!("pnpm install task failed: {e}"))?
            .map_err(|e| format!("Failed to run pnpm install: {e}"))?;
            if !status.success() {
                return Err("pnpm install failed".into());
            }
        }

        // Route Vite output to a log file instead of /dev/null
        let vite_log_path = project_root.join(".context/vite.log");
        let vite_log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&vite_log_path)
            .map_err(|e| {
                format!(
                    "Failed to open vite log at {}: {e}",
                    vite_log_path.display()
                )
            })?;
        let vite_stderr = vite_log
            .try_clone()
            .map_err(|e| format!("Failed to clone vite log handle: {e}"))?;

        info!("Starting Vite dev server on port {port}...");
        info!("Vite logs: {}", vite_log_path.display());
        let pr = project_root.clone();
        let child = tokio::task::spawn_blocking(move || {
            std::process::Command::new("pnpm")
                .args([
                    "--dir",
                    "apps/notebook",
                    "dev",
                    "--port",
                    &port.to_string(),
                    "--strictPort",
                ])
                .current_dir(&pr)
                .env("PATH", augmented_path())
                .stdin(Stdio::null())
                .stdout(Stdio::from(vite_log))
                .stderr(Stdio::from(vite_stderr))
                .spawn()
        })
        .await
        .map_err(|e| format!("Vite spawn task failed: {e}"))?
        .map_err(|e| format!("Failed to start Vite: {e}"))?;

        info!("Vite dev server started (PID {}, port {port})", child.id());

        // Phase 3: Re-acquire lock to store the process
        let mut state = self.state.write().await;
        state.managed.insert(
            "vite".into(),
            ManagedProcess {
                child,
                port: Some(port),
            },
        );

        Ok(port)
    }

    /// Launch the notebook app in dev mode connected to the managed Vite server.
    async fn show_notebook_dev(
        &self,
        request: &CallToolRequestParams,
        vite_port: u16,
    ) -> Result<CallToolResult, McpError> {
        let state = self.state.read().await;

        let binary_name = format!("notebook{}", std::env::consts::EXE_SUFFIX);
        let binary = state.project_root.join("target/debug").join(&binary_name);
        if !binary.exists() {
            return Ok(CallToolResult::success(vec![Content::text(
                "No notebook binary found. Run `cargo build -p notebook --no-default-features` first.",
            )]));
        }

        // Resolve notebook path from arguments
        let notebook_id = request
            .arguments
            .as_ref()
            .and_then(|args| args.get("notebook_id"))
            .and_then(Value::as_str);

        let path = match notebook_id {
            Some(id) => {
                if !std::path::Path::new(id).is_absolute() {
                    return Ok(CallToolResult::success(vec![Content::text(format!(
                        "Notebook '{id}' is untitled (not saved to disk). \
                         Use save_notebook(path) first, then call show_notebook()."
                    ))]));
                }
                id.to_string()
            }
            None => {
                // No notebook_id — fall through to the child's show_notebook
                // which can resolve the current session's notebook.
                drop(state);
                return self.forward_tool_call(request.clone()).await;
            }
        };

        // Launch the dev binary with Vite URL
        let mut cmd = std::process::Command::new(&binary);
        cmd.arg(&path)
            .env("RUNTIMED_DEV", "1")
            .env("RUNTIMED_VITE_PORT", vite_port.to_string())
            .env("PATH", augmented_path())
            .stdout(Stdio::null())
            .stderr(Stdio::null());

        if let Some(wp) = runt_workspace::get_workspace_path() {
            cmd.env("RUNTIMED_WORKSPACE_PATH", &wp);
        }

        drop(state);

        match cmd.spawn() {
            Ok(child) => {
                info!(
                    "Launched notebook app (PID {}, Vite port {vite_port}): {path}",
                    child.id()
                );
                // Track as a managed process, stopping any existing instance first
                let mut state = self.state.write().await;
                if let Some(mut old) = state.managed.remove("notebook-app") {
                    info!("Stopping previous notebook-app (PID {})...", old.child.id());
                    let _ = old.child.kill();
                    let _ = old.child.wait();
                }
                state
                    .managed
                    .insert("notebook-app".into(), ManagedProcess { child, port: None });
                Ok(CallToolResult::success(vec![Content::text(format!(
                    "Opened notebook in nteract (dev, Vite port {vite_port}): {path}"
                ))]))
            }
            Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                "Failed to launch notebook app: {e}"
            ))])),
        }
    }

    /// Stop a managed process by name.
    async fn stop_managed(&self, name: &str) -> Result<(), String> {
        // Remove from map under the lock, then kill outside the lock
        let mut proc = {
            let mut state = self.state.write().await;
            state
                .managed
                .remove(name)
                .ok_or_else(|| format!("No managed process named '{name}'"))?
            // Lock dropped here
        };

        info!(
            "Stopping managed process '{}' (PID {})...",
            name,
            proc.child.id()
        );
        let _ = proc.child.kill();
        let _ = proc.child.wait();
        Ok(())
    }

    /// Handle a file change event: restart child (and rebuild if Rust changed).
    async fn handle_file_change(&self, kind: ChangeKind) {
        if kind == ChangeKind::RustChanged {
            info!("Rust files changed, running maturin develop...");
            let project_root = {
                let state = self.state.read().await;
                state.project_root.clone()
            };
            if !run_maturin_develop(&project_root) {
                error!("maturin develop failed, keeping current child");
                return;
            }
        }

        // Clear circuit breaker for file-change-triggered restarts
        {
            let mut state = self.state.write().await;
            state.recent_crashes.clear();
        }

        match self.restart_child().await {
            Ok(()) => {
                info!("Child restarted after file change ({kind:?})");
                // Signal that the tool list may have changed
                let state = self.state.read().await;
                if let Some(ref tx) = state.tool_list_changed_tx {
                    let _ = tx.send(()).await;
                }
            }
            Err(e) => {
                error!("Failed to restart child after file change: {e}");
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize, schemars::JsonSchema)]
struct ManagedProcessStatus {
    pid: u32,
    alive: bool,
    port: Option<u16>,
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
    /// Current build mode for daemon binaries: "debug" or "release".
    build_mode: String,
    /// Version of the running daemon (e.g. "2.0.2+6147ffa"), if known.
    daemon_version: Option<String>,
    /// Version of the built runtimed binary, if available.
    expected_version: Option<String>,
    /// True if daemon_version != expected_version (stale daemon).
    version_mismatch: bool,
    /// Status of managed long-running processes (vite, app, etc.).
    managed_processes: HashMap<String, ManagedProcessStatus>,
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[allow(dead_code)] // `lines` is read via serde deserialization, not directly
struct SupervisorLogsParams {
    /// Number of lines to return from the end of the log. Defaults to 50.
    #[serde(default = "default_log_lines")]
    lines: usize,
}

fn default_log_lines() -> usize {
    50
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct StopProcessParams {
    /// Name of the managed process to stop (e.g. "vite").
    name: String,
}

// The supervisor_status tool schema — no input params needed.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct EmptyParams {}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[allow(dead_code)] // `mode` is read via serde deserialization, not directly
struct SetModeParams {
    /// The build mode: "debug" or "release".
    mode: String,
}

/// MCP ServerHandler that proxies to the nteract child + injects supervisor tools.
impl ServerHandler for Supervisor {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: Default::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation {
                name: "nteract-dev".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                ..Default::default()
            },
            instructions: Some(
                "nteract-dev — MCP supervisor proxying to the nteract notebook server. \
                 Includes supervisor_status, supervisor_restart, supervisor_rebuild, \
                 supervisor_logs, supervisor_start_vite, and supervisor_stop tools \
                 for managing the server lifecycle and dev environment. \
                 File watching is active: Python changes hot-reload instantly, \
                 Rust changes trigger maturin develop + reload. Changed tool \
                 behavior takes effect immediately; new/removed tools may take \
                 a moment for the client to discover."
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
        let empty_schema = serde_json::to_value(schemars::schema_for!(EmptyParams))
            .unwrap()
            .as_object()
            .cloned()
            .unwrap_or_default();

        tools.push(Tool::new(
            "supervisor_status",
            "Get the status of the MCP supervisor, child process, and daemon.",
            empty_schema.clone(),
        ));
        tools.push(Tool::new(
            "supervisor_restart",
            "Restart the nteract MCP server child process, or the daemon. Use target='child' (default) or target='daemon' (restarts both).",
            serde_json::to_value(schemars::schema_for!(SupervisorRestartParams))
                .unwrap()
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ));
        tools.push(Tool::new(
            "supervisor_rebuild",
            "Full rebuild: compile the daemon binary (cargo build -p runtimed), rebuild the Rust Python bindings (maturin develop), restart the daemon, and restart the MCP server. Use after changing crates/runtimed/, crates/runtimed-py/, or python/ source.",
            empty_schema.clone(),
        ));
        tools.push(Tool::new(
            "supervisor_logs",
            "Read the last N lines of the daemon log file. Defaults to 50 lines.",
            serde_json::to_value(schemars::schema_for!(SupervisorLogsParams))
                .unwrap()
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ));
        tools.push(Tool::new(
            "supervisor_vite_logs",
            "Read the last N lines of the Vite dev server log. Defaults to 50 lines.",
            serde_json::to_value(schemars::schema_for!(SupervisorLogsParams))
                .unwrap()
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ));
        tools.push(Tool::new(
            "supervisor_start_vite",
            "Start the Vite dev server for hot-reload frontend development. Returns the port number. If already running, returns the existing port.",
            empty_schema.clone(),
        ));
        tools.push(Tool::new(
            "supervisor_stop",
            "Stop a managed process by name (e.g. 'vite').",
            serde_json::to_value(schemars::schema_for!(StopProcessParams))
                .unwrap()
                .as_object()
                .cloned()
                .unwrap_or_default(),
        ));
        tools.push(Tool::new(
            "supervisor_set_mode",
            "Switch the daemon between debug and release builds at runtime. \
             Stops the current daemon, flips the binary path, and restarts. \
             Use mode='debug' or mode='release'. No settings.json change needed.",
            serde_json::to_value(schemars::schema_for!(SetModeParams))
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
                        // Restart daemon, then child.
                        // Stop whatever daemon is on the socket — whether we
                        // started it or launchd did.
                        let mut state = self.state.write().await;
                        let project_root = state.project_root.clone();

                        if let Some(ref mut child) = state.daemon_child {
                            info!("Stopping managed daemon (PID {:?})...", child.id());
                            let _ = child.kill();
                            let _ = child.wait();
                            state.daemon_child = None;
                        }

                        // Also stop any unmanaged daemon on the socket
                        if let Some(info) = daemon_status(&project_root) {
                            if info.running {
                                if let Some(di) = &info.daemon_info {
                                    if let Some(pid) = di.pid {
                                        info!("Stopping unmanaged daemon (PID {pid})...");
                                        stop_daemon_by_pid(&project_root, pid);
                                    }
                                }
                            }
                        }

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
            "show_notebook" => {
                // Check if we have a managed Vite process
                let vite_port = {
                    let mut state = self.state.write().await;
                    if let Some(p) = state.managed.get_mut("vite") {
                        if p.is_alive() {
                            p.port
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };

                match vite_port {
                    Some(port) => {
                        // Managed Vite running: launch the dev binary ourselves
                        self.show_notebook_dev(&request, port).await
                    }
                    None => {
                        // No managed Vite — forward to child's show_notebook
                        // (uses bundled build or installed app) and hint that
                        // Vite is available via the supervisor
                        let mut result = self.forward_tool_call(request).await?;
                        result.content.push(Content::text(
                            "\n\nTip: Use supervisor_start_vite to start a Vite \
                             dev server for hot-reload, then show_notebook will \
                             launch with live frontend changes."
                                .to_string(),
                        ));
                        Ok(result)
                    }
                }
            }
            "supervisor_start_vite" => match self.start_vite().await {
                Ok(port) => Ok(CallToolResult::success(vec![Content::text(format!(
                    "Vite dev server running on http://localhost:{port}"
                ))])),
                Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                    "Failed to start Vite: {e}"
                ))])),
            },
            "supervisor_stop" => {
                let name = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                if name.is_empty() {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "Missing required 'name' argument (e.g. \"vite\")",
                    )]));
                }
                match self.stop_managed(name).await {
                    Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                        "Stopped '{name}'"
                    ))])),
                    Err(e) => Ok(CallToolResult::success(vec![Content::text(e)])),
                }
            }
            "supervisor_rebuild" => {
                info!("Manual rebuild triggered via MCP tool");
                let project_root = {
                    let state = self.state.read().await;
                    state.project_root.clone()
                };

                // 1. Rebuild daemon binary (cargo build -p runtimed)
                if !run_cargo_build_daemon(&project_root) {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "cargo build -p runtimed failed — check the supervisor logs for details",
                    )]));
                }

                // 2. Rebuild Python bindings (maturin develop)
                if !run_maturin_develop(&project_root) {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "maturin develop failed — check the supervisor logs for details\n\
                         (daemon binary was rebuilt successfully)",
                    )]));
                }

                // 3. Stop whatever daemon is running (managed or not) and start fresh
                {
                    let mut state = self.state.write().await;
                    if let Some(ref mut child) = state.daemon_child {
                        info!(
                            "Stopping managed daemon (PID {:?}) for rebuild...",
                            child.id()
                        );
                        let _ = child.kill();
                        let _ = child.wait();
                        state.daemon_child = None;
                    }

                    // Also stop any unmanaged daemon on the socket (e.g. launchd)
                    if let Some(info) = daemon_status(&project_root) {
                        if info.running {
                            if let Some(di) = &info.daemon_info {
                                if let Some(pid) = di.pid {
                                    info!("Stopping unmanaged daemon (PID {pid}) for rebuild...");
                                    stop_daemon_by_pid(&project_root, pid);
                                }
                            }
                        }
                    }

                    state.daemon_child = start_daemon(&project_root);
                }

                if !wait_for_daemon(&project_root, Duration::from_secs(30)) {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "Rebuild succeeded but daemon did not become ready within 30s",
                    )]));
                }

                // Verify the running daemon matches the just-built binary
                let version_ok = match (
                    daemon_status(&project_root)
                        .and_then(|i| i.daemon_info)
                        .and_then(|di| di.version),
                    expected_daemon_version(&project_root),
                ) {
                    (Some(running), Some(expected)) => {
                        if running != expected {
                            warn!(
                                "Daemon version mismatch after rebuild: running={running}, expected={expected}"
                            );
                            false
                        } else {
                            true
                        }
                    }
                    _ => true, // can't determine — assume ok
                };

                // 4. Clear circuit breaker and restart child MCP server
                {
                    let mut state = self.state.write().await;
                    state.recent_crashes.clear();
                }
                let version_warning = if !version_ok {
                    "\n\n⚠️  Warning: daemon version mismatch — another process may have \
                     claimed the socket (e.g. launchd). Run supervisor_restart target=daemon \
                     to force a clean restart."
                } else {
                    ""
                };
                match self.restart_child().await {
                    Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                        "Rebuilt daemon + Python bindings and restarted everything successfully{version_warning}",
                    ))])),
                    Err(e) => Ok(CallToolResult::success(vec![Content::text(format!(
                        "Rebuild and daemon restart succeeded but MCP server restart failed: {e}{version_warning}"
                    ))])),
                }
            }
            "supervisor_vite_logs" => {
                let lines = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("lines"))
                    .and_then(Value::as_u64)
                    .unwrap_or(50) as usize;

                let state = self.state.read().await;
                let log_path = Some(state.log_dir.join("vite.log"));

                match log_path {
                    Some(path) if path.exists() => {
                        let text = tail_file(&path, lines);
                        Ok(CallToolResult::success(vec![Content::text(text)]))
                    }
                    _ => Ok(CallToolResult::success(vec![Content::text(
                        "No Vite log found. Start Vite first with supervisor_start_vite.",
                    )])),
                }
            }
            "supervisor_logs" => {
                let lines = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("lines"))
                    .and_then(Value::as_u64)
                    .unwrap_or(50) as usize;

                let state = self.state.read().await;
                let log_path = daemon_log_path(&state.project_root, &state.socket_path);

                match log_path {
                    Some(path) if path.exists() => {
                        let text = tail_file(&path, lines);
                        Ok(CallToolResult::success(vec![Content::text(text)]))
                    }
                    Some(path) => Ok(CallToolResult::success(vec![Content::text(format!(
                        "Daemon log not found at {}",
                        path.display()
                    ))])),
                    None => Ok(CallToolResult::success(vec![Content::text(
                        "Could not determine daemon log path",
                    )])),
                }
            }
            "supervisor_set_mode" => {
                let mode = request
                    .arguments
                    .as_ref()
                    .and_then(|args| args.get("mode"))
                    .and_then(Value::as_str)
                    .unwrap_or("");

                let new_release = match mode {
                    "release" => true,
                    "debug" => false,
                    other => {
                        return Ok(CallToolResult::success(vec![Content::text(format!(
                            "Unknown mode '{other}'. Use 'debug' or 'release'."
                        ))]));
                    }
                };

                let old_release = RELEASE_MODE.load(Ordering::Relaxed);
                if old_release == new_release {
                    return Ok(CallToolResult::success(vec![Content::text(format!(
                        "Already in {mode} mode, no change needed."
                    ))]));
                }

                info!(
                    "Switching daemon mode: {} → {mode}",
                    if old_release { "release" } else { "debug" }
                );

                let project_root = {
                    let state = self.state.read().await;
                    state.project_root.clone()
                };

                // Validate both target binaries exist before stopping anything.
                // wait_for_daemon uses `runt` to probe status, so both are needed.
                RELEASE_MODE.store(new_release, Ordering::Relaxed);
                let target_runtimed = cargo_binary(&project_root, "runtimed");
                let target_runt = cargo_binary(&project_root, "runt");
                let profile_flag = if new_release { "--release " } else { "" };

                if !target_runtimed.exists() || !target_runt.exists() {
                    RELEASE_MODE.store(old_release, Ordering::Relaxed);
                    let mut missing = Vec::new();
                    if !target_runtimed.exists() {
                        missing.push(format!("runtimed: {}", target_runtimed.display()));
                    }
                    if !target_runt.exists() {
                        missing.push(format!("runt: {}", target_runt.display()));
                    }
                    return Ok(CallToolResult::success(vec![Content::text(format!(
                        "Cannot switch to {mode} mode — missing binaries:\n  {}\n\
                         Build them first with: cargo build {}-p runtimed -p runt-cli",
                        missing.join("\n  "),
                        profile_flag
                    ))]));
                }

                // Stop the current daemon. If we manage it, kill the child directly.
                // If it was already running (unmanaged), stop it via `runt daemon stop`
                // using the OLD mode's binary so the CLI matches the running daemon.
                {
                    let mut state = self.state.write().await;
                    if let Some(ref mut child) = state.daemon_child {
                        info!("Stopping managed daemon for mode switch...");
                        let _ = child.kill();
                        let _ = child.wait();
                        state.daemon_child = None;
                    } else {
                        // Unmanaged daemon — stop via runt CLI (old mode binary)
                        RELEASE_MODE.store(old_release, Ordering::Relaxed);
                        let old_runt = cargo_binary(&project_root, "runt");
                        RELEASE_MODE.store(new_release, Ordering::Relaxed);
                        if old_runt.exists() {
                            info!("Stopping unmanaged daemon via runt CLI...");
                            let mut stop_cmd = std::process::Command::new(&old_runt);
                            stop_cmd
                                .args(["daemon", "stop"])
                                .env("RUNTIMED_DEV", "1")
                                // Always target this worktree's daemon — never the system daemon.
                                .env("RUNTIMED_WORKSPACE_PATH", &project_root);
                            let _ = stop_cmd.status();
                            // Give it a moment to release the socket
                            std::thread::sleep(Duration::from_secs(2));
                        }
                    }
                }

                // Start the new daemon (now using the new mode's binary)
                {
                    let mut state = self.state.write().await;
                    state.daemon_child = start_daemon(&project_root);
                }

                if !wait_for_daemon(&project_root, Duration::from_secs(30)) {
                    // Roll back on failure
                    RELEASE_MODE.store(old_release, Ordering::Relaxed);
                    return Ok(CallToolResult::success(vec![Content::text(format!(
                        "Failed to switch to {mode} mode — daemon did not become ready within 30s. \
                         Rolled back to {} mode.",
                        if old_release { "release" } else { "debug" }
                    ))]));
                }

                // Restart the MCP child so it reconnects to the new daemon
                match self.restart_child().await {
                    Ok(()) => Ok(CallToolResult::success(vec![Content::text(format!(
                        "Switched to {mode} mode. Daemon and MCP server restarted."
                    ))])),
                    Err(e) => {
                        // Daemon is running in new mode but child failed — don't roll back
                        // the daemon, just report the child failure
                        Ok(CallToolResult::success(vec![Content::text(format!(
                            "Switched to {mode} mode. Daemon restarted but MCP server failed: {e}"
                        ))]))
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

// ---------------------------------------------------------------------------
// Log helpers
// ---------------------------------------------------------------------------

/// Read the last N lines of a log file (caps at 64KB to avoid huge reads).
fn tail_file(path: &Path, lines: usize) -> String {
    use std::io::{Read, Seek, SeekFrom};
    match std::fs::File::open(path) {
        Ok(mut file) => {
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            let max_bytes: u64 = 64 * 1024;
            if len > max_bytes {
                let _ = file.seek(SeekFrom::End(-(max_bytes as i64)));
            }
            let mut buf = String::new();
            file.read_to_string(&mut buf).ok();
            let tail: Vec<&str> = buf.lines().rev().take(lines).collect();
            let tail: Vec<&str> = tail.into_iter().rev().collect();
            if tail.is_empty() {
                "(log file is empty)".to_string()
            } else {
                tail.join("\n")
            }
        }
        Err(e) => format!("Failed to read log at {}: {e}", path.display()),
    }
}

// ---------------------------------------------------------------------------
// Daemon log path helper
// ---------------------------------------------------------------------------

/// Derive the daemon log path from the socket path.
/// The socket is at `.../worktrees/{hash}/runtimed.sock` and the log is
/// `.../worktrees/{hash}/runtimed.log`.
fn daemon_log_path(_project_root: &Path, socket_path: &str) -> Option<PathBuf> {
    // Try the socket's sibling file first
    let socket = PathBuf::from(socket_path);
    let log_from_socket = socket.with_file_name("runtimed.log");
    if log_from_socket.parent().is_some_and(|p| p.exists()) {
        return Some(log_from_socket);
    }

    // Fallback: compute from runt-workspace
    let cache_base = dirs::cache_dir()?
        .join(runt_workspace::cache_namespace())
        .join("worktrees");
    let hash = runt_workspace::get_workspace_path().map(|p| runt_workspace::worktree_hash(&p))?;
    Some(cache_base.join(hash).join("runtimed.log"))
}

// ---------------------------------------------------------------------------
// File watcher (phase 2)
// ---------------------------------------------------------------------------

/// Classify a changed file path into a change kind.
fn classify_change(path: &Path, project_root: &Path) -> Option<ChangeKind> {
    let rel = path.strip_prefix(project_root).ok()?;
    let rel_str = rel.to_string_lossy();

    // Rust source files in runtimed-py or runtimed crates
    if (rel_str.starts_with("crates/runtimed-py/src/")
        || rel_str.starts_with("crates/runtimed/src/"))
        && rel_str.ends_with(".rs")
    {
        return Some(ChangeKind::RustChanged);
    }

    // Python source files
    if (rel_str.starts_with("python/nteract/src/") || rel_str.starts_with("python/runtimed/src/"))
        && rel_str.ends_with(".py")
    {
        return Some(ChangeKind::PythonOnly);
    }

    None
}

/// Start the file watcher. Returns a channel receiver that emits the most
/// significant change kind when files are modified.
fn start_file_watcher(
    project_root: &Path,
) -> Result<mpsc::Receiver<ChangeKind>, Box<dyn std::error::Error>> {
    let (tx, rx) = mpsc::channel::<ChangeKind>(8);
    let project_root_owned = project_root.to_path_buf();

    // Paths to watch
    let watch_paths: Vec<PathBuf> = [
        "python/nteract/src",
        "python/runtimed/src",
        "crates/runtimed-py/src",
        "crates/runtimed/src",
    ]
    .iter()
    .map(|p| project_root.join(p))
    .filter(|p| p.exists())
    .collect();

    if watch_paths.is_empty() {
        warn!("No watch paths found — file watching disabled");
        return Ok(rx);
    }

    // Debounced watcher (500ms)
    let mut debouncer = new_debouncer(
        Duration::from_millis(500),
        move |events: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            let events = match events {
                Ok(events) => events,
                Err(e) => {
                    eprintln!("[file-watcher] Error: {e}");
                    return;
                }
            };

            // Classify all changed files and pick the most significant change
            let mut most_significant: Option<ChangeKind> = None;
            for event in &events {
                if event.kind != DebouncedEventKind::Any {
                    continue;
                }
                if let Some(kind) = classify_change(&event.path, &project_root_owned) {
                    most_significant = Some(match (&most_significant, &kind) {
                        // Rust trumps Python
                        (_, ChangeKind::RustChanged) => ChangeKind::RustChanged,
                        (Some(ChangeKind::RustChanged), _) => ChangeKind::RustChanged,
                        _ => kind,
                    });
                }
            }

            if let Some(kind) = most_significant {
                // Non-blocking send — if the channel is full, the watcher
                // will coalesce into the next debounce window
                let _ = tx.try_send(kind);
            }
        },
    )?;

    for path in &watch_paths {
        debouncer
            .watcher()
            .watch(path, notify::RecursiveMode::Recursive)?;
        info!("Watching {}", path.display());
    }

    // Leak the debouncer so it lives for the process lifetime.
    // The supervisor runs until the MCP client disconnects, at which
    // point the whole process exits.
    std::mem::forget(debouncer);

    Ok(rx)
}

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
    // Log to both stderr and a file (.context/nteract-dev.log)
    // stderr goes to whatever the MCP client captures; the file is
    // readable via supervisor tools for debugging.
    let project_root_for_log = resolve_project_root();
    let log_dir = project_root_for_log.join(".context");
    let _ = std::fs::create_dir_all(&log_dir);

    // Rotate previous session's log so the current file only contains this session.
    // Keeps one old copy (.log.1) for debugging daemon-restart-on-failure scenarios.
    let dev_log_path = log_dir.join("nteract-dev.log");
    if dev_log_path.exists() {
        let _ = std::fs::rename(&dev_log_path, log_dir.join("nteract-dev.log.1"));
    }

    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&dev_log_path)
        .ok();

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    if let Some(file) = log_file {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false);
        let stderr_layer = tracing_subscriber::fmt::layer().with_writer(std::io::stderr);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .with(stderr_layer)
            .init();
    } else {
        // Fallback: stderr only
        tracing_subscriber::fmt()
            .with_env_filter(env_filter)
            .with_writer(std::io::stderr)
            .init();
    }

    // Initialize release mode from env var (can be toggled at runtime via supervisor_set_mode)
    if std::env::var("RUNTIMED_RELEASE").is_ok_and(|v| v == "1") {
        RELEASE_MODE.store(true, Ordering::Relaxed);
        info!("Starting in release mode (RUNTIMED_RELEASE=1)");
    }

    let project_root = resolve_project_root();
    info!("Project root: {}", project_root.display());

    // Step 1: Ensure daemon is running
    let mut daemon_child = None;
    let socket_path = match daemon_status(&project_root) {
        Some(info) if info.running => {
            info!("Dev daemon already running at {}", info.socket_path);
            // Check version — warn if stale
            if let Some(expected) = expected_daemon_version(&project_root) {
                let running = info
                    .daemon_info
                    .as_ref()
                    .and_then(|di| di.version.as_deref());
                match running {
                    Some(v) if v != expected => {
                        warn!(
                            "Running daemon version ({v}) doesn't match built binary ({expected}). \
                             Use supervisor_rebuild or supervisor_restart target=daemon to update."
                        );
                    }
                    Some(v) => info!("Daemon version: {v} (matches built binary)"),
                    None => {}
                }
            }
            info.socket_path
        }
        Some(_info) => {
            info!("Daemon not running, starting it...");
            daemon_child = start_daemon(&project_root);
            if !wait_for_daemon(&project_root, Duration::from_secs(30)) {
                error!("Daemon failed to start within 30s");
                std::process::exit(1);
            }
            // Re-query to get the socket path from the now-running daemon
            match daemon_status(&project_root) {
                Some(info) if info.running => info.socket_path,
                _ => {
                    error!("Daemon started but status query failed");
                    std::process::exit(1);
                }
            }
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
                        if !wait_for_daemon(&project_root, Duration::from_secs(30)) {
                            error!("Daemon failed to start within 30s");
                            std::process::exit(1);
                        }
                        // Re-query for fresh socket path
                        match daemon_status(&project_root) {
                            Some(fresh) if fresh.running => fresh.socket_path,
                            _ => {
                                error!("Daemon started but status query failed");
                                std::process::exit(1);
                            }
                        }
                    } else {
                        info.socket_path
                    }
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

    // Step 4: Start file watcher
    let mut file_change_rx = start_file_watcher(&project_root).unwrap_or_else(|e| {
        warn!("File watcher failed to start: {e}");
        // Return a dummy receiver that never fires
        mpsc::channel(1).1
    });

    // Channel for the file watcher to signal tool list changes
    let (tool_list_changed_tx, mut tool_list_changed_rx) = mpsc::channel::<()>(4);

    // Step 5: Start supervisor server on stdin/stdout
    let supervisor = Supervisor::new(
        project_root,
        socket_path,
        child_client,
        daemon_child,
        tool_list_changed_tx,
    );

    let transport = rmcp::transport::io::stdio();
    let server = supervisor.serve(transport).await?;

    // Clone what we need before waiting() consumes the server
    let state_for_watcher = server.service().state.clone();
    let state_for_cleanup = state_for_watcher.clone();
    let peer = server.peer().clone();

    // Spawn the file watcher handler task
    let watcher_supervisor = Supervisor {
        state: state_for_watcher,
    };
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(change_kind) = file_change_rx.recv() => {
                    info!("File change detected: {change_kind:?}");
                    watcher_supervisor.handle_file_change(change_kind).await;
                }
                Some(()) = tool_list_changed_rx.recv() => {
                    // The child was restarted — notify the MCP client
                    if let Err(e) = peer.notify_tool_list_changed().await {
                        warn!("Failed to send tools/list_changed: {e}");
                    } else {
                        info!("Sent tools/list_changed notification to client");
                    }
                }
                else => break,
            }
        }
    });

    info!("MCP supervisor running (file watching active), waiting for client disconnect...");
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
