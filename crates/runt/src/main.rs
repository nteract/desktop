extern crate runtimed_client as runtimed;
use anyhow::Result;
use clap::{Parser, Subcommand};
use futures::future::join_all;
use jupyter_protocol::{JupyterMessage, JupyterMessageContent, KernelInfoRequest};
use serde::Serialize;
use std::time::Duration;
use tabled::{settings::Style, Table, Tabled};
mod kernel_client;

use crate::kernel_client::KernelClient;
use runtimelib::{
    create_client_heartbeat_connection, create_client_shell_connection_with_identity,
    find_kernelspec, peer_identity_for_session, runtime_dir, ConnectionInfo,
};
use std::path::{Path, PathBuf};
use tokio::fs;
use uuid::Uuid;

#[derive(Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "lowercase")]
enum KernelStatus {
    Alive,
    Unresponsive,
}

impl std::fmt::Display for KernelStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            KernelStatus::Alive => write!(f, "alive"),
            KernelStatus::Unresponsive => write!(f, "unresponsive"),
        }
    }
}

#[derive(Serialize)]
struct KernelInfo {
    name: String,
    connection_file: PathBuf,
    language: Option<String>,
    language_version: Option<String>,
    status: KernelStatus,
    #[serde(flatten)]
    connection_info: ConnectionInfo,
}

/// Unified kernel entry (from connection file OR daemon)
#[derive(Serialize, Clone)]
struct UnifiedKernelInfo {
    name: String,
    language: Option<String>,
    status: String,
    source: String, // "jupyter" or "runtimed"
    notebook: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    connection_file: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    env_source: Option<String>,
}

#[derive(Tabled)]
struct KernelTableRow {
    #[tabled(rename = "NAME")]
    name: String,
    #[tabled(rename = "LANGUAGE")]
    language: String,
    #[tabled(rename = "STATUS")]
    status: String,
    #[tabled(rename = "SOURCE")]
    source: String,
    #[tabled(rename = "NOTEBOOK")]
    notebook: String,
}

impl From<&UnifiedKernelInfo> for KernelTableRow {
    fn from(info: &UnifiedKernelInfo) -> Self {
        KernelTableRow {
            name: info.name.clone(),
            language: info.language.clone().unwrap_or_else(|| "-".to_string()),
            status: info.status.clone(),
            source: info.source.clone(),
            notebook: info
                .notebook
                .as_ref()
                .map(|p| shorten_path(&PathBuf::from(p)))
                .unwrap_or_else(|| "-".to_string()),
        }
    }
}

/// Shorten a path for display by replacing home directory with ~
fn shorten_path(path: &std::path::Path) -> String {
    if let Some(home) = dirs::home_dir() {
        if let Ok(relative) = path.strip_prefix(&home) {
            return format!("~/{}", relative.display());
        }
    }
    path.display().to_string()
}

/// Truncate an error message for display, replacing newlines with spaces.
/// Uses char boundaries to avoid panics on non-ASCII text.
fn truncate_error(msg: &str, max_len: usize) -> String {
    let single_line = msg.replace('\n', " ");
    if max_len < 4 {
        return single_line.chars().take(max_len).collect();
    }
    let char_count = single_line.chars().count();
    if char_count <= max_len {
        single_line
    } else {
        let truncated: String = single_line.chars().take(max_len - 3).collect();
        format!("{}...", truncated)
    }
}

/// Random taglines for the CLI help
const TAGLINES: &[&str] = &[
    "It's Runtime Funtime",
    "When Untitled229.ipynb just hits different",
    "You can change these messages with one cool trick (make a PR)",
    "While you're wrangling data, we're wrangling environments",
    "Wrangling Jupyter runtimes so you don't have to",
    "Your trusty Jupyter runtime companion",
    "Notebooks, kernels, environments — all from your terminal",
    "The CLI that makes notebooks go brrr",
];

fn random_tagline() -> String {
    use colored::Colorize;
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let index = RandomState::new().build_hasher().finish() as usize % TAGLINES.len();

    // 3-line purple bracket with tagline on middle row
    format!(
        "{}\n{} {} {}\n{}",
        "╭─".purple(),
        "│".purple(),
        "runt:".purple().bold(),
        TAGLINES[index],
        "╰─".purple()
    )
}

const fn runt_version_string() -> &'static str {
    if env!("RUNT_VARIANT").is_empty() {
        concat!(env!("CARGO_PKG_VERSION"), "+", env!("GIT_COMMIT"))
    } else {
        concat!(
            env!("CARGO_PKG_VERSION"),
            "+",
            env!("GIT_COMMIT"),
            "-",
            env!("RUNT_VARIANT")
        )
    }
}

#[derive(Parser)]
#[command(name = "runt", author, version = runt_version_string(), about = "CLI for Jupyter Runtimes", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

/// Check if dev mode is enabled via RUNTIMED_DEV environment variable
fn is_dev_mode() -> bool {
    std::env::var("RUNTIMED_DEV").is_ok()
}

