//! runtimed CLI entry point.
//!
//! This runs the runtime daemon as a standalone process that manages
//! prewarmed Python environments for notebook windows.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use log::info;
use runtimed::client::PoolClient;
use runtimed::daemon::{Daemon, DaemonConfig};
use runtimed::service::ServiceManager;
use runtimed::singleton::get_running_daemon_info;

#[derive(Parser, Debug)]
#[command(name = "runtimed")]
#[command(version = concat!(env!("CARGO_PKG_VERSION"), "+", env!("GIT_COMMIT")))]
#[command(about = "Runtime daemon for managing Jupyter environments")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Log level (defaults to "info" on nightly, "warn" on stable)
    #[arg(long, global = true)]
    log_level: Option<String>,

    /// Run in development mode (per-worktree isolation)
    ///
    /// When enabled, the daemon stores all state in ~/.cache/runt/worktrees/{hash}/
    /// instead of ~/.cache/runt/, allowing multiple worktrees to run their own
    /// isolated daemon instances.
    #[arg(long, global = true)]
    dev: bool,
}

fn daemon_binary_name() -> &'static str {
    runt_workspace::daemon_binary_basename()
}

fn daemon_service_name() -> &'static str {
    runt_workspace::daemon_service_basename()
}

fn cli_command_name() -> &'static str {
    runt_workspace::cli_command_name()
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Run the daemon (default if no command specified)
    Run {
        /// Socket path for the unified IPC socket (default: ~/.cache/runt*/runtimed.sock)
        #[arg(long)]
        socket: Option<PathBuf>,

        /// Cache directory for environments (default: ~/.cache/runt/envs)
        #[arg(long)]
        cache_dir: Option<PathBuf>,

        /// Directory for the content-addressed blob store (default: ~/.cache/runt/blobs)
        #[arg(long)]
        blob_store_dir: Option<PathBuf>,

        /// Number of UV environments to maintain
        #[arg(long, default_value = "3")]
        uv_pool_size: usize,

        /// Number of Conda environments to maintain
        #[arg(long, default_value = "3")]
        conda_pool_size: usize,
    },

    /// Install daemon as a system service
    Install {
        /// Path to the daemon binary to install (default: current binary)
        #[arg(long)]
        binary: Option<PathBuf>,
    },

    // =========================================================================
    // Deprecated commands - use 'runt daemon' instead
    // =========================================================================
    /// [DEPRECATED] Use 'runt daemon uninstall' instead
    #[command(hide = true)]
    Uninstall,

    /// [DEPRECATED] Use 'runt daemon status' instead
    #[command(hide = true)]
    Status {
        #[arg(long)]
        json: bool,
    },

    /// [DEPRECATED] Use 'runt daemon start' instead
    #[command(hide = true)]
    Start,

    /// [DEPRECATED] Use 'runt daemon stop' instead
    #[command(hide = true)]
    Stop,

    /// [DEPRECATED] Use 'runt daemon flush' instead
    #[command(hide = true)]
    FlushPool,

    /// Run as a runtime agent subprocess (internal, used by coordinator)
    #[command(hide = true)]
    Agent,
}

/// Get a log path that works even when HOME is not set.
/// Falls back to /tmp if the normal cache directory is unavailable.
fn early_log_path() -> PathBuf {
    // Try the standard location first
    if let Some(cache) = dirs::cache_dir() {
        let path = cache
            .join(runt_workspace::cache_namespace())
            .join("runtimed.log");
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_ok() {
                return path;
            }
        }
    }
    // Fallback to /tmp which should always be writable
    PathBuf::from("/tmp/runtimed-startup.log")
}

