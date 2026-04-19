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

// Allow `expect()` and `unwrap()` in tests
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, ListResourceTemplatesResult,
    ListResourcesResult, ListToolsResult, ReadResourceRequestParams, ReadResourceResult,
    ServerCapabilities, ServerInfo, Tool,
};
use rmcp::schemars;
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt};
use runt_mcp_proxy::{McpProxy, ProxyConfig};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, Notify, RwLock};
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
    info!("Building daemon + CLI (cargo build -p runtimed -p runt-cli)...");
    let mut cmd = std::process::Command::new("cargo");
    cmd.arg("build")
        .arg("-p")
        .arg("runtimed")
        .arg("-p")
        .arg("runt-cli")
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

/// Build the runt CLI binary (which includes runt-mcp).
/// Respects release mode so the built binary matches what cargo_binary() resolves.
fn build_runt_cli(project_root: &Path) -> bool {
    let mut args = vec!["build", "-p", "runt-cli"];
    if use_release_binaries() {
        args.push("--release");
    }
    let status = std::process::Command::new("cargo")
        .args(&args)
        .current_dir(project_root)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(s) if s.success() => {
            info!("cargo build -p runt-cli succeeded");
            true
        }
        Ok(s) => {
            error!("cargo build -p runt-cli failed with {s}");
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
    // Use a separate target directory so maturin's cdylib build doesn't
    // invalidate fingerprints in the main target/ dir. Without this,
    // concurrent or subsequent cargo builds see stale timestamps from
    // maturin's writes and recompile the entire dependency tree.
    let maturin_target = project_root.join("target/maturin");
    let status = std::process::Command::new("uv")
        .args([
            "run",
            "--directory",
            &project_root.join("python/runtimed").to_string_lossy(),
            "maturin",
            "develop",
            "--target-dir",
            &maturin_target.to_string_lossy(),
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
// Supervisor server (faces the MCP client)
// ---------------------------------------------------------------------------

/// What kind of file change was detected.
#[derive(Debug, Clone, PartialEq)]
enum ChangeKind {
    /// Only Python files changed — restart child only.
    PythonOnly,
    /// Rust bindings files changed (runtimed-py, runtimed) — needs maturin develop + restart.
    RustChanged,
    /// Rust MCP server files changed (runt-mcp, runtimed-client) — needs cargo build + restart.
    RustMcpChanged,
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
    /// MCP proxy core — handles child spawning, forwarding, restart, session tracking.
    /// Created after daemon discovery in the background init task.
    proxy: Option<McpProxy>,
    /// Project root path.
    project_root: PathBuf,
    /// Log directory (.context/ in the project root).
    log_dir: PathBuf,
    /// Daemon socket path.
    socket_path: String,
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

#[derive(Clone)]
struct Supervisor {
    state: Arc<RwLock<SupervisorState>>,
    /// Signaled when the child client is first connected by the background
    /// init task. `list_tools` waits on this so the initial tool list
    /// includes the proxied nteract tools (not just supervisor tools).
    child_ready: Arc<Notify>,
}

impl Supervisor {
    /// Create a supervisor with no proxy yet. The stdio MCP server starts
    /// immediately so the client doesn't time out; the proxy and child are
    /// connected later via a background task.
    fn new_empty(project_root: PathBuf, tool_list_changed_tx: mpsc::Sender<()>) -> Self {
        let log_dir = project_root.join(".context");
        let _ = std::fs::create_dir_all(&log_dir);
        Self {
            state: Arc::new(RwLock::new(SupervisorState {
                proxy: None,
                log_dir,
                project_root,
                socket_path: String::new(),
                last_error: None,
                daemon_child: None,
                tool_list_changed_tx: Some(tool_list_changed_tx),
                managed: HashMap::new(),
            })),
            child_ready: Arc::new(Notify::new()),
        }
    }

    /// Get the proxy, returning an error if not yet initialized.
    fn get_proxy(state: &SupervisorState) -> Result<&McpProxy, McpError> {
        state
            .proxy
            .as_ref()
            .ok_or_else(|| McpError::internal_error("nteract MCP server not yet initialized", None))
    }

    /// Restart the child process via the proxy.
    async fn restart_child(&self) -> Result<(), String> {
        let proxy = {
            let state = self.state.read().await;
            state.proxy.clone()
        };
        match proxy {
            Some(proxy) => {
                let result = proxy.restart_child().await;
                if let Err(ref e) = result {
                    self.state.write().await.last_error = Some(e.clone());
                } else {
                    self.state.write().await.last_error = None;
                    self.child_ready.notify_waiters();
                }
                result
            }
            None => Err("Proxy not initialized".to_string()),
        }
    }

    /// Forward a tool call via the proxy (handles restart-on-disconnect, session tracking).
    async fn forward_tool_call(
        &self,
        params: CallToolRequestParams,
    ) -> Result<CallToolResult, McpError> {
        let state = self.state.read().await;
        let proxy = Self::get_proxy(&state)?;
        let proxy = proxy.clone();
        drop(state);
        proxy.forward_tool_call(params).await
    }

    /// Forward a resource read via the proxy.
    async fn forward_read_resource(
        &self,
        params: ReadResourceRequestParams,
    ) -> Result<ReadResourceResult, McpError> {
        let state = self.state.read().await;
        let proxy = Self::get_proxy(&state)?;
        let proxy = proxy.clone();
        drop(state);
        proxy.forward_read_resource(params).await
    }

    /// Build the supervisor_status result.
    async fn status(&self) -> SupervisorStatus {
        let mut state = self.state.write().await;

        // Failsafe: warn if the socket path doesn't look like a dev daemon socket.
        // Dev daemon sockets should be under worktrees/ to ensure isolation from
        // the system nightly daemon.
        if !state.socket_path.is_empty() && !state.socket_path.contains("worktrees/") {
            warn!(
                "⚠️  Socket path does not contain 'worktrees/': {}\n\
                 This may indicate direnv is not active and the supervisor is \
                 connecting to the system daemon instead of a per-worktree dev daemon.\n\
                 Fix: run `direnv allow` in the repo root.",
                state.socket_path
            );
        }

        // Check proxy's child status
        let (child_running, restart_count) = if let Some(ref proxy) = state.proxy {
            let ps = proxy.state.read().await;
            (ps.child_client.is_some(), ps.restart_count)
        } else {
            (false, 0)
        };
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
            restart_count,
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

        // Ensure pnpm install is in sync with the lockfile. Previously this
        // only ran when node_modules was completely missing, which missed
        // the common case of `git pull` adding a new dep (Rolldown then
        // fails at Vite startup with "failed to resolve import X"). Now:
        //
        //   - if node_modules doesn't exist → install
        //   - if pnpm-lock.yaml is newer than node_modules/.modules.yaml
        //     → install (lockfile drift after a pull)
        //   - otherwise → skip
        let modules_stamp = project_root.join("node_modules/.modules.yaml");
        let lockfile = project_root.join("pnpm-lock.yaml");
        let needs_install = match (modules_stamp.exists(), lockfile.exists()) {
            (false, _) => true, // never installed
            (true, true) => {
                let lock_mtime = std::fs::metadata(&lockfile).and_then(|m| m.modified()).ok();
                let stamp_mtime = std::fs::metadata(&modules_stamp)
                    .and_then(|m| m.modified())
                    .ok();
                matches!((lock_mtime, stamp_mtime), (Some(l), Some(s)) if l > s)
            }
            _ => false,
        };
        if needs_install {
            info!("[supervisor] Running pnpm install (lockfile drift or fresh checkout)");
            let pr = project_root.clone();
            let status = tokio::task::spawn_blocking(move || {
                std::process::Command::new("pnpm")
                    .args(["install", "--prefer-offline"])
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
        let mut child = tokio::task::spawn_blocking(move || {
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

        // Phase 3: Health probe — wait until Vite is actually serving, or
        // fail loudly. Without this, `start_vite` returns Ok as soon as the
        // `pnpm dev` process spawns, even if it crashes milliseconds later
        // (e.g. a broken import in renderer-plugin build). Callers would
        // then tell the user "Vite is running on :PORT" and the notebook
        // window would come up blank. See vite_logs for the failure.
        if let Err(e) = await_vite_ready(&mut child, port).await {
            // Kill whatever the (possibly zombied) child is doing and
            // propagate. No managed entry is stored — caller's report
            // should say "vite: FAILED — …".
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }

        // Phase 4: Re-acquire lock to store the process
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
        let project_root = {
            let state = self.state.read().await;
            state.project_root.clone()
        };

        let skip_maturin = std::env::var("SKIP_MATURIN").unwrap_or_default() == "1";

        match kind {
            ChangeKind::RustMcpChanged => {
                info!("Rust MCP files changed, building runt-cli...");
                if !build_runt_cli(&project_root) {
                    error!("cargo build -p runt-cli failed, keeping current child");
                    return;
                }
                if !skip_maturin {
                    let pr = project_root.clone();
                    tokio::task::spawn_blocking(move || {
                        let _ = run_maturin_develop(&pr);
                    });
                }
            }
            ChangeKind::RustChanged => {
                info!(
                    "Rust binding files changed, building runt-cli{}...",
                    if skip_maturin {
                        ""
                    } else {
                        " + maturin develop"
                    }
                );
                if !build_runt_cli(&project_root) {
                    error!("cargo build -p runt-cli failed, keeping current child");
                    return;
                }
                if !skip_maturin && !run_maturin_develop(&project_root) {
                    warn!("maturin develop failed (runt mcp will still restart)");
                }
            }
            ChangeKind::PythonOnly => {
                if !skip_maturin {
                    let pr = project_root.clone();
                    tokio::task::spawn_blocking(move || {
                        let _ = run_maturin_develop(&pr);
                    });
                }
            }
        }

        // Clear circuit breaker for file-change-triggered restarts
        {
            let state = self.state.read().await;
            if let Some(ref proxy) = state.proxy {
                proxy.reset_circuit_breaker().await;
            }
        }

        // Restart child via proxy (auto-rejoin is handled inside restart_child)
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

    /// Kill any non-managed process holding our Vite port. Skipped on
    /// Windows (no `lsof`) — on Windows, Vite's `--strictPort` failure
    /// surfaces the conflict, and the user can clear it manually.
    ///
    /// Returns the list of PIDs killed, for the `up` status report.
    #[cfg(not(unix))]
    async fn sweep_zombie_vites(&self, _port: u16) -> Vec<u32> {
        Vec::new()
    }

    #[cfg(unix)]
    async fn sweep_zombie_vites(&self, port: u16) -> Vec<u32> {
        {
            let managed_pid: Option<u32> = {
                let mut state = self.state.write().await;
                let proc = state.managed.get_mut("vite");
                match proc {
                    Some(p) => {
                        if p.is_alive() {
                            Some(p.child.id())
                        } else {
                            None
                        }
                    }
                    None => None,
                }
            };

            let output = match tokio::task::spawn_blocking(move || {
                std::process::Command::new("lsof")
                    .args(["-t", "-i", &format!("tcp:{port}"), "-sTCP:LISTEN"])
                    .output()
            })
            .await
            {
                Ok(Ok(out)) => out,
                _ => return Vec::new(),
            };

            if !output.status.success() {
                return Vec::new();
            }

            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut killed = Vec::new();
            for pid_str in stdout.split_whitespace() {
                let Ok(pid) = pid_str.parse::<u32>() else {
                    continue;
                };
                if Some(pid) == managed_pid {
                    continue; // our own process — leave it alone
                }
                warn!("[supervisor] Zombie process on vite port {port}: pid {pid} — killing");
                let _ = std::process::Command::new("kill")
                    .args(["-TERM", &pid.to_string()])
                    .status();
                killed.push(pid);
            }
            // Brief grace period so TIME_WAIT clears before our Vite tries to bind
            if !killed.is_empty() {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            killed
        }
    }

    /// Bring the dev environment up. Idempotent — safe to call repeatedly.
    ///
    /// Sequence:
    /// 1. Apply `mode` if provided (flips the RELEASE_MODE flag).
    /// 2. If `rebuild`, rebuild the daemon binary and Python bindings.
    /// 3. Sweep zombie Vite processes on our port.
    /// 4. Ensure the daemon is running (start if missing; restart on version mismatch).
    /// 5. If `vite`, start the Vite dev server.
    /// 6. Ensure the child is healthy (restart if not running).
    async fn handle_up(&self, request: &CallToolRequestParams) -> Result<CallToolResult, McpError> {
        let params: UpParams = request
            .arguments
            .as_ref()
            .and_then(|v| serde_json::from_value(Value::Object(v.clone())).ok())
            .unwrap_or(UpParams {
                vite: false,
                rebuild: false,
                mode: None,
            });

        let mut report = Vec::<String>::new();

        // Step 1: mode switch
        if let Some(ref mode) = params.mode {
            let want_release = match mode.as_str() {
                "release" => true,
                "debug" => false,
                other => {
                    return Ok(CallToolResult::success(vec![Content::text(format!(
                        "up: unknown mode '{other}'. Use 'debug' or 'release'."
                    ))]));
                }
            };
            let old_release = RELEASE_MODE.load(Ordering::Relaxed);
            if old_release != want_release {
                RELEASE_MODE.store(want_release, Ordering::Relaxed);
                report.push(format!(
                    "mode: switched to {}",
                    if want_release { "release" } else { "debug" }
                ));
            } else {
                report.push(format!(
                    "mode: already {}",
                    if want_release { "release" } else { "debug" }
                ));
            }
        }

        let project_root = self.state.read().await.project_root.clone();

        // Step 2: rebuild (cargo + maturin)
        if params.rebuild {
            info!("[supervisor] up: rebuild requested");
            if !run_cargo_build_daemon(&project_root) {
                return Ok(CallToolResult::success(vec![Content::text(
                    "up: cargo build -p runtimed failed. See the supervisor logs for details.",
                )]));
            }
            report.push("rebuild: daemon binary built".into());

            if std::env::var("SKIP_MATURIN").unwrap_or_default() != "1" {
                if !run_maturin_develop(&project_root) {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "up: maturin develop failed. Daemon binary was rebuilt; \
                         Python bindings were not. See the supervisor logs.",
                    )]));
                }
                report.push("rebuild: python bindings rebuilt".into());
            } else {
                report.push("rebuild: maturin skipped (SKIP_MATURIN=1)".into());
            }
        }

        // Step 3: sweep zombie vites
        let vite_port = runt_workspace::vite_port_for_workspace(&project_root);
        let killed = self.sweep_zombie_vites(vite_port).await;
        if !killed.is_empty() {
            report.push(format!(
                "sweep: killed {} zombie process(es) on port {vite_port}: {:?}",
                killed.len(),
                killed
            ));
        }

        // Step 4: ensure daemon running
        let needs_daemon_restart = if params.rebuild {
            true
        } else {
            match daemon_status(&project_root) {
                Some(info) if info.running => {
                    // Check for version mismatch with expected
                    let running = info.daemon_info.as_ref().and_then(|di| di.version.as_ref());
                    let expected = expected_daemon_version(&project_root);
                    match (running, expected) {
                        (Some(r), Some(e)) if r != &e => {
                            warn!(
                                "[supervisor] Daemon version mismatch: running={r}, expected={e}"
                            );
                            report.push(format!(
                                "daemon: version mismatch (running={r}, expected={e}) — restarting"
                            ));
                            true
                        }
                        _ => {
                            report.push("daemon: already running".into());
                            false
                        }
                    }
                }
                _ => {
                    report.push("daemon: not running — starting".into());
                    true
                }
            }
        };

        if needs_daemon_restart {
            let mut state = self.state.write().await;
            if let Some(ref mut child) = state.daemon_child {
                let _ = child.kill();
                let _ = child.wait();
                state.daemon_child = None;
            }
            // Also stop any unmanaged daemon on the socket
            if let Some(info) = daemon_status(&project_root) {
                if info.running {
                    if let Some(pid) = info.daemon_info.and_then(|di| di.pid) {
                        stop_daemon_by_pid(&project_root, pid);
                    }
                }
            }
            state.daemon_child = start_daemon(&project_root);
            drop(state);

            if !wait_for_daemon(&project_root, Duration::from_secs(30)) {
                return Ok(CallToolResult::success(vec![Content::text(format!(
                    "up: daemon did not become ready within 30s.\n\n{}",
                    report.join("\n")
                ))]));
            }
            report.push("daemon: ready".into());
        }

        // Step 5: optionally start vite
        if params.vite {
            match self.start_vite().await {
                Ok(port) => report.push(format!("vite: running on http://localhost:{port}")),
                Err(e) => report.push(format!("vite: FAILED — {e}")),
            }
        }

        // Step 6: ensure child healthy
        let child_healthy = {
            let state = self.state.read().await;
            if let Some(ref proxy) = state.proxy {
                let ps = proxy.state.read().await;
                ps.child_client.is_some()
            } else {
                false
            }
        };

        if !child_healthy || needs_daemon_restart {
            // Clear circuit breaker on manual up
            {
                let state = self.state.read().await;
                if let Some(ref proxy) = state.proxy {
                    proxy.reset_circuit_breaker().await;
                }
            }
            match self.restart_child().await {
                Ok(()) => report.push("child: restarted".into()),
                Err(e) => report.push(format!("child: restart FAILED — {e}")),
            }
        } else {
            report.push("child: healthy".into());
        }

        Ok(CallToolResult::success(vec![Content::text(
            report.join("\n"),
        )]))
    }

    /// Stop managed processes. Opt-in daemon stop via `daemon=true`.
    async fn handle_down(
        &self,
        request: &CallToolRequestParams,
    ) -> Result<CallToolResult, McpError> {
        let params: DownParams = request
            .arguments
            .as_ref()
            .and_then(|v| serde_json::from_value(Value::Object(v.clone())).ok())
            .unwrap_or(DownParams { daemon: false });

        let mut report = Vec::<String>::new();

        // Stop vite if managed
        match self.stop_managed("vite").await {
            Ok(()) => report.push("vite: stopped".into()),
            Err(e) if e.to_lowercase().contains("no managed") => {
                report.push("vite: not running".into())
            }
            Err(e) => report.push(format!("vite: stop FAILED — {e}")),
        }

        // Stop daemon if requested
        if params.daemon {
            let project_root = self.state.read().await.project_root.clone();
            let mut state = self.state.write().await;
            if let Some(ref mut child) = state.daemon_child {
                let _ = child.kill();
                let _ = child.wait();
                state.daemon_child = None;
                report.push("daemon: managed child stopped".into());
            }
            if let Some(info) = daemon_status(&project_root) {
                if info.running {
                    if let Some(pid) = info.daemon_info.and_then(|di| di.pid) {
                        stop_daemon_by_pid(&project_root, pid);
                        report.push(format!("daemon: unmanaged process pid {pid} stopped"));
                    }
                }
            }
        } else {
            report.push("daemon: left running (pass daemon=true to stop)".into());
        }

        Ok(CallToolResult::success(vec![Content::text(
            report.join("\n"),
        )]))
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

/// Build a JSON Schema object for the given params type.
///
/// The `schemars` schema is guaranteed to be a JSON object at the top level,
/// but the conversion through `serde_json::to_value` is fallible in principle.
/// Propagate any failure as an `McpError::internal_error` so list_tools can
/// return it to the client rather than silently advertising an empty schema.
fn schema_object<T: schemars::JsonSchema>() -> Result<serde_json::Map<String, Value>, McpError> {
    let schema = schemars::schema_for!(T);
    let value = serde_json::to_value(&schema).map_err(|e| {
        McpError::internal_error(
            format!(
                "failed to serialize JSON schema for {}: {e}",
                std::any::type_name::<T>()
            ),
            None,
        )
    })?;
    match value {
        Value::Object(map) => Ok(map),
        other => Err(McpError::internal_error(
            format!(
                "JSON schema for {} was not an object (got {}): this indicates a schemars/serde_json bug",
                std::any::type_name::<T>(),
                match other {
                    Value::Null => "null",
                    Value::Bool(_) => "bool",
                    Value::Number(_) => "number",
                    Value::String(_) => "string",
                    Value::Array(_) => "array",
                    Value::Object(_) => "object",
                }
            ),
            None,
        )),
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[allow(dead_code)] // `mode` is read via serde deserialization, not directly
struct SetModeParams {
    /// The build mode: "debug" or "release".
    mode: String,
}

/// Arguments for the `up` tool. All fields optional — `up` with no args
/// is the idiomatic "get me to a working state" call.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct UpParams {
    /// Start the Vite dev server in addition to the daemon + child.
    /// Defaults to `false` (daemon + child only). Safe to pass `true`
    /// repeatedly — Vite startup is idempotent.
    #[serde(default)]
    vite: bool,

    /// Rebuild the daemon binary and Python bindings before bringing
    /// things up. Equivalent to the old `supervisor_rebuild`. Defaults
    /// to `false`.
    #[serde(default)]
    rebuild: bool,

    /// Optional build mode toggle: "debug" or "release". Omit to leave
    /// the current mode alone.
    #[serde(default)]
    mode: Option<String>,
}

/// Arguments for the `down` tool.
#[derive(Debug, Deserialize, schemars::JsonSchema)]
#[allow(dead_code)]
struct DownParams {
    /// Also stop the daemon (not just vite + child). Defaults to
    /// `false` — `down` by default stops everything *except* the
    /// daemon, because daemons are often managed by launchd / the
    /// installed app and we don't want to fight with them.
    #[serde(default)]
    daemon: bool,
}

/// MCP ServerHandler that proxies to the nteract child + injects supervisor tools.
impl ServerHandler for Supervisor {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_tool_list_changed()
                .enable_resources()
                .enable_resources_list_changed()
                .build(),
        )
        .with_server_info(Implementation::new(
            "nteract-dev",
            env!("CARGO_PKG_VERSION"),
        ))
        .with_instructions(
            "nteract-dev — MCP supervisor proxying to the nteract notebook server. \
             Includes supervisor_status, supervisor_restart, supervisor_rebuild, \
             supervisor_logs, supervisor_start_vite, and supervisor_stop tools \
             for managing the server lifecycle and dev environment. \
             File watching is active: Python changes hot-reload instantly, \
             Rust changes trigger maturin develop + reload. Changed tool \
             behavior takes effect immediately; new/removed tools may take \
             a moment for the client to discover.",
        )
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let mut tools = Vec::new();

        // Supervisor's own tools. Schema generation is fallible in principle
        // (serde_json::to_value can return Err on a type whose Serialize impl
        // misbehaves). Propagate the error rather than unwrapping — a broken
        // schema means a broken tool registration, and it's better to fail
        // the list_tools call than to silently advertise an empty schema.
        let empty_schema = schema_object::<EmptyParams>()?;
        let up_schema = schema_object::<UpParams>()?;
        let down_schema = schema_object::<DownParams>()?;
        let logs_schema = schema_object::<SupervisorLogsParams>()?;

        tools.push(Tool::new(
            "up",
            "Bring the dev environment to a working state. Idempotent: \
             sweeps zombie Vite processes, ensures the daemon is running \
             (starting it if needed), ensures the MCP child is healthy, \
             and optionally starts Vite and/or rebuilds the daemon + \
             Python bindings first. Pass vite=true to also start Vite, \
             rebuild=true to rebuild before starting, mode='debug'|'release' \
             to switch build mode. Returns a structured status report.",
            up_schema,
        ));
        tools.push(Tool::new(
            "down",
            "Stop the managed Vite dev server (if running). Does NOT stop \
             the MCP proxy child — the child auto-restarts on disconnect \
             and killing it here would just cause a restart. Does NOT \
             stop the daemon by default — daemons are often managed by \
             launchd or the installed app. Pass daemon=true to also stop \
             the managed daemon process.",
            down_schema,
        ));
        tools.push(Tool::new(
            "status",
            "Report the current state of the supervisor, child process, \
             daemon, and managed processes (Vite, etc.). Read-only — does \
             not touch anything.",
            empty_schema.clone(),
        ));
        tools.push(Tool::new(
            "logs",
            "Read the last N lines of the daemon log file. Defaults to \
             50 lines.",
            logs_schema.clone(),
        ));
        tools.push(Tool::new(
            "vite_logs",
            "Read the last N lines of the Vite dev server log. Defaults \
             to 50 lines.",
            logs_schema,
        ));

        // Get child tools from the proxy (live or cached), falling back
        // to the built-in tool cache if the proxy isn't ready yet.
        {
            let state = self.state.read().await;
            if let Some(ref proxy) = state.proxy {
                let child_tools = proxy.child_tools().await;
                tools.extend(child_tools);
            } else if let Some(builtin) = runt_mcp_proxy::tools::load_builtin_tools() {
                info!("Serving {} built-in tools (proxy not ready)", builtin.len());
                tools.extend(builtin);
            }
        }

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
        let state = self.state.read().await;
        if let Some(ref proxy) = state.proxy {
            Ok(proxy.child_resources(request).await)
        } else {
            Ok(ListResourcesResult::default())
        }
    }

    async fn list_resource_templates(
        &self,
        request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourceTemplatesResult, McpError> {
        let state = self.state.read().await;
        if let Some(ref proxy) = state.proxy {
            Ok(proxy.child_resource_templates(request).await)
        } else {
            Ok(ListResourceTemplatesResult::default())
        }
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
        match request.name.as_ref() {
            "up" => self.handle_up(&request).await,
            "down" => self.handle_down(&request).await,
            // New short names for read-only tools — handled by the same
            // branches as their supervisor_* predecessors below.
            "status" | "supervisor_status" => {
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
                            let state = self.state.read().await;
                            if let Some(ref proxy) = state.proxy {
                                proxy.reset_circuit_breaker().await;
                            }
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

                // 1. Rebuild daemon binary and CLI (cargo build -p runtimed -p runt-cli)
                if !run_cargo_build_daemon(&project_root) {
                    return Ok(CallToolResult::success(vec![Content::text(
                        "cargo build -p runtimed failed — check the supervisor logs for details",
                    )]));
                }

                // 2. Rebuild Python bindings (maturin develop) — skip if
                // SKIP_MATURIN=1 is set (speeds up Rust-only iteration).
                if std::env::var("SKIP_MATURIN").unwrap_or_default() != "1" {
                    if !run_maturin_develop(&project_root) {
                        return Ok(CallToolResult::success(vec![Content::text(
                            "maturin develop failed — check the supervisor logs for details\n\
                             (daemon binary was rebuilt successfully)",
                        )]));
                    }
                } else {
                    info!("Skipping maturin develop (SKIP_MATURIN=1)");
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
                    let state = self.state.read().await;
                    if let Some(ref proxy) = state.proxy {
                        proxy.reset_circuit_breaker().await;
                    }
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
            "vite_logs" | "supervisor_vite_logs" => {
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
            "logs" | "supervisor_logs" => {
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
            // Everything else → forward to child via proxy.
            // If the proxy isn't ready yet, wait for it.
            _ => {
                let notified = self.child_ready.notified();
                let needs_wait = {
                    let state = self.state.read().await;
                    state.proxy.is_none()
                        || state.proxy.as_ref().is_some_and(|p| {
                            p.state.try_read().is_ok_and(|s| s.child_client.is_none())
                        })
                };
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
    }
}

// ---------------------------------------------------------------------------
// Entrypoint
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Log helpers
// ---------------------------------------------------------------------------

/// Read the last N lines of a log file (caps at 64KB to avoid huge reads).
/// Maximum time to wait for Vite to start accepting connections.
const VITE_READY_TIMEOUT: Duration = Duration::from_secs(20);

/// After first successful TCP connect, wait this long to see whether
/// Vite crashes on its initial build. The renderer-plugin build phase
/// is an async promise — Vite prints "Local: http://…" *before* the
/// build resolves, so a port-bind alone isn't proof of health.
const VITE_POST_CONNECT_STABILIZE: Duration = Duration::from_millis(1500);

/// Wait for a freshly-spawned Vite child to be actually serving, or
/// return a specific failure. Polls two signals in a loop:
///
/// 1. Did the child process die? → fail with exit status.
/// 2. Can we connect a TCP socket to the port? → call it ready, then
///    wait `VITE_POST_CONNECT_STABILIZE` to catch a crash-on-first-build
///    (re-check process death after).
///
/// Returns Ok only when both the port is accepting connections and the
/// process is still alive after the stabilize window.
async fn await_vite_ready(child: &mut std::process::Child, port: u16) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + VITE_READY_TIMEOUT;
    let addr = format!("127.0.0.1:{port}");

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!(
                    "Vite exited during startup (status: {status}). \
                     Run the `vite_logs` tool for the error."
                ));
            }
            Ok(None) => {}
            Err(e) => return Err(format!("Failed to poll Vite process: {e}")),
        }

        let connect = tokio::time::timeout(
            Duration::from_millis(250),
            tokio::net::TcpStream::connect(&addr),
        )
        .await;

        if let Ok(Ok(_stream)) = connect {
            // Port is accepting — stabilize, then re-check the process.
            tokio::time::sleep(VITE_POST_CONNECT_STABILIZE).await;
            match child.try_wait() {
                Ok(Some(status)) => {
                    return Err(format!(
                        "Vite accepted a connection then crashed (status: {status}). \
                         The dev server announces its URL before Rolldown finishes \
                         building — a plugin build failure shows up here. Run \
                         `vite_logs` for the stack trace."
                    ));
                }
                Ok(None) => {
                    info!("[supervisor] Vite health probe passed on port {port}");
                    return Ok(());
                }
                Err(e) => return Err(format!("Failed to poll Vite process: {e}")),
            }
        }

        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "Vite did not accept connections on port {port} within {}s. \
                 Run `vite_logs` for startup output.",
                VITE_READY_TIMEOUT.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

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

    // Rust source files that affect Python bindings (needs maturin develop + cargo build)
    // runtimed-client is shared between runtimed-py and runt-mcp, so changes there
    // affect both the Python and Rust MCP servers.
    if (rel_str.starts_with("crates/runtimed-py/src/")
        || rel_str.starts_with("crates/runtimed/src/")
        || rel_str.starts_with("crates/runtimed-client/src/"))
        && rel_str.ends_with(".rs")
    {
        return Some(ChangeKind::RustChanged);
    }

    // Rust MCP server files only (needs cargo build -p runt-cli, no maturin)
    if rel_str.starts_with("crates/runt-mcp/src/") && rel_str.ends_with(".rs") {
        return Some(ChangeKind::RustMcpChanged);
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
        "crates/runt-mcp/src",
        "crates/runtimed-client/src",
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

fn resolve_project_root() -> std::io::Result<PathBuf> {
    // Allow explicit override via env var — required when launched from a
    // context where cwd is not the repo (e.g. Claude Desktop sets cwd to /).
    if let Ok(path) = std::env::var("RUNTIMED_WORKSPACE_PATH") {
        let p = PathBuf::from(path);
        if p.join("Cargo.toml").exists() {
            return Ok(p);
        }
    }
    // Walk up from current dir looking for Cargo.toml with [workspace].
    // If `current_dir` itself fails (cwd was deleted, permission denied, etc.)
    // propagate the io::Error rather than panicking. If we walk to the
    // filesystem root without finding a workspace Cargo.toml, fail with
    // NotFound — silently returning cwd would leave the supervisor pointed
    // at a non-repo directory and every downstream `project_root.join(...)`
    // would resolve to garbage.
    let start = std::env::current_dir()?;
    let mut dir = start.clone();
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            // Propagate permission / I/O errors on a Cargo.toml that exists
            // but can't be read. Only a successfully-read non-workspace file
            // should cause us to keep walking.
            let contents = std::fs::read_to_string(&cargo_toml)?;
            if contents.contains("[workspace]") {
                return Ok(dir);
            }
        }
        if !dir.pop() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "no workspace Cargo.toml found above {}; \
                     set RUNTIMED_WORKSPACE_PATH to the repo root",
                    start.display()
                ),
            ));
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Log to both stderr and a file (.context/nteract-dev.log)
    // stderr goes to whatever the MCP client captures; the file is
    // readable via supervisor tools for debugging.
    // Resolve the project root up front. If cwd is unreadable there's nothing
    // sensible we can do — fail fast with a clear message so the MCP client
    // sees a startup error instead of a silently-misconfigured supervisor.
    let project_root_for_log = resolve_project_root().map_err(|e| {
        format!(
            "failed to resolve project root (could not read current directory): {e}. \
             Set RUNTIMED_WORKSPACE_PATH to the repo root to override."
        )
    })?;
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

    let project_root = resolve_project_root().map_err(|e| {
        format!(
            "failed to resolve project root: {e}. \
             Set RUNTIMED_WORKSPACE_PATH to the repo root to override."
        )
    })?;
    info!("Project root: {}", project_root.display());

    // Step 1: Start the stdio MCP server IMMEDIATELY so the client doesn't
    // time out waiting for the initialize response. The child process and
    // daemon are connected in a background task — until then, the supervisor
    // returns only its own tools and empty resource lists.
    let (tool_list_changed_tx, mut tool_list_changed_rx) = mpsc::channel::<()>(4);
    let supervisor = Supervisor::new_empty(project_root.clone(), tool_list_changed_tx);

    let transport = rmcp::transport::io::stdio();
    let server = supervisor.serve(transport).await?;
    info!("MCP server initialized on stdio (supervisor tools available)");

    // Extract upstream client identity from the MCP initialize handshake.
    // This is forwarded to the child so its peer label reflects the real
    // client (e.g. "Claude Code") rather than the supervisor's name.
    let (upstream_name, upstream_title) = server
        .peer()
        .peer_info()
        .map(|info| {
            let name = info.client_info.name.clone();
            let title = info.client_info.title.clone();
            info!("Upstream MCP client: name={name:?}, title={title:?}");
            (name, title)
        })
        .unwrap_or_else(|| ("nteract-dev".to_string(), None));

    // Clone what we need before waiting() consumes the server
    let state_for_init = server.service().state.clone();
    let state_for_watcher = state_for_init.clone();
    let state_for_cleanup = state_for_init.clone();
    let child_ready = server.service().child_ready.clone();
    let peer = server.peer().clone();
    let peer_for_init = peer.clone();

    // Step 2: Spawn background task to do the heavy setup (daemon, build,
    // child spawn, file watcher). When done, populates state and notifies
    // the client that new tools are available.
    let init_project_root = project_root.clone();
    tokio::spawn(async move {
        let project_root = init_project_root;

        // 2a: Ensure daemon is running
        let mut daemon_child = None;
        let socket_path = match daemon_status(&project_root) {
            Some(info) if info.running => {
                info!("Dev daemon already running at {}", info.socket_path);
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
                    child_ready.notify_waiters();
                    return;
                }
                match daemon_status(&project_root) {
                    Some(info) if info.running => info.socket_path,
                    _ => {
                        error!("Daemon started but status query failed");
                        child_ready.notify_waiters();
                        return;
                    }
                }
            }
            None => {
                warn!("runt CLI not found, building...");
                let pr = project_root.clone();
                let build_ok = tokio::task::spawn_blocking(move || {
                    std::process::Command::new("cargo")
                        .args(["build", "-p", "runt-cli"])
                        .current_dir(&pr)
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false)
                })
                .await
                .unwrap_or(false);
                if !build_ok {
                    error!("Failed to build runt CLI");
                    child_ready.notify_waiters();
                    return;
                }

                match daemon_status(&project_root) {
                    Some(info) => {
                        if !info.running {
                            daemon_child = start_daemon(&project_root);
                            if !wait_for_daemon(&project_root, Duration::from_secs(30)) {
                                error!("Daemon failed to start within 30s");
                                child_ready.notify_waiters();
                                return;
                            }
                            match daemon_status(&project_root) {
                                Some(fresh) if fresh.running => fresh.socket_path,
                                _ => {
                                    error!("Daemon started but status query failed");
                                    child_ready.notify_waiters();
                                    return;
                                }
                            }
                        } else {
                            info.socket_path
                        }
                    }
                    None => {
                        error!("Cannot determine daemon socket path");
                        child_ready.notify_waiters();
                        return;
                    }
                }
            }
        };

        // 2b: Build runt-cli
        info!("Building runt-cli...");
        let pr = project_root.clone();
        let build_ok = tokio::task::spawn_blocking(move || build_runt_cli(&pr))
            .await
            .unwrap_or(false);
        if !build_ok {
            error!("Failed to build runt-cli — MCP server will not work");
        }
        // Also ensure maturin develop in background (dev workflow)
        let pr = project_root.clone();
        tokio::task::spawn_blocking(move || {
            if !ensure_maturin_develop(&pr) {
                warn!("maturin develop failed — Python bindings may be stale");
            }
        });

        // 2c: Create the MCP proxy and spawn the child
        let mut child_env = std::collections::HashMap::new();
        child_env.insert("RUNTIMED_DEV".to_string(), "1".to_string());
        child_env.insert("RUNTIMED_SOCKET_PATH".to_string(), socket_path.clone());

        // Extract tool_list_changed_tx from state for the proxy
        let tool_list_changed_tx = {
            let mut state = state_for_init.write().await;
            state.tool_list_changed_tx.take()
        };

        let project_root_for_resolve = project_root.clone();
        let proxy = McpProxy::new(
            ProxyConfig {
                resolve_child_command: Box::new(move || {
                    let path = cargo_binary(&project_root_for_resolve, "runt");
                    if path.exists() {
                        Ok(path)
                    } else {
                        Err(format!("runt binary not found at {}", path.display()))
                    }
                }),
                child_args: vec!["mcp".to_string()],
                child_env,
                server_name: "nteract-dev".to_string(),
                cache_dir: Some(project_root.join(".context")),
                daemon_socket_path: None,
                monitor_poll_interval_ms: 500,
            },
            tool_list_changed_tx,
        );
        proxy
            .set_upstream_identity(upstream_name, upstream_title)
            .await;

        match proxy.init_child().await {
            Ok(()) => info!("nteract MCP server connected via proxy"),
            Err(e) => {
                error!("Failed to initialize MCP proxy child: {e}");
                let mut state = state_for_init.write().await;
                state.socket_path = socket_path;
                state.daemon_child = daemon_child;
                state.last_error = Some(e);
                state.proxy = Some(proxy);
                child_ready.notify_waiters();
                return;
            }
        }

        // 2d: Store proxy, socket, daemon in supervisor state
        {
            let mut state = state_for_init.write().await;
            state.proxy = Some(proxy);
            state.socket_path = socket_path;
            state.daemon_child = daemon_child;
        }
        // Unblock any call_tool waiting for the child
        child_ready.notify_waiters();

        // 2e: Notify the client that tools/resources are now available.
        // Even if cached tools matched, the child is now live so tool calls work.
        if let Err(e) = peer_for_init.notify_tool_list_changed().await {
            warn!("Failed to send tools/list_changed after init: {e}");
        } else {
            info!("Background init complete — sent tools/list_changed to client");
        }
        if let Err(e) = peer_for_init.notify_resource_list_changed().await {
            warn!("Failed to send resources/list_changed after init: {e}");
        }

        // 2f: Start file watcher
        let mut file_change_rx = start_file_watcher(&project_root).unwrap_or_else(|e| {
            warn!("File watcher failed to start: {e}");
            mpsc::channel(1).1
        });

        // Run the file watcher loop in this task
        let watcher_supervisor = Supervisor {
            state: state_for_watcher,
            child_ready: Arc::new(Notify::new()),
        };
        while let Some(change_kind) = file_change_rx.recv().await {
            info!("File change detected: {change_kind:?}");
            watcher_supervisor.handle_file_change(change_kind).await;
        }
    });

    // Step 3: Handle tool_list_changed notifications from restarts
    let peer_for_notify = peer.clone();
    tokio::spawn(async move {
        while let Some(()) = tool_list_changed_rx.recv().await {
            if let Err(e) = peer_for_notify.notify_tool_list_changed().await {
                warn!("Failed to send tools/list_changed: {e}");
            } else {
                info!("Sent tools/list_changed notification to client");
            }
            if let Err(e) = peer_for_notify.notify_resource_list_changed().await {
                warn!("Failed to send resources/list_changed: {e}");
            } else {
                info!("Sent resources/list_changed notification to client");
            }
        }
    });

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

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/repo")
    }

    // Path-classification: maps a touched file under the project root to
    // the kind of restart action needed. Pure logic, easy to test.

    #[test]
    fn classify_python_change_only() {
        for rel in [
            "python/nteract/src/__init__.py",
            "python/nteract/src/sub/dir/file.py",
            "python/runtimed/src/foo.py",
        ] {
            assert_eq!(
                classify_change(&root().join(rel), &root()),
                Some(ChangeKind::PythonOnly),
                "{rel} should be PythonOnly",
            );
        }
    }

    #[test]
    fn classify_rust_bindings_change() {
        for rel in [
            "crates/runtimed-py/src/lib.rs",
            "crates/runtimed/src/lib.rs",
            "crates/runtimed-client/src/lib.rs",
        ] {
            assert_eq!(
                classify_change(&root().join(rel), &root()),
                Some(ChangeKind::RustChanged),
                "{rel} should be RustChanged",
            );
        }
    }

    #[test]
    fn classify_runt_mcp_change() {
        // runt-mcp-only changes don't need maturin develop — just cargo
        // build + child restart.
        assert_eq!(
            classify_change(&root().join("crates/runt-mcp/src/main.rs"), &root()),
            Some(ChangeKind::RustMcpChanged),
        );
    }

    #[test]
    fn classify_ignores_pyi_stub_files() {
        // .pyi is a type stub — no runtime impact, no need to reload.
        assert_eq!(
            classify_change(&root().join("python/runtimed/src/_internals.pyi"), &root()),
            None,
        );
    }

    #[test]
    fn classify_ignores_non_source_files() {
        for rel in [
            "crates/runtimed/Cargo.toml",
            "crates/runtimed/src/lib.md",
            "crates/runtimed/README.md",
            "python/nteract/pyproject.toml",
            "python/runtimed/src/foo.txt",
            "docs/something.md",
            "README.md",
        ] {
            assert_eq!(
                classify_change(&root().join(rel), &root()),
                None,
                "{rel} should not trigger any restart",
            );
        }
    }

    #[test]
    fn classify_ignores_paths_outside_project_root() {
        // strip_prefix returns Err → classify_change returns None.
        let outside = PathBuf::from("/somewhere/else/crates/runtimed/src/lib.rs");
        assert_eq!(classify_change(&outside, &root()), None);
    }

    #[test]
    fn classify_ignores_test_dirs_outside_src() {
        // crates/{name}/tests/ is not under .../src/ — should not match.
        assert_eq!(
            classify_change(
                &root().join("crates/runtimed/tests/integration.rs"),
                &root(),
            ),
            None,
        );
    }

    #[test]
    fn classify_ignores_python_outside_known_packages() {
        // python/dx/src/ isn't in the watcher's matrix — dx changes
        // don't trigger child restart by design (dx is kernel-side).
        assert_eq!(
            classify_change(&root().join("python/dx/src/foo.py"), &root()),
            None,
        );
    }

    #[test]
    fn should_skip_blob_sweep_helpers_have_sane_defaults() {
        // Sanity guards on the small accessor functions the dispatcher reads.
        assert_eq!(default_restart_target(), "child");
        assert_eq!(default_log_lines(), 50);
    }
}