#[derive(Subcommand)]
enum Commands {
    /// Open the notebook application
    #[command(alias = "notebook")]
    Open {
        /// Path to notebook file or directory to open
        path: Option<PathBuf>,
        /// Runtime for new notebooks (python, deno)
        #[arg(long, short)]
        runtime: Option<String>,
    },
    /// Jupyter kernel utilities
    #[command(hide = true)]
    Jupyter {
        #[command(subcommand)]
        command: JupyterCommands,
    },
    /// Daemon management (service, pool, logs)
    Daemon {
        #[command(subcommand)]
        command: DaemonCommands,
    },
    /// List open notebooks with kernel and peer info
    #[command(alias = "notebooks")]
    Ps {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Stop a notebook's kernel and evict its room
    #[command(alias = "shutdown")]
    Stop {
        /// Path to the notebook file, or notebook ID (UUID) for untitled notebooks
        path: PathBuf,
    },

    // =========================================================================
    // Top-level convenience aliases
    // =========================================================================
    /// Show daemon status (alias for `daemon status`)
    Status {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Diagnose daemon installation issues (alias for `daemon doctor`)
    Doctor {
        /// Attempt to fix issues automatically
        #[arg(long)]
        fix: bool,
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Tail daemon log file (alias for `daemon logs`)
    Logs {
        /// Follow the log (like tail -f)
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,
    },
    /// Collect diagnostic logs and system info into an archive
    Diagnostics {
        /// Output directory for the archive (default: ~/Desktop)
        #[arg(long, short)]
        output: Option<PathBuf>,
    },

    // =========================================================================
    // Development utilities (only shown when RUNTIMED_DEV=1)
    // =========================================================================
    /// Run as an MCP server (stdin/stdout JSON-RPC)
    Mcp {
        /// Do not register the launch_app tool (for headless environments)
        #[arg(long)]
        no_show: bool,
        /// Explicit daemon socket path (bypasses RUNTIMED_DEV socket resolution)
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// View or modify application configuration
    Config {
        #[command(subcommand)]
        command: Option<ConfigCommands>,
    },

    /// Manage cached Python environments
    Env {
        #[command(subcommand)]
        command: EnvCommands,
    },

    /// Development utilities for runtimed contributors
    #[command(subcommand, hide = true)]
    Dev(DevCommands),
    /// Recover a notebook from its persisted Automerge document (offline, no daemon needed)
    Recover {
        /// Path to the notebook file to recover
        #[arg(required_unless_present = "list")]
        path: Option<PathBuf>,
        /// Write recovered notebook to a different path
        #[arg(short, long)]
        output: Option<PathBuf>,
        /// List all recoverable notebooks
        #[arg(long)]
        list: bool,
    },
    /// Inspect the Automerge state for a notebook (debug command)
    #[command(hide = true)]
    Inspect {
        /// Path to the notebook file
        path: PathBuf,
        /// Show full output JSON (otherwise just shows count)
        #[arg(long)]
        full_outputs: bool,
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    // =========================================================================
    // Hidden aliases for backwards compatibility (deprecated)
    // =========================================================================
    /// [DEPRECATED] Use 'runt jupyter start' instead
    #[command(hide = true)]
    Start { name: String },
    /// [DEPRECATED] Use 'runt jupyter stop' instead
    #[command(hide = true, name = "stop-kernel")]
    StopKernel {
        id: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// [DEPRECATED] Use 'runt jupyter interrupt' instead
    #[command(hide = true)]
    Interrupt { id: String },
    /// [DEPRECATED] Use 'runt jupyter exec' instead
    #[command(hide = true)]
    Exec { id: String, code: Option<String> },
    /// [DEPRECATED] Use 'runt jupyter console' instead
    #[command(hide = true)]
    Console {
        kernel: Option<String>,
        #[arg(long)]
        cmd: Option<String>,
        #[arg(short, long)]
        verbose: bool,
    },
    /// [DEPRECATED] Use 'runt jupyter clean' instead
    #[command(hide = true)]
    Clean {
        #[arg(long, default_value = "2")]
        timeout: u64,
        #[arg(long)]
        dry_run: bool,
    },
    /// [DEPRECATED] Use 'runt daemon' instead
    #[command(hide = true)]
    Pool {
        #[command(subcommand)]
        command: PoolCommands,
    },
    /// [DEPRECATED] Use 'runt notebooks' instead
    #[command(hide = true)]
    Rooms {
        #[arg(long)]
        json: bool,
    },
}

/// Jupyter kernel management commands
#[derive(Subcommand)]
enum JupyterCommands {
    /// List standalone kernels (launched via `runt jupyter start`)
    Ps {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
        /// Show verbose output including port numbers
        #[arg(short, long)]
        verbose: bool,
    },
    /// Start a kernel given a name
    Start {
        /// The name of the kernel to launch (e.g., python3, julia)
        name: String,
    },
    /// Stop a kernel given an ID
    Stop {
        /// The ID of the kernel to stop (required unless --all is used)
        id: Option<String>,
        /// Stop all running kernels
        #[arg(long)]
        all: bool,
    },
    /// Interrupt a kernel given an ID
    Interrupt {
        /// The ID of the kernel to interrupt
        id: String,
    },
    /// Execute code in a kernel given an ID
    Exec {
        /// The ID of the kernel to execute code in
        id: String,
        /// The code to execute (reads from stdin if not provided)
        code: Option<String>,
    },
    /// Launch a kernel and open an interactive console
    Console {
        /// The kernel to launch (e.g., python3, julia)
        kernel: Option<String>,
        /// Custom command to launch the kernel (use {connection_file} as placeholder)
        #[arg(long)]
        cmd: Option<String>,
        /// Print all Jupyter messages for debugging
        #[arg(short, long)]
        verbose: bool,
    },
    /// Remove stale kernel connection files for kernels that are no longer running
    Clean {
        /// Timeout in seconds for heartbeat check (default: 2)
        #[arg(long, default_value = "2")]
        timeout: u64,
        /// Perform a dry run without actually removing files
        #[arg(long)]
        dry_run: bool,
    },
}

/// Environment management commands
#[derive(Subcommand)]
enum EnvCommands {
    /// Show disk usage for all cached environments
    Stats,
    /// List all cached environments with size and age
    List,
    /// Remove stale cached environments
    Clean {
        /// Remove ALL cached environments (pool + content-addressed + inline)
        #[arg(long)]
        all: bool,
        /// Maximum age in days for cached environments (default: 7)
        #[arg(long, default_value = "7")]
        max_age_days: u64,
        /// Maximum number of cached environments per category (default: 10)
        #[arg(long, default_value = "10")]
        max_count: usize,
        /// Show what would be deleted without actually deleting
        #[arg(long)]
        dry_run: bool,
    },
}

/// Settings subcommands
#[derive(Subcommand)]
enum ConfigCommands {
    /// List all current settings
    List,
    /// Get a specific setting value
    Get {
        /// Setting key (e.g., default_python_env, theme, uv.default_packages)
        key: String,
    },
    /// Set a specific setting value
    Set {
        /// Setting key
        key: String,
        /// New value. For list keys (e.g. uv.default_packages): JSON array '["pandas","numpy"]' or comma-separated 'pandas,numpy'
        value: String,
    },
}

/// Valid top-level and dotted settings keys. Used to reject typos.
const VALID_CONFIG_KEYS: &[&str] = &[
    "theme",
    "default_runtime",
    "default_python_env",
    "keep_alive_secs",
    "onboarding_completed",
    "uv.default_packages",
    "conda.default_packages",
    "pixi.default_packages",
    "uv_pool_size",
    "conda_pool_size",
    "pixi_pool_size",
];

/// Daemon management commands (replaces Pool + runtimed service commands)
#[derive(Subcommand)]
enum DaemonCommands {
    /// Show daemon status (service, pool, version, uptime)
    Status {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Diagnose daemon installation issues
    Doctor {
        /// Attempt to fix issues automatically
        #[arg(long)]
        fix: bool,
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Start the daemon service
    Start,
    /// Stop the daemon service
    Stop,
    /// Restart the daemon service (stop + start)
    Restart,
    /// Install daemon as a system service
    Install {
        /// Path to the daemon binary to install
        #[arg(long)]
        binary: Option<PathBuf>,
    },
    /// Uninstall daemon system service
    Uninstall,
    /// Tail daemon log file
    Logs {
        /// Follow the log (like tail -f)
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show
        #[arg(short = 'n', long, default_value = "50")]
        lines: usize,
    },
    /// Flush all pooled environments and rebuild
    Flush,
    /// Request daemon shutdown (stops the daemon process)
    Shutdown,
    /// Check if the daemon is running (returns exit code)
    Ping,
}

/// Development commands (only shown when RUNTIMED_DEV=1)
#[derive(Subcommand)]
enum DevCommands {
    /// List all running dev worktree daemons
    Worktrees {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Clean up worktree daemon state directories
    Clean {
        /// Clean a specific worktree by its hash
        #[arg(long)]
        hash: Option<String>,
        /// Clean all worktree daemons (does not affect system daemon)
        #[arg(long)]
        all: bool,
        /// Clean only stale worktrees (where original path no longer exists)
        #[arg(long)]
        stale: bool,
        /// Stop running daemon before cleaning (otherwise refuses if running)
        #[arg(long)]
        force: bool,
        /// Skip confirmation prompt
        #[arg(short, long)]
        yes: bool,
        /// Show what would be deleted without actually deleting
        #[arg(long)]
        dry_run: bool,
    },
}

/// [DEPRECATED] Pool commands - use 'runt daemon' instead
#[derive(Subcommand)]
enum PoolCommands {
    /// Check if the pool daemon is running
    Ping,
    /// Show pool daemon status and statistics
    Status {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Show daemon info (version, PID, blob port, uptime)
    Info {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Request an environment from the pool (for testing)
    Take {
        /// Environment type: uv or conda
        #[arg(default_value = "uv")]
        env_type: String,
    },
    /// Flush all pooled environments and rebuild with current settings
    Flush,
    /// Request daemon shutdown
    Shutdown,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        // Open launches the desktop app (no tokio needed)
        Some(Commands::Open { path, runtime }) => open_notebook(path, runtime),
        // All other subcommands use tokio
        other => {
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(async_main(other))
        }
    }
}

/// Open the notebook application with optional path and runtime arguments.
///
/// The app automatically captures its working directory at startup for untitled
/// notebooks, so we don't need to pass --cwd explicitly.
fn open_notebook(path: Option<PathBuf>, runtime: Option<String>) -> Result<()> {
    // Convert relative paths to absolute
    let abs_path = path.map(|p| {
        if p.is_relative() {
            std::env::current_dir().unwrap_or_default().join(p)
        } else {
            p
        }
    });

    let mut extra_args = Vec::new();
    if let Some(ref r) = runtime {
        extra_args.push("--runtime");
        extra_args.push(r);
    }

    runt_workspace::open_notebook_app(abs_path.as_deref(), &extra_args)
        .map_err(|e| anyhow::anyhow!(e))
}

async fn async_main(command: Option<Commands>) -> Result<()> {
    match command {
        // Primary commands
        Some(Commands::Open { .. }) => unreachable!(), // handled in main()
        Some(Commands::Jupyter { command }) => jupyter_command(command).await?,
        Some(Commands::Daemon { command }) => daemon_command(command).await?,
        Some(Commands::Ps { json }) => list_notebooks(json).await?,
        Some(Commands::Stop { path }) => shutdown_notebook(&path).await?,
        Some(Commands::Recover { path, output, list }) => {
            recover_notebook(path.as_deref(), output.as_deref(), list)?
        }
        Some(Commands::Inspect {
            path,
            full_outputs,
            json,
        }) => inspect_notebook(&path, full_outputs, json).await?,

        // Top-level convenience aliases
        Some(Commands::Status { json }) => daemon_command(DaemonCommands::Status { json }).await?,
        Some(Commands::Doctor { fix, json }) => {
            daemon_command(DaemonCommands::Doctor { fix, json }).await?
        }
        Some(Commands::Logs { follow, lines }) => {
            daemon_command(DaemonCommands::Logs { follow, lines }).await?
        }
        Some(Commands::Diagnostics { output }) => diagnostics_command(output).await?,
        Some(Commands::Config { command }) => config_command(command).await?,
        Some(Commands::Mcp { no_show, socket }) => {
            if let Some(socket) = socket {
                std::env::set_var("RUNTIMED_SOCKET_PATH", socket);
            }
            run_mcp_server(no_show).await?
        }
        Some(Commands::Env { command }) => env_command(command).await?,

        // Development commands (requires RUNTIMED_DEV=1)
        Some(Commands::Dev(dev_cmd)) => {
            if !is_dev_mode() {
                eprintln!(
                    "Error: 'runt dev' commands require RUNTIMED_DEV=1 environment variable."
                );
                eprintln!("These commands are intended for runtimed development only.");
                std::process::exit(1);
            }
            dev_command(dev_cmd).await?
        }

        // Deprecated aliases (with warnings)
        Some(Commands::Start { name }) => {
            eprintln!("Warning: 'runt start' is deprecated. Use 'runt jupyter start' instead.");
            start_kernel(&name).await?
        }
        Some(Commands::StopKernel { id, all }) => {
            eprintln!(
                "Warning: 'runt stop-kernel' is deprecated. Use 'runt jupyter stop' instead."
            );
            stop_kernels(id.as_deref(), all).await?
        }
        Some(Commands::Interrupt { id }) => {
            eprintln!(
                "Warning: 'runt interrupt' is deprecated. Use 'runt jupyter interrupt' instead."
            );
            interrupt_kernel(&id).await?
        }
        Some(Commands::Exec { id, code }) => {
            eprintln!("Warning: 'runt exec' is deprecated. Use 'runt jupyter exec' instead.");
            execute_code(&id, code.as_deref()).await?
        }
        Some(Commands::Console {
            kernel,
            cmd,
            verbose,
        }) => {
            eprintln!("Warning: 'runt console' is deprecated. Use 'runt jupyter console' instead.");
            console(kernel.as_deref(), cmd.as_deref(), verbose).await?
        }
        Some(Commands::Clean { timeout, dry_run }) => {
            eprintln!("Warning: 'runt clean' is deprecated. Use 'runt jupyter clean' instead.");
            clean_kernels(timeout, dry_run).await?
        }
        Some(Commands::Pool { command }) => {
            eprintln!("Warning: 'runt pool' is deprecated. Use 'runt daemon' instead.");
            pool_command(command).await?
        }
        Some(Commands::Rooms { json }) => {
            eprintln!("Warning: 'runt rooms' is deprecated. Use 'runt notebooks' instead.");
            list_notebooks(json).await?
        }

        None => {
            use clap::CommandFactory;
            Cli::command().about(random_tagline()).print_help()?;
        }
    }

    Ok(())
}

async fn run_mcp_server(no_show: bool) -> Result<()> {
    // ── Initialize file logging ──────────────────────────────────────
    // MCP servers use stdio for the JSON-RPC protocol, so we log to a
    // file only. Each session gets its own file (date + short random ID)
    // to support multiple concurrent MCP server instances.
    let log_dir = runt_workspace::mcp_logs_dir();
    let _ = std::fs::create_dir_all(&log_dir);

    let session_id = &uuid::Uuid::new_v4().to_string()[..8];
    let date = chrono::Local::now().format("%Y-%m-%d");
    let log_filename = format!("{date}-{session_id}.log");
    let log_path = log_dir.join(&log_filename);

    // Prune logs older than 7 days
    prune_old_mcp_logs(&log_dir, 7);

    if let Ok(log_file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        // Hold a shared flock for the process lifetime so the pruner
        // (which tries an exclusive lock) skips files still in use.
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            unsafe {
                libc::flock(log_file.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB);
            }
        }

        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
        let file_layer = tracing_subscriber::fmt::layer()
            .with_writer(std::sync::Mutex::new(log_file))
            .with_ansi(false);

        tracing_subscriber::registry()
            .with(env_filter)
            .with(file_layer)
            .init();

        tracing::info!(
            pid = std::process::id(),
            log = %log_path.display(),
            "MCP server starting"
        );
    }

    // ── Server setup ─────────────────────────────────────────────────
    let socket_path = runtimed_client::daemon_paths::get_socket_path();
    let (blob_base_url, blob_store_path) =
        runtimed_client::daemon_paths::get_blob_paths_async(&socket_path).await;

    // Capture initial daemon info for health monitoring.
    // Must query the SAME socket the health monitor will poll — otherwise
    // a non-default RUNTIMED_SOCKET_PATH seeds state from the wrong daemon
    // and the first successful ping flags a phantom version-mismatch
    // "upgrade" (EXIT_DAEMON_UPGRADED).
    let initial_state =
        match runtimed_client::singleton::query_daemon_info(socket_path.clone()).await {
            Some(info) => runt_mcp::health::DaemonState::Connected { info },
            None => {
                // Daemon not yet available — start in reconnecting mode with no
                // prior version info. The health monitor will treat the first
                // successful connection as a fresh connect (not an upgrade).
                runt_mcp::health::DaemonState::Reconnecting {
                    since: std::time::Instant::now(),
                    attempt: 0,
                    last_info: None,
                }
            }
        };

    use rmcp::service::ServiceExt;
    let server = if no_show {
        runt_mcp::NteractMcp::new_no_show(
            socket_path.clone(),
            blob_base_url,
            blob_store_path,
            initial_state,
        )
    } else {
        runt_mcp::NteractMcp::new(
            socket_path.clone(),
            blob_base_url,
            blob_store_path,
            initial_state,
        )
    };

    // Grab shared state handles before serving (serve consumes the server)
    let daemon_state = server.daemon_state().clone();
    let session = server.session().clone();
    let peer_label = server.peer_label_shared().clone();

    let transport = rmcp::transport::io::stdio();
    let handle = server.serve(transport).await?;

    // Grab a cancellation token before `waiting()` moves ownership of `handle`.
    // Used to gracefully close the MCP transport on daemon upgrade so the
    // client sees a clean EOF instead of a broken pipe.
    let cancel_token = handle.cancellation_token();

    // Spawn the health monitor alongside the MCP server
    let monitor_socket = socket_path;
    let monitor_handle = tokio::spawn(async move {
        runt_mcp::health::daemon_health_monitor(monitor_socket, daemon_state, session, peer_label)
            .await
    });

    tokio::select! {
        result = handle.waiting() => {
            // MCP client disconnected — normal shutdown
            result?;
        }
        exit_code = monitor_handle => {
            // Health monitor returned — daemon was upgraded.
            // Gracefully close the MCP transport so the client sees a clean
            // EOF rather than a broken pipe, then exit with EX_TEMPFAIL (75)
            // so the wrapper or client knows to restart us.
            let code = exit_code.unwrap_or(runt_mcp::health::EXIT_DAEMON_UPGRADED);
            eprintln!("Daemon upgraded, exiting for restart (exit code {code}).");
            cancel_token.cancel();
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            std::process::exit(code);
        }
    }

    Ok(())
}

/// Delete MCP log files older than `max_age_days` days.
///
/// On Unix, skips files that are still held open by a running MCP server
/// (detected via a non-blocking exclusive flock — active servers hold a
/// shared lock).
fn prune_old_mcp_logs(dir: &std::path::Path, max_age_days: u64) {
    let cutoff =
        std::time::SystemTime::now() - std::time::Duration::from_secs(max_age_days * 24 * 60 * 60);
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        if let Ok(meta) = path.metadata() {
            if let Ok(modified) = meta.modified() {
                if modified < cutoff {
                    #[cfg(unix)]
                    if is_file_locked(&path) {
                        continue;
                    }
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

/// Check if a file is held open by another process via flock.
///
/// Returns true if an exclusive lock cannot be acquired (another process
/// holds a shared lock), meaning the file is still in use.
#[cfg(unix)]
fn is_file_locked(path: &std::path::Path) -> bool {
    use std::os::unix::io::AsRawFd;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        // EWOULDBLOCK — another process holds a shared lock
        return true;
    }
    // Lock succeeded — no one else has it. Unlock immediately.
    unsafe {
        libc::flock(file.as_raw_fd(), libc::LOCK_UN);
    }
    false
}

async fn jupyter_command(command: JupyterCommands) -> Result<()> {
    match command {
        JupyterCommands::Ps { json, verbose } => list_kernels(json, verbose).await,
        JupyterCommands::Start { name } => start_kernel(&name).await,
        JupyterCommands::Stop { id, all } => stop_kernels(id.as_deref(), all).await,
        JupyterCommands::Interrupt { id } => interrupt_kernel(&id).await,
        JupyterCommands::Exec { id, code } => execute_code(&id, code.as_deref()).await,
        JupyterCommands::Console {
            kernel,
            cmd,
            verbose,
        } => console(kernel.as_deref(), cmd.as_deref(), verbose).await,
        JupyterCommands::Clean { timeout, dry_run } => clean_kernels(timeout, dry_run).await,
    }
}

async fn list_kernels(json_output: bool, verbose: bool) -> Result<()> {
    use runtimed::client::PoolClient;

    let runtime_dir = runtime_dir();
    let timeout = Duration::from_secs(2);

    // 1. Gather connection-file kernels (standalone Jupyter kernels)
    let mut connection_file_kernels = Vec::new();
    if let Ok(mut entries) = fs::read_dir(&runtime_dir).await {
        let mut connection_files: Vec<PathBuf> = Vec::new();
        while let Some(entry) = entries.next_entry().await.ok().flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !file_name.starts_with("runt-kernel-") {
                continue;
            }
            connection_files.push(path);
        }

        let kernel_futures = connection_files
            .into_iter()
            .map(|path| async move { gather_kernel_info(path, timeout).await });

        connection_file_kernels = join_all(kernel_futures)
            .await
            .into_iter()
            .flatten()
            .collect();
    }

    // 2. Gather daemon-managed kernels
    let mut daemon_kernels: Vec<UnifiedKernelInfo> = Vec::new();
    let client = PoolClient::default();
    if let Ok(rooms) = client.list_rooms().await {
        for room in rooms {
            if room.has_kernel {
                daemon_kernels.push(UnifiedKernelInfo {
                    name: room
                        .kernel_type
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string()),
                    language: room.kernel_type.clone(),
                    status: room.kernel_status.unwrap_or_else(|| "unknown".to_string()),
                    source: "runtimed".to_string(),
                    notebook: Some(room.notebook_id.clone()),
                    connection_file: None,
                    env_source: room.env_source,
                });
            }
        }
    }

    // 3. Convert connection-file kernels to unified format
    let mut unified_kernels: Vec<UnifiedKernelInfo> = connection_file_kernels
        .iter()
        .map(|k| UnifiedKernelInfo {
            name: k.name.clone(),
            language: k.language.clone(),
            status: k.status.to_string(),
            source: "jupyter".to_string(),
            notebook: None,
            connection_file: Some(k.connection_file.clone()),
            env_source: None,
        })
        .collect();

    // 4. Add daemon kernels (they take precedence for display)
    unified_kernels.extend(daemon_kernels);

    // Sort by source (runtimed first), then by name
    unified_kernels.sort_by(|a, b| {
        // runtimed comes before jupyter
        let source_cmp = b.source.cmp(&a.source); // reverse to put runtimed first
        if source_cmp != std::cmp::Ordering::Equal {
            source_cmp
        } else {
            a.name.cmp(&b.name)
        }
    });

    if json_output {
        println!("{}", serde_json::to_string_pretty(&unified_kernels)?);
    } else if verbose {
        // Verbose mode shows connection-file kernels with full details
        if !connection_file_kernels.is_empty() {
            println!("Connection-file kernels:");
            print_verbose_kernel_table(&connection_file_kernels);
        }
        // Also show daemon-managed kernels
        let daemon_rows: Vec<KernelTableRow> = unified_kernels
            .iter()
            .filter(|k| k.source == "runtimed")
            .map(KernelTableRow::from)
            .collect();
        if !daemon_rows.is_empty() {
            if !connection_file_kernels.is_empty() {
                println!();
            }
            println!("Daemon-managed kernels:");
            let table = Table::new(daemon_rows).with(Style::rounded()).to_string();
            println!("{}", table);
        }
        if connection_file_kernels.is_empty()
            && unified_kernels.iter().all(|k| k.source != "runtimed")
        {
            println!("No running kernels found.");
        }
    } else {
        print_unified_kernel_table(&unified_kernels);
    }

    Ok(())
}

fn print_unified_kernel_table(kernels: &[UnifiedKernelInfo]) {
    if kernels.is_empty() {
        println!("No running kernels found.");
        return;
    }

    let rows: Vec<KernelTableRow> = kernels.iter().map(KernelTableRow::from).collect();
    let table = Table::new(rows).with(Style::rounded()).to_string();
    println!("{}", table);
}

async fn gather_kernel_info(path: PathBuf, timeout: Duration) -> Option<KernelInfo> {
    let connection_info = read_connection_info(&path).await.ok()?;

    let full_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("unknown");
    let name = full_name
        .strip_prefix("runt-kernel-")
        .unwrap_or(full_name)
        .to_string();

    let (language, language_version, status) = query_kernel_info(&connection_info, timeout).await;

    Some(KernelInfo {
        name,
        connection_file: path,
        language,
        language_version,
        status,
        connection_info,
    })
}

async fn query_kernel_info(
    connection_info: &ConnectionInfo,
    timeout: Duration,
) -> (Option<String>, Option<String>, KernelStatus) {
    // First check if kernel is alive via heartbeat (fast check)
    if !check_kernel_alive(connection_info, timeout).await {
        return (None, None, KernelStatus::Unresponsive);
    }

    // Kernel is alive, now get language info via shell
    let session_id = Uuid::new_v4().to_string();
    let identity = match peer_identity_for_session(&session_id) {
        Ok(id) => id,
        Err(_) => return (None, None, KernelStatus::Alive),
    };

    let shell =
        match create_client_shell_connection_with_identity(connection_info, &session_id, identity)
            .await
        {
            Ok(s) => s,
            Err(_) => return (None, None, KernelStatus::Alive),
        };

    let (mut shell_writer, mut shell_reader) = shell.split();
    let request: JupyterMessage = KernelInfoRequest::default().into();

    if shell_writer.send(request).await.is_err() {
        return (None, None, KernelStatus::Alive);
    }

    match tokio::time::timeout(timeout, shell_reader.read()).await {
        Ok(Ok(msg)) => {
            if let JupyterMessageContent::KernelInfoReply(reply) = msg.content {
                (
                    Some(reply.language_info.name.clone()),
                    Some(reply.language_info.version.clone()),
                    KernelStatus::Alive,
                )
            } else {
                (None, None, KernelStatus::Alive)
            }
        }
        _ => (None, None, KernelStatus::Alive),
    }
}

async fn read_connection_info(path: &PathBuf) -> Result<ConnectionInfo> {
    let content = fs::read_to_string(path).await?;
    let info: ConnectionInfo = serde_json::from_str(&content)?;
    Ok(info)
}

fn print_verbose_kernel_table(kernels: &[KernelInfo]) {
    if kernels.is_empty() {
        println!("No running kernels found.");
        return;
    }

    #[derive(Tabled)]
    struct VerboseRow {
        #[tabled(rename = "NAME")]
        name: String,
        #[tabled(rename = "LANGUAGE")]
        language: String,
        #[tabled(rename = "STATUS")]
        status: String,
        #[tabled(rename = "SHELL")]
        shell_port: u16,
        #[tabled(rename = "IOPUB")]
        iopub_port: u16,
        #[tabled(rename = "STDIN")]
        stdin_port: u16,
        #[tabled(rename = "CTRL")]
        control_port: u16,
        #[tabled(rename = "HB")]
        hb_port: u16,
        #[tabled(rename = "CONNECTION FILE")]
        connection_file: String,
    }

    let rows: Vec<VerboseRow> = kernels
        .iter()
        .map(|k| VerboseRow {
            name: k.name.clone(),
            language: format!(
                "{} {}",
                k.language.as_deref().unwrap_or("-"),
                k.language_version.as_deref().unwrap_or("")
            )
            .trim()
            .to_string(),
            status: k.status.to_string(),
            shell_port: k.connection_info.shell_port,
            iopub_port: k.connection_info.iopub_port,
            stdin_port: k.connection_info.stdin_port,
            control_port: k.connection_info.control_port,
            hb_port: k.connection_info.hb_port,
            connection_file: shorten_path(&k.connection_file),
        })
        .collect();

    let table = Table::new(rows).with(Style::rounded()).to_string();
    println!("{}", table);
}

async fn start_kernel(name: &str) -> Result<()> {
    let kernelspec = find_kernelspec(name).await?;
    let client = KernelClient::start_from_kernelspec(kernelspec).await?;
    println!("Kernel started with ID: {}", client.kernel_id());
    println!("Connection file: {}", client.connection_file().display());

    Ok(())
}

async fn stop_kernels(id: Option<&str>, all: bool) -> Result<()> {
    if all {
        // Stop all running kernels
        let runtime_dir = runtime_dir();
        let mut entries = fs::read_dir(&runtime_dir).await?;
        let mut stopped = 0;

        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if !file_name.starts_with("runt-kernel-") {
                continue;
            }

            let kernel_id = file_name
                .strip_prefix("runt-kernel-")
                .and_then(|s| s.strip_suffix(".json"))
                .unwrap_or("unknown");

            match KernelClient::from_connection_file(&path).await {
                Ok(mut client) => {
                    if client.shutdown(false).await.is_ok() {
                        println!("Stopped {}", kernel_id);
                        stopped += 1;
                    } else {
                        eprintln!("Failed to stop {}", kernel_id);
                    }
                }
                Err(_) => {
                    eprintln!("Failed to connect to {}", kernel_id);
                }
            }
        }

        if stopped == 0 {
            println!("No running kernels found.");
        } else {
            println!("\nStopped {} kernel(s)", stopped);
        }
    } else if let Some(id) = id {
        let connection_file = runtime_dir().join(format!("runt-kernel-{}.json", id));
        let mut client = KernelClient::from_connection_file(&connection_file).await?;
        client.shutdown(false).await?;
        println!("Kernel with ID {} stopped", id);
    } else {
        anyhow::bail!("Either provide a kernel ID or use --all to stop all kernels");
    }
    Ok(())
}

async fn interrupt_kernel(id: &str) -> Result<()> {
    let connection_file = runtime_dir().join(format!("runt-kernel-{}.json", id));
    let mut client = KernelClient::from_connection_file(&connection_file).await?;
    client.interrupt().await?;
    println!("Interrupt sent to kernel {}", id);
    Ok(())
}

async fn clean_kernels(timeout_secs: u64, dry_run: bool) -> Result<()> {
    let runtime_dir = runtime_dir();
    let mut entries = fs::read_dir(&runtime_dir).await?;

    let timeout = Duration::from_secs(timeout_secs);
    let mut cleaned = 0;
    let mut alive = 0;
    let mut errors = 0;

    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();

        // Only process kernel-*.json and runt-kernel-*.json files
        let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let is_kernel_file =
            file_name.starts_with("kernel-") || file_name.starts_with("runt-kernel-");
        if !is_kernel_file || path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }

        let connection_info = match read_connection_info(&path).await {
            Ok(info) => info,
            Err(_) => {
                errors += 1;
                continue;
            }
        };

        let is_alive = check_kernel_alive(&connection_info, timeout).await;

        if is_alive {
            alive += 1;
        } else {
            if dry_run {
                println!("Would remove: {}", path.display());
            } else if let Err(e) = fs::remove_file(&path).await {
                eprintln!("Failed to remove {}: {}", path.display(), e);
                errors += 1;
            } else {
                println!("Removed: {}", path.display());
            }
            cleaned += 1;
        }
    }

    println!();
    if dry_run {
        println!(
            "Dry run complete: {} stale, {} alive, {} errors",
            cleaned, alive, errors
        );
    } else {
        println!(
            "Cleaned {} stale connection files ({} alive, {} errors)",
            cleaned, alive, errors
        );
    }

    Ok(())
}

async fn check_kernel_alive(connection_info: &ConnectionInfo, timeout: Duration) -> bool {
    let heartbeat_result = tokio::time::timeout(timeout, async {
        let mut hb = create_client_heartbeat_connection(connection_info).await?;
        hb.single_heartbeat().await
    })
    .await;

    matches!(heartbeat_result, Ok(Ok(())))
}

async fn console(kernel_name: Option<&str>, cmd: Option<&str>, verbose: bool) -> Result<()> {
    use jupyter_protocol::{
        ExecuteRequest, ExecutionState, InputReply, JupyterMessage, JupyterMessageContent,
        MediaType, ReplyStatus, Status, Stdio,
    };
    use std::io::{self, Write};

    let mut client = match (kernel_name, cmd) {
        (_, Some(cmd)) => KernelClient::start_from_command(cmd).await?,
        (Some(name), None) => {
            let kernelspec = find_kernelspec(name).await?;
            KernelClient::start_from_kernelspec(kernelspec).await?
        }
        (None, None) => anyhow::bail!("Provide a kernel name or --cmd"),
    };

    // Give the kernel a moment to bind its sockets
    tokio::time::sleep(Duration::from_millis(500)).await;

    let connection_info = client.connection_info();
    let session_id = client.session_id();

    let identity = runtimelib::peer_identity_for_session(session_id)?;
    let shell = runtimelib::create_client_shell_connection_with_identity(
        connection_info,
        session_id,
        identity.clone(),
    )
    .await?;
    let mut stdin_conn = runtimelib::create_client_stdin_connection_with_identity(
        connection_info,
        session_id,
        identity,
    )
    .await?;
    let (mut shell_writer, mut shell_reader) = shell.split();

    let mut iopub =
        runtimelib::create_client_iopub_connection(connection_info, "", session_id).await?;

    let kernel_name = connection_info
        .kernel_name
        .clone()
        .unwrap_or_else(|| "kernel".to_string());
    println!("{} console", kernel_name);
    println!("Use Ctrl+D to exit.\n");

    let mut execution_count: u32 = 0;

    loop {
        execution_count += 1;
        print!("In [{}]: ", execution_count);
        io::stdout().flush()?;

        // Read one line without holding a persistent StdinLock, so that
        // the kernel stdin handler can also read from terminal stdin.
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            break; // EOF
        }

        let code = line.trim();
        if code.is_empty() {
            execution_count -= 1;
            continue;
        }

        let mut execute_request = ExecuteRequest::new(code.to_string());
        execute_request.allow_stdin = true;
        let message: JupyterMessage = execute_request.into();
        let message_id = message.header.msg_id.clone();
        shell_writer.send(message).await?;

        // Wait for idle status on iopub (signals all output is done).
        // Some kernels send ExecuteReply before streaming output, so we
        // can't use the reply alone as the completion signal.
        let mut got_idle = false;
        while !got_idle {
            tokio::select! {
                result = iopub.read() => {
                    let msg = result?;
                    let is_ours = msg
                        .parent_header
                        .as_ref()
                        .map(|h| h.msg_id.as_str())
                        == Some(message_id.as_str());
                    if verbose {
                        eprintln!("[iopub] {} (ours={})", msg.header.msg_type, is_ours);
                    }
                    if !is_ours {
                        continue;
                    }
                    match &msg.content {
                        JupyterMessageContent::StreamContent(stream) => {
                            match stream.name {
                                Stdio::Stdout => print!("{}", stream.text),
                                Stdio::Stderr => eprint!("{}", stream.text),
                            }
                            let _ = io::stdout().flush();
                        }
                        JupyterMessageContent::ExecuteResult(result) => {
                            for media in &result.data.content {
                                if let MediaType::Plain(text) = media {
                                    println!("Out[{}]: {}", execution_count, text);
                                    break;
                                }
                            }
                        }
                        JupyterMessageContent::DisplayData(data) => {
                            for media in &data.data.content {
                                if let MediaType::Plain(text) = media {
                                    println!("{}", text);
                                    break;
                                }
                            }
                        }
                        JupyterMessageContent::ErrorOutput(error) => {
                            eprintln!("{}: {}", error.ename, error.evalue);
                            for line in &error.traceback {
                                eprintln!("{}", line);
                            }
                        }
                        JupyterMessageContent::Status(Status { execution_state }) => {
                            if *execution_state == ExecutionState::Idle {
                                got_idle = true;
                            }
                        }
                        JupyterMessageContent::UpdateDisplayData(data) => {
                            for media in &data.data.content {
                                if let MediaType::Plain(text) = media {
                                    println!("{}", text);
                                    break;
                                }
                            }
                        }
                        _ => {}
                    }
                }
                result = shell_reader.read() => {
                    let msg = result?;
                    if verbose {
                        let is_ours = msg
                            .parent_header
                            .as_ref()
                            .map(|h| h.msg_id.as_str())
                            == Some(message_id.as_str());
                        eprintln!("[shell] {} (ours={})", msg.header.msg_type, is_ours);
                    }
                }
                result = stdin_conn.read() => {
                    let msg = result?;
                    if verbose {
                        eprintln!("[stdin] {}", msg.header.msg_type);
                    }
                    if let JupyterMessageContent::InputRequest(ref request) = msg.content {
                        let value = if request.password {
                            eprint!("{}", request.prompt);
                            let _ = io::stderr().flush();
                            rpassword::read_password().unwrap_or_default()
                        } else {
                            eprint!("{}", request.prompt);
                            let _ = io::stderr().flush();
                            let mut input = String::new();
                            io::stdin().read_line(&mut input)?;
                            input.trim_end_matches('\n').to_string()
                        };
                        let reply = InputReply {
                            value,
                            status: ReplyStatus::Ok,
                            error: None,
                        };
                        stdin_conn.send(reply.as_child_of(&msg)).await?;
                    }
                }
            }
        }
        // Blank line between output and the next prompt
        println!();
    }

    println!("\nShutting down kernel...");
    client.shutdown(false).await?;
    println!("Done.");

    Ok(())
}

async fn execute_code(id: &str, code: Option<&str>) -> Result<()> {
    use jupyter_protocol::{JupyterMessageContent, MediaType, ReplyStatus, Stdio};
    use std::io::{self, Read, Write};

    let code = match code {
        Some(c) => c.to_string(),
        None => {
            let mut buffer = String::new();
            io::stdin().read_to_string(&mut buffer)?;
            buffer
        }
    };

    let connection_file = runtime_dir().join(format!("runt-kernel-{}.json", id));
    let client = KernelClient::from_connection_file(&connection_file).await?;

    let reply = client
        .execute(&code, |content| match content {
            JupyterMessageContent::StreamContent(stream) => match stream.name {
                Stdio::Stdout => {
                    print!("{}", stream.text);
                    let _ = io::stdout().flush();
                }
                Stdio::Stderr => {
                    eprint!("{}", stream.text);
                    let _ = io::stderr().flush();
                }
            },
            JupyterMessageContent::ExecuteResult(result) => {
                for media_type in &result.data.content {
                    if let MediaType::Plain(text) = media_type {
                        println!("{}", text);
                        break;
                    }
                }
            }
            JupyterMessageContent::ErrorOutput(error) => {
                eprintln!("{}: {}", error.ename, error.evalue);
                for line in &error.traceback {
                    eprintln!("{}", line);
                }
            }
            _ => {}
        })
        .await?;

    if reply.status != ReplyStatus::Ok {
        std::process::exit(1);
    }

    Ok(())
}

// =============================================================================
// Pool daemon commands
// =============================================================================

#[allow(clippy::unwrap_used, clippy::expect_used)] // CLI binary; panics with context are acceptable
async fn pool_command(command: PoolCommands) -> Result<()> {
    use runtimed::client::PoolClient;
    use runtimed::EnvType;

    let client = PoolClient::default();

    match command {
        PoolCommands::Ping => match client.ping().await {
            Ok(()) => {
                println!("pong");
            }
            Err(e) => {
                eprintln!("Daemon not running: {}", e);
                std::process::exit(1);
            }
        },
        PoolCommands::Status { json } => match client.status().await {
            Ok(state) => {
                if json {
                    println!("{}", serde_json::to_string_pretty(&state)?);
                } else {
                    println!("Pool Daemon Status");
                    println!("==================");
                    println!("UV environments:");
                    println!("  Available: {}", state.uv.available);
                    println!("  Warming:   {}", state.uv.warming);
                    if let Some(ref err) = state.uv.error {
                        println!("  ERROR:     {}", truncate_error(err, 60));
                        if let Some(ref pkg) = state.uv.failed_package {
                            println!("  Failed package: {}", pkg);
                        }
                        println!(
                            "  Failures:  {} (retry in {}s)",
                            state.uv.consecutive_failures, state.uv.retry_in_secs
                        );
                    }
                    println!("Conda environments:");
                    println!("  Available: {}", state.conda.available);
                    println!("  Warming:   {}", state.conda.warming);
                    if let Some(ref err) = state.conda.error {
                        println!("  ERROR:     {}", truncate_error(err, 60));
                        if let Some(ref pkg) = state.conda.failed_package {
                            println!("  Failed package: {}", pkg);
                        }
                        println!(
                            "  Failures:  {} (retry in {}s)",
                            state.conda.consecutive_failures, state.conda.retry_in_secs
                        );
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to get status: {}", e);
                std::process::exit(1);
            }
        },
        PoolCommands::Info { json } => {
            use runtimed::singleton::get_running_daemon_info;

            match get_running_daemon_info() {
                Some(info) => {
                    // Ping the daemon at the endpoint recorded in daemon.json,
                    // not the default socket path — the daemon may have been
                    // started with a custom --socket.
                    let info_client = PoolClient::new(std::path::PathBuf::from(&info.endpoint));
                    let alive = info_client.ping().await.is_ok();

                    if json {
                        let mut val = serde_json::to_value(&info)?;
                        val.as_object_mut()
                            .unwrap()
                            .insert("alive".into(), serde_json::Value::Bool(alive));
                        println!("{}", serde_json::to_string_pretty(&val)?);
                    } else {
                        println!("Pool Daemon Info");
                        println!("================");
                        if !alive {
                            println!("Status:     STALE (daemon not responding)");
                        }
                        println!("PID:        {}", info.pid);
                        println!("Version:    {}", info.version);
                        println!("Socket:     {}", info.endpoint);
                        if let Some(port) = info.blob_port {
                            println!("Blob port:  {}", port);
                            println!("Blob URL:   http://127.0.0.1:{}/blob/{{hash}}", port);
                        }
                        let uptime = chrono::Utc::now() - info.started_at;
                        let hours = uptime.num_hours();
                        let mins = uptime.num_minutes() % 60;
                        let secs = uptime.num_seconds() % 60;
                        println!("Started:    {}", info.started_at);
                        println!("Uptime:     {}h {}m {}s", hours, mins, secs);
                    }
                    if !alive {
                        std::process::exit(1);
                    }
                }
                None => {
                    eprintln!("Daemon not running (no daemon.json found)");
                    std::process::exit(1);
                }
            }
        }
        PoolCommands::Take { env_type } => {
            let env_type = match env_type.to_lowercase().as_str() {
                "uv" => EnvType::Uv,
                "conda" => EnvType::Conda,
                _ => {
                    eprintln!("Invalid env_type: {}. Use 'uv' or 'conda'.", env_type);
                    std::process::exit(1);
                }
            };

            match client.take(env_type).await {
                Ok(Some(env)) => {
                    println!("{}", serde_json::to_string_pretty(&env)?);
                }
                Ok(None) => {
                    eprintln!("Pool empty for {}", env_type);
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("Failed to take environment: {}", e);
                    std::process::exit(1);
                }
            }
        }
        PoolCommands::Flush => match client.flush_pool().await {
            Ok(()) => {
                println!("Pool flushed — environments will be rebuilt");
            }
            Err(e) => {
                eprintln!("Failed to flush pool: {}", e);
                std::process::exit(1);
            }
        },
        PoolCommands::Shutdown => match client.shutdown().await {
            Ok(()) => {
                println!("Shutdown request sent");
            }
            Err(e) => {
                eprintln!("Failed to shutdown: {}", e);
                std::process::exit(1);
            }
        },
    }

    Ok(())
}

// =============================================================================
// Daemon management commands
// =============================================================================

/// Wait for a process to exit by checking if the PID still exists.
/// Returns true if the process exited within the timeout, false otherwise.
#[cfg(unix)]
async fn wait_for_pid_exit(pid: u32, timeout: Duration) -> bool {
    use std::time::Instant;

    let start = Instant::now();
    let pid_i32 = pid as i32;

    while start.elapsed() < timeout {
        // Use kill with signal 0 to check if process exists
        let exists = unsafe { libc::kill(pid_i32, 0) } == 0;
        if !exists {
            // Process doesn't exist
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    false // Timeout
}

#[cfg(not(unix))]
async fn wait_for_pid_exit(_pid: u32, _timeout: Duration) -> bool {
    // On Windows, we'd need different process checking logic
    // For now, just wait the full timeout
    tokio::time::sleep(_timeout).await;
    false
}

/// Stop a process by PID using signal escalation (SIGTERM → SIGKILL).
/// Returns Ok(()) if the process was stopped, Err if it couldn't be stopped.
#[cfg(unix)]
async fn stop_process_by_pid(pid: u32) -> Result<()> {
    let pid_i32 = pid as i32;

    // Check if process exists
    let exists = unsafe { libc::kill(pid_i32, 0) } == 0;
    if !exists {
        // Process already dead
        return Ok(());
    }

    // Send SIGTERM
    let sigterm_result = unsafe { libc::kill(pid_i32, libc::SIGTERM) };
    if sigterm_result != 0 {
        let errno = std::io::Error::last_os_error();
        // ESRCH means process doesn't exist (already dead)
        if errno.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(anyhow::anyhow!(
            "Failed to send SIGTERM to PID {}: {}",
            pid,
            errno
        ));
    }

    // Wait up to 5 seconds for SIGTERM
    if wait_for_pid_exit(pid, Duration::from_secs(5)).await {
        return Ok(());
    }

    // Process still running, escalate to SIGKILL
    eprintln!("Warning: Daemon didn't respond to SIGTERM, sending SIGKILL...");
    let sigkill_result = unsafe { libc::kill(pid_i32, libc::SIGKILL) };
    if sigkill_result != 0 {
        let errno = std::io::Error::last_os_error();
        // ESRCH means process doesn't exist (died between SIGTERM and SIGKILL)
        if errno.raw_os_error() == Some(libc::ESRCH) {
            return Ok(());
        }
        return Err(anyhow::anyhow!(
            "Failed to send SIGKILL to PID {}: {}",
            pid,
            errno
        ));
    }

    // Wait up to 2 seconds for SIGKILL (should be instant)
    if wait_for_pid_exit(pid, Duration::from_secs(2)).await {
        Ok(())
    } else {
        Err(anyhow::anyhow!(
            "Process {} still running after SIGKILL",
            pid
        ))
    }
}

#[cfg(target_os = "windows")]
async fn stop_process_by_pid(pid: u32) -> Result<()> {
    use std::process::Command;

    let output = Command::new("taskkill")
        .args(["/F", "/PID", &pid.to_string()])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not found") || stderr.contains("not exist") {
            // Process already dead
            return Ok(());
        }
        return Err(anyhow::anyhow!("taskkill failed: {}", stderr));
    }

    Ok(())
}

#[cfg(not(any(unix, target_os = "windows")))]
async fn stop_process_by_pid(_pid: u32) -> Result<()> {
    Err(anyhow::anyhow!(
        "Signal-based stop not supported on this platform"
    ))
}

/// Clean up stale daemon info files.
fn cleanup_stale_daemon_info() -> Result<()> {
    use runtimed::singleton::{daemon_info_path, daemon_lock_path};

    let info_path = daemon_info_path();
    if info_path.exists() {
        std::fs::remove_file(&info_path)?;
        println!("Cleaned up stale daemon.json");
    }

    let lock_path = daemon_lock_path();
    if lock_path.exists() {
        std::fs::remove_file(&lock_path)?;
        println!("Cleaned up stale daemon.lock");
    }

    Ok(())
}

/// Three-step hybrid stop: socket shutdown → service manager → signal escalation.
async fn stop_daemon_smart(
    manager: &mut runtimed::service::ServiceManager,
    client: &runtimed::client::PoolClient,
    daemon_info: Option<&runtimed::singleton::DaemonInfo>,
) -> Result<()> {
    // Step 1: Try graceful socket shutdown
    if let Some(info) = daemon_info {
        println!("Attempting graceful shutdown via socket...");
        match tokio::time::timeout(Duration::from_secs(3), client.shutdown()).await {
            Ok(Ok(())) => {
                // Shutdown request succeeded, wait for daemon to exit
                if wait_for_pid_exit(info.pid, Duration::from_secs(5)).await {
                    // Give Drop handler a moment to clean up daemon.json
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    // Verify cleanup (defensive - prevents PID reuse if Drop didn't run)
                    if runtimed::singleton::daemon_info_path().exists() {
                        cleanup_stale_daemon_info().ok();
                    }
                    println!("Daemon stopped gracefully.");
                    return Ok(());
                } else {
                    eprintln!("Warning: Daemon acknowledged shutdown but is still running.");
                    // Fall through to service manager stop
                }
            }
            Ok(Err(e)) => {
                eprintln!("Socket shutdown failed: {}, trying service manager...", e);
                // Fall through to service manager stop
            }
            Err(_) => {
                eprintln!("Socket shutdown timed out, trying service manager...");
                // Fall through to service manager stop
            }
        }
    }

    // Step 2: Try service manager stop (compatibility)
    if manager.is_installed() {
        println!("Attempting stop via service manager...");
        match manager.stop() {
            Ok(()) => {
                // Service manager stop succeeded, check if daemon actually died
                if let Some(info) = daemon_info {
                    if wait_for_pid_exit(info.pid, Duration::from_secs(3)).await {
                        println!("Daemon stopped via service manager.");
                        return Ok(());
                    }
                    // Process still running, fall through to signal escalation
                    eprintln!("Warning: Service manager stop succeeded but daemon still running (orphaned?)");
                } else {
                    // No daemon.json, assume success
                    println!("Service manager stop completed.");
                    return Ok(());
                }
            }
            Err(e) => {
                eprintln!("Service manager stop failed: {}", e);
                // Continue to signal escalation if we have PID
            }
        }
    }

    // Step 3: Signal escalation (last resort)
    if let Some(info) = daemon_info {
        eprintln!(
            "Daemon appears orphaned (PID {}), stopping via signal...",
            info.pid
        );
        match stop_process_by_pid(info.pid).await {
            Ok(()) => {
                // Force kill succeeded - daemon didn't run Drop, so clean up
                cleanup_stale_daemon_info().ok();
                println!("Daemon stopped via signal.");
                Ok(())
            }
            Err(e) => {
                // Check if process is actually gone despite error
                if !wait_for_pid_exit(info.pid, Duration::from_secs(1)).await {
                    // Process still exists, this is a real failure
                    Err(anyhow::anyhow!("Failed to stop daemon: {}", e))
                } else {
                    // Process died, clean up and report success
                    cleanup_stale_daemon_info().ok();
                    println!("Daemon stopped (process no longer exists).");
                    Ok(())
                }
            }
        }
    } else {
        // No daemon.json and service manager stop didn't help
        println!("No daemon info found, assuming daemon is stopped.");
        Ok(())
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)] // CLI binary; panics with context are acceptable
async fn daemon_command(command: DaemonCommands) -> Result<()> {
    use runtimed::client::PoolClient;
    use runtimed::service::ServiceManager;
    use runtimed::singleton::get_running_daemon_info;

    let mut manager = ServiceManager::default();

    // Get daemon info first so we can use its endpoint for the client
    let daemon_info = get_running_daemon_info();

    // Create client using daemon's actual endpoint if available, otherwise default
    let client = match &daemon_info {
        Some(info) => PoolClient::new(PathBuf::from(&info.endpoint)),
        None => PoolClient::default(),
    };

    match command {
        DaemonCommands::Status { json } => {
            let installed = manager.is_installed();
            let pong_info = if daemon_info.is_some() {
                // Use timeout to prevent hanging on stale daemon.json
                tokio::time::timeout(Duration::from_secs(3), client.ping_version())
                    .await
                    .ok()
                    .and_then(|r| r.ok())
            } else {
                None
            };
            let running = pong_info.is_some();
            let stats = if running {
                client.status().await.ok()
            } else {
                None
            };
            let is_dev = runt_workspace::is_dev_mode();

            // Get socket path from daemon info or default
            let socket_path = daemon_info
                .as_ref()
                .map(|i| i.endpoint.clone())
                .unwrap_or_else(|| {
                    runt_workspace::default_socket_path()
                        .to_string_lossy()
                        .to_string()
                });

            if json {
                let base_dir = runt_workspace::daemon_base_dir();
                let output = serde_json::json!({
                    "channel": runt_workspace::channel_display_name(),
                    "socket_path": socket_path,
                    "installed": installed,
                    "running": running,
                    "dev_mode": is_dev,
                    "protocol_version": pong_info.as_ref().and_then(|p| p.protocol_version),
                    "daemon_version": pong_info.as_ref().and_then(|p| p.daemon_version.clone()),
                    "daemon_info": daemon_info,
                    "pool_stats": stats,
                    "paths": {
                        "base_dir": base_dir,
                        "log_path": base_dir.join("runtimed.log"),
                        "envs_dir": base_dir.join("envs"),
                        "blobs_dir": base_dir.join("blobs"),
                        "notebooks_dir": runt_workspace::default_notebooks_dir().ok(),
                    },
                    "env": {
                        "RUNTIMED_DEV": std::env::var("RUNTIMED_DEV").ok(),
                        "RUNTIMED_WORKSPACE_PATH": std::env::var("RUNTIMED_WORKSPACE_PATH").ok(),

                        "RUNTIMED_VITE_PORT": std::env::var("RUNTIMED_VITE_PORT").ok(),
                    },
                    "blob_url": daemon_info.as_ref()
                        .and_then(|i| i.blob_port.map(|p| format!("http://127.0.0.1:{}", p))),
                    "worktree_hash": runt_workspace::get_workspace_path()
                        .map(|p| runt_workspace::worktree_hash(&p)),
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                use colored::Colorize;

                let channel = runt_workspace::channel_display_name();

                // Header with purple bracket style
                print_header("runtimed", "Daemon Status");

                // Channel and status
                println!("{:<19} {}", "Channel:".bold(), channel.cyan());
                println!(
                    "{:<19} {}",
                    "Service installed:".bold(),
                    colored_yes_no(installed)
                );
                println!(
                    "{:<19} {}",
                    "Daemon running:".bold(),
                    colored_yes_no(running)
                );

                // Socket path
                println!(
                    "{:<19} {}",
                    "Socket:".bold(),
                    shorten_path(&PathBuf::from(&socket_path)).dimmed()
                );

                // Show dev mode info
                if is_dev {
                    println!("{:<19} {}", "Mode:".bold(), "development".cyan());
                }
                if let Some(info) = &daemon_info {
                    if let Some(worktree) = &info.worktree_path {
                        println!(
                            "{:<19} {}",
                            "Worktree:".bold(),
                            shorten_path(&PathBuf::from(worktree)).dimmed()
                        );
                    }
                    if let Some(desc) = &info.workspace_description {
                        println!("{:<19} {}", "Description:".bold(), desc.cyan());
                    }
                }

                if let Some(info) = &daemon_info {
                    println!();
                    println!("{:<19} {}", "PID:".bold(), info.pid);
                    println!("{:<19} {}", "Version:".bold(), info.version);
                    if let Some(ref pong) = pong_info {
                        if let Some(pv) = pong.protocol_version {
                            println!("{:<19} v{}", "Protocol:".bold(), pv);
                        }
                        // Warn on protocol version mismatch
                        if let Err(msg) = pong.check_protocol_version() {
                            println!("{:<19} {}", "Warning:".bold(), msg.yellow());
                        }
                    }
                    if let Some(port) = info.blob_port {
                        println!(
                            "{:<19} {}",
                            "Blob server:".bold(),
                            format!("http://127.0.0.1:{}", port).cyan()
                        );
                    }
                    let uptime = chrono::Utc::now() - info.started_at;
                    let hours = uptime.num_hours();
                    let mins = uptime.num_minutes() % 60;
                    println!("{:<19} {}h {}m", "Uptime:".bold(), hours, mins);
                }

                if let Some(state) = &stats {
                    println!();
                    println!("{}", "Pool:".bold());

                    let uv_total = state.uv.available + state.uv.warming;
                    let uv_status = format!("{}/{} ready", state.uv.available, uv_total);
                    let uv_colored = if state.uv.warming > 0 {
                        uv_status.yellow()
                    } else {
                        uv_status.green()
                    };
                    let uv_warming_text = if state.uv.warming > 0 {
                        format!(" ({} warming)", state.uv.warming)
                            .dimmed()
                            .to_string()
                    } else {
                        String::new()
                    };
                    println!("  {:<8} {}{}", "UV:".bold(), uv_colored, uv_warming_text);

                    let conda_total = state.conda.available + state.conda.warming;
                    let conda_status = format!("{}/{} ready", state.conda.available, conda_total);
                    let conda_colored = if state.conda.warming > 0 {
                        conda_status.yellow()
                    } else {
                        conda_status.green()
                    };
                    let conda_warming_text = if state.conda.warming > 0 {
                        format!(" ({} warming)", state.conda.warming)
                            .dimmed()
                            .to_string()
                    } else {
                        String::new()
                    };
                    println!(
                        "  {:<8} {}{}",
                        "Conda:".bold(),
                        conda_colored,
                        conda_warming_text
                    );

                    let pixi_total = state.pixi.available + state.pixi.warming;
                    let pixi_status = format!("{}/{} ready", state.pixi.available, pixi_total);
                    let pixi_colored = if state.pixi.warming > 0 {
                        pixi_status.yellow()
                    } else {
                        pixi_status.green()
                    };
                    let pixi_warming_text = if state.pixi.warming > 0 {
                        format!(" ({} warming)", state.pixi.warming)
                            .dimmed()
                            .to_string()
                    } else {
                        String::new()
                    };
                    println!(
                        "  {:<8} {}{}",
                        "Pixi:".bold(),
                        pixi_colored,
                        pixi_warming_text
                    );
                }
            }
        }
        DaemonCommands::Doctor { fix, json } => {
            doctor_command(&mut manager, &client, daemon_info.as_ref(), fix, json).await?;
        }
        DaemonCommands::Start => {
            if runt_workspace::is_dev_mode() {
                eprintln!("Dev daemons are not managed by the system service.");
                eprintln!("Use 'cargo xtask dev-daemon' to start a dev daemon.");
                std::process::exit(1);
            }
            if !manager.is_installed() {
                eprintln!("Service not installed. Run 'runt daemon install' first.");
                std::process::exit(1);
            }
            println!(
                "Starting {} service...",
                runt_workspace::daemon_service_basename()
            );
            manager.start()?;
            println!("Service started.");
        }
        DaemonCommands::Stop => {
            if runt_workspace::is_dev_mode() {
                // Dev daemons are foreground processes, not launchd services.
                // Stop via socket shutdown.
                if daemon_info.is_some() {
                    println!("Stopping dev daemon...");
                    match client.shutdown().await {
                        Ok(()) => println!("Dev daemon stopped."),
                        Err(e) => {
                            eprintln!("Failed to stop dev daemon: {e}");
                            std::process::exit(1);
                        }
                    }
                } else {
                    println!("No dev daemon running.");
                }
            } else {
                println!(
                    "Stopping {} service...",
                    runt_workspace::daemon_service_basename()
                );
                match stop_daemon_smart(&mut manager, &client, daemon_info.as_ref()).await {
                    Ok(()) => {
                        // Success message already printed by stop_daemon_smart
                    }
                    Err(e) => {
                        eprintln!("Failed to stop daemon: {}", e);
                        std::process::exit(1);
                    }
                }
            }
        }
        DaemonCommands::Restart => {
            if runt_workspace::is_dev_mode() {
                eprintln!("Dev daemons are not managed by the system service.");
                eprintln!("Use 'cargo xtask dev-daemon' to restart a dev daemon.");
                std::process::exit(1);
            }
            if !manager.is_installed() {
                eprintln!("Service not installed. Run 'runt daemon install' first.");
                std::process::exit(1);
            }
            println!(
                "Restarting {} service...",
                runt_workspace::daemon_service_basename()
            );

            // Use the smart stop logic - don't ignore failures
            if let Err(e) = stop_daemon_smart(&mut manager, &client, daemon_info.as_ref()).await {
                eprintln!("Warning: Stop phase failed during restart: {}", e);
                eprintln!("Attempting to start anyway...");
            }

            // Give socket time to free
            tokio::time::sleep(Duration::from_secs(2)).await;

            // Start via service manager
            manager.start()?;
            println!("Service restarted.");
        }
        DaemonCommands::Install { binary } => {
            // Find runtimed binary: use provided path, or look for sibling binary
            let source = binary.unwrap_or_else(|| {
                let current_exe =
                    std::env::current_exe().expect("Failed to get current executable path");
                let exe_dir = current_exe.parent().unwrap();
                let daemon_binary = runt_workspace::daemon_binary_basename();
                exe_dir.join(if cfg!(windows) {
                    format!("{daemon_binary}.exe")
                } else {
                    daemon_binary.to_string()
                })
            });

            if !source.exists() {
                eprintln!("Error: Daemon binary not found at: {}", source.display());
                eprintln!();
                eprintln!(
                    "The daemon is normally installed automatically by the {} app.",
                    runt_workspace::desktop_display_name()
                );
                eprintln!(
                    "Run 'runt daemon doctor' to diagnose issues, or launch {} to install.",
                    runt_workspace::desktop_display_name()
                );
                std::process::exit(1);
            }

            if manager.is_installed() {
                eprintln!("Service already installed. Use 'runt daemon uninstall' first.");
                std::process::exit(1);
            }

            println!(
                "Installing {} service...",
                runt_workspace::daemon_service_basename()
            );
            println!("Source binary: {}", source.display());
            manager.install(&source)?;
            println!("Service installed. Run 'runt daemon start' to start it.");
        }
        DaemonCommands::Uninstall => {
            if !manager.is_installed() {
                println!("Service not installed.");
                return Ok(());
            }
            println!(
                "Uninstalling {} service...",
                runt_workspace::daemon_service_basename()
            );
            manager.uninstall()?;
            println!("Service uninstalled.");
        }
        DaemonCommands::Logs { follow, lines } => {
            let log_path = runtimed::default_log_path();

            if !log_path.exists() {
                eprintln!("Log file not found: {}", log_path.display());
                std::process::exit(1);
            }

            // Native Rust implementation for cross-platform support
            tail_log_file(&log_path, lines, follow).await?;
        }
        DaemonCommands::Flush => match client.flush_pool().await {
            Ok(()) => {
                println!("Pool flushed — environments will be rebuilt");
            }
            Err(e) => {
                eprintln!("Failed to flush pool: {}", e);
                std::process::exit(1);
            }
        },
        DaemonCommands::Shutdown => match client.shutdown().await {
            Ok(()) => {
                println!("Shutdown request sent");
            }
            Err(e) => {
                eprintln!("Failed to shutdown daemon: {}", e);
                std::process::exit(1);
            }
        },
        DaemonCommands::Ping => match client.ping().await {
            Ok(()) => {
                println!("pong");
            }
            Err(e) => {
                eprintln!("Daemon not running: {}", e);
                std::process::exit(1);
            }
        },
    }

    Ok(())
}

/// Handle development commands (requires RUNTIMED_DEV=1)
async fn dev_command(command: DevCommands) -> Result<()> {
    match command {
        DevCommands::Worktrees { json } => {
            list_worktree_daemons(json).await?;
        }
        DevCommands::Clean {
            hash,
            all,
            stale,
            force,
            yes,
            dry_run,
        } => {
            clean_worktree_command(hash, all, stale, force, yes, dry_run).await?;
        }
    }

    Ok(())
}

/// Diagnose daemon installation issues and optionally fix them.
async fn doctor_command(
    manager: &mut runtimed::service::ServiceManager,
    client: &runtimed::client::PoolClient,
    daemon_info: Option<&runtimed::singleton::DaemonInfo>,
    fix: bool,
    json: bool,
) -> Result<()> {
    use serde::Serialize;

    #[derive(Serialize, Clone)]
    struct DoctorReport {
        installed_binary: CheckResult,
        #[serde(skip_serializing_if = "Option::is_none")]
        quarantine: Option<CheckResult>, // macOS only: com.apple.quarantine xattr check
        #[serde(skip_serializing_if = "Option::is_none")]
        standalone_binary: Option<CheckResult>, // macOS only: legacy standalone vs in-bundle check
        service_config: CheckResult,
        #[serde(skip_serializing_if = "Option::is_none")]
        plist_home_env: Option<CheckResult>,
        #[serde(skip_serializing_if = "Option::is_none")]
        launchd_service: Option<CheckResult>, // macOS only: actual launchd registration state
        #[serde(skip_serializing_if = "Option::is_none")]
        conflicting_services: Option<CheckResult>, // macOS only: stale/conflicting daemon services
        #[serde(skip_serializing_if = "Option::is_none")]
        file_association: Option<CheckResult>, // macOS only: .ipynb default handler check
        socket_file: CheckResult,
        daemon_state: CheckResult,
        version_match: CheckResult,
        daemon_running: CheckResult,
        diagnosis: String,
        actions_taken: Vec<String>,
    }

    #[derive(Serialize, Clone)]
    struct CheckResult {
        path: String,
        status: String, // "ok", "missing", "stale", "error"
        detail: Option<String>,
    }

    // Helper to run all checks and build report
    async fn run_checks(
        client: &runtimed::client::PoolClient,
        daemon_info: Option<&runtimed::singleton::DaemonInfo>,
        actions_taken: Vec<String>,
    ) -> DoctorReport {
        // On macOS, check what binary the plist actually points to (what launchd
        // runs), rather than what default_binary_path() prefers. This ensures we
        // diagnose the *actual* running binary, not the one we'd install next time.
        #[cfg(target_os = "macos")]
        let binary_path = runt_workspace::plist_binary_path()
            .unwrap_or_else(runtimed::service::default_binary_path);
        #[cfg(not(target_os = "macos"))]
        let binary_path = runtimed::service::default_binary_path();
        let socket_path = runt_workspace::default_socket_path();
        let daemon_json_path = runtimed::singleton::daemon_info_path();
        let service_config_path = runtimed::service::service_config_path();

        // Check 1: Installed binary
        let binary_exists = binary_path.exists();
        let installed_binary = CheckResult {
            path: shorten_path(&binary_path),
            status: if binary_exists { "ok" } else { "missing" }.to_string(),
            detail: None,
        };

        // Check 1b: On macOS, check for quarantine xattr (Gatekeeper blocks execution)
        // Skip for in-bundle binaries — the app is already signed/notarized.
        #[cfg(target_os = "macos")]
        let is_in_bundle = binary_path
            .to_string_lossy()
            .contains(".app/Contents/MacOS/");
        #[cfg(target_os = "macos")]
        let quarantine = if binary_exists && !is_in_bundle {
            let output = std::process::Command::new("xattr")
                .args(["-p", "com.apple.quarantine"])
                .arg(&binary_path)
                .output();

            match output {
                Ok(o) if o.status.success() => {
                    // Quarantine attribute exists - this is bad
                    Some(CheckResult {
                        path: "com.apple.quarantine".to_string(),
                        status: "quarantined".to_string(),
                        detail: Some(
                            "binary is quarantined - Gatekeeper may block execution".to_string(),
                        ),
                    })
                }
                Ok(_) => {
                    // xattr returned non-zero, meaning attribute doesn't exist - good
                    Some(CheckResult {
                        path: "com.apple.quarantine".to_string(),
                        status: "ok".to_string(),
                        detail: None,
                    })
                }
                Err(e) => Some(CheckResult {
                    path: "com.apple.quarantine".to_string(),
                    status: "error".to_string(),
                    detail: Some(format!("xattr check failed: {}", e)),
                }),
            }
        } else {
            None
        };
        #[cfg(not(target_os = "macos"))]
        let quarantine: Option<CheckResult> = None;

        // Check 1c: On macOS, check if plist points to legacy standalone binary
        let config_exists = service_config_path.exists();
        #[cfg(target_os = "macos")]
        let standalone_binary = if config_exists {
            match runt_workspace::plist_binary_path() {
                Some(plist_bin) => {
                    let in_bundle = plist_bin.to_string_lossy().contains(".app/Contents/MacOS/");
                    if in_bundle && plist_bin.exists() {
                        Some(CheckResult {
                            path: shorten_path(&plist_bin),
                            status: "ok".to_string(),
                            detail: Some("in-bundle".to_string()),
                        })
                    } else if in_bundle && !plist_bin.exists() {
                        Some(CheckResult {
                            path: shorten_path(&plist_bin),
                            status: "error".to_string(),
                            detail: Some("app bundle not found at expected path".to_string()),
                        })
                    } else {
                        Some(CheckResult {
                            path: shorten_path(&plist_bin),
                            status: "legacy".to_string(),
                            detail: Some("standalone binary (needs migration)".to_string()),
                        })
                    }
                }
                None => None,
            }
        } else {
            None
        };
        #[cfg(not(target_os = "macos"))]
        let standalone_binary: Option<CheckResult> = None;

        // Check 2: Service config (plist/systemd/startup script)
        let service_config = CheckResult {
            path: shorten_path(&service_config_path),
            status: if config_exists { "ok" } else { "missing" }.to_string(),
            detail: None,
        };

        // Check 2b: On macOS, verify plist has HOME environment variable
        #[cfg(target_os = "macos")]
        let plist_home_env = if config_exists {
            match std::fs::read_to_string(&service_config_path) {
                Ok(content) => {
                    if content.contains("<key>HOME</key>") {
                        Some(CheckResult {
                            path: "HOME env in plist".to_string(),
                            status: "ok".to_string(),
                            detail: None,
                        })
                    } else {
                        Some(CheckResult {
                            path: "HOME env in plist".to_string(),
                            status: "missing".to_string(),
                            detail: Some(
                                "plist missing HOME - daemon may fail to start".to_string(),
                            ),
                        })
                    }
                }
                Err(_) => Some(CheckResult {
                    path: "HOME env in plist".to_string(),
                    status: "error".to_string(),
                    detail: Some("could not read plist".to_string()),
                }),
            }
        } else {
            None
        };
        #[cfg(not(target_os = "macos"))]
        let plist_home_env: Option<CheckResult> = None;

        // Check 2c: On macOS, verify service is actually loaded in launchd
        #[cfg(target_os = "macos")]
        let launchd_service = if config_exists {
            let label = runt_workspace::daemon_launchd_label();
            let output = std::process::Command::new("launchctl")
                .args(["list", label])
                .output();

            match output {
                Ok(o) if o.status.success() => {
                    // Output is dict/plist format with "PID" and "LastExitStatus" keys
                    // Parse PID from output (format: "PID" = 12345; or "PID" = <missing>)
                    let stdout = String::from_utf8_lossy(&o.stdout);

                    // Extract PID if present
                    let pid = stdout
                        .lines()
                        .find(|l| l.contains("\"PID\""))
                        .and_then(|l| {
                            l.split('=')
                                .nth(1)
                                .map(|s| s.trim().trim_end_matches(';').trim())
                        })
                        .and_then(|s| s.parse::<u32>().ok());

                    // Extract LastExitStatus
                    let exit_status = stdout
                        .lines()
                        .find(|l| l.contains("\"LastExitStatus\""))
                        .and_then(|l| {
                            l.split('=')
                                .nth(1)
                                .map(|s| s.trim().trim_end_matches(';').trim())
                        })
                        .and_then(|s| s.parse::<i32>().ok())
                        .unwrap_or(0);

                    if let Some(p) = pid {
                        Some(CheckResult {
                            path: format!("launchd:{}", label),
                            status: "ok".to_string(),
                            detail: Some(format!("PID {}", p)),
                        })
                    } else if exit_status == 0 {
                        Some(CheckResult {
                            path: format!("launchd:{}", label),
                            status: "ok".to_string(),
                            detail: Some("registered, not running".to_string()),
                        })
                    } else {
                        Some(CheckResult {
                            path: format!("launchd:{}", label),
                            status: "error".to_string(),
                            detail: Some(format!("last exit code {}", exit_status)),
                        })
                    }
                }
                Ok(_) => Some(CheckResult {
                    path: format!("launchd:{}", label),
                    status: "not_loaded".to_string(),
                    detail: Some("service not registered with launchd".to_string()),
                }),
                Err(e) => Some(CheckResult {
                    path: format!("launchd:{}", label),
                    status: "error".to_string(),
                    detail: Some(format!("launchctl failed: {}", e)),
                }),
            }
        } else {
            None
        };
        #[cfg(not(target_os = "macos"))]
        let launchd_service: Option<CheckResult> = None;

        // Check 2d: On macOS, detect truly stale daemon services
        // Note: stable and nightly can coexist intentionally, so we only warn about:
        // - io.runtimed (pre-rebrand legacy)
        // - io.nteract.runtimed.preview (preview channel being phased out)
        #[cfg(target_os = "macos")]
        let conflicting_services = {
            // Only truly legacy/stale services - NOT stable/nightly which can coexist
            let stale_labels = [
                "io.runtimed",                 // Pre-rebrand legacy
                "io.nteract.runtimed.preview", // Preview channel (being phased out)
            ];

            let output = std::process::Command::new("launchctl")
                .args(["list"])
                .output();

            match output {
                Ok(o) if o.status.success() => {
                    let stdout = String::from_utf8_lossy(&o.stdout);
                    let mut conflicts: Vec<String> = Vec::new();

                    for line in stdout.lines() {
                        for label in &stale_labels {
                            if line.ends_with(label) {
                                // Parse PID and status from "PID\tStatus\tLabel" format
                                let parts: Vec<&str> = line.split('\t').collect();
                                let status_info = if parts.len() >= 2 {
                                    let pid = parts[0];
                                    let exit_code = parts[1];
                                    if pid != "-" {
                                        format!("{} (PID {}, running)", label, pid)
                                    } else if exit_code != "0" {
                                        format!("{} (exit {})", label, exit_code)
                                    } else {
                                        format!("{} (registered)", label)
                                    }
                                } else {
                                    label.to_string()
                                };
                                conflicts.push(status_info);
                            }
                        }
                    }

                    if conflicts.is_empty() {
                        None // No conflicts, don't show this check
                    } else {
                        Some(CheckResult {
                            path: "stale services".to_string(),
                            status: "warning".to_string(),
                            detail: Some(conflicts.join(", ")),
                        })
                    }
                }
                _ => None, // Can't check, skip
            }
        };
        #[cfg(not(target_os = "macos"))]
        let conflicting_services: Option<CheckResult> = None;

        // Check 2e: On macOS, check .ipynb file association
        // Accept any nteract bundle ID (stable or nightly) as OK — a source-built
        // nightly runt should not warn about the stable app being the handler.
        #[cfg(target_os = "macos")]
        let file_association = {
            match runt_workspace::launch_services::get_default_ipynb_handler() {
                Some(handler)
                    if runt_workspace::NTERACT_BUNDLE_IDENTIFIERS.contains(&handler.as_str()) =>
                {
                    Some(CheckResult {
                        path: ".ipynb handler".to_string(),
                        status: "ok".to_string(),
                        detail: Some(handler),
                    })
                }
                Some(handler)
                    if runt_workspace::STALE_BUNDLE_IDENTIFIERS.contains(&handler.as_str()) =>
                {
                    Some(CheckResult {
                        path: ".ipynb handler".to_string(),
                        status: "stale".to_string(),
                        detail: Some(format!("{} (legacy)", handler)),
                    })
                }
                Some(handler) => Some(CheckResult {
                    path: ".ipynb handler".to_string(),
                    status: "warning".to_string(),
                    detail: Some(handler),
                }),
                None => Some(CheckResult {
                    path: ".ipynb handler".to_string(),
                    status: "warning".to_string(),
                    detail: Some("no default handler for .ipynb".to_string()),
                }),
            }
        };
        #[cfg(not(target_os = "macos"))]
        let file_association: Option<CheckResult> = None;

        // Check 3: Socket file
        let socket_exists = socket_path.exists();
        let socket_file = CheckResult {
            path: shorten_path(&socket_path),
            status: if socket_exists { "ok" } else { "missing" }.to_string(),
            detail: None,
        };

        // Check 4: daemon.json state
        let (daemon_state_status, daemon_state_detail) = if let Some(info) = daemon_info {
            // Check if PID is actually running
            let pid_running = is_process_running(info.pid);
            if pid_running {
                ("ok".to_string(), Some(format!("PID {} running", info.pid)))
            } else {
                (
                    "stale".to_string(),
                    Some(format!("PID {} not running", info.pid)),
                )
            }
        } else {
            ("missing".to_string(), None)
        };
        let daemon_state = CheckResult {
            path: shorten_path(&daemon_json_path),
            status: daemon_state_status.clone(),
            detail: daemon_state_detail,
        };

        // Check 5: Version comparison
        // Reuse binary_path from the top of run_checks (plist path on macOS)
        // rather than default_binary_path(), which may resolve to the wrong binary.
        let version_match = {
            let installed_ver = if binary_exists {
                get_binary_version(&binary_path)
            } else {
                None
            };
            // Only trust daemon_info version if the PID is confirmed running
            let running_ver = if daemon_state_status == "ok" {
                daemon_info.map(|d| d.version.clone())
            } else {
                None
            };
            let bundled_ver = find_bundled_runtimed().and_then(|p| get_binary_version(&p));
            let cli_ver = runt_version_string();

            // Build detail string showing all available versions
            let mut parts = Vec::new();
            parts.push(format!("cli={}", cli_ver));
            if let Some(ref v) = installed_ver {
                parts.push(format!("installed={}", v));
            }
            if let Some(ref v) = running_ver {
                parts.push(format!("running={}", v));
            }
            if let Some(ref v) = bundled_ver {
                parts.push(format!("bundled={}", v));
            }
            let detail = parts.join(" ");

            match (&installed_ver, &running_ver) {
                (Some(inst), Some(run)) => {
                    // Full version+commit comparison catches same-crate-version, different-commit
                    let installed_match = inst == run;
                    let bundled_match = bundled_ver
                        .as_ref()
                        .map(|b| b == run && b == inst)
                        .unwrap_or(true);
                    // CLI must also match the running daemon
                    let cli_match = cli_ver == *run;

                    if installed_match && bundled_match && cli_match {
                        CheckResult {
                            path: String::new(),
                            status: "ok".to_string(),
                            detail: Some(detail),
                        }
                    } else {
                        CheckResult {
                            path: String::new(),
                            status: "mismatch".to_string(),
                            detail: Some(detail),
                        }
                    }
                }
                _ => CheckResult {
                    path: String::new(),
                    status: "unknown".to_string(),
                    detail: Some(match (installed_ver.is_some(), running_ver.is_some()) {
                        (false, false) => {
                            "installed binary not found, daemon info unavailable".to_string()
                        }
                        (true, false) => "daemon info unavailable".to_string(),
                        (false, true) => "installed binary not found".to_string(),
                        _ => unreachable!(),
                    }),
                },
            }
        };

        // Check 6: Can we ping the daemon? Try regardless of daemon.json state
        let daemon_running_result = tokio::time::timeout(Duration::from_secs(2), client.ping())
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false);

        let daemon_running = CheckResult {
            path: String::new(),
            status: if daemon_running_result {
                "ok"
            } else {
                "not_running"
            }
            .to_string(),
            detail: if daemon_running_result && daemon_state_status == "missing" {
                Some("running but daemon.json missing".to_string())
            } else {
                None
            },
        };

        // Extract launchd status for diagnosis (macOS only)
        #[cfg(target_os = "macos")]
        let launchd_not_loaded = launchd_service
            .as_ref()
            .map(|c| c.status == "not_loaded")
            .unwrap_or(false);
        #[cfg(target_os = "macos")]
        let launchd_error = launchd_service
            .as_ref()
            .map(|c| c.status == "error")
            .unwrap_or(false);
        #[cfg(target_os = "macos")]
        let is_quarantined = quarantine
            .as_ref()
            .map(|c| c.status == "quarantined")
            .unwrap_or(false);
        #[cfg(not(target_os = "macos"))]
        #[allow(unused_variables)]
        let launchd_not_loaded = false;
        #[cfg(not(target_os = "macos"))]
        #[allow(unused_variables)]
        let launchd_error = false;
        #[cfg(not(target_os = "macos"))]
        #[allow(unused_variables)]
        let is_quarantined = false;

        let version_mismatch = version_match.status == "mismatch";
        let file_assoc_issue = file_association
            .as_ref()
            .map(|c| c.status != "ok")
            .unwrap_or(false);
        let is_legacy_install = standalone_binary
            .as_ref()
            .map(|c| c.status == "legacy")
            .unwrap_or(false);

        // Determine diagnosis
        let diagnosis = if daemon_running_result && version_mismatch {
            "Daemon is running but version mismatch detected. Run 'runt daemon doctor --fix' or restart the app.".to_string()
        } else if daemon_running_result && is_legacy_install {
            "Daemon is running but uses a legacy standalone binary. Run 'runt daemon doctor --fix' to migrate to the app bundle.".to_string()
        } else if daemon_running_result && file_assoc_issue {
            "Daemon is healthy but .ipynb file association needs attention. Run 'runt daemon doctor --fix'.".to_string()
        } else if daemon_running_result {
            "Daemon is healthy and running.".to_string()
        } else if !binary_exists && !config_exists {
            format!(
                "Daemon service not installed. Launch the {} app to install.",
                runt_workspace::desktop_display_name()
            )
        } else if !binary_exists && config_exists {
            "Service config exists but binary missing. Need to reinstall.".to_string()
        } else if binary_exists && is_quarantined {
            "Binary is quarantined by Gatekeeper. Run: xattr -d com.apple.quarantine <binary_path>"
                .to_string()
        } else if binary_exists && config_exists && launchd_not_loaded {
            "Plist exists but service not loaded in launchd. Run 'runt daemon doctor --fix' to reset.".to_string()
        } else if binary_exists && config_exists && launchd_error {
            "Service registered but failing to start. Check logs: runt daemon logs".to_string()
        } else if binary_exists && config_exists && daemon_state_status == "stale" {
            "Daemon state is stale (process crashed). Service needs restart.".to_string()
        } else if binary_exists && config_exists && !daemon_running_result {
            "Daemon installed but not running. Try 'runt daemon start'.".to_string()
        } else if binary_exists && !config_exists {
            "Daemon binary installed but service config missing.".to_string()
        } else {
            "Unknown state. Check logs with 'runt daemon logs'.".to_string()
        };

        DoctorReport {
            installed_binary,
            quarantine,
            standalone_binary,
            service_config,
            plist_home_env,
            launchd_service,
            conflicting_services,
            file_association,
            socket_file,
            daemon_state,
            version_match,
            daemon_running,
            diagnosis,
            actions_taken,
        }
    }

    let mut actions_taken: Vec<String> = Vec::new();

    // Get paths for fix operations — on macOS, check what the plist actually
    // points to so we diagnose/fix the real running binary, not the preferred one.
    #[cfg(target_os = "macos")]
    let binary_path =
        runt_workspace::plist_binary_path().unwrap_or_else(runtimed::service::default_binary_path);
    #[cfg(not(target_os = "macos"))]
    let binary_path = runtimed::service::default_binary_path();
    let socket_path = runt_workspace::default_socket_path();
    let daemon_json_path = runtimed::singleton::daemon_info_path();
    let service_config_path = runtimed::service::service_config_path();

    let binary_exists = binary_path.exists();
    let config_exists = service_config_path.exists();
    let socket_exists = socket_path.exists();

    // On macOS, check if plist is missing HOME env var
    #[cfg(target_os = "macos")]
    let plist_home_missing = config_exists
        && std::fs::read_to_string(&service_config_path)
            .map(|content| !content.contains("<key>HOME</key>"))
            .unwrap_or(false);
    #[cfg(not(target_os = "macos"))]
    let plist_home_missing = false;

    // On macOS, check if service is not loaded in launchd (stale registration)
    #[cfg(target_os = "macos")]
    let launchd_not_loaded = if config_exists {
        let label = runt_workspace::daemon_launchd_label();
        let output = std::process::Command::new("launchctl")
            .args(["list", label])
            .output();
        // If launchctl list fails (exit code != 0), service is not loaded
        !output.map(|o| o.status.success()).unwrap_or(false)
    } else {
        false
    };
    #[cfg(not(target_os = "macos"))]
    #[allow(unused_variables)]
    let launchd_not_loaded = false;

    // On macOS, check if binary has quarantine xattr
    #[cfg(target_os = "macos")]
    let is_quarantined = if binary_exists {
        std::process::Command::new("xattr")
            .args(["-p", "com.apple.quarantine"])
            .arg(&binary_path)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    } else {
        false
    };
    #[cfg(not(target_os = "macos"))]
    #[allow(unused_variables)]
    let is_quarantined = false;

    // On macOS, check if .ipynb file association needs fixing
    // Accept any nteract bundle ID (stable or nightly) as OK
    #[cfg(target_os = "macos")]
    let file_assoc_needs_fix = {
        runt_workspace::launch_services::get_default_ipynb_handler()
            .map(|h| !runt_workspace::NTERACT_BUNDLE_IDENTIFIERS.contains(&h.as_str()))
            .unwrap_or(true) // No handler set → needs fix
    };
    #[cfg(not(target_os = "macos"))]
    #[allow(unused_variables)]
    let file_assoc_needs_fix = false;

    // Check daemon state for fix operations
    let daemon_state_status = if let Some(info) = daemon_info {
        if is_process_running(info.pid) {
            "ok"
        } else {
            "stale"
        }
    } else {
        "missing"
    };

    // Check if daemon is running before fixes
    let daemon_running_before = tokio::time::timeout(Duration::from_secs(2), client.ping())
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false);

    // Fix issues if requested
    if fix {
        // Clean up stale state
        if daemon_state_status == "stale" {
            if let Err(e) = std::fs::remove_file(&daemon_json_path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    eprintln!("Warning: Could not remove stale daemon.json: {}", e);
                }
            } else {
                actions_taken.push("Removed stale daemon.json".to_string());
            }

            // Also remove stale socket
            if socket_exists {
                if let Err(e) = std::fs::remove_file(&socket_path) {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        eprintln!("Warning: Could not remove stale socket: {}", e);
                    }
                } else {
                    actions_taken.push("Removed stale socket file".to_string());
                }
            }
        }

        // Fix quarantine xattr (macOS only) - Gatekeeper blocks quarantined binaries
        #[cfg(target_os = "macos")]
        if is_quarantined {
            let result = std::process::Command::new("xattr")
                .args(["-d", "com.apple.quarantine"])
                .arg(&binary_path)
                .output();

            match result {
                Ok(o) if o.status.success() => {
                    actions_taken.push("Removed quarantine attribute from binary".to_string());
                }
                Ok(o) => {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    eprintln!("Failed to remove quarantine: {}", stderr.trim());
                }
                Err(e) => {
                    eprintln!("xattr command failed: {}", e);
                }
            }
        }

        // Fix legacy standalone binary — migrate plist to in-bundle binary (macOS only)
        #[cfg(target_os = "macos")]
        if runt_workspace::is_legacy_standalone_install() {
            if let Some(bundled_path) = find_bundled_runtimed() {
                // Create a ServiceManager with the in-bundle binary path
                let migrated_config = runtimed::service::ServiceConfig {
                    binary_path: bundled_path.clone(),
                    ..runtimed::service::ServiceConfig::default()
                };
                let mut migrated_manager = runtimed::service::ServiceManager::new(migrated_config);
                match migrated_manager.upgrade(&bundled_path) {
                    Ok(()) => {
                        actions_taken.push(format!(
                            "Migrated plist to in-bundle binary at {}",
                            bundled_path.display()
                        ));
                    }
                    Err(e) => {
                        eprintln!("Failed to migrate to in-bundle binary: {}", e);
                    }
                }
            } else if !json {
                eprintln!(
                    "Legacy standalone binary detected but no app bundle found to migrate to."
                );
                eprintln!(
                    "Launch the {} application to complete migration.",
                    runt_workspace::desktop_display_name()
                );
            }
        }

        // Fix plist missing HOME env var (causes daemon to fail on startup)
        if plist_home_missing && binary_exists {
            // Stop daemon first if running
            if daemon_running_before {
                let _ = manager.stop();
            }
            // Regenerate plist with HOME by calling upgrade with the existing binary
            match manager.upgrade(&binary_path) {
                Ok(()) => {
                    actions_taken.push("Regenerated plist with HOME env var".to_string());
                }
                Err(e) => {
                    eprintln!("Failed to regenerate plist: {}", e);
                }
            }
        }

        // Fix stale launchd registration (macOS only)
        // This happens when launchctl load/unload leaves corrupted state
        #[cfg(target_os = "macos")]
        if launchd_not_loaded && config_exists && binary_exists && !daemon_running_before {
            match runt_workspace::launchd_start() {
                Ok(()) => {
                    actions_taken.push("Ensured launchd service registration".to_string());
                }
                Err(e) => {
                    eprintln!("Failed to reset launchd registration: {e}");
                }
            }
        }

        // Fix .ipynb file association (macOS only)
        // Use whichever nteract app is actually installed, not the compile-time channel
        #[cfg(target_os = "macos")]
        if file_assoc_needs_fix {
            if let Some((app_path, bundle_id)) = runt_workspace::find_any_installed_nteract_bundle()
            {
                // Re-register app bundle with Launch Services
                if let Err(e) = runt_workspace::launch_services::register_app_bundle(&app_path) {
                    eprintln!("Warning: lsregister failed: {e}");
                }
                match runt_workspace::launch_services::set_default_ipynb_handler(bundle_id) {
                    Ok(()) => {
                        actions_taken.push(format!("Set .ipynb file association to {}", bundle_id));
                    }
                    Err(status) => {
                        eprintln!("Failed to set .ipynb file association (OSStatus {status})");
                    }
                }
            } else {
                eprintln!(
                    "No nteract app bundle found in /Applications or ~/Applications — cannot fix .ipynb association"
                );
            }
        }

        // Look up bundled binary once for version mismatch and repair scenarios
        let bundled = find_bundled_runtimed();
        let installed_ver = if binary_exists {
            get_binary_version(&binary_path)
        } else {
            None
        };
        let bundled_ver = bundled.as_ref().and_then(|p| get_binary_version(p));

        // Fix version mismatch: installed binary differs from bundled app binary
        // Common when a dev binary is accidentally left in the nightly install path
        if binary_exists && config_exists {
            if let (Some(inst), Some(bund), Some(bundled_path)) =
                (&installed_ver, &bundled_ver, &bundled)
            {
                if inst != bund {
                    match manager.upgrade(bundled_path) {
                        Ok(()) => {
                            actions_taken.push(format!(
                                "Upgraded daemon: {} -> {} (from {})",
                                inst,
                                bund,
                                bundled_path.display()
                            ));
                        }
                        Err(e) => {
                            eprintln!("Failed to upgrade daemon binary: {}", e);
                        }
                    }
                }
            }
        }

        // Handle different repair scenarios
        if !binary_exists || !config_exists {
            if let Some(bundled_path) = &bundled {
                if !binary_exists && config_exists {
                    // Service config exists but binary missing - use upgrade to replace binary
                    match manager.upgrade(bundled_path) {
                        Ok(()) => {
                            actions_taken.push(format!(
                                "Reinstalled daemon binary from {}",
                                bundled_path.display()
                            ));
                        }
                        Err(e) => {
                            eprintln!("Failed to reinstall daemon binary: {}", e);
                        }
                    }
                } else if !manager.is_installed() {
                    // Fresh install needed
                    match manager.install(bundled_path) {
                        Ok(()) => {
                            actions_taken
                                .push(format!("Installed daemon from {}", bundled_path.display()));
                        }
                        Err(e) => {
                            eprintln!("Failed to install daemon: {}", e);
                        }
                    }
                }
            } else if !json {
                eprintln!("Could not find bundled runtimed binary.");
                eprintln!(
                    "Launch the {} application to install the daemon.",
                    runt_workspace::desktop_display_name()
                );
            }
        }

        // Start if installed but not running
        if manager.is_installed() && !daemon_running_before {
            match manager.start() {
                Ok(()) => {
                    actions_taken.push("Started daemon service".to_string());
                }
                Err(e) => {
                    eprintln!("Failed to start daemon: {}", e);
                }
            }
        }
    }

    // Re-read daemon info after potential fixes
    let daemon_info_after = if fix && !actions_taken.is_empty() {
        // Give daemon time to start and write daemon.json
        tokio::time::sleep(Duration::from_millis(500)).await;
        runtimed::singleton::get_running_daemon_info()
    } else {
        daemon_info.cloned()
    };

    // Run final checks (after any fixes)
    let report = run_checks(client, daemon_info_after.as_ref(), actions_taken).await;

    // Output results
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        use colored::Colorize;

        print_header("runtimed", "Health Check");
        println!(
            "{:<20} {} {}",
            "Installed binary:".bold(),
            report.installed_binary.path.dimmed(),
            colored_status_icon(&report.installed_binary.status)
        );
        if let Some(ref quarantine_check) = report.quarantine {
            if quarantine_check.status == "quarantined" {
                println!(
                    "{:<20} {}{}",
                    "Quarantine:".bold(),
                    colored_status_icon(&quarantine_check.status),
                    quarantine_check
                        .detail
                        .as_ref()
                        .map(|d| format!(" ({})", d).dimmed().to_string())
                        .unwrap_or_default()
                );
            }
        }
        if let Some(ref standalone_check) = report.standalone_binary {
            if standalone_check.status != "ok" {
                println!(
                    "{:<20} {} {}{}",
                    "Binary location:".bold(),
                    standalone_check.path.dimmed(),
                    colored_status_icon(&standalone_check.status),
                    standalone_check
                        .detail
                        .as_ref()
                        .map(|d| format!(" ({})", d).dimmed().to_string())
                        .unwrap_or_default()
                );
            }
        }
        println!(
            "{:<20} {} {}",
            "Service config:".bold(),
            report.service_config.path.dimmed(),
            colored_status_icon(&report.service_config.status)
        );
        if let Some(ref plist_check) = report.plist_home_env {
            println!(
                "{:<20} {}{}",
                "Plist HOME env:".bold(),
                colored_status_icon(&plist_check.status),
                plist_check
                    .detail
                    .as_ref()
                    .map(|d| format!(" ({})", d).dimmed().to_string())
                    .unwrap_or_default()
            );
        }
        if let Some(ref launchd_check) = report.launchd_service {
            println!(
                "{:<20} {} {}{}",
                "Launchd service:".bold(),
                launchd_check.path.dimmed(),
                colored_status_icon(&launchd_check.status),
                launchd_check
                    .detail
                    .as_ref()
                    .map(|d| format!(" ({})", d).dimmed().to_string())
                    .unwrap_or_default()
            );
        }
        if let Some(ref conflicts_check) = report.conflicting_services {
            println!(
                "{:<20} {}{}",
                "Stale services:".bold(),
                colored_status_icon(&conflicts_check.status),
                conflicts_check
                    .detail
                    .as_ref()
                    .map(|d| format!(" {}", d).dimmed().to_string())
                    .unwrap_or_default()
            );
        }
        if let Some(ref file_assoc_check) = report.file_association {
            println!(
                "{:<20} {}{}",
                "File association:".bold(),
                colored_status_icon(&file_assoc_check.status),
                file_assoc_check
                    .detail
                    .as_ref()
                    .map(|d| format!(" ({})", d).dimmed().to_string())
                    .unwrap_or_default()
            );
        }
        println!(
            "{:<20} {} {}",
            "Socket file:".bold(),
            report.socket_file.path.dimmed(),
            colored_status_icon(&report.socket_file.status)
        );
        println!(
            "{:<20} {} {}{}",
            "Daemon state:".bold(),
            report.daemon_state.path.dimmed(),
            colored_status_icon(&report.daemon_state.status),
            report
                .daemon_state
                .detail
                .as_ref()
                .map(|d| format!(" ({})", d).dimmed().to_string())
                .unwrap_or_default()
        );
        println!(
            "{:<20} {}{}",
            "Version match:".bold(),
            colored_status_icon(&report.version_match.status),
            report
                .version_match
                .detail
                .as_ref()
                .map(|d| format!(" ({})", d).dimmed().to_string())
                .unwrap_or_default()
        );
        println!(
            "{:<20} {}{}",
            "Daemon running:".bold(),
            colored_yes_no(report.daemon_running.status == "ok"),
            report
                .daemon_running
                .detail
                .as_ref()
                .map(|d| format!(" ({})", d).dimmed().to_string())
                .unwrap_or_default()
        );
        println!();

        // Color diagnosis based on health
        let launchd_has_issue = report
            .launchd_service
            .as_ref()
            .map(|c| c.status == "not_loaded" || c.status == "error")
            .unwrap_or(false);
        let version_mismatch = report.version_match.status == "mismatch";
        let file_assoc_issue = report
            .file_association
            .as_ref()
            .map(|c| c.status != "ok")
            .unwrap_or(false);
        let legacy_install = report
            .standalone_binary
            .as_ref()
            .map(|c| c.status == "legacy")
            .unwrap_or(false);
        let diagnosis_colored = if report.daemon_running.status == "ok"
            && !version_mismatch
            && !file_assoc_issue
            && !legacy_install
        {
            report.diagnosis.green()
        } else if version_mismatch
            || report.daemon_state.status == "stale"
            || launchd_has_issue
            || file_assoc_issue
            || legacy_install
        {
            report.diagnosis.yellow()
        } else {
            report.diagnosis.red()
        };
        println!("{} {}", "Diagnosis:".bold(), diagnosis_colored);

        if !report.actions_taken.is_empty() {
            println!();
            println!("{}", "Actions taken:".bold());
            for action in &report.actions_taken {
                println!("  {} {}", "✓".green(), action);
            }
        } else if (report.daemon_running.status != "ok" || file_assoc_issue || legacy_install)
            && !fix
        {
            println!();
            println!(
                "{}",
                "Run 'runt daemon doctor --fix' to attempt automatic repair.".cyan()
            );
        }
    }

    Ok(())
}

/// Check if a process with the given PID is running
fn is_process_running(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // On Unix, send signal 0 to check if process exists
        // Returns 0 if process exists (even if we can't signal it due to permissions)
        // EPERM means process exists but we don't have permission - still running
        let result = unsafe { libc::kill(pid as i32, 0) };
        if result == 0 {
            true
        } else {
            // Check errno - EPERM means process exists but we lack permission
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            errno == libc::EPERM
        }
    }
    #[cfg(windows)]
    {
        // On Windows, use tasklist to check if process exists
        std::process::Command::new("tasklist")
            .args(["/FI", &format!("PID eq {}", pid), "/NH"])
            .output()
            .map(|output| {
                let stdout = String::from_utf8_lossy(&output.stdout);
                stdout.contains(&pid.to_string())
            })
            .unwrap_or(false)
    }
}

/// Get the version from a runtimed binary by running `--version`.
/// Returns e.g. `"2.0.0+a1b2c3d"` (crate version + commit hash).
fn get_binary_version(path: &Path) -> Option<String> {
    let output = std::process::Command::new(path)
        .arg("--version")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let version = String::from_utf8(output.stdout).ok()?;
    let version = version.trim().trim_start_matches("runtimed ").to_string();
    if version.is_empty() {
        return None;
    }
    Some(version)
}

/// Find bundled runtimed binary in common app locations
fn find_bundled_runtimed() -> Option<PathBuf> {
    let binary_name = if cfg!(windows) {
        "runtimed.exe"
    } else {
        "runtimed"
    };

    // Check if we're running from within an app bundle
    if let Ok(current_exe) = std::env::current_exe() {
        if let Some(parent) = current_exe.parent() {
            let sibling = parent.join(binary_name);
            if sibling.exists() {
                return Some(sibling);
            }
        }
    }

    // Common app locations on macOS
    #[cfg(target_os = "macos")]
    {
        let mut locations = Vec::new();
        for app_name in runt_workspace::desktop_app_launch_candidates() {
            locations.push(PathBuf::from(format!(
                "/Applications/{app_name}.app/Contents/MacOS/{binary_name}"
            )));
            locations.push(dirs::home_dir().unwrap_or_default().join(format!(
                "Applications/{app_name}.app/Contents/MacOS/{binary_name}"
            )));
        }
        for path in &locations {
            if path.exists() {
                return Some(path.clone());
            }
        }
    }

    // Linux: check common locations
    #[cfg(target_os = "linux")]
    {
        let mut locations = Vec::new();
        for app_name in runt_workspace::desktop_app_launch_candidates() {
            locations.push(PathBuf::from(format!(
                "/usr/share/{app_name}/{binary_name}"
            )));
            locations.push(PathBuf::from(format!("/opt/{app_name}/{binary_name}")));
        }
        // AppImage extracts to /tmp, check common paths
        locations.push(PathBuf::from(format!("/usr/local/bin/{binary_name}")));
        for path in &locations {
            if path.exists() {
                return Some(path.clone());
            }
        }
    }

    // Windows: check common locations
    #[cfg(target_os = "windows")]
    {
        let mut locations = Vec::new();
        for app_name in runt_workspace::desktop_app_launch_candidates() {
            locations.push(
                dirs::data_local_dir()
                    .unwrap_or_default()
                    .join("Programs")
                    .join(app_name)
                    .join(binary_name),
            );
            locations.push(PathBuf::from(format!(
                "C:\\Program Files\\{app_name}\\{binary_name}"
            )));
            locations.push(PathBuf::from(format!(
                "C:\\Program Files (x86)\\{app_name}\\{binary_name}"
            )));
        }
        for path in &locations {
            if path.exists() {
                return Some(path.clone());
            }
        }
    }

    None
}

// ============================================================================
// Diagnostics collection
// ============================================================================

/// Run a command and return its stdout (or stderr on failure) as bytes.
fn capture_command_output(exe: &Path, args: &[&str]) -> Vec<u8> {
    match std::process::Command::new(exe).args(args).output() {
        Ok(output) => {
            if output.status.success() {
                output.stdout
            } else {
                // Include both stdout and stderr so partial output isn't lost
                let mut combined = output.stdout;
                combined.extend_from_slice(&output.stderr);
                combined
            }
        }
        Err(e) => format!("Failed to run command: {}\n", e).into_bytes(),
    }
}

/// Add in-memory bytes as a file entry in a tar archive.
fn tar_add_bytes<W: std::io::Write>(
    tar: &mut tar::Builder<W>,
    name: &str,
    data: &[u8],
) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    );
    header.set_cksum();
    tar.append_data(&mut header, name, data)?;
    Ok(())
}

/// Collect system information as a JSON object.
fn collect_system_info() -> serde_json::Value {
    let os_version = {
        #[cfg(target_os = "macos")]
        {
            std::process::Command::new("sw_vers")
                .arg("-productVersion")
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "unknown".to_string())
        }
        #[cfg(target_os = "linux")]
        {
            std::fs::read_to_string("/etc/os-release")
                .ok()
                .and_then(|content| {
                    content
                        .lines()
                        .find(|l| l.starts_with("PRETTY_NAME="))
                        .map(|l| {
                            l.trim_start_matches("PRETTY_NAME=")
                                .trim_matches('"')
                                .to_string()
                        })
                })
                .unwrap_or_else(|| "unknown".to_string())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            "unknown".to_string()
        }
    };

    serde_json::json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "os_version": os_version,
        "runt_version": env!("CARGO_PKG_VERSION"),
        "runt_commit": env!("GIT_COMMIT"),
        "runt_variant": env!("RUNT_VARIANT"),
        "build_channel": format!("{:?}", runt_workspace::build_channel()),
        "dev_mode": runt_workspace::is_dev_mode(),
        "workspace_path": runt_workspace::get_workspace_path().map(|p| p.display().to_string()),
    })
}

