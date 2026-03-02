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
use std::path::PathBuf;
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

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Open the notebook application
    Notebook {
        /// Path to notebook file or directory to open
        path: Option<PathBuf>,
        /// Runtime for new notebooks (python, deno)
        #[arg(long, short)]
        runtime: Option<String>,
    },
    /// Jupyter kernel utilities
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
    #[command(alias = "ps")]
    Notebooks {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
    },
    /// Shutdown a notebook's kernel and evict its room
    Shutdown {
        /// Path to the notebook file, or notebook ID (UUID) for untitled notebooks
        path: PathBuf,
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
    /// Debug message passing between sidecar and kernel
    #[command(hide = true)]
    Debug {
        /// The kernel to launch (e.g., python3, julia)
        kernel: Option<String>,
        /// Custom command to launch the kernel (use {connection_file} as placeholder)
        #[arg(long)]
        cmd: Option<String>,
        /// Code to execute after kernel starts
        #[arg(long)]
        exec: Option<String>,
        /// Path to dump all messages (defaults to temp file)
        #[arg(long)]
        dump: Option<PathBuf>,
        /// Keep running after execution for manual interaction (Ctrl+C to exit)
        #[arg(long, short)]
        wait: bool,
    },

    // =========================================================================
    // Hidden aliases for backwards compatibility (deprecated)
    // =========================================================================
    /// [DEPRECATED] Use 'runt jupyter start' instead
    #[command(hide = true)]
    Start { name: String },
    /// [DEPRECATED] Use 'runt jupyter stop' instead
    #[command(hide = true)]
    Stop {
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
    /// [DEPRECATED] Use 'runt jupyter sidecar' instead
    #[command(hide = true)]
    Sidecar {
        file: PathBuf,
        #[arg(short, long)]
        quiet: bool,
        #[arg(long)]
        dump: Option<PathBuf>,
    },
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
    /// Launch the sidecar viewer for a kernel
    Sidecar {
        /// Path to a kernel connection file
        file: PathBuf,
        /// Suppress output
        #[arg(short, long)]
        quiet: bool,
        /// Dump all messages to a JSON file
        #[arg(long)]
        dump: Option<PathBuf>,
    },
}

/// Daemon management commands (replaces Pool + runtimed service commands)
#[derive(Subcommand)]
enum DaemonCommands {
    /// Show daemon status (service, pool, version, uptime)
    Status {
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
    /// List all running dev worktree daemons
    ListWorktrees {
        /// Output in JSON format
        #[arg(long)]
        json: bool,
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
        // Sidecar runs a tao event loop on the main thread (no tokio needed)
        Some(Commands::Jupyter {
            command: JupyterCommands::Sidecar { file, quiet, dump },
        }) => sidecar::launch(&file, quiet, dump.as_deref()),
        // Deprecated alias
        Some(Commands::Sidecar { file, quiet, dump }) => {
            eprintln!("Warning: 'runt sidecar' is deprecated. Use 'runt jupyter sidecar' instead.");
            sidecar::launch(&file, quiet, dump.as_deref())
        }
        // Notebook launches the desktop app (no tokio needed)
        Some(Commands::Notebook { path, runtime }) => open_notebook(path, runtime),
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

    #[cfg(target_os = "macos")]
    {
        let mut cmd = std::process::Command::new("open");
        cmd.arg("-a").arg("nteract");

        if abs_path.is_some() || runtime.is_some() {
            cmd.arg("--args");
        }
        if let Some(p) = abs_path {
            cmd.arg(p);
        }
        if let Some(r) = runtime {
            cmd.arg("--runtime").arg(r);
        }

        cmd.spawn()
            .map_err(|e| anyhow::anyhow!("Failed to launch nteract: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows, try common install locations or use shell execution
        let app_name = "nteract.exe";
        let mut cmd = std::process::Command::new(app_name);

        if let Some(p) = abs_path {
            cmd.arg(p);
        }
        if let Some(r) = runtime {
            cmd.arg("--runtime").arg(r);
        }

        cmd.spawn()
            .map_err(|e| anyhow::anyhow!("Failed to launch nteract: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        // On Linux, try to find the app in PATH or common locations
        let app_name = "nteract";
        let mut cmd = std::process::Command::new(app_name);

        if let Some(p) = abs_path {
            cmd.arg(p);
        }
        if let Some(r) = runtime {
            cmd.arg("--runtime").arg(r);
        }

        cmd.spawn()
            .map_err(|e| anyhow::anyhow!("Failed to launch nteract: {}", e))?;
    }

    Ok(())
}

async fn async_main(command: Option<Commands>) -> Result<()> {
    match command {
        // Primary commands
        Some(Commands::Notebook { .. }) => unreachable!(), // handled in main()
        Some(Commands::Jupyter { command }) => jupyter_command(command).await?,
        Some(Commands::Daemon { command }) => daemon_command(command).await?,
        Some(Commands::Notebooks { json }) => list_notebooks(json).await?,
        Some(Commands::Shutdown { path }) => shutdown_notebook(&path).await?,
        Some(Commands::Inspect {
            path,
            full_outputs,
            json,
        }) => inspect_notebook(&path, full_outputs, json).await?,
        Some(Commands::Debug {
            kernel,
            cmd,
            exec,
            dump,
            wait,
        }) => {
            debug_session(
                kernel.as_deref(),
                cmd.as_deref(),
                exec.as_deref(),
                dump,
                wait,
            )
            .await?
        }

        // Deprecated aliases (with warnings)
        Some(Commands::Start { name }) => {
            eprintln!("Warning: 'runt start' is deprecated. Use 'runt jupyter start' instead.");
            start_kernel(&name).await?
        }
        Some(Commands::Stop { id, all }) => {
            eprintln!("Warning: 'runt stop' is deprecated. Use 'runt jupyter stop' instead.");
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
        Some(Commands::Sidecar { .. }) => unreachable!(), // handled in main()
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

        None => println!("No command specified. Use --help for usage information."),
    }

    Ok(())
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
        JupyterCommands::Sidecar { .. } => unreachable!(), // handled in main()
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
            Ok(stats) => {
                if json {
                    println!("{}", serde_json::to_string_pretty(&stats)?);
                } else {
                    println!("Pool Daemon Status");
                    println!("==================");
                    println!("UV environments:");
                    println!("  Available: {}", stats.uv_available);
                    println!("  Warming:   {}", stats.uv_warming);
                    if let Some(ref err) = stats.uv_error {
                        println!("  ERROR:     {}", truncate_error(&err.message, 60));
                        if let Some(ref pkg) = err.failed_package {
                            println!("  Failed package: {}", pkg);
                        }
                        println!(
                            "  Failures:  {} (retry in {}s)",
                            err.consecutive_failures, err.retry_in_secs
                        );
                    }
                    println!("Conda environments:");
                    println!("  Available: {}", stats.conda_available);
                    println!("  Warming:   {}", stats.conda_warming);
                    if let Some(ref err) = stats.conda_error {
                        println!("  ERROR:     {}", truncate_error(&err.message, 60));
                        if let Some(ref pkg) = err.failed_package {
                            println!("  Failed package: {}", pkg);
                        }
                        println!(
                            "  Failures:  {} (retry in {}s)",
                            err.consecutive_failures, err.retry_in_secs
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

async fn daemon_command(command: DaemonCommands) -> Result<()> {
    use runtimed::client::PoolClient;
    use runtimed::service::ServiceManager;
    use runtimed::singleton::get_running_daemon_info;

    let manager = ServiceManager::default();

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
            let running = if daemon_info.is_some() {
                client.ping().await.is_ok()
            } else {
                false
            };
            let stats = if running {
                client.status().await.ok()
            } else {
                None
            };
            let is_dev = runtimed::is_dev_mode();

            if json {
                let output = serde_json::json!({
                    "installed": installed,
                    "running": running,
                    "dev_mode": is_dev,
                    "daemon_info": daemon_info,
                    "pool_stats": stats,
                });
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else {
                println!("runtimed Daemon Status");
                println!("======================");
                println!(
                    "Service installed: {}",
                    if installed { "yes" } else { "no" }
                );
                println!("Daemon running:    {}", if running { "yes" } else { "no" });

                // Show dev mode info
                if is_dev {
                    println!("Mode:              development");
                }
                if let Some(info) = &daemon_info {
                    if let Some(worktree) = &info.worktree_path {
                        println!(
                            "Worktree:          {}",
                            shorten_path(&PathBuf::from(worktree))
                        );
                    }
                    if let Some(desc) = &info.workspace_description {
                        println!("Description:       {}", desc);
                    }
                }

                if let Some(info) = &daemon_info {
                    println!("PID:               {}", info.pid);
                    println!("Version:           {}", info.version);
                    if let Some(port) = info.blob_port {
                        println!("Blob server:       http://127.0.0.1:{}", port);
                    }
                    let uptime = chrono::Utc::now() - info.started_at;
                    let hours = uptime.num_hours();
                    let mins = uptime.num_minutes() % 60;
                    println!("Uptime:            {}h {}m", hours, mins);
                }

                if let Some(stats) = &stats {
                    println!();
                    println!("Pool:");
                    println!(
                        "  UV:    {}/{} ready{}",
                        stats.uv_available,
                        stats.uv_available + stats.uv_warming,
                        if stats.uv_warming > 0 {
                            format!(" ({} warming)", stats.uv_warming)
                        } else {
                            String::new()
                        }
                    );
                    println!(
                        "  Conda: {}/{} ready{}",
                        stats.conda_available,
                        stats.conda_available + stats.conda_warming,
                        if stats.conda_warming > 0 {
                            format!(" ({} warming)", stats.conda_warming)
                        } else {
                            String::new()
                        }
                    );
                }
            }
        }
        DaemonCommands::Start => {
            if !manager.is_installed() {
                eprintln!("Service not installed. Run 'runt daemon install' first.");
                std::process::exit(1);
            }
            println!("Starting runtimed service...");
            manager.start()?;
            println!("Service started.");
        }
        DaemonCommands::Stop => {
            if !manager.is_installed() {
                eprintln!("Service not installed.");
                std::process::exit(1);
            }
            println!("Stopping runtimed service...");
            manager.stop()?;
            println!("Service stopped.");
        }
        DaemonCommands::Restart => {
            if !manager.is_installed() {
                eprintln!("Service not installed. Run 'runt daemon install' first.");
                std::process::exit(1);
            }
            println!("Restarting runtimed service...");
            let _ = manager.stop(); // Ignore if not running
            manager.start()?;
            println!("Service restarted.");
        }
        DaemonCommands::Install { binary } => {
            // Find runtimed binary: use provided path, or look for sibling binary
            let source = binary.unwrap_or_else(|| {
                let current_exe =
                    std::env::current_exe().expect("Failed to get current executable path");
                let exe_dir = current_exe.parent().unwrap();
                exe_dir.join(if cfg!(windows) {
                    "runtimed.exe"
                } else {
                    "runtimed"
                })
            });

            if !source.exists() {
                eprintln!("Daemon binary not found at: {}", source.display());
                eprintln!("Build it with: cargo build -p runtimed");
                std::process::exit(1);
            }

            if manager.is_installed() {
                eprintln!("Service already installed. Use 'runt daemon uninstall' first.");
                std::process::exit(1);
            }

            println!("Installing runtimed service...");
            println!("Source binary: {}", source.display());
            manager.install(&source)?;
            println!("Service installed. Run 'runt daemon start' to start it.");
        }
        DaemonCommands::Uninstall => {
            if !manager.is_installed() {
                println!("Service not installed.");
                return Ok(());
            }
            println!("Uninstalling runtimed service...");
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
        DaemonCommands::ListWorktrees { json } => {
            list_worktree_daemons(json).await?;
        }
    }

    Ok(())
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
        .join("runt")
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
                            c.outputs.iter().map(|o| {
                                serde_json::from_str(o).unwrap_or(serde_json::Value::String(o.clone()))
                            }).collect()
                        } else {
                            c.outputs.iter().map(|o| {
                                // Parse and summarize
                                if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(o) {
                                    if let Some(otype) = parsed.get("output_type").and_then(|v| v.as_str()) {
                                        serde_json::json!({
                                            "output_type": otype,
                                            "size": o.len(),
                                        })
                                    } else {
                                        serde_json::json!({ "size": o.len() })
                                    }
                                } else {
                                    serde_json::json!({ "size": o.len(), "parse_error": true })
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
                            // Pretty print the JSON
                            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output) {
                                println!(
                                    "      output[{}]: {}",
                                    j,
                                    serde_json::to_string_pretty(&parsed)?
                                );
                            } else {
                                println!("      output[{}]: {}", j, output);
                            }
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

async fn debug_session(
    kernel_name: Option<&str>,
    cmd: Option<&str>,
    exec: Option<&str>,
    dump: Option<PathBuf>,
    wait: bool,
) -> Result<()> {
    use jupyter_protocol::{
        ExecuteRequest, ExecutionState, JupyterMessage, JupyterMessageContent, MediaType, Status,
        Stdio,
    };
    use std::io::{self, Write};
    use std::process::{Child, Command, Stdio as ProcessStdio};

    // Determine dump file path
    let dump_path = dump.unwrap_or_else(|| {
        let temp_dir = std::env::temp_dir();
        temp_dir.join(format!("runt-debug-{}.jsonl", uuid::Uuid::new_v4()))
    });

    // Start kernel
    let mut client = match (kernel_name, cmd) {
        (_, Some(cmd)) => KernelClient::start_from_command(cmd).await?,
        (Some(name), None) => {
            let kernelspec = find_kernelspec(name).await?;
            KernelClient::start_from_kernelspec(kernelspec).await?
        }
        (None, None) => anyhow::bail!("Provide a kernel name or --cmd"),
    };

    let connection_file = client.connection_file().to_path_buf();
    let kernel_id = client.kernel_id().to_string();

    println!("Kernel started: {}", kernel_id);
    println!("Connection file: {}", connection_file.display());
    println!("Dump file: {}", dump_path.display());

    // Find sidecar binary (same directory as current executable)
    let current_exe = std::env::current_exe()?;
    let exe_dir = current_exe.parent().unwrap();
    let sidecar_path = exe_dir.join(if cfg!(windows) {
        "sidecar.exe"
    } else {
        "sidecar"
    });

    if !sidecar_path.exists() {
        anyhow::bail!(
            "Sidecar binary not found at {}. Build with: cargo build -p sidecar",
            sidecar_path.display()
        );
    }

    // Spawn sidecar as subprocess
    let mut sidecar_child: Child = Command::new(&sidecar_path)
        .arg(&connection_file)
        .arg("--dump")
        .arg(&dump_path)
        .stdout(ProcessStdio::null())
        .stderr(ProcessStdio::piped())
        .spawn()?;

    println!("Sidecar started (PID: {})", sidecar_child.id());

    // Give sidecar time to initialize
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Execute code if provided
    if let Some(code) = exec {
        println!("\nExecuting: {}", &code[..code.len().min(80)]);

        let connection_info = client.connection_info();
        let session_id = client.session_id();
        let identity = runtimelib::peer_identity_for_session(session_id)?;
        let shell = runtimelib::create_client_shell_connection_with_identity(
            connection_info,
            session_id,
            identity,
        )
        .await?;
        let (mut shell_writer, mut shell_reader) = shell.split();

        let mut iopub =
            runtimelib::create_client_iopub_connection(connection_info, "", session_id).await?;

        let execute_request = ExecuteRequest::new(code.to_string());
        let message: JupyterMessage = execute_request.into();
        let message_id = message.header.msg_id.clone();
        shell_writer.send(message).await?;

        // Wait for idle status
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
                                    println!("Out: {}", text);
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
                        _ => {}
                    }
                }
                result = shell_reader.read() => {
                    let _ = result?; // Just drain shell replies
                }
            }
        }
        println!("\nExecution complete.");
    }

    // Wait for user interaction if requested
    if wait {
        println!("\nSidecar running. Interact with widgets, then press Ctrl+C to exit.");
        tokio::signal::ctrl_c().await?;
        println!("\nReceived Ctrl+C, shutting down...");
    }

    // Cleanup
    let _ = sidecar_child.kill();
    let _ = sidecar_child.wait();

    println!("\nShutting down kernel...");
    client.shutdown(false).await?;

    println!("\nDebug session complete.");
    println!("Dump file: {}", dump_path.display());
    println!("\nTo analyze:");
    println!(
        "  cat {} | jq -c '{{ts: .ts, dir: .dir, ch: .ch, type: .msg.header.msg_type}}'",
        dump_path.display()
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