/// Write an early diagnostic message before logging is initialized.
/// This ensures we capture startup failures even when HOME is not set.
fn early_log(msg: &str) {
    use std::io::Write;
    let path = early_log_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(file, "{} [STARTUP] {}", timestamp, msg);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Install panic hook to ensure panics are logged to the daemon log file.
    // Uses early_log_path() which falls back to /tmp if HOME is not set.
    std::panic::set_hook(Box::new(|panic_info| {
        use std::io::Write;

        let log_path = early_log_path();
        let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let bt = std::backtrace::Backtrace::force_capture();
        let msg = format!("{} [PANIC] runtimed: {}\n{}", timestamp, panic_info, bt);

        // Write to stderr (visible in terminal)
        eprintln!("{}", msg);

        // Also append to log file so it's captured for debugging.
        if let Some(parent) = log_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(file, "{}", msg);
        }
    }));

    let cli = Cli::parse();

    // Set dev mode environment variable if flag is used
    if cli.dev {
        std::env::set_var("RUNTIMED_DEV", "1");
    }

    // Initialize logging - write to both stderr and log file
    let log_path = runtimed::default_log_path();
    if let Some(parent) = log_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    // Rotate the previous session's log so the current file only contains
    // this daemon run. Keeps one old copy (.log.1) for crash diagnosis via
    // `runt diagnostics`. Only runs on the daemon path — subcommands like
    // `status` or `install` must not touch the live log.
    if matches!(cli.command, None | Some(Commands::Run { .. })) {
        let prev = log_path.with_extension("log.1");
        let _ = std::fs::rename(&log_path, &prev);
    }

    // Log startup diagnostics after rotation so the breadcrumb lands in the
    // current session's log file, not in .log.1. The panic hook above still
    // catches crashes before this point.
    early_log(&format!(
        "runtimed starting: pid={}, HOME={:?}, USER={:?}",
        std::process::id(),
        std::env::var("HOME").ok(),
        std::env::var("USER").ok()
    ));

    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path);

    let effective_log_level =
        cli.log_level
            .unwrap_or_else(|| match runt_workspace::build_channel() {
                runt_workspace::BuildChannel::Nightly => {
                    "info,notebook_sync=debug,runtimed::notebook_sync_server=debug".to_string()
                }
                runt_workspace::BuildChannel::Stable => "warn".to_string(),
            });
    let mut builder = env_logger::Builder::from_env(
        env_logger::Env::default().default_filter_or(&effective_log_level),
    );

    // If we can open the log file, write to it; otherwise just use stderr
    if let Ok(file) = log_file {
        use std::io::Write;
        use std::sync::{Arc, Mutex};

        let file = Arc::new(Mutex::new(file));
        builder.format(move |_buf, record| {
            let formatted = format!(
                "{} [{}] {}: {}\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                record.level(),
                record.target(),
                record.args()
            );
            // Write to stderr (terminal)
            eprint!("{}", formatted);
            // Write to file
            if let Ok(mut f) = file.lock() {
                let _ = f.write_all(formatted.as_bytes());
                let _ = f.flush();
            }
            Ok(())
        });
    }
    builder.init();

    // Log dev mode status
    if runtimed::is_dev_mode() {
        if let Some(worktree) = runtimed::get_workspace_path() {
            info!(
                "Development mode enabled for worktree: {}",
                worktree.display()
            );
            info!("Logs: {}", log_path.display());
            if let Some(name) = runtimed::get_workspace_name() {
                info!("Workspace description: {}", name);
            }
        } else {
            info!("Development mode enabled (no worktree detected)");
        }
    }

    match cli.command {
        None | Some(Commands::Run { .. }) => {
            // Extract run args from command or use defaults
            let (socket, cache_dir, blob_store_dir, uv_pool_size, conda_pool_size) =
                match cli.command {
                    Some(Commands::Run {
                        socket,
                        cache_dir,
                        blob_store_dir,
                        uv_pool_size,
                        conda_pool_size,
                    }) => (
                        socket,
                        cache_dir,
                        blob_store_dir,
                        uv_pool_size,
                        conda_pool_size,
                    ),
                    _ => (None, None, None, 3, 3),
                };

            run_daemon(
                socket,
                cache_dir,
                blob_store_dir,
                uv_pool_size,
                conda_pool_size,
            )
            .await
        }
        Some(Commands::Install { binary }) => install_service(binary),
        // Deprecated commands - still work but print warnings
        Some(Commands::Uninstall) => {
            eprintln!(
                "Warning: '{} uninstall' is deprecated. Use '{} daemon uninstall' instead.",
                daemon_binary_name(),
                cli_command_name()
            );
            uninstall_service()
        }
        Some(Commands::Status { json }) => {
            eprintln!(
                "Warning: '{} status' is deprecated. Use '{} daemon status' instead.",
                daemon_binary_name(),
                cli_command_name()
            );
            status(json).await
        }
        Some(Commands::Start) => {
            eprintln!(
                "Warning: '{} start' is deprecated. Use '{} daemon start' instead.",
                daemon_binary_name(),
                cli_command_name()
            );
            start_service()
        }
        Some(Commands::Stop) => {
            eprintln!(
                "Warning: '{} stop' is deprecated. Use '{} daemon stop' instead.",
                daemon_binary_name(),
                cli_command_name()
            );
            stop_service()
        }
        Some(Commands::FlushPool) => {
            eprintln!(
                "Warning: '{} flush-pool' is deprecated. Use '{} daemon flush' instead.",
                daemon_binary_name(),
                cli_command_name()
            );
            flush_pool().await
        }
        Some(Commands::Agent) => {
            // Agent mode: communicate over stdin/stdout using framed protocol.
            #[cfg(unix)]
            {
                // Use tokio::fs::File from raw fd to avoid tokio::io::stdout()
                // buffering issues that prevent frames from being read by the parent.
                use std::os::unix::io::FromRawFd;
                let stdin = unsafe { tokio::fs::File::from_raw_fd(0) };
                let stdout = unsafe { tokio::fs::File::from_raw_fd(1) };
                runtimed::agent::run_agent(stdin, stdout)
                    .await
                    .map_err(|e| {
                        eprintln!("[agent] Fatal: {}", e);
                        e
                    })
            }
            #[cfg(not(unix))]
            {
                runtimed::agent::run_agent(tokio::io::stdin(), tokio::io::stdout())
                    .await
                    .map_err(|e| {
                        eprintln!("[agent] Fatal: {}", e);
                        e
                    })
            }
        }
    }
}