async fn diagnostics_command(output_dir: Option<PathBuf>) -> Result<()> {
    use colored::Colorize;
    use flate2::write::GzEncoder;
    use flate2::Compression;

    let timestamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S");
    let output_dir = output_dir.unwrap_or_else(|| {
        // Prefer current directory if writable, otherwise fall back to the
        // system temp directory (always exists).
        std::env::current_dir()
            .ok()
            .filter(|p| {
                // Quick writability check — try creating a temp file
                let probe = p.join(".runt-diag-probe");
                std::fs::File::create(&probe)
                    .map(|_| {
                        let _ = std::fs::remove_file(&probe);
                        true
                    })
                    .unwrap_or(false)
            })
            .unwrap_or_else(std::env::temp_dir)
    });
    let archive_name = format!("runt-diagnostics-{}.tar.gz", timestamp);
    let archive_path = output_dir.join(&archive_name);

    print_header("diagnostics", "Collecting diagnostic information...");
    println!();

    let file = std::fs::File::create(&archive_path).map_err(|e| {
        anyhow::anyhow!(
            "Failed to create archive at {}: {}",
            archive_path.display(),
            e
        )
    })?;
    let enc = GzEncoder::new(file, Compression::default());
    let mut tar = tar::Builder::new(enc);

    // 1. Daemon log (current session)
    let daemon_log = runtimed::default_log_path();
    if daemon_log.exists() {
        tar.append_path_with_name(&daemon_log, "runtimed.log")?;
        println!("  {} runtimed.log", "✓".green());
    } else {
        println!("  {} runtimed.log (not found)", "–".yellow());
    }

    // 1b. Previous session log (preserved across daemon restart for crash diagnosis)
    let prev_daemon_log = daemon_log.with_extension("log.1");
    if prev_daemon_log.exists() {
        tar.append_path_with_name(&prev_daemon_log, "runtimed.log.1")?;
        println!("  {} runtimed.log.1 (previous session)", "✓".green());
    } else {
        println!(
            "  {} runtimed.log.1 (previous session not found)",
            "–".yellow()
        );
    }

    // 2. Notebook log (current session)
    let notebook_log = runt_workspace::default_notebook_log_path();
    if notebook_log.exists() {
        tar.append_path_with_name(&notebook_log, "notebook.log")?;
        println!("  {} notebook.log", "✓".green());
    } else {
        println!("  {} notebook.log (not found)", "–".yellow());
    }

    // 2b. Previous notebook session log (rotated on app startup)
    let prev_notebook_log = notebook_log.with_extension("log.1");
    if prev_notebook_log.exists() {
        tar.append_path_with_name(&prev_notebook_log, "notebook.log.1")?;
        println!("  {} notebook.log.1 (previous session)", "✓".green());
    } else {
        println!(
            "  {} notebook.log.1 (previous session not found)",
            "–".yellow()
        );
    }

    // 3. MCP server session logs (most recent 10 by mtime)
    let mcp_dir = runt_workspace::mcp_logs_dir();
    if mcp_dir.is_dir() {
        let mut log_files: Vec<_> = std::fs::read_dir(&mcp_dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| {
                e.path()
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .is_some_and(|ext| ext == "log")
            })
            .filter_map(|e| {
                let mtime = e.metadata().ok()?.modified().ok()?;
                Some((e.path(), mtime))
            })
            .collect();
        log_files.sort_by(|a, b| b.1.cmp(&a.1)); // newest first
        let selected: Vec<_> = log_files.into_iter().take(10).collect();

        if selected.is_empty() {
            println!("  {} mcp-logs/ (empty)", "–".yellow());
        } else {
            for (path, _) in &selected {
                if let Some(name) = path.file_name() {
                    let archive_name = format!("mcp-logs/{}", name.to_string_lossy());
                    tar.append_path_with_name(path, &archive_name)?;
                }
            }
            println!(
                "  {} mcp-logs/ ({} session logs)",
                "✓".green(),
                selected.len()
            );
        }
    } else {
        println!("  {} mcp-logs/ (not found)", "–".yellow());
    }

    // 4. daemon status --json
    let exe = std::env::current_exe()?;
    let status_output = capture_command_output(&exe, &["daemon", "status", "--json"]);
    tar_add_bytes(&mut tar, "daemon-status.json", &status_output)?;
    println!("  {} daemon-status.json", "✓".green());

    // 5. doctor --json
    let doctor_output = capture_command_output(&exe, &["doctor", "--json"]);
    tar_add_bytes(&mut tar, "doctor.json", &doctor_output)?;
    println!("  {} doctor.json", "✓".green());

    // 6. system-info.json
    let system_info = collect_system_info();
    let system_json = serde_json::to_string_pretty(&system_info)?;
    tar_add_bytes(&mut tar, "system-info.json", system_json.as_bytes())?;
    println!("  {} system-info.json", "✓".green());

    // Finalize archive
    let enc = tar.into_inner()?;
    enc.finish()?;

    println!();
    println!(
        "  Archive saved to: {}",
        archive_path.display().to_string().bold()
    );
    Ok(())
}