async fn run_daemon(
    socket: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    blob_store_dir: Option<PathBuf>,
    uv_pool_size: usize,
    conda_pool_size: usize,
) -> anyhow::Result<()> {
    info!("runtimed starting...");

    let config = DaemonConfig {
        socket_path: socket.unwrap_or_else(runtimed::default_socket_path),
        cache_dir: cache_dir.unwrap_or_else(runtimed::default_cache_dir),
        blob_store_dir: blob_store_dir.unwrap_or_else(runtimed::default_blob_store_dir),
        uv_pool_size,
        conda_pool_size,
        ..Default::default()
    };

    info!("Configuration:");
    info!("  Socket: {:?}", config.socket_path);
    info!("  Cache dir: {:?}", config.cache_dir);
    info!("  Blob store: {:?}", config.blob_store_dir);
    info!("  UV pool size: {}", config.uv_pool_size);
    info!("  Conda pool size: {}", config.conda_pool_size);
    // Agent mode status is logged after daemon initialization
    // (reads from persisted settings)

    let daemon = match Daemon::new(config) {
        Ok(d) => {
            if d.agent_mode.load(std::sync::atomic::Ordering::Relaxed) {
                info!("  Agent mode: ENABLED (kernels run in subprocess)");
            }
            d
        }
        Err(e) => {
            // Another daemon is already running — this is expected during
            // launchd double-start races, NOT a crash. Exit 0 so launchd's
            // KeepAlive.Crashed does not restart us.
            let msg = format!(
                "Another daemon already running (pid={}, endpoint={}), exiting cleanly",
                e.info.pid, e.info.endpoint
            );
            early_log(&msg);
            eprintln!("{msg}");
            std::process::exit(0);
        }
    };

    // Set up signal handlers for graceful shutdown with logging
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let shutdown_daemon = daemon.clone();

        tokio::spawn(async move {
            #[allow(clippy::expect_used)]
            // Signal registration failure is a fundamental OS issue with no recovery
            let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM");
            #[allow(clippy::expect_used)]
            // Signal registration failure is a fundamental OS issue with no recovery
            let mut sigint = signal(SignalKind::interrupt()).expect("failed to register SIGINT");

            tokio::select! {
                _ = sigterm.recv() => {
                    early_log("Received SIGTERM, initiating shutdown");
                }
                _ = sigint.recv() => {
                    early_log("Received SIGINT, initiating shutdown");
                }
            }
            shutdown_daemon.trigger_shutdown().await;
        });
    }

    let result = daemon.run().await;
    match &result {
        Ok(()) => early_log("Daemon exited: Ok (graceful shutdown)"),
        Err(e) => early_log(&format!("Daemon exited: Err: {}", e)),
    }
    result
}