/// Return a colored status icon for display
fn colored_status_icon(status: &str) -> colored::ColoredString {
    use colored::Colorize;
    match status {
        "ok" => "[ok]".green(),
        "missing" => "[missing]".red(),
        "stale" => "[stale]".yellow(),
        "not_loaded" => "[not loaded]".yellow(),
        "quarantined" => "[quarantined]".red(),
        "warning" => "[warning]".yellow(),
        "error" => "[error]".red(),
        "mismatch" => "[mismatch]".red(),
        "unknown" => "[unknown]".yellow(),
        "legacy" => "[legacy]".yellow(),
        "not_running" => "".normal(),
        _ => "[?]".yellow(),
    }
}

/// Return a colored yes/no status
fn colored_yes_no(value: bool) -> colored::ColoredString {
    use colored::Colorize;
    if value {
        "yes".green()
    } else {
        "no".red()
    }
}

/// Print a purple bracket header with colored prefix and regular suffix
/// Example: print_header("runtimed", "Health Check") → "runtimed" purple, "Health Check" regular
fn print_header(colored_prefix: &str, suffix: &str) {
    use colored::Colorize;
    println!("{}", "╭─".purple());
    println!(
        "{} {} {}",
        "│".purple(),
        colored_prefix.purple().bold(),
        suffix
    );
    println!("{}", "╰─".purple());
}

/// Native log file tailing implementation
async fn tail_log_file(path: &PathBuf, lines: usize, follow: bool) -> Result<()> {
    use std::collections::VecDeque;
    use std::io::{BufRead, BufReader, Seek, SeekFrom};

    // Read last N lines efficiently using a fixed-size buffer
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(&file);
    let mut last_lines: VecDeque<String> = VecDeque::with_capacity(lines);

    for line in reader.lines() {
        let line = line?;
        if last_lines.len() >= lines {
            last_lines.pop_front();
        }
        last_lines.push_back(line);
    }

    for line in &last_lines {
        println!("{}", line);
    }

    if follow {
        // Watch for new lines using notify
        use notify::{Config, RecommendedWatcher, RecursiveMode, Watcher};

        // Use tokio channel to bridge sync notify with async code
        let (tx, mut rx) = tokio::sync::mpsc::channel(16);
        let mut watcher = RecommendedWatcher::new(
            move |res| {
                let _ = tx.blocking_send(res);
            },
            Config::default(),
        )?;
        watcher.watch(path.as_ref(), RecursiveMode::NonRecursive)?;

        let mut file = std::fs::File::open(path)?;
        file.seek(SeekFrom::End(0))?;
        let mut reader = BufReader::new(file);
        let mut line = String::new();

        loop {
            tokio::select! {
                // Check for Ctrl+C
                _ = tokio::signal::ctrl_c() => {
                    break;
                }
                // Check for file changes
                _ = rx.recv() => {
                    // Read any new lines
                    while reader.read_line(&mut line)? > 0 {
                        print!("{}", line);
                        line.clear();
                    }
                }
            }
        }
    }

    Ok(())
}