fn install_service(binary: Option<PathBuf>) -> anyhow::Result<()> {
    let source_binary = match binary {
        Some(path) => path,
        None => std::env::current_exe()?,
    };

    println!("Installing {} service...", daemon_service_name());
    println!("Source binary: {}", source_binary.display());

    let mut manager = ServiceManager::default();

    if manager.is_installed() {
        // Already installed - upgrade the binary instead of failing
        // upgrade() handles: stop old -> copy binary -> start new
        println!("Service already installed, upgrading...");
        manager.upgrade(&source_binary)?;
    } else {
        // Fresh install
        manager.install(&source_binary)?;
        println!("Starting daemon...");
        manager.start()?;
    }

    println!();
    println!("Service installed and running!");
    println!("The daemon will start automatically at login.");
    println!();
    println!("To check status: {} daemon status", cli_command_name());
    println!("To uninstall:    {} daemon uninstall", cli_command_name());

    Ok(())
}

fn uninstall_service() -> anyhow::Result<()> {
    println!("Uninstalling {} service...", daemon_service_name());

    let manager = ServiceManager::default();

    if !manager.is_installed() {
        println!("Service not installed.");
        return Ok(());
    }

    manager.uninstall()?;

    println!("Service uninstalled successfully.");

    Ok(())
}

async fn status(json: bool) -> anyhow::Result<()> {
    let manager = ServiceManager::default();
    let installed = manager.is_installed();

    // Check if daemon is running
    let daemon_info = get_running_daemon_info();
    let running = if daemon_info.is_some() {
        // Try to ping to confirm it's actually responding
        let client = PoolClient::default();
        client.ping().await.is_ok()
    } else {
        false
    };

    // Get pool stats if running
    let stats = if running {
        let client = PoolClient::default();
        client.status().await.ok()
    } else {
        None
    };

    if json {
        let output = serde_json::json!({
            "installed": installed,
            "running": running,
            "daemon_info": daemon_info,
            "pool_stats": stats,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{} Status", daemon_service_name());
        println!("===============");
        println!(
            "Service installed: {}",
            if installed { "yes" } else { "no" }
        );
        println!("Daemon running:    {}", if running { "yes" } else { "no" });

        if let Some(info) = daemon_info {
            println!();
            println!("Daemon Info:");
            println!("  PID:      {}", info.pid);
            println!("  Endpoint: {}", info.endpoint);
            println!("  Version:  {}", info.version);
            println!("  Started:  {}", info.started_at);
        }

        if let Some(state) = stats {
            println!();
            println!("Pool Statistics:");
            println!(
                "  UV:    {}/{} available",
                state.uv.available,
                state.uv.available + state.uv.warming
            );
            println!(
                "  Conda: {}/{} available",
                state.conda.available,
                state.conda.available + state.conda.warming
            );
        }
    }

    Ok(())
}

fn start_service() -> anyhow::Result<()> {
    let manager = ServiceManager::default();

    if !manager.is_installed() {
        eprintln!(
            "Service not installed. Run '{} install' first.",
            daemon_binary_name()
        );
        std::process::exit(1);
    }

    println!("Starting {} service...", daemon_service_name());
    manager.start()?;
    println!("Service started.");

    Ok(())
}

fn stop_service() -> anyhow::Result<()> {
    let manager = ServiceManager::default();

    if !manager.is_installed() {
        eprintln!("Service not installed.");
        std::process::exit(1);
    }

    println!("Stopping {} service...", daemon_service_name());
    manager.stop()?;
    println!("Service stopped.");

    Ok(())
}

async fn flush_pool() -> anyhow::Result<()> {
    let client = PoolClient::default();

    if !client.is_daemon_running().await {
        eprintln!("Daemon is not running.");
        std::process::exit(1);
    }

    println!("Flushing pool environments...");
    client
        .flush_pool()
        .await
        .map_err(|e| anyhow::anyhow!("Failed to flush pool: {}", e))?;
    println!("Pool flushed. Environments will be rebuilt with current settings.");

    Ok(())
}