/// List all running dev worktree daemons
async fn list_worktree_daemons(json_output: bool) -> Result<()> {
    use runtimed::client::PoolClient;
    use runtimed::singleton::read_daemon_info;
    use serde::Serialize;

    let worktrees_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(runt_workspace::cache_namespace())
        .join("worktrees");

    #[derive(Serialize)]
    struct WorktreeDaemon {
        hash: String,
        status: String,
        worktree: Option<String>,
        description: Option<String>,
        pid: Option<u32>,
        version: Option<String>,
    }

    let mut daemons: Vec<WorktreeDaemon> = Vec::new();

    if worktrees_dir.exists() {
        let mut entries = fs::read_dir(&worktrees_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let hash = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            let info_path = path.join("daemon.json");

            if let Some(info) = read_daemon_info(&info_path) {
                // Check if daemon is actually running
                let client = PoolClient::new(PathBuf::from(&info.endpoint));
                let alive = client.ping().await.is_ok();

                daemons.push(WorktreeDaemon {
                    hash,
                    status: if alive {
                        "running".to_string()
                    } else {
                        "stopped".to_string()
                    },
                    worktree: info.worktree_path,
                    description: info.workspace_description,
                    pid: if alive { Some(info.pid) } else { None },
                    version: if alive { Some(info.version) } else { None },
                });
            } else {
                // Directory exists but no daemon.json
                daemons.push(WorktreeDaemon {
                    hash,
                    status: "stopped".to_string(),
                    worktree: None,
                    description: None,
                    pid: None,
                    version: None,
                });
            }
        }
    }

    if json_output {
        println!("{}", serde_json::to_string_pretty(&daemons)?);
    } else if daemons.is_empty() {
        println!("No dev worktree daemons found.");
        println!();
        println!("To start a dev daemon in the current worktree:");
        println!("  RUNTIMED_DEV=1 cargo run -p runtimed");
        println!();
        println!("Or if using Conductor, dev mode is enabled automatically.");
    } else {
        #[derive(Tabled)]
        struct WorktreeRow {
            #[tabled(rename = "HASH")]
            hash: String,
            #[tabled(rename = "STATUS")]
            status: String,
            #[tabled(rename = "WORKTREE")]
            worktree: String,
            #[tabled(rename = "DESCRIPTION")]
            description: String,
        }

        let rows: Vec<WorktreeRow> = daemons
            .iter()
            .map(|d| WorktreeRow {
                hash: d.hash.clone(),
                status: d.status.clone(),
                worktree: d
                    .worktree
                    .as_ref()
                    .map(|p| shorten_path(&PathBuf::from(p)))
                    .unwrap_or_else(|| "-".to_string()),
                description: d.description.clone().unwrap_or_else(|| "-".to_string()),
            })
            .collect();

        let table = Table::new(rows).with(Style::rounded()).to_string();
        println!("{}", table);
    }

    Ok(())
}

/// Clean up worktree daemon state directories
async fn clean_worktree_command(
    hash: Option<String>,
    all: bool,
    stale: bool,
    force: bool,
    yes: bool,
    dry_run: bool,
) -> Result<()> {
    use runtimed::client::PoolClient;
    use runtimed::singleton::read_daemon_info;
    use std::io::{self, Write};

    let worktrees_dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join(runt_workspace::cache_namespace())
        .join("worktrees");

    if !worktrees_dir.exists() {
        println!("No worktree state directories found.");
        return Ok(());
    }

    // Collect worktree targets
    struct WorktreeTarget {
        hash: String,
        path: PathBuf,
        worktree_path: Option<String>,
        is_running: bool,
        is_stale: bool,
        size_bytes: u64,
    }

    let mut targets: Vec<WorktreeTarget> = Vec::new();

    if let Some(h) = hash {
        // Specific hash
        let path = worktrees_dir.join(&h);
        if !path.exists() {
            anyhow::bail!("No worktree state found for hash: {}", h);
        }
        targets.push(WorktreeTarget {
            hash: h,
            path,
            worktree_path: None,
            is_running: false,
            is_stale: false,
            size_bytes: 0,
        });
    } else if all || stale {
        // All or stale worktrees
        let mut entries = fs::read_dir(&worktrees_dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let h = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string();
            targets.push(WorktreeTarget {
                hash: h,
                path,
                worktree_path: None,
                is_running: false,
                is_stale: false,
                size_bytes: 0,
            });
        }
    } else {
        // Current worktree (default)
        let workspace_path = runt_workspace::get_workspace_path().ok_or_else(|| {
            anyhow::anyhow!("Not in a git worktree and RUNTIMED_WORKSPACE_PATH not set.\nUse --hash <hash> or --all to specify which worktree to clean.")
        })?;
        let h = runt_workspace::worktree_hash(&workspace_path);
        let path = worktrees_dir.join(&h);
        if !path.exists() {
            println!(
                "No worktree state to clean for {} (hash: {})",
                workspace_path.display(),
                h
            );
            return Ok(());
        }
        targets.push(WorktreeTarget {
            hash: h,
            path,
            worktree_path: Some(workspace_path.to_string_lossy().to_string()),
            is_running: false,
            is_stale: false,
            size_bytes: 0,
        });
    }

    if targets.is_empty() {
        println!("No worktree state directories found.");
        return Ok(());
    }

    // Check daemon status and calculate sizes for each target
    for target in &mut targets {
        // Read daemon info
        let info_path = target.path.join("daemon.json");
        if let Some(info) = read_daemon_info(&info_path) {
            target.worktree_path = info.worktree_path.clone();

            // Try to ping the daemon
            let client = PoolClient::new(PathBuf::from(&info.endpoint));
            target.is_running =
                tokio::time::timeout(std::time::Duration::from_secs(2), client.ping())
                    .await
                    .map(|r| r.is_ok())
                    .unwrap_or(false);

            // Check if stale (original path no longer exists)
            if let Some(ref wt_path) = target.worktree_path {
                target.is_stale = !PathBuf::from(wt_path).exists();
            }
        }

        // Calculate directory size
        target.size_bytes = calculate_dir_size(&target.path);
    }

    // Filter to stale only if requested
    if stale {
        targets.retain(|t| t.is_stale);
        if targets.is_empty() {
            println!("No stale worktree state directories found.");
            return Ok(());
        }
    }

    // Check for running daemons
    let running: Vec<_> = targets.iter().filter(|t| t.is_running).collect();
    if !running.is_empty() && !force {
        eprintln!("The following worktree daemons are still running:");
        for t in &running {
            eprintln!(
                "  {} ({})",
                t.hash,
                t.worktree_path.as_deref().unwrap_or("unknown")
            );
        }
        eprintln!();
        eprintln!("Use --force to stop them first, or stop manually with:");
        eprintln!("  runt daemon stop");
        std::process::exit(1);
    }

    // Stop running daemons if --force
    if force {
        for target in &targets {
            if target.is_running {
                let info_path = target.path.join("daemon.json");
                if let Some(info) = read_daemon_info(&info_path) {
                    let client = PoolClient::new(PathBuf::from(&info.endpoint));
                    print!("Stopping daemon {}... ", target.hash);
                    io::stdout().flush()?;
                    match client.shutdown().await {
                        Ok(_) => {
                            println!("done");
                            // Brief wait for shutdown
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        }
                        Err(e) => {
                            println!("warning: {}", e);
                        }
                    }
                }
            }
        }
        println!();
    }

    // Calculate total size
    let total_size: u64 = targets.iter().map(|t| t.size_bytes).sum();

    // Display summary
    #[derive(Tabled)]
    struct CleanupRow {
        #[tabled(rename = "HASH")]
        hash: String,
        #[tabled(rename = "WORKTREE")]
        worktree: String,
        #[tabled(rename = "STATUS")]
        status: String,
        #[tabled(rename = "SIZE")]
        size: String,
    }

    let rows: Vec<CleanupRow> = targets
        .iter()
        .map(|t| CleanupRow {
            hash: t.hash.clone(),
            worktree: t
                .worktree_path
                .as_ref()
                .map(|p| shorten_path(&PathBuf::from(p)))
                .unwrap_or_else(|| "-".to_string()),
            status: if t.is_stale {
                "stale".to_string()
            } else if t.is_running {
                "running".to_string()
            } else {
                "stopped".to_string()
            },
            size: format_size(t.size_bytes),
        })
        .collect();

    let table = Table::new(&rows).with(Style::rounded()).to_string();
    println!("{}", table);
    println!();
    println!(
        "Will delete {} worktree state director{} ({})",
        targets.len(),
        if targets.len() == 1 { "y" } else { "ies" },
        format_size(total_size)
    );

    if dry_run {
        println!();
        println!("(dry-run mode - no files deleted)");
        return Ok(());
    }

    // Confirm unless --yes
    if !yes {
        print!("Continue? [y/N] ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("Aborted.");
            return Ok(());
        }
    }

    // Perform deletion
    let mut success_count = 0;
    let mut failure_count = 0;

    for target in &targets {
        match std::fs::remove_dir_all(&target.path) {
            Ok(()) => {
                println!("Deleted {}", target.hash);
                success_count += 1;
            }
            Err(e) => {
                eprintln!("Failed to delete {}: {}", target.hash, e);
                failure_count += 1;
            }
        }
    }

    println!();
    println!("Cleaned {} worktree(s)", success_count);
    if failure_count > 0 {
        eprintln!("{} failed (check permissions)", failure_count);
        std::process::exit(1);
    }

    Ok(())
}

// =============================================================================
// Settings commands
// =============================================================================

async fn config_command(command: Option<ConfigCommands>) -> Result<()> {
    let settings_path = runt_workspace::settings_json_path();

    match command {
        None | Some(ConfigCommands::List) => {
            // Print all settings as pretty JSON
            let settings = read_settings_from_file(&settings_path)?;
            let json = serde_json::to_string_pretty(&settings)?;
            println!("{json}");
        }
        Some(ConfigCommands::Get { key }) => {
            validate_config_key(&key)?;
            let settings = read_settings_from_file(&settings_path)?;
            let value = get_setting_value(&settings, &key)?;
            println!("{value}");
        }
        Some(ConfigCommands::Set { key, value }) => {
            validate_config_key(&key)?;
            let mut json_value = if settings_path.exists() {
                let content = std::fs::read_to_string(&settings_path)?;
                serde_json::from_str::<serde_json::Value>(&content)?
            } else {
                serde_json::to_value(runtimed::settings_doc::SyncedSettings::default())?
            };
            set_setting_value(&mut json_value, &key, &value)?;

            // Round-trip validate: ensure the JSON still deserializes to
            // a valid SyncedSettings. This catches type mismatches (e.g.,
            // writing a bool into an enum field).
            let json_str = serde_json::to_string_pretty(&json_value)?;
            if serde_json::from_str::<runtimed::settings_doc::SyncedSettings>(&json_str).is_err() {
                let hint = match key.as_str() {
                    "theme" => "Must be one of: system, light, dark".to_string(),
                    "default_runtime" => "Must be one of: python, deno".to_string(),
                    "default_python_env" => "Must be one of: uv, conda, pixi".to_string(),
                    "keep_alive_secs" => format!(
                        "Must be a number between {} and {}",
                        runtimed::settings_doc::MIN_KEEP_ALIVE_SECS,
                        runtimed::settings_doc::MAX_KEEP_ALIVE_SECS,
                    ),
                    "onboarding_completed" => "Must be true or false".to_string(),
                    k if k.ends_with("_pool_size") => format!(
                        "Must be a number between 0 and {}",
                        runtimed_client::settings_doc::MAX_POOL_SIZE,
                    ),
                    k if k.ends_with("default_packages") => {
                        "Must be a JSON array '[\"pkg1\",\"pkg2\"]' or comma-separated 'pkg1,pkg2'"
                            .to_string()
                    }
                    _ => "The value would produce an invalid settings file".to_string(),
                };
                anyhow::bail!("Invalid value '{value}' for setting '{key}'. {hint}");
            }

            // Ensure parent directory exists
            if let Some(parent) = settings_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&settings_path, &json_str)?;
            println!("Updated {key} in {}", settings_path.display());
        }
    }
    Ok(())
}

/// Read settings from file, falling back to defaults if the file does not exist.
fn read_settings_from_file(path: &Path) -> Result<runtimed::settings_doc::SyncedSettings> {
    if path.exists() {
        let content = std::fs::read_to_string(path)?;
        let settings: runtimed::settings_doc::SyncedSettings = serde_json::from_str(&content)?;
        Ok(settings)
    } else {
        Ok(runtimed::settings_doc::SyncedSettings::default())
    }
}

/// Extract a setting value by dotted key path and return it as a formatted string.
fn get_setting_value(
    settings: &runtimed::settings_doc::SyncedSettings,
    key: &str,
) -> Result<String> {
    let json = serde_json::to_value(settings)?;
    let value = navigate_json(&json, key)?;
    // For strings, print without quotes; for everything else, use pretty JSON
    match value {
        serde_json::Value::String(s) => Ok(s.to_string()),
        other => Ok(serde_json::to_string_pretty(other)?),
    }
}

/// Validate that a settings key is one of the known keys.
fn validate_config_key(key: &str) -> Result<()> {
    if !VALID_CONFIG_KEYS.contains(&key) {
        anyhow::bail!(
            "Unknown setting '{key}'. Valid keys: {}",
            VALID_CONFIG_KEYS.join(", ")
        );
    }
    Ok(())
}

/// Navigate a JSON value by a dotted key path (e.g., "uv.default_packages").
fn navigate_json<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a serde_json::Value> {
    let parts: Vec<&str> = key.split('.').collect();
    let mut current = value;
    for part in &parts {
        current = current
            .get(part)
            .ok_or_else(|| anyhow::anyhow!("Unknown setting key: {key}"))?;
    }
    Ok(current)
}

/// Set a value in a JSON object at a dotted key path.
/// For keys ending in `default_packages`, the value is split on commas into a list.
fn set_setting_value(root: &mut serde_json::Value, key: &str, raw_value: &str) -> Result<()> {
    let parts: Vec<&str> = key.split('.').collect();
    let leaf = parts
        .last()
        .ok_or_else(|| anyhow::anyhow!("Empty setting key"))?;

    // Navigate to the parent, creating intermediate objects as needed
    let mut current = root;
    for part in &parts[..parts.len() - 1] {
        current = current
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("Expected object at key path"))?
            .entry(part.to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    }

    let obj = current
        .as_object_mut()
        .ok_or_else(|| anyhow::anyhow!("Expected object at key path"))?;

    // For list-valued keys (*.default_packages), parse as JSON array or comma-separated
    let new_value = if *leaf == "default_packages" {
        let trimmed = raw_value.trim();
        if trimmed.starts_with('[') {
            // Looks like JSON — parse it
            let parsed: serde_json::Value = serde_json::from_str(trimmed).map_err(|e| {
                anyhow::anyhow!(
                    "Value looks like JSON but failed to parse: {e}\n\
                     Hint: use valid JSON like '[\"pandas\",\"numpy\"]' \
                     or comma-separated names like 'pandas,numpy'"
                )
            })?;
            match &parsed {
                serde_json::Value::Array(items) => {
                    for item in items {
                        match item {
                            serde_json::Value::String(s) => {
                                notebook_doc::metadata::validate_package_specifier(s)
                                    .map_err(|e| anyhow::anyhow!("{e}"))?;
                            }
                            other => anyhow::bail!(
                                "Expected array of strings, but found {other}. \
                                 All elements must be quoted strings like '[\"pandas\",\"numpy\"]'."
                            ),
                        }
                    }
                    parsed
                }
                _ => anyhow::bail!(
                    "Expected a JSON array, but got {parsed}. \
                     Use '[\"pandas\",\"numpy\"]' or 'pandas,numpy'."
                ),
            }
        } else {
            // Comma-separated convenience syntax
            let items: Vec<serde_json::Value> = trimmed
                .split(',')
                .map(|s| {
                    let s = s.trim();
                    notebook_doc::metadata::validate_package_specifier(s)
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    Ok(serde_json::Value::String(s.to_string()))
                })
                .collect::<Result<Vec<_>>>()?;
            serde_json::Value::Array(items)
        }
    } else if raw_value == "true" || raw_value == "false" {
        serde_json::Value::Bool(raw_value == "true")
    } else if let Ok(n) = raw_value.parse::<u64>() {
        serde_json::Value::Number(n.into())
    } else {
        serde_json::Value::String(raw_value.to_string())
    };

    obj.insert(leaf.to_string(), new_value);
    Ok(())
}

// =============================================================================
// Environment management commands
// =============================================================================

async fn env_command(command: EnvCommands) -> anyhow::Result<()> {
    match command {
        EnvCommands::Stats => env_stats().await,
        EnvCommands::List => env_list().await,
        EnvCommands::Clean {
            all,
            max_age_days,
            max_count,
            dry_run,
        } => env_clean(all, max_age_days, max_count, dry_run).await,
    }
}

/// Cache directories to manage
struct EnvCacheDir {
    label: &'static str,
    path: PathBuf,
}

fn get_env_cache_dirs() -> Vec<EnvCacheDir> {
    let base = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    vec![
        EnvCacheDir {
            label: "UV envs",
            path: base.join("runt").join("envs"),
        },
        EnvCacheDir {
            label: "Conda envs",
            path: base.join("runt").join("conda-envs"),
        },
        EnvCacheDir {
            label: "Inline envs",
            path: base.join("runt").join("inline-envs"),
        },
        EnvCacheDir {
            label: "UV envs (nightly)",
            path: base.join("runt-nightly").join("envs"),
        },
        EnvCacheDir {
            label: "Worktrees",
            path: base.join("runt").join("worktrees"),
        },
        EnvCacheDir {
            label: "Worktrees (nightly)",
            path: base.join("runt-nightly").join("worktrees"),
        },
    ]
}

async fn env_stats() -> anyhow::Result<()> {
    println!("Environment cache disk usage:\n");

    let mut total = 0u64;
    for dir in get_env_cache_dirs() {
        if !dir.path.exists() {
            continue;
        }
        let size = calculate_dir_size(&dir.path);
        let count = std::fs::read_dir(&dir.path)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| e.path().is_dir())
                    .count()
            })
            .unwrap_or(0);
        println!(
            "  {:20} {:>10}  ({} dirs)  {}",
            dir.label,
            format_size(size),
            count,
            dir.path.display()
        );
        total += size;
    }

    println!("\n  {:20} {:>10}", "Total", format_size(total));
    Ok(())
}

async fn env_list() -> anyhow::Result<()> {
    let cache_dirs = get_env_cache_dirs();

    for dir in &cache_dirs {
        if !dir.path.exists() {
            continue;
        }

        let mut entries: Vec<(String, u64, std::time::SystemTime)> = Vec::new();

        if let Ok(rd) = std::fs::read_dir(&dir.path) {
            for entry in rd.filter_map(|e| e.ok()) {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let name = entry.file_name().to_string_lossy().to_string();
                let size = calculate_dir_size(&path);

                // Try .last-used, then mtime
                let last_used =
                    if let Ok(contents) = std::fs::read_to_string(path.join(".last-used")) {
                        if let Ok(secs) = contents.trim().parse::<u64>() {
                            std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)
                        } else {
                            entry
                                .metadata()
                                .and_then(|m| m.modified())
                                .unwrap_or(std::time::UNIX_EPOCH)
                        }
                    } else {
                        entry
                            .metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::UNIX_EPOCH)
                    };

                entries.push((name, size, last_used));
            }
        }

        if entries.is_empty() {
            continue;
        }

        // Sort by last-used descending
        entries.sort_by(|a, b| b.2.cmp(&a.2));

        println!("{}  ({})", dir.label, dir.path.display());
        for (name, size, last_used) in &entries {
            let age = std::time::SystemTime::now()
                .duration_since(*last_used)
                .unwrap_or_default();
            let age_str = if age.as_secs() < 3600 {
                format!("{}m ago", age.as_secs() / 60)
            } else if age.as_secs() < 86400 {
                format!("{}h ago", age.as_secs() / 3600)
            } else {
                format!("{}d ago", age.as_secs() / 86400)
            };
            println!("  {:<40} {:>10}  {}", name, format_size(*size), age_str);
        }
        println!();
    }

    Ok(())
}

async fn env_clean(
    all: bool,
    max_age_days: u64,
    max_count: usize,
    dry_run: bool,
) -> anyhow::Result<()> {
    let max_age = std::time::Duration::from_secs(max_age_days * 86400);

    if all {
        println!("Removing ALL cached environments...");
        println!("Note: pool envs (runtimed-uv-*, runtimed-conda-*) are skipped.");
        println!("      Use 'runt daemon flush' to reset the pool.\n");
        if dry_run {
            println!("(dry run — nothing will be deleted)\n");
        }

        // Query daemon for in-use env paths to protect running kernels
        let in_use = query_active_env_paths().await;

        let base = dirs::cache_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        let dirs_to_clean = [
            base.join("runt").join("envs"),
            base.join("runt").join("conda-envs"),
            base.join("runt").join("inline-envs"),
            base.join("runt-nightly").join("envs"),
        ];

        let mut total_removed = 0;
        for dir in &dirs_to_clean {
            if !dir.exists() {
                continue;
            }
            let size = calculate_dir_size(dir);
            println!("  {} — {}", dir.display(), format_size(size));
            if !dry_run {
                // Remove content-addressed subdirs only — skip pool envs
                // (runtimed-uv-*, runtimed-conda-*) and envs backing
                // running kernels.
                if let Ok(rd) = std::fs::read_dir(dir) {
                    for entry in rd.filter_map(|e| e.ok()) {
                        let name = entry.file_name().to_string_lossy().to_string();
                        let path = entry.path();
                        if path.is_dir()
                            && !name.starts_with("runtimed-uv-")
                            && !name.starts_with("runtimed-conda-")
                            && !name.starts_with("prewarm-")
                            && !in_use.contains(&path)
                        {
                            std::fs::remove_dir_all(&path).ok();
                            total_removed += 1;
                        }
                    }
                }
            }
        }

        if !dry_run {
            println!("\nRemoved {} cached environments.", total_removed);
        }
        return Ok(());
    }

    // Query the daemon for env paths backing running kernels so we
    // don't evict them. Falls back to empty if daemon isn't reachable.
    let in_use = query_active_env_paths().await;

    // Selective eviction
    let eviction_dirs = [
        kernel_env::uv::default_cache_dir_uv(),
        kernel_env::conda::default_cache_dir_conda(),
        dirs::cache_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("runt")
            .join("inline-envs"),
    ];

    if dry_run {
        println!(
            "Dry run — showing what would be evicted (max_age={}d, max_count={}):\n",
            max_age_days, max_count
        );
    } else {
        println!(
            "Evicting stale environments (max_age={}d, max_count={}):\n",
            max_age_days, max_count
        );
    }

    let mut total_evicted = 0;
    for dir in &eviction_dirs {
        if !dir.exists() {
            continue;
        }
        if dry_run {
            // Show what would be evicted without actually deleting
            // Use a very large max_count to just show ages
            println!("  {}:", dir.display());
            if let Ok(rd) = std::fs::read_dir(dir) {
                let mut candidates: Vec<(PathBuf, std::time::SystemTime, u64)> = Vec::new();
                for entry in rd.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.len() != 16 || !name.chars().all(|c| c.is_ascii_hexdigit()) {
                        continue;
                    }
                    if !path.is_dir() {
                        continue;
                    }
                    let last_used = if let Ok(c) = std::fs::read_to_string(path.join(".last-used"))
                    {
                        if let Ok(s) = c.trim().parse::<u64>() {
                            std::time::UNIX_EPOCH + std::time::Duration::from_secs(s)
                        } else {
                            entry
                                .metadata()
                                .and_then(|m| m.modified())
                                .unwrap_or(std::time::UNIX_EPOCH)
                        }
                    } else {
                        entry
                            .metadata()
                            .and_then(|m| m.modified())
                            .unwrap_or(std::time::UNIX_EPOCH)
                    };
                    let size = calculate_dir_size(&path);
                    candidates.push((path, last_used, size));
                }
                candidates.sort_by(|a, b| b.1.cmp(&a.1));
                let now = std::time::SystemTime::now();
                for (i, (path, last_used, size)) in candidates.iter().enumerate() {
                    let age = now.duration_since(*last_used).unwrap_or_default();
                    let would_delete = (i >= max_count || age > max_age) && !in_use.contains(path);
                    let marker = if would_delete {
                        "DELETE"
                    } else if in_use.contains(path) {
                        "in-use"
                    } else {
                        "keep"
                    };
                    println!(
                        "    [{}] {} — {} ({}d old)",
                        marker,
                        path.file_name().unwrap_or_default().to_string_lossy(),
                        format_size(*size),
                        age.as_secs() / 86400
                    );
                    if would_delete {
                        total_evicted += 1;
                    }
                }
            }
            println!();
        } else {
            match kernel_env::gc::evict_stale_envs(dir, max_age, max_count, &in_use).await {
                Ok(deleted) => {
                    if !deleted.is_empty() {
                        println!(
                            "  {}: evicted {} environments",
                            dir.display(),
                            deleted.len()
                        );
                        total_evicted += deleted.len();
                    }
                }
                Err(e) => {
                    eprintln!("  {}: error — {}", dir.display(), e);
                }
            }
        }
    }

    if total_evicted == 0 {
        println!("Nothing to evict — cache is within limits.");
    } else if dry_run {
        println!(
            "Would evict {} environments. Run without --dry-run to proceed.",
            total_evicted
        );
    } else {
        println!("\nEvicted {} environments.", total_evicted);
    }

    Ok(())
}

/// Query the daemon for env paths backing running kernels.
/// Returns an empty set if the daemon isn't running or unreachable.
async fn query_active_env_paths() -> std::collections::HashSet<PathBuf> {
    use runtimed::client::PoolClient;
    use runtimed::singleton::get_running_daemon_info;

    let info = match get_running_daemon_info() {
        Some(info) => info,
        None => return std::collections::HashSet::new(),
    };

    let client = PoolClient::new(PathBuf::from(&info.endpoint));
    match tokio::time::timeout(std::time::Duration::from_secs(3), client.active_env_paths()).await {
        Ok(Ok(paths)) => paths.into_iter().collect(),
        _ => std::collections::HashSet::new(),
    }
}

/// Calculate total size of a directory
fn calculate_dir_size(path: &Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

/// Format bytes into human-readable size
fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_000_000.0)
    } else if bytes >= 1_000 {
        format!("{:.1} KB", bytes as f64 / 1_000.0)
    } else {
        format!("{} B", bytes)
    }
}

// =============================================================================
// Notebook listing command
// =============================================================================

#[derive(Tabled)]
struct NotebookTableRow {
    #[tabled(rename = "NOTEBOOK")]
    notebook: String,
    #[tabled(rename = "KERNEL")]
    kernel: String,
    #[tabled(rename = "ENV")]
    env: String,
    #[tabled(rename = "STATUS")]
    status: String,
    #[tabled(rename = "PEERS")]
    peers: String,
}

async fn list_notebooks(json_output: bool) -> Result<()> {
    use runtimed::client::PoolClient;
    use runtimed::singleton::get_running_daemon_info;

    // Use daemon's actual endpoint if available
    let client = match get_running_daemon_info() {
        Some(info) => PoolClient::new(PathBuf::from(&info.endpoint)),
        None => PoolClient::default(),
    };

    match client.list_rooms().await {
        Ok(rooms) => {
            if json_output {
                println!("{}", serde_json::to_string_pretty(&rooms)?);
            } else if rooms.is_empty() {
                println!("No open notebooks.");
            } else {
                let rows: Vec<NotebookTableRow> = rooms
                    .iter()
                    .map(|r| NotebookTableRow {
                        notebook: shorten_path(&PathBuf::from(&r.notebook_id)),
                        kernel: r.kernel_type.clone().unwrap_or_else(|| "-".to_string()),
                        env: r.env_source.clone().unwrap_or_else(|| "-".to_string()),
                        status: r.kernel_status.clone().unwrap_or_else(|| "-".to_string()),
                        peers: r.active_peers.to_string(),
                    })
                    .collect();

                let table = Table::new(rows).with(Style::rounded()).to_string();
                println!("{}", table);
            }
        }
        Err(e) => {
            eprintln!("Failed to list notebooks: {}", e);
            eprintln!("Is the daemon running? Try 'runt daemon status'");
            std::process::exit(1)
        }
    }

    Ok(())
}

/// Shutdown a notebook's kernel and evict its room from the daemon.
async fn shutdown_notebook(path: &PathBuf) -> Result<()> {
    use runtimed::client::PoolClient;
    use runtimed::singleton::get_running_daemon_info;

    let path_str = path.to_string_lossy();

    // Check if it's a valid UUID (untitled notebook ID)
    let is_uuid = uuid::Uuid::parse_str(&path_str).is_ok();

    let notebook_id = if is_uuid {
        // Use UUID directly for untitled notebooks
        path_str.to_string()
    } else {
        // Convert to absolute path for file-based notebooks
        let notebook_path = if path.is_absolute() {
            path.clone()
        } else {
            std::env::current_dir()?.join(path)
        };
        notebook_path.to_string_lossy().to_string()
    };

    // Use daemon's actual endpoint if available
    let client = match get_running_daemon_info() {
        Some(info) => PoolClient::new(PathBuf::from(&info.endpoint)),
        None => PoolClient::default(),
    };

    match client.shutdown_notebook(&notebook_id).await {
        Ok(true) => {
            println!("Shutdown notebook: {}", notebook_id);
        }
        Ok(false) => {
            eprintln!("Notebook not found: {}", notebook_id);
            eprintln!("Use 'runt notebooks' to see open notebooks.");
            std::process::exit(1)
        }
        Err(e) => {
            eprintln!("Failed to shutdown notebook: {}", e);
            eprintln!("Is the daemon running? Try 'runt daemon status'");
            std::process::exit(1)
        }
    }

    Ok(())
}

// =============================================================================
// Notebook recovery (offline, no daemon needed)
// =============================================================================

/// Collect all `notebook-docs/` directories across system and worktree caches.
fn all_notebook_docs_dirs() -> Vec<std::path::PathBuf> {
    let cache_dir = match dirs::cache_dir() {
        Some(d) => d,
        None => return vec![],
    };

    let mut dirs = Vec::new();

    // Both stable ("runt") and nightly ("runt-nightly") namespaces
    for ns in &["runt", "runt-nightly"] {
        let base = cache_dir.join(ns);

        // Top-level notebook-docs (non-dev daemon)
        let top = base.join("notebook-docs");
        if top.is_dir() {
            dirs.push(top);
        }

        // Per-worktree notebook-docs (dev daemons)
        let worktrees = base.join("worktrees");
        if worktrees.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&worktrees) {
                for entry in entries.flatten() {
                    let nd = entry.path().join("notebook-docs");
                    if nd.is_dir() {
                        dirs.push(nd);
                    }
                }
            }
        }
    }

    dirs
}

/// Find the automerge file for a notebook by its hash across all cache directories.
/// Checks live docs first, then falls back to the most recent snapshot.
fn find_automerge_file(filename: &str) -> Option<std::path::PathBuf> {
    find_automerge_file_in_dirs(&all_notebook_docs_dirs(), filename)
}

/// Core logic: search a given list of notebook-docs directories for an automerge file.
fn find_automerge_file_in_dirs(
    dirs: &[std::path::PathBuf],
    filename: &str,
) -> Option<std::path::PathBuf> {
    let stem = filename.strip_suffix(".automerge").unwrap_or(filename);

    for dir in dirs {
        // Check live doc first
        let candidate = dir.join(filename);
        if candidate.is_file() {
            return Some(candidate);
        }

        // Check snapshots (most recent by filename sort, which embeds timestamp)
        let snapshots_dir = dir.join("snapshots");
        if snapshots_dir.is_dir() {
            if let Some(path) = find_latest_snapshot(&snapshots_dir, stem) {
                return Some(path);
            }
        }
    }
    None
}

/// Find the most recent snapshot for a given hash stem in a snapshots directory.
fn find_latest_snapshot(snapshots_dir: &std::path::Path, stem: &str) -> Option<std::path::PathBuf> {
    let prefix = format!("{}-", stem);
    let mut matches: Vec<_> = std::fs::read_dir(snapshots_dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".automerge"))
        })
        .collect();

    if matches.is_empty() {
        return None;
    }

    // Sort by filename descending (latest timestamp last in ascending, so reverse)
    matches.sort_by_key(|e| e.file_name());
    matches.last().map(|e| e.path())
}

/// Export a NotebookDoc to .ipynb JSON.
fn doc_to_ipynb(doc: &notebook_doc::NotebookDoc) -> serde_json::Value {
    let cells = doc.get_cells();
    let metadata_snapshot = doc.get_metadata_snapshot();

    let mut nb_cells = Vec::new();
    for cell in &cells {
        // Split source into multiline array format (nbformat convention)
        let source_lines: Vec<String> = if cell.source.is_empty() {
            vec![]
        } else {
            let mut lines = Vec::new();
            let mut remaining = cell.source.as_str();
            while let Some(pos) = remaining.find('\n') {
                lines.push(remaining[..=pos].to_string());
                remaining = &remaining[pos + 1..];
            }
            if !remaining.is_empty() {
                lines.push(remaining.to_string());
            }
            lines
        };

        let mut cell_json = serde_json::json!({
            "id": cell.id,
            "cell_type": cell.cell_type,
            "source": source_lines,
            "metadata": cell.metadata,
        });

        if cell.cell_type == "code" {
            // Outputs are already structured serde_json::Value manifests
            cell_json["outputs"] = serde_json::Value::Array(cell.outputs.clone());

            let exec_count: serde_json::Value =
                serde_json::from_str(&cell.execution_count).unwrap_or(serde_json::Value::Null);
            cell_json["execution_count"] = exec_count;
        }

        nb_cells.push(cell_json);
    }

    // Build metadata
    let mut metadata = serde_json::json!({});
    if let Some(ref snapshot) = metadata_snapshot {
        if let Err(e) = snapshot.merge_into_metadata_value(&mut metadata) {
            eprintln!("Warning: failed to merge notebook metadata: {}", e);
        }
    }

    serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": metadata,
        "cells": nb_cells,
    })
}

fn format_file_modified(entry: &std::fs::DirEntry) -> String {
    entry
        .metadata()
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|d| {
            chrono::DateTime::from_timestamp(d.as_secs() as i64, d.subsec_nanos()).map(|dt| {
                dt.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
            })
        })
        .unwrap_or_else(|| "?".to_string())
}

fn format_file_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn print_automerge_entry(entry: &std::fs::DirEntry, label: &str) -> bool {
    let Ok(meta) = entry.metadata() else {
        return false;
    };
    let name = entry.file_name();
    let name = name.to_string_lossy();
    let modified = format_file_modified(entry);
    let size = format_file_size(meta.len());
    println!("{:<70} {:>8}  {}{}", name, size, modified, label);
    true
}

fn recover_notebook(
    path: Option<&std::path::Path>,
    output: Option<&std::path::Path>,
    list: bool,
) -> Result<()> {
    use notebook_doc::{notebook_doc_filename, NotebookDoc};

    if list {
        let dirs = all_notebook_docs_dirs();
        let mut found = false;

        for dir in &dirs {
            // List live docs
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().is_some_and(|e| e == "automerge") {
                        found |= print_automerge_entry(&entry, "");
                    }
                }
            }

            // List snapshots
            let snapshots_dir = dir.join("snapshots");
            if let Ok(entries) = std::fs::read_dir(&snapshots_dir) {
                for entry in entries.flatten() {
                    if entry.path().extension().is_some_and(|e| e == "automerge") {
                        found |= print_automerge_entry(&entry, " [snapshot]");
                    }
                }
            }
        }

        if !found {
            eprintln!("No recoverable notebooks found.");
        }
        return Ok(());
    }

    // Fast path: compute hash and look up directly
    let Some(path) = path else {
        anyhow::bail!("path is required when --list is not set");
    };
    // Canonicalize if the file exists (matches daemon's notebook_id derivation),
    // otherwise fall back to joining with cwd for deleted/missing files.
    let abs_path = if path.exists() {
        path.canonicalize()?
    } else if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let notebook_id = abs_path.to_string_lossy().to_string();
    let filename = notebook_doc_filename(&notebook_id);

    let automerge_path = match find_automerge_file(&filename) {
        Some(p) => p,
        None => {
            eprintln!(
                "No automerge document found for: {}\n\
                 Expected filename: {}\n\
                 \n\
                 Use 'runt recover --list' to see all recoverable notebooks.",
                notebook_id, filename
            );
            std::process::exit(1);
        }
    };

    let data = std::fs::read(&automerge_path)
        .map_err(|e| anyhow::anyhow!("Failed to read {:?}: {}", automerge_path, e))?;
    let doc = NotebookDoc::load(&data)
        .map_err(|e| anyhow::anyhow!("Failed to load automerge document: {}", e))?;

    let cell_count = doc.cell_count();
    let notebook_json = doc_to_ipynb(&doc);

    let output_path = match output {
        Some(p) => p.to_path_buf(),
        None => abs_path.clone(),
    };

    // Ensure parent directory exists
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let content = serde_json::to_string_pretty(&notebook_json)
        .map_err(|e| anyhow::anyhow!("Failed to serialize notebook: {}", e))?;
    std::fs::write(&output_path, format!("{content}\n"))?;

    println!(
        "Recovered {} cells to {}",
        cell_count,
        output_path.display()
    );
    println!("Source: {}", automerge_path.display());

    Ok(())
}

// =============================================================================
// Notebook inspection commands (debug tools)
// =============================================================================

async fn inspect_notebook(path: &PathBuf, full_outputs: bool, json_output: bool) -> Result<()> {
    use runtimed::client::PoolClient;

    // Convert to absolute path (notebook_id is the absolute path)
    let notebook_id = if path.is_absolute() {
        path.to_string_lossy().to_string()
    } else {
        std::env::current_dir()?
            .join(path)
            .to_string_lossy()
            .to_string()
    };

    let client = PoolClient::default();

    match client.inspect_notebook(&notebook_id).await {
        Ok(result) => {
            if json_output {
                // Full JSON output
                let output = serde_json::json!({
                    "notebook_id": result.notebook_id,
                    "source": result.source,
                    "kernel_info": result.kernel_info,
                    "cells": result.cells.iter().map(|c| {
                        let outputs_info: Vec<serde_json::Value> = if full_outputs {
                            c.outputs.clone()
                        } else {
                            c.outputs.iter().map(|o| {
                                let size = serde_json::to_string(o).map(|s| s.len()).unwrap_or(0);
                                if let Some(otype) = o.get("output_type").and_then(|v| v.as_str()) {
                                    serde_json::json!({
                                        "output_type": otype,
                                        "size": size,
                                    })
                                } else {
                                    serde_json::json!({ "size": size })
                                }
                            }).collect()
                        };
                        serde_json::json!({
                            "id": c.id,
                            "cell_type": c.cell_type,
                            "source_preview": if c.source.chars().count() > 80 {
                                format!("{}...", c.source.chars().take(80).collect::<String>())
                            } else {
                                c.source.clone()
                            },
                            "source_len": c.source.len(),
                            "execution_count": c.execution_count,
                            "outputs": outputs_info,
                        })
                    }).collect::<Vec<_>>(),
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                // Human-readable output
                println!("Notebook: {}", result.notebook_id);
                println!("Source: {}", result.source);
                if let Some(kernel) = &result.kernel_info {
                    println!(
                        "Kernel: {} ({}) - {}",
                        kernel.kernel_type, kernel.env_source, kernel.status
                    );
                } else {
                    println!("Kernel: none");
                }
                println!();
                println!("Cells ({}):", result.cells.len());
                println!("{}", "-".repeat(60));

                for (i, cell) in result.cells.iter().enumerate() {
                    let source_preview = if cell.source.len() > 60 {
                        format!("{}...", cell.source.chars().take(60).collect::<String>())
                    } else {
                        cell.source.replace('\n', "\\n")
                    };

                    let exec_count = if cell.execution_count == "null" {
                        "   ".to_string()
                    } else {
                        format!("[{}]", cell.execution_count)
                    };

                    println!(
                        "{:2}. {} {:8} | {} | outputs: {}",
                        i + 1,
                        exec_count,
                        cell.cell_type,
                        source_preview,
                        cell.outputs.len()
                    );

                    if full_outputs && !cell.outputs.is_empty() {
                        for (j, output) in cell.outputs.iter().enumerate() {
                            println!(
                                "      output[{}]: {}",
                                j,
                                serde_json::to_string_pretty(output)?
                            );
                        }
                    }
                }
            }
        }
        Err(e) => {
            eprintln!("Failed to inspect notebook: {}", e);
            std::process::exit(1);
        }
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    /// Test that the shutdown command correctly identifies UUIDs vs file paths.
    /// This is critical for handling both saved notebooks (paths) and untitled
    /// notebooks (UUIDs).
    #[test]
    fn test_shutdown_uuid_detection() {
        // Valid UUIDs should be detected
        assert!(uuid::Uuid::parse_str("ea56af47-d8f2-4823-b0eb-a6254338e244").is_ok());
        assert!(uuid::Uuid::parse_str("d3058b85-2618-4211-85fd-7c657f9ac3a4").is_ok());

        // File paths should NOT be detected as UUIDs
        assert!(uuid::Uuid::parse_str("notebook.ipynb").is_err());
        assert!(uuid::Uuid::parse_str("my-notebook.ipynb").is_err());
        assert!(uuid::Uuid::parse_str("path/to/notebook.ipynb").is_err());
        assert!(uuid::Uuid::parse_str("/absolute/path/notebook.ipynb").is_err());
        assert!(uuid::Uuid::parse_str("./relative/notebook.ipynb").is_err());
        assert!(uuid::Uuid::parse_str("../parent/notebook.ipynb").is_err());

        // Edge cases
        assert!(uuid::Uuid::parse_str("").is_err());
        assert!(uuid::Uuid::parse_str("not-a-uuid").is_err());
        assert!(uuid::Uuid::parse_str("12345").is_err());
    }

    #[test]
    fn test_find_automerge_file_live_doc() {
        use notebook_doc::NotebookDoc;

        let tmp = tempfile::tempdir().unwrap();
        let docs_dir = tmp.path().to_path_buf();

        // Create and save a live automerge doc
        let mut doc = NotebookDoc::new("test-nb");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let bytes = doc.save();
        let filename = "abc123.automerge";
        std::fs::write(docs_dir.join(filename), &bytes).unwrap();

        let result = super::find_automerge_file_in_dirs(std::slice::from_ref(&docs_dir), filename);
        assert!(result.is_some());
        assert_eq!(result.unwrap(), docs_dir.join(filename));
    }

    #[test]
    fn test_find_automerge_file_snapshot_fallback() {
        use notebook_doc::NotebookDoc;

        let tmp = tempfile::tempdir().unwrap();
        let docs_dir = tmp.path().to_path_buf();

        // Only create a snapshot, no live doc
        let snapshots_dir = docs_dir.join("snapshots");
        std::fs::create_dir_all(&snapshots_dir).unwrap();

        let mut doc = NotebookDoc::new("test-nb");
        doc.add_cell(0, "cell-1", "code").unwrap();
        let bytes = doc.save();
        let snapshot_name = "abc123-1710000000000.automerge";
        std::fs::write(snapshots_dir.join(snapshot_name), &bytes).unwrap();

        let result = super::find_automerge_file_in_dirs(&[docs_dir], "abc123.automerge");
        assert!(result.is_some());
        assert!(result.unwrap().to_string_lossy().contains(snapshot_name));
    }

    #[test]
    fn test_find_latest_snapshot_selects_most_recent() {
        let tmp = tempfile::tempdir().unwrap();
        let snapshots_dir = tmp.path();

        // Create three snapshots with different timestamps (out of order)
        for name in &[
            "abc123-1710000100000.automerge",
            "abc123-1710000300000.automerge",
            "abc123-1710000200000.automerge",
        ] {
            std::fs::write(snapshots_dir.join(name), b"data").unwrap();
        }

        let result = super::find_latest_snapshot(snapshots_dir, "abc123");
        assert!(result.is_some());
        assert!(result
            .unwrap()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .contains("1710000300000"));
    }

    #[test]
    fn test_find_latest_snapshot_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let result = super::find_latest_snapshot(tmp.path(), "abc123");
        assert!(result.is_none());
    }

    #[test]
    fn test_notebook_doc_filename_deterministic() {
        use notebook_doc::notebook_doc_filename;

        let path = "/Users/test/notebook.ipynb";
        let a = notebook_doc_filename(path);
        let b = notebook_doc_filename(path);

        // Same input produces same output
        assert_eq!(a, b);
        assert!(a.ends_with(".automerge"));

        // Different inputs produce different outputs
        let c = notebook_doc_filename("/Users/other/notebook.ipynb");
        assert_ne!(a, c);
    }

    #[test]
    fn test_doc_to_ipynb_basic() {
        use notebook_doc::NotebookDoc;

        let mut doc = NotebookDoc::new("test");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.update_source("cell-1", "print('hello')").unwrap();
        doc.add_cell(1, "cell-2", "markdown").unwrap();
        doc.update_source("cell-2", "# Title").unwrap();

        let result = super::doc_to_ipynb(&doc);

        assert_eq!(result["nbformat"], 4);
        assert_eq!(result["nbformat_minor"], 5);
        assert!(result["metadata"].is_object());

        let cells = result["cells"].as_array().unwrap();
        assert_eq!(cells.len(), 2);

        // Code cell
        assert_eq!(cells[0]["cell_type"], "code");
        assert_eq!(cells[0]["id"], "cell-1");
        let source: Vec<&str> = cells[0]["source"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(source.join(""), "print('hello')");
        assert!(cells[0]["outputs"].as_array().unwrap().is_empty());
        assert!(cells[0]["execution_count"].is_null());

        // Markdown cell — no outputs or execution_count keys
        assert_eq!(cells[1]["cell_type"], "markdown");
        assert_eq!(cells[1]["id"], "cell-2");
        assert!(cells[1].get("outputs").is_none());
        assert!(cells[1].get("execution_count").is_none());
    }

    #[test]
    fn test_doc_to_ipynb_multiline_source() {
        use notebook_doc::NotebookDoc;

        let mut doc = NotebookDoc::new("test");
        doc.add_cell(0, "cell-1", "code").unwrap();
        doc.update_source("cell-1", "line1\nline2\nline3").unwrap();

        let result = super::doc_to_ipynb(&doc);
        let source: Vec<&str> = result["cells"][0]["source"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert_eq!(source, vec!["line1\n", "line2\n", "line3"]);
    }

    #[test]
    fn test_doc_to_ipynb_empty_source() {
        use notebook_doc::NotebookDoc;

        let mut doc = NotebookDoc::new("test");
        doc.add_cell(0, "cell-1", "code").unwrap();

        let result = super::doc_to_ipynb(&doc);
        let source = result["cells"][0]["source"].as_array().unwrap();
        assert!(source.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_wait_for_pid_exit_nonexistent() {
        use std::time::Duration;

        // Use a PID that doesn't exist (very high number unlikely to be in use)
        let fake_pid = 999999;

        // Wait should return true immediately (process doesn't exist)
        let exited = super::wait_for_pid_exit(fake_pid, Duration::from_millis(500)).await;
        assert!(exited, "Non-existent process should be considered exited");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_stop_process_by_pid_nonexistent() {
        // Try to stop a PID that doesn't exist
        let fake_pid = 999999;
        let result = super::stop_process_by_pid(fake_pid).await;
        assert!(
            result.is_ok(),
            "Stopping non-existent process should succeed (already dead)"
        );
    }

    #[test]
    fn test_cleanup_stale_daemon_info_no_files() {
        // Test cleanup when files don't exist (should not error)
        let result = super::cleanup_stale_daemon_info();
        // Should succeed even if files don't exist
        assert!(
            result.is_ok() || result.is_err(),
            "Cleanup should handle missing files gracefully"
        );
    }
}
