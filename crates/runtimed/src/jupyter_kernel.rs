// Mutex::lock only fails if another thread panicked while holding the lock.
// In that case the program is already crashing, so unwrap is acceptable here.
// The shell_writer unwrap at execute() is guarded by an early-return check.
#![allow(clippy::unwrap_used)]
//! JupyterKernel — concrete `KernelConnection` implementation.
//!
//! Owns the IO-bound parts of a Jupyter kernel: ZeroMQ connections, spawned
//! task handles, request/response infrastructure, process lifecycle.  Does
//! **not** hold queue, executing cell, or status — those live in `KernelState`.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::Result;
use bytes::Bytes;
use jupyter_protocol::{
    CompleteRequest, ConnectionInfo, ExecuteRequest, HistoryRequest, InterruptRequest,
    JupyterMessage, JupyterMessageContent, KernelInfoRequest, ShutdownRequest,
};
use runtime_doc::{KernelActivity, RuntimeLifecycle};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::kernel_connection::{KernelConnection, KernelLaunchConfig, KernelSharedRefs};
use crate::output_prep::{
    blob_store_large_state_values, escape_glob_pattern, extract_buffer_paths,
    media_to_display_data, message_content_to_nbformat, store_widget_buffers,
    update_output_by_display_id_with_manifests, QueueCommand,
};
use crate::output_store::{self, ContentRef, OutputManifest, DEFAULT_INLINE_THRESHOLD};
use crate::protocol::{CompletionItem, HistoryEntry, NotebookBroadcast};
use crate::stream_terminal::{StreamOutputState, StreamTerminals};
use crate::task_supervisor::{spawn_best_effort, spawn_supervised};
use crate::terminal_size::{TERMINAL_COLUMNS_STR, TERMINAL_LINES_STR};
use crate::EnvType;
use notebook_protocol::protocol::{KernelPorts, LaunchedEnvConfig};

#[cfg(unix)]
fn ipc_path_prefix(kernel_id: &str) -> PathBuf {
    crate::ipc_socket_dir().join(format!("kernel-{}-ipc", kernel_id))
}

#[cfg(unix)]
pub(crate) fn cleanup_ipc_sockets(prefix: &Path) {
    for port in 1..=5u16 {
        let _ = std::fs::remove_file(format!("{}-{}", prefix.display(), port));
    }
}

async fn bind_kernel_port_listeners(ip: IpAddr, ports: KernelPorts) -> Result<Vec<TcpListener>> {
    let port_numbers = [
        ports.stdin,
        ports.control,
        ports.hb,
        ports.shell,
        ports.iopub,
    ];
    let mut listeners = Vec::with_capacity(port_numbers.len());
    for port in port_numbers {
        listeners.push(TcpListener::bind(SocketAddr::new(ip, port)).await?);
    }
    Ok(listeners)
}

/// Type alias for pending completion response channels.
type PendingCompletions =
    Arc<StdMutex<HashMap<String, oneshot::Sender<(Vec<CompletionItem>, usize, usize)>>>>;

/// Handle for interrupting a kernel without exclusive access.
///
/// What an IOPub status message translates to on the RuntimeStateDoc.
///
/// Either a lifecycle transition (Starting/Restarting/Terminating/Dead —
/// infrequent, always written) or an activity flip (Idle/Busy — hot path,
/// throttled by `set_activity`). Kept local to this file because it is
/// only a dispatch helper for the IOPub handler.
enum IoPubStateUpdate {
    Activity(KernelActivity),
    Lifecycle(RuntimeLifecycle),
}

/// Created at kernel launch time, captures connection_info and session_id.
/// Can be used concurrently with other kernel operations since interrupt
/// creates its own ZMQ control connection.
#[derive(Clone)]
pub struct InterruptHandle {
    connection_info: ConnectionInfo,
    session_id: String,
}

impl InterruptHandle {
    pub async fn interrupt(&self) -> Result<()> {
        let mut control =
            runtimelib::create_client_control_connection(&self.connection_info, &self.session_id)
                .await?;

        let request: JupyterMessage = InterruptRequest {}.into();
        control.send(request).await?;

        info!("[jupyter-kernel] Sent interrupt_request (via handle)");

        match tokio::time::timeout(std::time::Duration::from_secs(5), control.read()).await {
            Ok(Ok(_reply)) => {
                info!("[jupyter-kernel] Received interrupt_reply");
            }
            Ok(Err(e)) => {
                warn!("[jupyter-kernel] Error receiving interrupt_reply: {}", e);
            }
            Err(_) => {
                warn!("[jupyter-kernel] Timed out waiting for interrupt_reply (5s)");
            }
        }

        Ok(())
    }
}

/// A Jupyter kernel connection that implements `KernelConnection`.
///
/// Holds the IO-bound parts of a kernel connection: ZeroMQ sockets, spawned
/// background tasks, and request/response infrastructure.  Queue management
/// and state transitions live in `KernelState`.
///
/// Some fields (e.g., `kernel_actor_id`, `comm_seq`, `stream_terminals`) are
/// cloned into spawned tasks during `launch()` and not read from `&self`
/// methods directly, but must be kept alive for the struct's lifetime.
#[allow(dead_code)]
pub struct JupyterKernel {
    /// Kernel type (e.g., "python", "deno").
    kernel_type: String,
    /// Environment source (e.g., "uv:inline", "conda:prewarmed").
    env_source: String,
    /// Environment configuration used at launch (for sync detection).
    launched_config: LaunchedEnvConfig,
    /// Path to the environment directory backing this kernel (if any).
    pub env_path: Option<PathBuf>,
    /// Session ID for Jupyter protocol.
    session_id: String,
    /// Automerge actor ID for kernel writes (e.g. "rt:kernel:a1b2c3d4").
    kernel_actor_id: String,
    /// Connection info for the kernel.
    connection_info: Option<ConnectionInfo>,
    /// Path to the connection file.
    connection_file: Option<PathBuf>,
    /// Shell writer for sending execute requests.
    shell_writer: Option<runtimelib::DealerSendConnection>,
    /// IPC socket path prefix for cleanup (Unix only).
    #[cfg(unix)]
    ipc_prefix: Option<PathBuf>,
    /// Kernel process PID for signal-based cleanup (Unix only).
    #[cfg(unix)]
    kernel_pid: Option<i32>,
    /// Handle to the iopub listener task.
    iopub_task: Option<JoinHandle<()>>,
    /// Handle to the shell reader task.
    shell_reader_task: Option<JoinHandle<()>>,
    /// Handle to the process watcher task (detects process exit).
    process_watcher_task: Option<JoinHandle<()>>,
    /// Handle to the heartbeat monitor task (detects unresponsive kernel).
    heartbeat_task: Option<JoinHandle<()>>,
    /// Channel for coalesced comm state writes (IOPub -> coalesce task).
    comm_coalesce_tx: Option<mpsc::UnboundedSender<(String, serde_json::Value)>>,
    /// Handle to the coalescing task for comm state CRDT writes.
    comm_coalesce_task: Option<JoinHandle<()>>,
    /// Mapping from execution_id -> cell_id for routing iopub messages.
    /// With msg_id = execution_id, parent_header.msg_id IS the execution_id.
    cell_id_map: Arc<StdMutex<HashMap<String, String>>>,
    /// Command sender for iopub/shell tasks.
    cmd_tx: Option<mpsc::Sender<QueueCommand>>,
    /// Monotonic counter for comm insertion order (written to RuntimeStateDoc).
    comm_seq: Arc<AtomicU64>,
    /// Pending history requests: msg_id -> response channel.
    pending_history: Arc<StdMutex<HashMap<String, oneshot::Sender<Vec<HistoryEntry>>>>>,
    /// Pending completion requests: msg_id -> response channel.
    pending_completions: PendingCompletions,
    /// Terminal emulators for stream outputs (stdout/stderr).
    stream_terminals: Arc<tokio::sync::Mutex<StreamTerminals>>,
}

impl KernelConnection for JupyterKernel {
    // ── Launch ────────────────────────────────────────────────────────────

    async fn launch(
        config: KernelLaunchConfig,
        shared: KernelSharedRefs,
    ) -> Result<(Self, mpsc::Receiver<QueueCommand>)> {
        let kernel_type = config.kernel_type;
        let env_source = config.env_source;
        let notebook_path = config.notebook_path;
        let env = config.pooled_env;
        let launched_config = config.launched_config;
        let bootstrap_dx = launched_config.feature_flags.bootstrap_dx;
        let env_path = env.as_ref().map(|e| e.venv_path.clone());

        // ── Build process command ────────────────────────────────────────

        // Determine kernel name for connection info
        let kernelspec_name = match kernel_type.as_str() {
            "python" => "python3",
            "deno" => "deno",
            _ => &kernel_type,
        };

        // Per-launch IP and stable connection-file path. Ports are
        // (re-)reserved inside the spawn loop below so the file's contents
        // can be rewritten with fresh port numbers on retry; the path itself
        // is what the kernel command-line points at, so keeping it stable
        // means we don't have to rebuild `cmd` between attempts.
        let ip = std::net::IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let conn_dir = crate::connections_dir();
        tokio::fs::create_dir_all(&conn_dir).await?;
        let kernel_id: String =
            petname::petname(2, "-").unwrap_or_else(|| Uuid::new_v4().to_string());
        let connection_file_path = conn_dir.join(format!("{}.json", kernel_id));

        // Determine working directory
        let cwd = if let Some(ref path) = notebook_path {
            if path.is_dir() {
                path.to_path_buf()
            } else {
                path.parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(std::env::temp_dir)
            }
        } else {
            runt_workspace::default_notebooks_dir().unwrap_or_else(|_| std::env::temp_dir())
        };

        // Build kernel command based on kernel type
        let mut cmd = match kernel_type.as_str() {
            "python" => {
                match env_source.as_str() {
                    "uv:inline" => {
                        let pooled_env = env.ok_or_else(|| {
                            anyhow::anyhow!(
                                "uv:inline requires a prepared environment (was it created?)"
                            )
                        })?;
                        info!(
                            "[jupyter-kernel] Starting Python kernel with cached inline env at {:?}",
                            pooled_env.python_path
                        );
                        let launcher_module = if bootstrap_dx {
                            "nteract_kernel_launcher"
                        } else {
                            "ipykernel_launcher"
                        };
                        let mut cmd = tokio::process::Command::new(&pooled_env.python_path);
                        cmd.args(["-Xfrozen_modules=off", "-m", launcher_module, "-f"]);
                        cmd.arg(&connection_file_path);
                        cmd.stdout(Stdio::null());
                        cmd.stderr(Stdio::piped());
                        cmd.env("VIRTUAL_ENV", &pooled_env.venv_path);
                        if bootstrap_dx {
                            // Inline envs are daemon-owned and normally have
                            // the launcher vendored into site-packages, but
                            // old cache entries or pool-promoted envs may be
                            // missing it. Inject the daemon-side launcher so
                            // `-m nteract_kernel_launcher` works on cache hits
                            // without requiring users to clear inline-envs.
                            let dir = crate::launcher_cache::launcher_cache_dir().await?;
                            cmd.env("PYTHONPATH", &dir);
                        }
                        let uv_path = kernel_launch::tools::get_uv_path().await?;
                        if let Some(uv_dir) = uv_path.parent() {
                            cmd.env("PATH", prepend_to_path(uv_dir));
                        }
                        cmd
                    }
                    "uv:pyproject" => {
                        let uv_path = kernel_launch::tools::get_uv_path().await?;
                        info!(
                            "[jupyter-kernel] Starting Python kernel with uv run (env_source: {})",
                            env_source
                        );
                        let mut cmd = tokio::process::Command::new(&uv_path);
                        let mut args: Vec<String> = vec![
                            "run".into(),
                            "--with".into(),
                            "ipykernel".into(),
                            "--with".into(),
                            "uv".into(),
                        ];
                        if bootstrap_dx {
                            // `dx` is on PyPI; the launcher is injected via
                            // PYTHONPATH below so `-m nteract_kernel_launcher`
                            // resolves without shadowing cwd in sys.path[0].
                            args.push("--with".into());
                            args.push("dx".into());
                        }
                        args.push("python".into());
                        args.push("-Xfrozen_modules=off".into());
                        args.push("-m".into());
                        let launcher_module = if bootstrap_dx {
                            "nteract_kernel_launcher"
                        } else {
                            "ipykernel_launcher"
                        };
                        args.push(launcher_module.into());
                        args.push("-f".into());
                        cmd.args(&args);
                        cmd.arg(&connection_file_path);
                        cmd.stdout(Stdio::null());
                        cmd.stderr(Stdio::piped());
                        if bootstrap_dx {
                            // Inject the daemon-side launcher via PYTHONPATH
                            // instead of invoking it as a script. Running
                            // `python /path/to/launcher.py` would set
                            // `sys.path[0]` to the launcher's cache dir, which
                            // breaks sibling-module imports from the notebook
                            // cwd. `uv run` preserves env vars, and putting
                            // the launcher dir on PYTHONPATH keeps cwd at
                            // `sys.path[0]`.
                            let dir = crate::launcher_cache::launcher_cache_dir().await?;
                            cmd.env("PYTHONPATH", &dir);
                        }
                        cmd
                    }
                    "conda:inline" => {
                        let pooled_env = env.ok_or_else(|| {
                            anyhow::anyhow!(
                                "conda:inline requires a prepared environment (was it created?)"
                            )
                        })?;
                        info!(
                            "[jupyter-kernel] Starting Python kernel with cached conda inline env at {:?}",
                            pooled_env.python_path
                        );
                        let launcher_module = if bootstrap_dx {
                            "nteract_kernel_launcher"
                        } else {
                            "ipykernel_launcher"
                        };
                        let mut cmd = tokio::process::Command::new(&pooled_env.python_path);
                        cmd.args(["-Xfrozen_modules=off", "-m", launcher_module, "-f"]);
                        cmd.arg(&connection_file_path);
                        cmd.stdout(Stdio::null());
                        cmd.stderr(Stdio::piped());
                        if bootstrap_dx {
                            // See the uv:inline branch above. Conda inline
                            // cache hits can predate launcher vendoring or be
                            // claimed from a pool env; PYTHONPATH injection
                            // makes those caches launchable.
                            let dir = crate::launcher_cache::launcher_cache_dir().await?;
                            cmd.env("PYTHONPATH", &dir);
                        }
                        cmd
                    }
                    "conda:env_yml" => {
                        // Use the project's named conda environment from environment.yml.
                        // The env prefix was resolved during launch preparation (named env
                        // lookup or creation via rattler) and passed as the pooled_env.
                        let conda_prefix =
                            env.as_ref().map(|e| e.venv_path.clone()).or_else(|| {
                                // Fallback: look up the named env from environment.yml
                                notebook_path.as_deref().and_then(|p| {
                                    crate::project_file::find_nearest_project_file(
                                        p,
                                        &[crate::project_file::ProjectFileKind::EnvironmentYml],
                                    )
                                    .and_then(|d| {
                                        crate::project_file::resolve_conda_env_prefix(&d.path)
                                    })
                                })
                            });

                        if let Some(ref prefix) = conda_prefix {
                            let python = crate::project_file::conda_python_path(prefix);
                            if python.exists() {
                                info!(
                                    "[jupyter-kernel] Starting Python kernel from conda:env_yml env ({})",
                                    python.display()
                                );
                                let launcher_module = if bootstrap_dx {
                                    "nteract_kernel_launcher"
                                } else {
                                    "ipykernel_launcher"
                                };
                                let mut cmd = tokio::process::Command::new(&python);
                                cmd.env("CONDA_PREFIX", prefix);
                                if let Some(ref nb_path) = notebook_path {
                                    if let Some(parent) = nb_path.parent() {
                                        cmd.current_dir(parent);
                                    }
                                }
                                cmd.args(["-Xfrozen_modules=off", "-m", launcher_module, "-f"]);
                                cmd.arg(&connection_file_path);
                                cmd.stdout(Stdio::null());
                                cmd.stderr(Stdio::piped());
                                if bootstrap_dx {
                                    // The env belongs to the user (created from
                                    // their environment.yml). Writing launcher
                                    // files into a shared conda env is a
                                    // permissions / cleanup hazard, so inject
                                    // the daemon-side launcher via PYTHONPATH —
                                    // same pattern as `uv run` and `pixi exec`.
                                    // Without this, `-m nteract_kernel_launcher`
                                    // fails immediately with ModuleNotFoundError.
                                    let dir = crate::launcher_cache::launcher_cache_dir().await?;
                                    cmd.env("PYTHONPATH", &dir);
                                }
                                cmd
                            } else {
                                return Err(anyhow::anyhow!(
                                    "conda:env_yml env at {:?} has no python binary",
                                    prefix
                                ));
                            }
                        } else {
                            return Err(anyhow::anyhow!(
                                "conda:env_yml could not resolve conda environment prefix"
                            ));
                        }
                    }
                    "pixi:toml" => {
                        let manifest_path = notebook_path.as_deref().and_then(|p| {
                            crate::project_file::detect_project_file(p)
                                .filter(|d| {
                                    d.kind == crate::project_file::ProjectFileKind::PixiToml
                                })
                                .map(|d| d.path)
                        });

                        let launcher_module = if bootstrap_dx {
                            "nteract_kernel_launcher"
                        } else {
                            "ipykernel_launcher"
                        };
                        if let Some(ref manifest) = manifest_path {
                            match kernel_launch::tools::pixi_shell_hook(manifest, None).await {
                                Ok(env_vars) => {
                                    let python = env_vars
                                        .get("CONDA_PREFIX")
                                        .map(|p| {
                                            let prefix = std::path::PathBuf::from(p);
                                            if cfg!(windows) {
                                                prefix.join("python.exe")
                                            } else {
                                                prefix.join("bin").join("python")
                                            }
                                        })
                                        .unwrap_or_else(|| std::path::PathBuf::from("python"));
                                    info!(
                                        "[jupyter-kernel] Starting Python kernel via pixi shell-hook ({})",
                                        python.display()
                                    );
                                    let mut cmd = tokio::process::Command::new(&python);
                                    cmd.envs(&env_vars);
                                    if let Some(parent) = manifest.parent() {
                                        cmd.current_dir(parent);
                                    }
                                    cmd.args(["-Xfrozen_modules=off", "-m", launcher_module, "-f"]);
                                    cmd.arg(&connection_file_path);
                                    cmd.stdout(Stdio::null());
                                    cmd.stderr(Stdio::piped());
                                    if bootstrap_dx {
                                        // pixi.toml envs belong to the user.
                                        // Installing nteract_kernel_launcher
                                        // into their env is a cleanup hazard,
                                        // so inject via PYTHONPATH — same
                                        // pattern as uv:pyproject,
                                        // conda:env_yml, and pixi exec.
                                        let dir =
                                            crate::launcher_cache::launcher_cache_dir().await?;
                                        cmd.env("PYTHONPATH", &dir);
                                    }
                                    cmd
                                }
                                Err(e) => {
                                    warn!(
                                        "[jupyter-kernel] pixi shell-hook failed ({}), falling back to pixi run",
                                        e
                                    );
                                    let pixi_path = kernel_launch::tools::get_pixi_path().await?;
                                    let mut cmd = tokio::process::Command::new(&pixi_path);
                                    cmd.args([
                                        "run",
                                        "python",
                                        "-Xfrozen_modules=off",
                                        "-m",
                                        launcher_module,
                                        "-f",
                                    ]);
                                    cmd.arg(&connection_file_path);
                                    cmd.stdout(Stdio::null());
                                    cmd.stderr(Stdio::piped());
                                    if let Some(parent) = manifest.parent() {
                                        cmd.current_dir(parent);
                                    }
                                    if bootstrap_dx {
                                        let dir =
                                            crate::launcher_cache::launcher_cache_dir().await?;
                                        cmd.env("PYTHONPATH", &dir);
                                    }
                                    cmd
                                }
                            }
                        } else {
                            let pixi_path = kernel_launch::tools::get_pixi_path().await?;
                            let mut cmd = tokio::process::Command::new(&pixi_path);
                            cmd.args([
                                "run",
                                "python",
                                "-Xfrozen_modules=off",
                                "-m",
                                launcher_module,
                                "-f",
                            ]);
                            cmd.arg(&connection_file_path);
                            cmd.stdout(Stdio::null());
                            cmd.stderr(Stdio::piped());
                            if let Some(ref nb_path) = notebook_path {
                                if let Some(parent) = nb_path.parent() {
                                    cmd.current_dir(parent);
                                }
                            }
                            if bootstrap_dx {
                                let dir = crate::launcher_cache::launcher_cache_dir().await?;
                                cmd.env("PYTHONPATH", &dir);
                            }
                            cmd
                        }
                    }
                    "pixi:inline" | "pixi:prewarmed" | "pixi:pep723" => {
                        let pixi_path = kernel_launch::tools::get_pixi_path().await?;
                        info!(
                            "[jupyter-kernel] Starting Python kernel with pixi exec (env_source: {})",
                            env_source
                        );
                        let launcher_module = if bootstrap_dx {
                            "nteract_kernel_launcher"
                        } else {
                            "ipykernel_launcher"
                        };
                        let mut cmd = tokio::process::Command::new(&pixi_path);
                        cmd.arg("exec");
                        for pkg in ["ipykernel", "ipywidgets", "anywidget", "nbformat"] {
                            cmd.args(["-w", pkg]);
                        }
                        if let Some(ref deps) = launched_config.pixi_deps {
                            for dep in deps {
                                cmd.args(["-w", dep]);
                            }
                        }
                        for pkg in &launched_config.prewarmed_packages {
                            cmd.args(["-w", pkg]);
                        }
                        cmd.args([
                            "python",
                            "-Xfrozen_modules=off",
                            "-m",
                            launcher_module,
                            "-f",
                        ]);
                        cmd.arg(&connection_file_path);
                        cmd.stdout(Stdio::null());
                        cmd.stderr(Stdio::piped());
                        if bootstrap_dx {
                            // pixi exec spins up an ephemeral env we don't
                            // vendor into; inject the daemon-side launcher via
                            // PYTHONPATH so `-m nteract_kernel_launcher`
                            // resolves without touching sys.path[0].
                            let dir = crate::launcher_cache::launcher_cache_dir().await?;
                            cmd.env("PYTHONPATH", &dir);
                        }
                        cmd
                    }
                    _ => {
                        // Prewarmed - use pooled environment
                        let pooled_env = env.ok_or_else(|| {
                            anyhow::anyhow!(
                                "Python kernel requires a pooled environment for env_source: {}",
                                env_source
                            )
                        })?;
                        info!(
                            "[jupyter-kernel] Starting Python kernel from env at {:?}",
                            pooled_env.python_path
                        );
                        // Every pool env (UV/conda/pixi) is vendored with the
                        // `nteract_kernel_launcher` package at creation + take
                        // time, so `-m nteract_kernel_launcher` resolves
                        // regardless of flavor when bootstrap_dx is on.
                        let launcher_module = if bootstrap_dx {
                            "nteract_kernel_launcher"
                        } else {
                            "ipykernel_launcher"
                        };
                        let mut cmd = tokio::process::Command::new(&pooled_env.python_path);
                        cmd.args(["-Xfrozen_modules=off", "-m", launcher_module, "-f"]);
                        cmd.arg(&connection_file_path);
                        cmd.stdout(Stdio::null());
                        cmd.stderr(Stdio::piped());
                        if bootstrap_dx {
                            // Pool envs are daemon-owned and vendored at
                            // creation/take time, but injecting the cached
                            // launcher keeps stale pools from failing with
                            // ModuleNotFoundError after an upgrade.
                            let dir = crate::launcher_cache::launcher_cache_dir().await?;
                            cmd.env("PYTHONPATH", &dir);
                        }

                        if pooled_env.env_type == EnvType::Uv {
                            cmd.env("VIRTUAL_ENV", &pooled_env.venv_path);
                            let uv_path = kernel_launch::tools::get_uv_path().await?;
                            if let Some(uv_dir) = uv_path.parent() {
                                cmd.env("PATH", prepend_to_path(uv_dir));
                            }
                        }

                        cmd
                    }
                }
            }
            "deno" => {
                let deno_path = kernel_launch::tools::get_deno_path().await?;
                info!("[jupyter-kernel] Starting Deno kernel with {:?}", deno_path);
                let mut cmd = tokio::process::Command::new(&deno_path);
                cmd.args(["jupyter", "--kernel", "--conn"]);
                cmd.arg(&connection_file_path);
                cmd.stdout(Stdio::null());
                cmd.stderr(Stdio::piped());
                cmd
            }
            _ => {
                return Err(anyhow::anyhow!(
                    "Unsupported kernel type: {}. Supported types: python, deno",
                    kernel_type
                ));
            }
        };
        cmd.current_dir(&cwd);

        // Set terminal size for consistent output formatting
        cmd.env("COLUMNS", TERMINAL_COLUMNS_STR);
        cmd.env("LINES", TERMINAL_LINES_STR);

        // Apply extra env vars from launch config
        for (key, value) in &config.env_vars {
            cmd.env(key, value);
        }

        // Signal dx bootstrap to the launcher module inside the kernel process.
        // The nteract_kernel_launcher reads RUNT_BOOTSTRAP_DX to decide whether
        // to append `import dx; dx.install()` to ipykernel's exec_lines.
        if bootstrap_dx {
            cmd.env("RUNT_BOOTSTRAP_DX", "1");
        }

        cmd.kill_on_drop(true);

        // Capture kernel stderr for diagnostics. Per-line logs go at debug
        // (or warn when the line looks error-shaped), but we also ring-buffer
        // the last N lines so the early-exit path can surface them in the
        // error message. Without this, users on stable (default warn) saw
        // only "exit status: 1" with no clue why the kernel died.
        const STDERR_BUFFER_LINES: usize = 50;

        type LaunchedKernel = (
            tokio::process::Child,
            Arc<StdMutex<VecDeque<String>>>,
            ConnectionInfo,
        );

        #[cfg(unix)]
        let use_ipc = kernel_type != "deno";

        #[cfg(unix)]
        let ipc_prefix = if use_ipc {
            crate::ensure_ipc_socket_dir()?;
            Some(ipc_path_prefix(&kernel_id))
        } else {
            None
        };

        let (mut process, _stderr_buffer, connection_info) = {
            #[cfg(unix)]
            if let Some(ref prefix) = ipc_prefix {
                let connection_info = ConnectionInfo {
                    transport: jupyter_protocol::connection_info::Transport::IPC,
                    ip: prefix.display().to_string(),
                    shell_port: 1,
                    iopub_port: 2,
                    stdin_port: 3,
                    control_port: 4,
                    hb_port: 5,
                    signature_scheme: "hmac-sha256".to_string(),
                    key: Uuid::new_v4().to_string(),
                    kernel_name: Some(kernelspec_name.to_string()),
                };
                tokio::fs::write(
                    &connection_file_path,
                    serde_json::to_string_pretty(&connection_info)?,
                )
                .await?;

                let mut process = cmd.spawn()?;
                let stderr_buffer: Arc<StdMutex<VecDeque<String>>> =
                    Arc::new(StdMutex::new(VecDeque::with_capacity(STDERR_BUFFER_LINES)));
                let stderr_drain: Option<JoinHandle<()>> =
                    if let Some(stderr) = process.stderr.take() {
                        let kid = kernel_id.clone();
                        let buffer = stderr_buffer.clone();
                        Some(spawn_best_effort("kernel-stderr", async move {
                            use tokio::io::{AsyncBufReadExt, BufReader};
                            let mut lines = BufReader::new(stderr).lines();
                            while let Ok(Some(line)) = lines.next_line().await {
                                let lower = line.to_ascii_lowercase();
                                if lower.contains("error") || lower.contains("traceback") {
                                    warn!("[kernel-stderr:{}] {}", kid, line);
                                } else {
                                    debug!("[kernel-stderr:{}] {}", kid, line);
                                }
                                let mut queue = buffer.lock().unwrap();
                                if queue.len() == STDERR_BUFFER_LINES {
                                    queue.pop_front();
                                }
                                queue.push_back(line);
                            }
                        }))
                    } else {
                        None
                    };

                info!(
                    "[jupyter-kernel] Spawned kernel process (pid={:?}, kernel_id={}, transport=ipc, prefix={})",
                    process.id(),
                    kernel_id,
                    prefix.display(),
                );

                const STARTUP_CEILING: std::time::Duration = std::time::Duration::from_millis(3000);
                let startup_deadline = std::time::Instant::now() + STARTUP_CEILING;
                let mut early_exit: Option<std::process::ExitStatus> = None;
                let mut wait_err: Option<std::io::Error> = None;
                loop {
                    match process.try_wait() {
                        Ok(Some(status)) => {
                            early_exit = Some(status);
                            break;
                        }
                        Ok(None) => {
                            if std::time::Instant::now() >= startup_deadline {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                        Err(e) => {
                            wait_err = Some(e);
                            break;
                        }
                    }
                }

                match (early_exit, wait_err) {
                    (Some(exit_status), _) => {
                        if let Some(handle) = stderr_drain {
                            let _ =
                                tokio::time::timeout(std::time::Duration::from_millis(500), handle)
                                    .await;
                        }
                        let captured = {
                            let queue = stderr_buffer.lock().unwrap();
                            queue.iter().cloned().collect::<Vec<_>>().join("\n")
                        };
                        let stderr_tail = if captured.is_empty() {
                            "(no stderr captured before exit)".to_string()
                        } else {
                            format!("stderr tail:\n{}", captured)
                        };
                        cleanup_ipc_sockets(prefix);
                        error!(
                            "[jupyter-kernel] Kernel process exited immediately: {} (kernel_id={})\n{}",
                            exit_status, kernel_id, stderr_tail
                        );
                        return Err(anyhow::anyhow!(
                            "Kernel process exited immediately: {}\n{}",
                            exit_status,
                            stderr_tail
                        ));
                    }
                    (None, Some(e)) => {
                        warn!(
                            "[jupyter-kernel] Could not check kernel process status: {}",
                            e
                        );
                        (process, stderr_buffer, connection_info) as LaunchedKernel
                    }
                    (None, None) => (process, stderr_buffer, connection_info) as LaunchedKernel,
                }
            } else {
                let kernel_ports = config.kernel_ports;
                let ports = [
                    kernel_ports.stdin,
                    kernel_ports.control,
                    kernel_ports.hb,
                    kernel_ports.shell,
                    kernel_ports.iopub,
                ];
                let connection_info = ConnectionInfo {
                    transport: jupyter_protocol::connection_info::Transport::TCP,
                    ip: ip.to_string(),
                    stdin_port: kernel_ports.stdin,
                    control_port: kernel_ports.control,
                    hb_port: kernel_ports.hb,
                    shell_port: kernel_ports.shell,
                    iopub_port: kernel_ports.iopub,
                    signature_scheme: "hmac-sha256".to_string(),
                    key: Uuid::new_v4().to_string(),
                    kernel_name: Some(kernelspec_name.to_string()),
                };
                tokio::fs::write(
                    &connection_file_path,
                    serde_json::to_string_pretty(&connection_info)?,
                )
                .await?;

                let listeners = bind_kernel_port_listeners(ip, kernel_ports).await?;
                #[cfg(windows)]
                drop(listeners);
                let mut process = cmd.spawn()?;
                #[cfg(not(windows))]
                drop(listeners);

                let stderr_buffer: Arc<StdMutex<VecDeque<String>>> =
                    Arc::new(StdMutex::new(VecDeque::with_capacity(STDERR_BUFFER_LINES)));
                let stderr_drain: Option<JoinHandle<()>> =
                    if let Some(stderr) = process.stderr.take() {
                        let kid = kernel_id.clone();
                        let buffer = stderr_buffer.clone();
                        Some(spawn_best_effort("kernel-stderr", async move {
                            use tokio::io::{AsyncBufReadExt, BufReader};
                            let mut lines = BufReader::new(stderr).lines();
                            while let Ok(Some(line)) = lines.next_line().await {
                                let lower = line.to_ascii_lowercase();
                                if lower.contains("error") || lower.contains("traceback") {
                                    warn!("[kernel-stderr:{}] {}", kid, line);
                                } else {
                                    debug!("[kernel-stderr:{}] {}", kid, line);
                                }
                                let mut queue = buffer.lock().unwrap();
                                if queue.len() == STDERR_BUFFER_LINES {
                                    queue.pop_front();
                                }
                                queue.push_back(line);
                            }
                        }))
                    } else {
                        None
                    };

                info!(
                    "[jupyter-kernel] Spawned kernel process (pid={:?}, kernel_id={}, transport=tcp, ports={:?})",
                    process.id(),
                    kernel_id,
                    ports
                );

                const STARTUP_CEILING: std::time::Duration = std::time::Duration::from_millis(3000);
                let startup_deadline = std::time::Instant::now() + STARTUP_CEILING;
                let mut early_exit: Option<std::process::ExitStatus> = None;
                let mut wait_err: Option<std::io::Error> = None;
                loop {
                    match process.try_wait() {
                        Ok(Some(status)) => {
                            early_exit = Some(status);
                            break;
                        }
                        Ok(None) => {
                            if std::time::Instant::now() >= startup_deadline {
                                break;
                            }
                            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                        }
                        Err(e) => {
                            wait_err = Some(e);
                            break;
                        }
                    }
                }

                match (early_exit, wait_err) {
                    (Some(exit_status), _) => {
                        if let Some(handle) = stderr_drain {
                            let _ =
                                tokio::time::timeout(std::time::Duration::from_millis(500), handle)
                                    .await;
                        }
                        let captured = {
                            let queue = stderr_buffer.lock().unwrap();
                            queue.iter().cloned().collect::<Vec<_>>().join("\n")
                        };
                        let stderr_tail = if captured.is_empty() {
                            "(no stderr captured before exit)".to_string()
                        } else {
                            format!("stderr tail:\n{}", captured)
                        };
                        error!(
                            "[jupyter-kernel] Kernel process exited immediately: {} (kernel_id={})\n{}",
                            exit_status, kernel_id, stderr_tail
                        );
                        return Err(anyhow::anyhow!(
                            "Kernel process exited immediately: {}\n{}",
                            exit_status,
                            stderr_tail
                        ));
                    }
                    (None, Some(e)) => {
                        warn!(
                            "[jupyter-kernel] Could not check kernel process status: {}",
                            e
                        );
                        (process, stderr_buffer, connection_info) as LaunchedKernel
                    }
                    (None, None) => (process, stderr_buffer, connection_info) as LaunchedKernel,
                }
            }
        };

        #[cfg(unix)]
        let kernel_pid = process.id().map(|pid| pid as i32);

        // Fresh session_id for ZMQ connections
        let session_id = Uuid::new_v4().to_string();
        let kernel_actor_id = format!("rt:kernel:{}", &session_id[..8]);

        // ── ZMQ connections and background tasks ─────────────────────────

        #[cfg(unix)]
        if connection_info.transport == jupyter_protocol::connection_info::Transport::IPC {
            let socket_paths: Vec<String> = [
                connection_info.shell_port,
                connection_info.iopub_port,
                connection_info.stdin_port,
                connection_info.control_port,
                connection_info.hb_port,
            ]
            .iter()
            .map(|port| format!("{}-{}", connection_info.ip, port))
            .collect();

            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if socket_paths
                    .iter()
                    .all(|p| std::path::Path::new(p).exists())
                {
                    debug!(
                        "[jupyter-kernel] All 5 IPC sockets ready (prefix={})",
                        connection_info.ip
                    );
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    let missing: Vec<_> = socket_paths
                        .iter()
                        .filter(|p| !std::path::Path::new(p.as_str()).exists())
                        .collect();
                    if let Some(ref prefix) = ipc_prefix {
                        cleanup_ipc_sockets(prefix);
                    }
                    return Err(anyhow::anyhow!(
                        "Kernel did not create IPC sockets within 30s. Missing: {:?}",
                        missing
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }

        // Create iopub connection and spawn listener
        let mut iopub =
            runtimelib::create_client_iopub_connection(&connection_info, "", &session_id).await?;

        // Create command channel for queue processing
        let (cmd_tx, cmd_rx) = mpsc::channel::<QueueCommand>(100);

        // Shared state refs for spawned tasks
        let cell_id_map: Arc<StdMutex<HashMap<String, String>>> =
            Arc::new(StdMutex::new(HashMap::new()));
        let comm_seq = Arc::new(AtomicU64::new(0));
        let pending_history: Arc<StdMutex<HashMap<String, oneshot::Sender<Vec<HistoryEntry>>>>> =
            Arc::new(StdMutex::new(HashMap::new()));
        let pending_completions: PendingCompletions = Arc::new(StdMutex::new(HashMap::new()));
        let stream_terminals = Arc::new(tokio::sync::Mutex::new(StreamTerminals::new()));

        // Spawn process watcher — detects process exit and signals via oneshot
        let process_cmd_tx = cmd_tx.clone();
        let panic_cmd_tx = cmd_tx.clone();
        let (died_tx, died_rx) = tokio::sync::oneshot::channel::<String>();
        let process_watcher_task = spawn_supervised(
            "process-watcher",
            async move {
                let status = process.wait().await;
                let msg = match status {
                    Ok(exit_status) => {
                        warn!("[jupyter-kernel] Kernel process exited: {}", exit_status);
                        format!("Kernel process exited: {}", exit_status)
                    }
                    Err(e) => {
                        error!("[jupyter-kernel] Error waiting for kernel process: {}", e);
                        format!("Error waiting for kernel process: {}", e)
                    }
                };
                let _ = died_tx.send(msg);
                let _ = process_cmd_tx.try_send(QueueCommand::KernelDied);
            },
            move |_| {
                let _ = panic_cmd_tx.try_send(QueueCommand::KernelDied);
            },
        );

        // ── IOPub listener task ──────────────────────────────────────────

        let broadcast_tx = shared.broadcast_tx.clone();
        let iopub_cell_id_map = cell_id_map.clone();
        let iopub_cmd_tx = cmd_tx.clone();
        let blob_store = shared.blob_store.clone();
        let iopub_comm_seq = comm_seq.clone();
        let iopub_stream_terminals = stream_terminals.clone();
        let state_for_iopub = shared.state.clone();
        // Stable per-task actor IDs. Each long-running task (iopub, shell,
        // coalesce) processes messages sequentially within its own loop, so
        // one actor per task is sufficient — Automerge's `(actor, seq)`
        // invariant holds as long as no two concurrent forks share an actor.
        // The widget-echo filter matches on the `rt:kernel:` prefix, which
        // all three still carry.
        let iopub_actor_id = format!("{kernel_actor_id}:iopub");

        // Create coalescing channel early so the IOPub task can capture the sender.
        let (coalesce_tx, coalesce_rx) = mpsc::unbounded_channel::<(String, serde_json::Value)>();
        let comm_coalesce_tx_for_iopub = Some(coalesce_tx.clone());

        let iopub_panic_cmd_tx = cmd_tx.clone();
        let iopub_task = spawn_supervised(
            "iopub",
            async move {
                // Track Output widgets with pending clear_output(wait=true).
                let mut pending_clear_widgets: std::collections::HashSet<String> =
                    std::collections::HashSet::new();

                // Capture routing cache: msg_id -> comm_id.
                let mut capture_cache: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();

                // Local comm_id -> target_name map, populated on comm_open and
                // removed on comm_close. Used by the dx comm filter in CommMsg
                // and CommClose arms so we can route without a CRDT read on the
                // hot path.
                let mut comm_targets: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();

                let comm_coalesce_tx = comm_coalesce_tx_for_iopub;

                loop {
                    match iopub.read().await {
                        Ok(message) => {
                            let iopub_start = std::time::Instant::now();
                            let msg_type = message.header.msg_type.clone();
                            debug!(
                                "[iopub] type={} parent_msg_id={:?}",
                                msg_type,
                                message.parent_header.as_ref().map(|h| &h.msg_id)
                            );

                            // parent_header.msg_id IS the execution_id (set in execute()).
                            // Look up cell_id from the execution_id -> cell_id map.
                            let execution_id =
                                message.parent_header.as_ref().map(|h| h.msg_id.clone());
                            let cell_id = execution_id
                                .as_ref()
                                .and_then(|eid| iopub_cell_id_map.lock().ok()?.get(eid).cloned());

                            // Handle different message types
                            match &message.content {
                                JupyterMessageContent::Status(status) => {
                                    // Map the kernel's execution_state to either a typed
                                    // activity flip (Idle/Busy — hot path, throttled) or a
                                    // lifecycle transition (Starting/Restarting/Terminating/Dead).
                                    let update = match status.execution_state {
                                        jupyter_protocol::ExecutionState::Busy => {
                                            Some(IoPubStateUpdate::Activity(KernelActivity::Busy))
                                        }
                                        jupyter_protocol::ExecutionState::Idle => {
                                            Some(IoPubStateUpdate::Activity(KernelActivity::Idle))
                                        }
                                        // Starting and Restarting both land in Connecting —
                                        // the kernel process has just come up and is
                                        // reporting its first status; we treat those as
                                        // "connected but pre-kernel_info" transitions.
                                        jupyter_protocol::ExecutionState::Starting
                                        | jupyter_protocol::ExecutionState::Restarting => {
                                            Some(IoPubStateUpdate::Lifecycle(
                                                RuntimeLifecycle::Connecting,
                                            ))
                                        }
                                        jupyter_protocol::ExecutionState::Terminating
                                        | jupyter_protocol::ExecutionState::Dead => Some(
                                            IoPubStateUpdate::Lifecycle(RuntimeLifecycle::Shutdown),
                                        ),
                                        _ => None,
                                    };

                                    if let Some(update) = update {
                                        // Non-execute messages (kernel_info, completions) have a
                                        // parent_header.msg_id that isn't in our execute map.
                                        // cell_id is None for those — treat activity flips from
                                        // those as transient (they don't reflect user code state).
                                        let is_transient_activity = cell_id.is_none()
                                            && matches!(update, IoPubStateUpdate::Activity(_));

                                        if !is_transient_activity {
                                            let result =
                                                state_for_iopub.with_doc(|sd| match update {
                                                    IoPubStateUpdate::Activity(a) => {
                                                        sd.set_activity(a)
                                                    }
                                                    IoPubStateUpdate::Lifecycle(lc) => {
                                                        sd.set_lifecycle(&lc)
                                                    }
                                                });
                                            if let Err(e) = result {
                                                warn!("[runtime-state] {}", e);
                                            }
                                        }
                                    }

                                    if status.execution_state
                                        == jupyter_protocol::ExecutionState::Idle
                                    {
                                        if let (Some(cid), Some(eid)) =
                                            (cell_id.clone(), execution_id.clone())
                                        {
                                            let _ = iopub_cmd_tx.try_send(
                                                QueueCommand::ExecutionDone {
                                                    cell_id: cid,
                                                    execution_id: eid,
                                                },
                                            );
                                        }
                                    }
                                }

                                JupyterMessageContent::ExecuteInput(input) => {
                                    if let Some(_cid) = cell_id {
                                        let execution_count = input.execution_count.0 as i64;

                                        if let Some(ref eid) = execution_id {
                                            if let Err(e) = state_for_iopub.with_doc(|sd| {
                                                sd.set_execution_count(eid, execution_count)
                                            }) {
                                                warn!("[runtime-state] {}", e);
                                            }
                                        }
                                    }
                                }

                                JupyterMessageContent::StreamContent(stream) => {
                                    // Check if this output should go to an Output widget
                                    let parent_msg_id = message
                                        .parent_header
                                        .as_ref()
                                        .map(|h| h.msg_id.as_str())
                                        .unwrap_or("");
                                    if let Some(widget_comm_id) =
                                        capture_cache.get(parent_msg_id).cloned()
                                    {
                                        let stream_name = match stream.name {
                                            jupyter_protocol::Stdio::Stdout => "stdout",
                                            jupyter_protocol::Stdio::Stderr => "stderr",
                                        };
                                        let output = serde_json::json!({
                                            "output_type": "stream",
                                            "name": stream_name,
                                            "text": stream.text
                                        });

                                        if let Ok(manifest) = crate::output_store::create_manifest(
                                            &output,
                                            &blob_store,
                                            crate::output_store::DEFAULT_INLINE_THRESHOLD,
                                        )
                                        .await
                                        {
                                            let manifest_json = manifest.to_json();
                                            let need_clear =
                                                pending_clear_widgets.remove(&widget_comm_id);
                                            let output_manifests = state_for_iopub
                                                .with_doc(|sd| {
                                                    if need_clear {
                                                        if let Err(e) =
                                                            sd.clear_comm_outputs(&widget_comm_id)
                                                        {
                                                            warn!("[runtime-state] {}", e);
                                                        }
                                                    }
                                                    if let Err(e) = sd.append_comm_output(
                                                        &widget_comm_id,
                                                        &manifest_json,
                                                    ) {
                                                        warn!("[runtime-state] {}", e);
                                                    }
                                                    Ok(
                                                        if let Some(entry) =
                                                            sd.get_comm(&widget_comm_id)
                                                        {
                                                            let manifests = entry.outputs.clone();
                                                            let manifests_json =
                                                                serde_json::Value::Array(
                                                                    manifests.clone(),
                                                                );
                                                            if let Err(e) = sd
                                                                .set_comm_state_property(
                                                                    &widget_comm_id,
                                                                    "outputs",
                                                                    &manifests_json,
                                                                )
                                                            {
                                                                warn!("[runtime-state] {}", e);
                                                            }
                                                            Some(manifests)
                                                        } else {
                                                            None
                                                        },
                                                    )
                                                })
                                                .ok()
                                                .flatten();
                                            if let Some(output_manifests) = output_manifests {
                                                let mut resolved_outputs = Vec::new();
                                                for m in &output_manifests {
                                                    if let Ok(manifest) =
                                                        serde_json::from_value::<OutputManifest>(
                                                            m.clone(),
                                                        )
                                                    {
                                                        if let Ok(resolved) =
                                                            output_store::resolve_manifest(
                                                                &manifest,
                                                                &blob_store,
                                                            )
                                                            .await
                                                        {
                                                            resolved_outputs.push(resolved);
                                                        }
                                                    }
                                                }
                                                let _ = iopub_cmd_tx
                                                    .send(QueueCommand::SendCommUpdate {
                                                        comm_id: widget_comm_id.clone(),
                                                        state: serde_json::json!({
                                                            "outputs": resolved_outputs,
                                                        }),
                                                    })
                                                    .await;
                                            }
                                        }
                                        continue;
                                    }

                                    if let Some(_cid) = cell_id {
                                        let stream_name = match stream.name {
                                            jupyter_protocol::Stdio::Stdout => "stdout",
                                            jupyter_protocol::Stdio::Stderr => "stderr",
                                        };
                                        let eid = execution_id.clone().unwrap_or_default();

                                        let (rendered_text, known_state) = {
                                            let mut terminals = iopub_stream_terminals.lock().await;
                                            let text =
                                                terminals.feed(&eid, stream_name, &stream.text);
                                            let state = terminals
                                                .get_output_state(&eid, stream_name)
                                                .cloned();
                                            (text, state)
                                        };

                                        let nbformat_value = serde_json::json!({
                                            "output_type": "stream",
                                            "name": stream_name,
                                            "text": rendered_text
                                        });

                                        let manifest = match output_store::create_manifest(
                                            &nbformat_value,
                                            &blob_store,
                                            DEFAULT_INLINE_THRESHOLD,
                                        )
                                        .await
                                        {
                                            Ok(m) => m,
                                            Err(e) => {
                                                warn!(
                                                "[jupyter-kernel] Failed to create stream manifest: {}",
                                                e
                                            );
                                                continue;
                                            }
                                        };
                                        let manifest_json = manifest.to_json();

                                        let blob_hash =
                                            if let OutputManifest::Stream { ref text, .. } =
                                                manifest
                                            {
                                                match text {
                                                    ContentRef::Blob { blob, .. } => blob.clone(),
                                                    ContentRef::Inline { inline } => inline.clone(),
                                                }
                                            } else {
                                                String::new()
                                            };

                                        let mut fork = match state_for_iopub.fork(&iopub_actor_id) {
                                            Ok(f) => f,
                                            Err(e) => {
                                                warn!("[runtime-state] fork: {}", e);
                                                continue;
                                            }
                                        };

                                        let upsert_result = fork.upsert_stream_output(
                                            &eid,
                                            stream_name,
                                            &manifest_json,
                                            known_state.as_ref(),
                                        );

                                        let merge_ok = match state_for_iopub.merge(&mut fork) {
                                            Ok(()) => true,
                                            Err(e) => {
                                                warn!("[runtime-state] merge: {}", e);
                                                false
                                            }
                                        };

                                        if merge_ok {
                                            match &upsert_result {
                                                Ok((_updated, output_index)) => {
                                                    let mut terminals =
                                                        iopub_stream_terminals.lock().await;
                                                    terminals.set_output_state(
                                                        &eid,
                                                        stream_name,
                                                        StreamOutputState {
                                                            index: *output_index,
                                                            blob_hash: blob_hash.clone(),
                                                        },
                                                    );
                                                }
                                                Err(e) => {
                                                    warn!(
                                                    "[jupyter-kernel] Failed to upsert stream output: {}",
                                                    e
                                                );
                                                }
                                            }
                                        }
                                    }
                                }

                                JupyterMessageContent::DisplayData(_)
                                | JupyterMessageContent::ExecuteResult(_) => {
                                    let parent_msg_id = message
                                        .parent_header
                                        .as_ref()
                                        .map(|h| h.msg_id.as_str())
                                        .unwrap_or("");

                                    // Dx blob-ref buffer preflight: if the kernel emitted a
                                    // display_data carrying raw bytes as trailing ZMQ
                                    // buffer frames plus a BLOB_REF_MIME entry, write each
                                    // referenced buffer to the blob store before the
                                    // manifest is built.
                                    let iopub_buffers: Vec<Vec<u8>> =
                                        message.buffers.iter().map(|b| b.to_vec()).collect();

                                    if let Some(widget_comm_id) =
                                        capture_cache.get(parent_msg_id).cloned()
                                    {
                                        if let Some(nbformat_value) =
                                            message_content_to_nbformat(&message.content)
                                        {
                                            crate::output_store::preflight_ref_buffers(
                                                &nbformat_value,
                                                &iopub_buffers,
                                                &blob_store,
                                            )
                                            .await;
                                            if let Ok(manifest) =
                                                crate::output_store::create_manifest(
                                                    &nbformat_value,
                                                    &blob_store,
                                                    crate::output_store::DEFAULT_INLINE_THRESHOLD,
                                                )
                                                .await
                                            {
                                                let manifest_json = manifest.to_json();
                                                let need_clear =
                                                    pending_clear_widgets.remove(&widget_comm_id);
                                                let output_manifests = state_for_iopub
                                                    .with_doc(|sd| {
                                                        if need_clear {
                                                            if let Err(e) = sd
                                                                .clear_comm_outputs(&widget_comm_id)
                                                            {
                                                                warn!("[runtime-state] {}", e);
                                                            }
                                                        }
                                                        if let Err(e) = sd.append_comm_output(
                                                            &widget_comm_id,
                                                            &manifest_json,
                                                        ) {
                                                            warn!("[runtime-state] {}", e);
                                                        }
                                                        Ok(
                                                            if let Some(entry) =
                                                                sd.get_comm(&widget_comm_id)
                                                            {
                                                                let manifests =
                                                                    entry.outputs.clone();
                                                                let manifests_json =
                                                                    serde_json::Value::Array(
                                                                        manifests.clone(),
                                                                    );
                                                                if let Err(e) = sd
                                                                    .set_comm_state_property(
                                                                        &widget_comm_id,
                                                                        "outputs",
                                                                        &manifests_json,
                                                                    )
                                                                {
                                                                    warn!("[runtime-state] {}", e);
                                                                }
                                                                Some(manifests)
                                                            } else {
                                                                None
                                                            },
                                                        )
                                                    })
                                                    .ok()
                                                    .flatten();
                                                if let Some(output_manifests) = output_manifests {
                                                    let mut resolved_outputs = Vec::new();
                                                    for m in &output_manifests {
                                                        if let Ok(manifest) = serde_json::from_value::<
                                                            OutputManifest,
                                                        >(
                                                            m.clone()
                                                        ) {
                                                            if let Ok(r) =
                                                                output_store::resolve_manifest(
                                                                    &manifest,
                                                                    &blob_store,
                                                                )
                                                                .await
                                                            {
                                                                resolved_outputs.push(r);
                                                            }
                                                        }
                                                    }
                                                    let _ = iopub_cmd_tx
                                                        .send(QueueCommand::SendCommUpdate {
                                                            comm_id: widget_comm_id.clone(),
                                                            state: serde_json::json!({
                                                                "outputs": resolved_outputs
                                                            }),
                                                        })
                                                        .await;
                                                }
                                            }
                                        }
                                        continue;
                                    }

                                    if let Some(cid) = cell_id.as_ref() {
                                        let _output_type = match &message.content {
                                            JupyterMessageContent::DisplayData(_) => "display_data",
                                            JupyterMessageContent::ExecuteResult(_) => {
                                                "execute_result"
                                            }
                                            _ => "unknown",
                                        };
                                        let eid = execution_id.clone().unwrap_or_default();

                                        {
                                            let mut terminals = iopub_stream_terminals.lock().await;
                                            terminals.clear(&eid);
                                        }

                                        if let Some(nbformat_value) =
                                            message_content_to_nbformat(&message.content)
                                        {
                                            crate::output_store::preflight_ref_buffers(
                                                &nbformat_value,
                                                &iopub_buffers,
                                                &blob_store,
                                            )
                                            .await;
                                            let manifest_json = match output_store::create_manifest(
                                                &nbformat_value,
                                                &blob_store,
                                                DEFAULT_INLINE_THRESHOLD,
                                            )
                                            .await
                                            {
                                                Ok(manifest) => manifest.to_json(),
                                                Err(e) => {
                                                    warn!(
                                                        "[jupyter-kernel] Failed to create manifest: {}",
                                                        e
                                                    );
                                                    crate::notebook_sync_server::fallback_output_with_id(&nbformat_value)
                                                }
                                            };

                                            let mut fork =
                                                match state_for_iopub.fork(&iopub_actor_id) {
                                                    Ok(f) => f,
                                                    Err(e) => {
                                                        warn!("[runtime-state] fork: {}", e);
                                                        continue;
                                                    }
                                                };

                                            if let Err(e) = fork.append_output(&eid, &manifest_json)
                                            {
                                                warn!(
                                                "[jupyter-kernel] Failed to append output to state doc: {}",
                                                e
                                            );
                                            }

                                            if let Err(e) = state_for_iopub.merge(&mut fork) {
                                                warn!("[runtime-state] merge: {}", e);
                                            }
                                        }

                                        // Rich-traceback detection. A display_data or
                                        // execute_result carrying TRACEBACK_MIME IS an error
                                        // semantically — the launcher short-circuits
                                        // `_showtraceback` and emits rich display_data instead
                                        // of classic ErrorOutput. Without this, the runtime
                                        // never flips `execution_had_error`, and
                                        // `Execution.result().success` comes back true for
                                        // failed cells. Route through the same CellError
                                        // command the classic arm uses.
                                        if matches!(
                                            crate::user_error::UserErrorOutput::from_iopub(
                                                &message.content
                                            ),
                                            Some(crate::user_error::UserErrorOutput::Rich(_))
                                        ) {
                                            let _ =
                                                iopub_cmd_tx.try_send(QueueCommand::CellError {
                                                    cell_id: cid.clone(),
                                                    execution_id: eid,
                                                });
                                        }
                                    }
                                }

                                JupyterMessageContent::UpdateDisplayData(update) => {
                                    if let Some(ref display_id) = update.transient.display_id {
                                        let mut fork = match state_for_iopub.fork(&iopub_actor_id) {
                                            Ok(f) => f,
                                            Err(e) => {
                                                warn!("[runtime-state] fork: {}", e);
                                                continue;
                                            }
                                        };

                                        let data_value =
                                            serde_json::to_value(&update.data).unwrap_or_default();

                                        // Preflight ZMQ buffers keyed by blob-ref MIME into
                                        // the blob store — mirror the initial-display path.
                                        // Without this, `DisplayHandle.update(df2)` carries
                                        // buffers the kernel still holds but the daemon never
                                        // persists, and the blob-ref promotion downstream
                                        // would drop the entry on `BlobStore::exists` miss.
                                        let iopub_buffers: Vec<Vec<u8>> =
                                            message.buffers.iter().map(|b| b.to_vec()).collect();
                                        let preflight_wrapper =
                                            serde_json::json!({ "data": data_value });
                                        crate::output_store::preflight_ref_buffers(
                                            &preflight_wrapper,
                                            &iopub_buffers,
                                            &blob_store,
                                        )
                                        .await;

                                        let updated = update_output_by_display_id_with_manifests(
                                            &mut fork,
                                            display_id,
                                            &data_value,
                                            &update.metadata,
                                            &blob_store,
                                        )
                                        .await;

                                        match updated {
                                            Ok(true) => {
                                                if let Err(e) = state_for_iopub.merge(&mut fork) {
                                                    warn!("[runtime-state] merge: {}", e);
                                                } else {
                                                    debug!(
                                                        "[jupyter-kernel] Updated display_id={}",
                                                        display_id
                                                    );
                                                }
                                            }
                                            Ok(false) => {
                                                error!(
                                                    "[jupyter-kernel] No output found for display_id={}",
                                                    display_id
                                                );
                                            }
                                            Err(e) => {
                                                error!(
                                                    "[jupyter-kernel] Failed to update display: {}",
                                                    e
                                                );
                                            }
                                        }
                                    }
                                }

                                JupyterMessageContent::ErrorOutput(_) => {
                                    let parent_msg_id = message
                                        .parent_header
                                        .as_ref()
                                        .map(|h| h.msg_id.as_str())
                                        .unwrap_or("");
                                    if let Some(widget_comm_id) =
                                        capture_cache.get(parent_msg_id).cloned()
                                    {
                                        if let Some(nbformat_value) =
                                            message_content_to_nbformat(&message.content)
                                        {
                                            if let Ok(manifest) =
                                                crate::output_store::create_manifest(
                                                    &nbformat_value,
                                                    &blob_store,
                                                    crate::output_store::DEFAULT_INLINE_THRESHOLD,
                                                )
                                                .await
                                            {
                                                let manifest_json = manifest.to_json();
                                                let need_clear =
                                                    pending_clear_widgets.remove(&widget_comm_id);
                                                let output_manifests = state_for_iopub
                                                    .with_doc(|sd| {
                                                        if need_clear {
                                                            if let Err(e) = sd
                                                                .clear_comm_outputs(&widget_comm_id)
                                                            {
                                                                warn!("[runtime-state] {}", e);
                                                            }
                                                        }
                                                        if let Err(e) = sd.append_comm_output(
                                                            &widget_comm_id,
                                                            &manifest_json,
                                                        ) {
                                                            warn!("[runtime-state] {}", e);
                                                        }
                                                        Ok(
                                                            if let Some(entry) =
                                                                sd.get_comm(&widget_comm_id)
                                                            {
                                                                let manifests =
                                                                    entry.outputs.clone();
                                                                let manifests_json =
                                                                    serde_json::Value::Array(
                                                                        manifests.clone(),
                                                                    );
                                                                if let Err(e) = sd
                                                                    .set_comm_state_property(
                                                                        &widget_comm_id,
                                                                        "outputs",
                                                                        &manifests_json,
                                                                    )
                                                                {
                                                                    warn!("[runtime-state] {}", e);
                                                                }
                                                                Some(manifests)
                                                            } else {
                                                                None
                                                            },
                                                        )
                                                    })
                                                    .ok()
                                                    .flatten();
                                                if let Some(output_manifests) = output_manifests {
                                                    let mut resolved_outputs = Vec::new();
                                                    for m in &output_manifests {
                                                        if let Ok(manifest) = serde_json::from_value::<
                                                            OutputManifest,
                                                        >(
                                                            m.clone()
                                                        ) {
                                                            if let Ok(r) =
                                                                output_store::resolve_manifest(
                                                                    &manifest,
                                                                    &blob_store,
                                                                )
                                                                .await
                                                            {
                                                                resolved_outputs.push(r);
                                                            }
                                                        }
                                                    }
                                                    let _ = iopub_cmd_tx
                                                        .send(QueueCommand::SendCommUpdate {
                                                            comm_id: widget_comm_id.clone(),
                                                            state: serde_json::json!({
                                                                "outputs": resolved_outputs
                                                            }),
                                                        })
                                                        .await;
                                                }
                                            }
                                        }
                                        continue;
                                    }

                                    if let Some(ref cid) = cell_id {
                                        let eid = execution_id.clone().unwrap_or_default();

                                        {
                                            let mut terminals = iopub_stream_terminals.lock().await;
                                            terminals.clear(&eid);
                                        }

                                        if let Some(nbformat_value) =
                                            message_content_to_nbformat(&message.content)
                                        {
                                            let manifest_json = match output_store::create_manifest(
                                                &nbformat_value,
                                                &blob_store,
                                                DEFAULT_INLINE_THRESHOLD,
                                            )
                                            .await
                                            {
                                                Ok(manifest) => manifest.to_json(),
                                                Err(e) => {
                                                    warn!(
                                                        "[jupyter-kernel] Failed to create error manifest: {}",
                                                        e
                                                    );
                                                    crate::notebook_sync_server::fallback_output_with_id(&nbformat_value)
                                                }
                                            };

                                            let mut fork =
                                                match state_for_iopub.fork(&iopub_actor_id) {
                                                    Ok(f) => f,
                                                    Err(e) => {
                                                        warn!("[runtime-state] fork: {}", e);
                                                        continue;
                                                    }
                                                };

                                            if let Err(e) = fork.append_output(&eid, &manifest_json)
                                            {
                                                warn!(
                                                "[jupyter-kernel] Failed to append error output to state doc: {}",
                                                e
                                            );
                                            }

                                            if let Err(e) = state_for_iopub.merge(&mut fork) {
                                                warn!("[runtime-state] merge: {}", e);
                                            }
                                        }

                                        let _ = iopub_cmd_tx.try_send(QueueCommand::CellError {
                                            cell_id: cid.clone(),
                                            execution_id: eid,
                                        });
                                    }
                                }

                                JupyterMessageContent::ClearOutput(clear) => {
                                    let parent_msg_id = message
                                        .parent_header
                                        .as_ref()
                                        .map(|h| h.msg_id.as_str())
                                        .unwrap_or("");
                                    if let Some(widget_comm_id) =
                                        capture_cache.get(parent_msg_id).cloned()
                                    {
                                        if clear.wait {
                                            pending_clear_widgets.insert(widget_comm_id.clone());
                                        } else {
                                            pending_clear_widgets.remove(&widget_comm_id);
                                            if let Err(e) = state_for_iopub.with_doc(|sd| {
                                                sd.clear_comm_outputs(&widget_comm_id)?;
                                                sd.set_comm_state_property(
                                                    &widget_comm_id,
                                                    "outputs",
                                                    &serde_json::json!([]),
                                                )?;
                                                Ok(())
                                            }) {
                                                warn!("[runtime-state] {}", e);
                                            }
                                            let _ = iopub_cmd_tx
                                                .send(QueueCommand::SendCommUpdate {
                                                    comm_id: widget_comm_id.clone(),
                                                    state: serde_json::json!({ "outputs": [] }),
                                                })
                                                .await;
                                        }
                                    }
                                }

                                JupyterMessageContent::CommOpen(open) => {
                                    // Record the comm_id -> target_name mapping early so
                                    // subsequent CommMsg / CommClose arms can route without
                                    // reading the CRDT.
                                    comm_targets
                                        .insert(open.comm_id.0.clone(), open.target_name.clone());

                                    // Short-circuit reserved nteract.dx.* comms: they carry
                                    // kernel-side protocol traffic (dx blob uploads, future
                                    // dx.query / dx.stream) that must NOT land in
                                    // RuntimeStateDoc::comms. The payload is handled in the
                                    // CommMsg arm below.
                                    if crate::dx_blob_comm::is_dx_target(&open.target_name) {
                                        debug!(
                                            "[dx] comm_open comm_id={} target={} (not persisted)",
                                            open.comm_id.0, open.target_name
                                        );
                                        continue;
                                    }

                                    let buffers: Vec<Vec<u8>> =
                                        message.buffers.iter().map(|b| b.to_vec()).collect();

                                    let data = serde_json::to_value(&open.data).unwrap_or_default();

                                    let comm_open_start = std::time::Instant::now();
                                    let state_json_size =
                                        serde_json::to_string(&data).map(|s| s.len()).unwrap_or(0);
                                    debug!(
                                        "[comm_open] comm_id={} target={} state_size={} bytes",
                                        open.comm_id.0, open.target_name, state_json_size
                                    );

                                    let empty_obj = serde_json::json!({});
                                    let state = data.get("state").unwrap_or(&empty_obj);
                                    let buffer_paths = extract_buffer_paths(&data);
                                    let (state_with_blobs, _used_paths) = store_widget_buffers(
                                        state,
                                        &buffer_paths,
                                        &buffers,
                                        &blob_store,
                                    )
                                    .await;

                                    let blob_elapsed = comm_open_start.elapsed();
                                    if blob_elapsed > std::time::Duration::from_millis(10) {
                                        warn!(
                                        "[iopub-timing] comm_open blob store took {:?} for comm_id={}",
                                        blob_elapsed, open.comm_id.0
                                    );
                                    }

                                    let state_with_blobs = blob_store_large_state_values(
                                        &state_with_blobs,
                                        &blob_store,
                                    )
                                    .await;

                                    {
                                        let lock_start = std::time::Instant::now();
                                        let model_module = state_with_blobs
                                            .get("_model_module")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let model_name = state_with_blobs
                                            .get("_model_name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        let seq = iopub_comm_seq.fetch_add(1, Ordering::Relaxed);
                                        let lock_wait = lock_start.elapsed();
                                        if lock_wait > std::time::Duration::from_millis(5) {
                                            warn!(
                                            "[iopub-timing] comm_open state handle lock waited {:?} for comm_id={}",
                                            lock_wait, open.comm_id.0
                                        );
                                        }
                                        let crdt_start = std::time::Instant::now();
                                        // Extract capture msg_id before with_doc so we can
                                        // update the local cache outside the lock.
                                        let capture_msg = if model_name == "OutputModel" {
                                            state_with_blobs
                                                .get("msg_id")
                                                .and_then(|v| v.as_str())
                                                .filter(|s| !s.is_empty())
                                                .map(|s| s.to_string())
                                        } else {
                                            None
                                        };
                                        if let Err(e) = state_for_iopub.with_doc(|sd| {
                                            sd.put_comm(
                                                &open.comm_id.0,
                                                &open.target_name,
                                                model_module,
                                                model_name,
                                                &state_with_blobs,
                                                seq,
                                            )?;
                                            if let Some(ref msg_id) = capture_msg {
                                                sd.set_comm_capture_msg_id(
                                                    &open.comm_id.0,
                                                    msg_id,
                                                )?;
                                            }
                                            Ok(())
                                        }) {
                                            warn!("[runtime-state] {}", e);
                                        }
                                        let crdt_elapsed = crdt_start.elapsed();
                                        if crdt_elapsed > std::time::Duration::from_millis(10) {
                                            warn!(
                                            "[iopub-timing] comm_open put_comm CRDT write took {:?} for comm_id={}, state_size={} bytes",
                                            crdt_elapsed, open.comm_id.0, state_json_size
                                        );
                                        }
                                        if let Some(msg_id) = capture_msg {
                                            capture_cache.insert(msg_id, open.comm_id.0.clone());
                                        }
                                    }

                                    let total = comm_open_start.elapsed();
                                    if total > std::time::Duration::from_millis(50) {
                                        warn!(
                                        "[iopub-timing] comm_open TOTAL {:?} for comm_id={} target={} state_size={} bytes",
                                        total, open.comm_id.0, open.target_name, state_json_size
                                    );
                                    }
                                }

                                JupyterMessageContent::CommMsg(msg) => {
                                    let content =
                                        serde_json::to_value(&message.content).unwrap_or_default();
                                    let buffers: Vec<Vec<u8>> =
                                        message.buffers.iter().map(|b| b.to_vec()).collect();

                                    let data = serde_json::to_value(&msg.data).unwrap_or_default();
                                    let method = data.get("method").and_then(|m| m.as_str());

                                    // dx-namespace comms short-circuit before any widget
                                    // state handling. v1 has no live handler — all reserved
                                    // nteract.dx.* targets drop with a warn log carrying the
                                    // raw target name for observability (future kernels
                                    // opening reserved targets we haven't implemented yet).
                                    if let Some(target) = comm_targets.get(&msg.comm_id.0).cloned()
                                    {
                                        if let Some(crate::dx_blob_comm::DxTarget::Unknown(raw)) =
                                            crate::dx_blob_comm::classify_dx_target(&target)
                                        {
                                            warn!(
                                            "[dx] comm_msg on reserved target {} (dropped; filtered from RuntimeStateDoc)",
                                            raw
                                        );
                                            continue;
                                        }
                                    }

                                    let comm_msg_start = std::time::Instant::now();
                                    debug!(
                                        "[comm_msg] comm_id={} method={:?}",
                                        msg.comm_id.0, method
                                    );
                                    if method == Some("update") {
                                        if let Some(state_delta) = data.get("state") {
                                            if let Some(new_msg_id) =
                                                state_delta.get("msg_id").and_then(|v| v.as_str())
                                            {
                                                capture_cache
                                                    .retain(|_, cid| cid != &msg.comm_id.0);
                                                if !new_msg_id.is_empty() {
                                                    if let Some(existing) =
                                                        capture_cache.get(new_msg_id)
                                                    {
                                                        warn!(
                                                        "[comm_msg] Nested capture: {} overrides {} for msg_id={}",
                                                        msg.comm_id.0, existing, new_msg_id
                                                    );
                                                    }
                                                    capture_cache.insert(
                                                        new_msg_id.to_string(),
                                                        msg.comm_id.0.clone(),
                                                    );
                                                }

                                                if let Err(e) = state_for_iopub.with_doc(|sd| {
                                                    sd.set_comm_capture_msg_id(
                                                        &msg.comm_id.0,
                                                        new_msg_id,
                                                    )
                                                }) {
                                                    warn!("[runtime-state] {}", e);
                                                }
                                            }

                                            let coalesce_delta = if !buffers.is_empty() {
                                                let buffer_paths = extract_buffer_paths(&data);
                                                let (state_with_blobs, _) = store_widget_buffers(
                                                    state_delta,
                                                    &buffer_paths,
                                                    &buffers,
                                                    &blob_store,
                                                )
                                                .await;
                                                state_with_blobs
                                            } else {
                                                state_delta.clone()
                                            };
                                            if let Some(ref tx) = comm_coalesce_tx {
                                                let _ = tx
                                                    .send((msg.comm_id.0.clone(), coalesce_delta));
                                            }
                                        }
                                    }

                                    let comm_msg_elapsed = comm_msg_start.elapsed();
                                    if comm_msg_elapsed > std::time::Duration::from_millis(10) {
                                        warn!(
                                        "[iopub-timing] comm_msg took {:?} for comm_id={} method={:?}",
                                        comm_msg_elapsed, msg.comm_id.0, method
                                    );
                                    }

                                    if method != Some("update") {
                                        let _ = broadcast_tx.send(NotebookBroadcast::Comm {
                                            msg_type: message.header.msg_type.clone(),
                                            content: content.clone(),
                                            buffers: buffers.clone(),
                                        });
                                    }
                                }

                                JupyterMessageContent::CommClose(close) => {
                                    debug!(
                                        "[jupyter-kernel] comm_close: comm_id={}",
                                        close.comm_id.0
                                    );

                                    let iopub_elapsed = iopub_start.elapsed();
                                    if iopub_elapsed > std::time::Duration::from_millis(50) {
                                        warn!(
                                            "[iopub-timing] message type={} took {:?} total",
                                            msg_type, iopub_elapsed
                                        );
                                    }

                                    // Drop the comm_id -> target_name mapping. If this was
                                    // a dx-namespace comm, skip the RuntimeStateDoc write.
                                    let was_dx_target = comm_targets
                                        .remove(&close.comm_id.0)
                                        .map(|t| crate::dx_blob_comm::is_dx_target(&t))
                                        .unwrap_or(false);
                                    if was_dx_target {
                                        continue;
                                    }

                                    capture_cache.retain(|_, cid| cid != &close.comm_id.0);

                                    if let Err(e) = state_for_iopub
                                        .with_doc(|sd| sd.remove_comm(&close.comm_id.0))
                                    {
                                        warn!("[runtime-state] {}", e);
                                    }
                                }

                                _ => {
                                    debug!(
                                        "[jupyter-kernel] Unhandled iopub message: {}",
                                        message.header.msg_type
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            error!("[jupyter-kernel] iopub read error: {}", e);
                            break;
                        }
                    }
                }
                warn!("[jupyter-kernel] iopub loop exited, signaling KernelDied");
                let _ = iopub_cmd_tx.try_send(QueueCommand::KernelDied);
            },
            move |_| {
                let _ = iopub_panic_cmd_tx.try_send(QueueCommand::KernelDied);
            },
        );

        // ── Shell connection ─────────────────────────────────────────────

        let identity = runtimelib::peer_identity_for_session(&session_id)?;
        let mut shell = runtimelib::create_client_shell_connection_with_identity(
            &connection_info,
            &session_id,
            identity,
        )
        .await?;

        // Verify kernel is alive — race kernel_info_reply against process death
        let request: JupyterMessage = KernelInfoRequest::default().into();
        shell.send(request).await?;

        let reply = tokio::select! {
            result = tokio::time::timeout(std::time::Duration::from_secs(30), shell.read()) => {
                match result {
                    Ok(Ok(msg)) => Ok(msg),
                    Ok(Err(e)) => Err(anyhow::anyhow!("Kernel did not respond: {}", e)),
                    Err(_) => Err(anyhow::anyhow!("Kernel did not respond within 30s")),
                }
            }
            died_msg = died_rx => {
                let msg = died_msg.unwrap_or_else(|_| "unknown".to_string());
                Err(anyhow::anyhow!("Kernel process died before responding: {}", msg))
            }
        };

        match reply {
            Ok(msg) => {
                info!(
                    "[jupyter-kernel] Kernel alive: got {} reply",
                    msg.header.msg_type
                );
            }
            Err(e) => {
                error!("[jupyter-kernel] {}", e);
                // Abort process watcher to clean up orphaned kernel
                process_watcher_task.abort();
                return Err(e);
            }
        }

        // Split shell into reader/writer
        let (shell_writer, mut shell_reader) = shell.split();

        // ── Shell reader task ────────────────────────────────────────────

        let _shell_broadcast_tx = shared.broadcast_tx.clone();
        let shell_cell_id_map = cell_id_map.clone();
        let shell_pending_history = pending_history.clone();
        let shell_pending_completions = pending_completions.clone();
        let shell_state = shared.state.clone();
        let shell_blob_store = shared.blob_store.clone();
        let shell_actor_id = format!("{kernel_actor_id}:shell");

        let shell_panic_cmd_tx = cmd_tx.clone();
        let shell_reader_task = spawn_supervised(
            "shell-reader",
            async move {
                loop {
                    match shell_reader.read().await {
                        Ok(msg) => {
                            let _parent_msg_id =
                                msg.parent_header.as_ref().map(|h| h.msg_id.clone());

                            match msg.content {
                                JupyterMessageContent::ExecuteReply(ref reply) => {
                                    let execution_id =
                                        msg.parent_header.as_ref().map(|h| h.msg_id.clone());
                                    let cell_id = execution_id.as_ref().and_then(|eid| {
                                        shell_cell_id_map.lock().ok()?.get(eid).cloned()
                                    });

                                    // Process page payloads
                                    if let Some(_cid) = cell_id {
                                        for payload in &reply.payload {
                                            if let jupyter_protocol::Payload::Page {
                                                data, ..
                                            } = payload
                                            {
                                                let nbformat_value = media_to_display_data(data);

                                                let manifest_json =
                                                    match output_store::create_manifest(
                                                        &nbformat_value,
                                                        &shell_blob_store,
                                                        DEFAULT_INLINE_THRESHOLD,
                                                    )
                                                    .await
                                                    {
                                                        Ok(manifest) => manifest.to_json(),
                                                        Err(e) => {
                                                            warn!(
                                                            "[jupyter-kernel] Failed to create page manifest: {}",
                                                            e
                                                        );
                                                            crate::notebook_sync_server::fallback_output_with_id(&nbformat_value)
                                                        }
                                                    };

                                                let eid = execution_id.clone().unwrap_or_default();
                                                let mut fork =
                                                    match shell_state.fork(&shell_actor_id) {
                                                        Ok(f) => f,
                                                        Err(e) => {
                                                            warn!("[runtime-state] fork: {}", e);
                                                            continue;
                                                        }
                                                    };

                                                if let Err(e) =
                                                    fork.append_output(&eid, &manifest_json)
                                                {
                                                    warn!(
                                                    "[jupyter-kernel] Failed to append page output to state doc: {}",
                                                    e
                                                );
                                                }

                                                if let Err(e) = shell_state.merge(&mut fork) {
                                                    warn!("[runtime-state] merge: {}", e);
                                                }
                                            }
                                        }
                                    }
                                }
                                JupyterMessageContent::HistoryReply(ref reply) => {
                                    if let Some(ref parent) = msg.parent_header {
                                        let msg_id = &parent.msg_id;
                                        if let Ok(mut pending) = shell_pending_history.lock() {
                                            if let Some(tx) = pending.remove(msg_id) {
                                                let entries: Vec<HistoryEntry> = reply
                                                    .history
                                                    .iter()
                                                    .map(|item| {
                                                        match item {
                                                    jupyter_protocol::HistoryEntry::Input(
                                                        session,
                                                        line,
                                                        source,
                                                    ) => HistoryEntry {
                                                        session: *session as i32,
                                                        line: *line as i32,
                                                        source: source.clone(),
                                                    },
                                                    jupyter_protocol::HistoryEntry::InputOutput(
                                                        session,
                                                        line,
                                                        (source, _output),
                                                    ) => HistoryEntry {
                                                        session: *session as i32,
                                                        line: *line as i32,
                                                        source: source.clone(),
                                                    },
                                                }
                                                    })
                                                    .collect();

                                                debug!(
                                                "[jupyter-kernel] Resolved history request: {} entries",
                                                entries.len()
                                            );
                                                let _ = tx.send(entries);
                                            }
                                        }
                                    }
                                }
                                JupyterMessageContent::CompleteReply(ref reply) => {
                                    if let Some(ref parent) = msg.parent_header {
                                        let msg_id = &parent.msg_id;
                                        if let Ok(mut pending) = shell_pending_completions.lock() {
                                            if let Some(tx) = pending.remove(msg_id) {
                                                let items: Vec<CompletionItem> = reply
                                                    .matches
                                                    .iter()
                                                    .map(|m| CompletionItem {
                                                        label: m.clone(),
                                                        kind: None,
                                                        detail: None,
                                                        source: Some("kernel".to_string()),
                                                    })
                                                    .collect();

                                                debug!(
                                                "[jupyter-kernel] Resolved completion request: {} items",
                                                items.len()
                                            );
                                                let _ = tx.send((
                                                    items,
                                                    reply.cursor_start,
                                                    reply.cursor_end,
                                                ));
                                            }
                                        }
                                    }
                                }
                                _ => {
                                    debug!(
                                        "[jupyter-kernel] shell reply: type={}",
                                        msg.header.msg_type
                                    );
                                }
                            }
                        }
                        Err(e) => {
                            error!("[jupyter-kernel] shell read error: {}", e);
                            break;
                        }
                    }
                }
            },
            move |_| {
                let _ = shell_panic_cmd_tx.try_send(QueueCommand::KernelDied);
            },
        );

        // ── Heartbeat monitor ────────────────────────────────────────────

        let hb_cmd_tx = cmd_tx.clone();
        let hb_panic_cmd_tx = cmd_tx.clone();
        let hb_conn_info = connection_info.clone();
        let heartbeat_task = spawn_supervised(
            "heartbeat",
            async move {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                    let check = async {
                        let mut hb =
                            runtimelib::create_client_heartbeat_connection(&hb_conn_info).await?;
                        hb.single_heartbeat().await
                    };

                    match tokio::time::timeout(std::time::Duration::from_secs(3), check).await {
                        Ok(Ok(())) => {
                            // Kernel is alive
                        }
                        Ok(Err(e)) => {
                            warn!("[jupyter-kernel] Heartbeat connection error: {}", e);
                            let _ = hb_cmd_tx.try_send(QueueCommand::KernelDied);
                            break;
                        }
                        Err(_) => {
                            warn!("[jupyter-kernel] Heartbeat timeout, kernel unresponsive");
                            let _ = hb_cmd_tx.try_send(QueueCommand::KernelDied);
                            break;
                        }
                    }
                }
            },
            move |_| {
                let _ = hb_panic_cmd_tx.try_send(QueueCommand::KernelDied);
            },
        );

        // ── Coalesced comm state writer ──────────────────────────────────

        let mut coalesce_rx = coalesce_rx;
        let coalesce_state = shared.state.clone();
        let coalesce_blob_store = shared.blob_store.clone();
        // Coalesced comm writes must carry the kernel actor ID so the
        // runtime agent's actor filter in `receive_sync_and_foreign_comms`
        // recognizes them as self-authored echoes and doesn't forward
        // them back to the kernel. Without this, writes inherit the
        // doc's default actor (`runtimed:state`) and the filter lets
        // them through, re-triggering the amplification loop.
        let coalesce_actor_id = format!("{kernel_actor_id}:coalesce");
        let coalesce_panic_cmd_tx = cmd_tx.clone();
        let comm_coalesce_task = spawn_supervised(
            "comm-coalesce",
            async move {
                let mut pending: HashMap<String, serde_json::Value> = HashMap::new();
                let mut timer = tokio::time::interval(std::time::Duration::from_millis(16));
                timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                loop {
                    tokio::select! {
                        msg = coalesce_rx.recv() => {
                            match msg {
                                Some((comm_id, delta)) => {
                                    let entry = pending.entry(comm_id)
                                        .or_insert_with(|| serde_json::json!({}));
                                    if let (Some(existing), Some(new)) =
                                        (entry.as_object_mut(), delta.as_object())
                                    {
                                        for (k, v) in new {
                                            existing.insert(k.clone(), v.clone());
                                        }
                                    }
                                }
                                None => break,
                            }
                        }
                        _ = timer.tick() => {
                            if pending.is_empty() {
                                continue;
                            }
                            let mut batch = std::mem::take(&mut pending);
                            for delta in batch.values_mut() {
                                *delta = blob_store_large_state_values(delta, &coalesce_blob_store).await;
                            }
                            if let Err(e) = coalesce_state.with_doc(|sd| {
                                sd.fork_and_merge(|f| {
                                    f.set_actor(&coalesce_actor_id);
                                    for (comm_id, delta) in &batch {
                                        if let Err(e) = f.merge_comm_state_delta(comm_id, delta) {
                                            warn!("[runtime-state] {}", e);
                                        }
                                    }
                                });
                                Ok(())
                            }) {
                                warn!("[runtime-state] {}", e);
                            }
                        }
                    }
                }
            },
            move |_| {
                let _ = coalesce_panic_cmd_tx.try_send(QueueCommand::KernelDied);
            },
        );

        // ── Construct the kernel struct ──────────────────────────────────

        let kernel = Self {
            kernel_type,
            env_source,
            launched_config,
            env_path,
            session_id,
            kernel_actor_id,
            connection_info: Some(connection_info),
            connection_file: Some(connection_file_path),
            shell_writer: Some(shell_writer),
            #[cfg(unix)]
            ipc_prefix,
            #[cfg(unix)]
            kernel_pid,
            iopub_task: Some(iopub_task),
            shell_reader_task: Some(shell_reader_task),
            process_watcher_task: Some(process_watcher_task),
            heartbeat_task: Some(heartbeat_task),
            comm_coalesce_tx: Some(coalesce_tx),
            comm_coalesce_task: Some(comm_coalesce_task),
            cell_id_map,
            cmd_tx: Some(cmd_tx),
            comm_seq,
            pending_history,
            pending_completions,
            stream_terminals,
        };

        info!("[jupyter-kernel] Kernel started: {}", kernel_id);
        Ok((kernel, cmd_rx))
    }

    // ── Execute ──────────────────────────────────────────────────────────

    async fn execute(&mut self, cell_id: &str, execution_id: &str, source: &str) -> Result<()> {
        let shell = self
            .shell_writer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("No kernel running"))?;

        // Build execute request with msg_id = execution_id.
        // IOPub replies echo this back as parent_header.msg_id, so we can
        // route outputs directly by execution_id without an indirection map.
        let request = ExecuteRequest::new(source.to_string());
        let mut message: JupyterMessage = request.into();
        message.header.msg_id = execution_id.to_string();

        // Register execution_id -> cell_id BEFORE sending.
        // Remove any old mappings for this cell_id first.
        {
            let mut map = self.cell_id_map.lock().unwrap();
            map.retain(|_, cid| cid != cell_id);
            map.insert(execution_id.to_string(), cell_id.to_string());
        }

        shell.send(message).await?;
        info!(
            "[jupyter-kernel] Sent execute_request: cell_id={} execution_id={}",
            cell_id, execution_id
        );

        Ok(())
    }

    // ── Interrupt ────────────────────────────────────────────────────────

    async fn interrupt(&mut self) -> Result<()> {
        let connection_info = self
            .connection_info
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("No kernel running"))?;

        let mut control =
            runtimelib::create_client_control_connection(connection_info, &self.session_id).await?;

        let request: JupyterMessage = InterruptRequest {}.into();
        control.send(request).await?;

        info!("[jupyter-kernel] Sent interrupt_request");

        // Wait for acknowledgement with timeout
        match tokio::time::timeout(std::time::Duration::from_secs(5), control.read()).await {
            Ok(Ok(_reply)) => {
                info!("[jupyter-kernel] Received interrupt_reply");
            }
            Ok(Err(e)) => {
                warn!("[jupyter-kernel] Error receiving interrupt_reply: {}", e);
            }
            Err(_) => {
                warn!("[jupyter-kernel] Timed out waiting for interrupt_reply (5s)");
            }
        }

        Ok(())
    }

    // ── Shutdown ─────────────────────────────────────────────────────────

    async fn shutdown(&mut self) -> Result<()> {
        info!("[jupyter-kernel] Shutting down kernel");

        // Abort background tasks first so they stop reading from ZeroMQ
        if let Some(task) = self.iopub_task.take() {
            task.abort();
        }
        if let Some(task) = self.shell_reader_task.take() {
            task.abort();
        }
        if let Some(task) = self.process_watcher_task.take() {
            task.abort();
        }
        if let Some(task) = self.heartbeat_task.take() {
            task.abort();
        }
        self.comm_coalesce_tx.take();
        if let Some(task) = self.comm_coalesce_task.take() {
            task.abort();
        }

        // Graceful shutdown: send shutdown_request, then escalate signals.
        // The kernel should exit on its own after receiving the request.
        if let Some(mut shell) = self.shell_writer.take() {
            let request: JupyterMessage = ShutdownRequest { restart: false }.into();
            let _ = shell.send(request).await;
        }

        #[cfg(unix)]
        if let Some(pid) = self.kernel_pid.take() {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;

            // Wait up to 2s for the kernel to exit after shutdown_request
            if !wait_for_pid_exit(pid, std::time::Duration::from_secs(2)).await {
                info!(
                    "[jupyter-kernel] Kernel didn't exit after shutdown_request, sending SIGTERM to pid {}",
                    pid
                );
                let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);

                // Wait up to 3s for SIGTERM
                if !wait_for_pid_exit(pid, std::time::Duration::from_secs(3)).await {
                    info!(
                        "[jupyter-kernel] Kernel didn't respond to SIGTERM, sending SIGKILL to pid {}",
                        pid
                    );
                    if let Err(e) = kill(Pid::from_raw(pid), Signal::SIGKILL) {
                        match e {
                            nix::errno::Errno::ESRCH => {}
                            other => {
                                debug!(
                                    "[jupyter-kernel] Failed to SIGKILL kernel pid {}: {}",
                                    pid, other
                                );
                            }
                        }
                    }
                }
            }
        }

        #[cfg(not(unix))]
        {
            // On non-Unix platforms, kill the child process directly
            // (process groups aren't available)
        }

        if let Some(ref path) = self.connection_file {
            let _ = std::fs::remove_file(path);
        }

        #[cfg(unix)]
        if let Some(ref prefix) = self.ipc_prefix {
            cleanup_ipc_sockets(prefix);
        }

        self.connection_info = None;
        self.connection_file = None;
        self.cell_id_map.lock().unwrap().clear();
        self.cmd_tx = None;

        info!("[jupyter-kernel] Kernel shutdown complete");
        Ok(())
    }

    // ── Comm messages ────────────────────────────────────────────────────

    async fn send_comm_message(&mut self, raw_message: serde_json::Value) -> Result<()> {
        let shell = self
            .shell_writer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("No kernel running"))?;

        let header: jupyter_protocol::Header = serde_json::from_value(
            raw_message
                .get("header")
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("Missing header in comm message"))?,
        )?;

        let msg_type = header.msg_type.clone();

        let parent_header: Option<jupyter_protocol::Header> =
            raw_message.get("parent_header").and_then(|v| {
                if v.is_null() {
                    None
                } else {
                    serde_json::from_value(v.clone()).ok()
                }
            });

        let metadata: serde_json::Value = raw_message
            .get("metadata")
            .cloned()
            .unwrap_or(serde_json::json!({}));

        let content_value = raw_message
            .get("content")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Missing content in comm message"))?;

        let message_content =
            JupyterMessageContent::from_type_and_content(&msg_type, content_value)?;

        let buffers: Vec<Bytes> = raw_message
            .get("buffers")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|buf| {
                        buf.as_array().map(|bytes| {
                            let bytes: Vec<u8> = bytes
                                .iter()
                                .filter_map(|b| b.as_u64().map(|n| n as u8))
                                .collect();
                            Bytes::from(bytes)
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let message = JupyterMessage {
            zmq_identities: Vec::new(),
            header,
            parent_header,
            metadata,
            content: message_content,
            buffers,
            channel: Some(jupyter_protocol::Channel::Shell),
        };

        debug!(
            "[jupyter-kernel] Sending comm message: type={} msg_id={}",
            msg_type, message.header.msg_id
        );

        shell.send(message).await?;
        Ok(())
    }

    async fn send_comm_update(&mut self, comm_id: &str, state: serde_json::Value) -> Result<()> {
        let shell = self
            .shell_writer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("shell_writer not available"))?;

        let comm_msg = jupyter_protocol::CommMsg {
            comm_id: jupyter_protocol::CommId(comm_id.to_string()),
            data: {
                let mut map = serde_json::Map::new();
                map.insert("method".to_string(), serde_json::json!("update"));
                map.insert("state".to_string(), state);
                map.insert("buffer_paths".to_string(), serde_json::json!([]));
                map
            },
        };
        let message: jupyter_protocol::JupyterMessage = comm_msg.into();
        shell.send(message).await?;
        debug!(
            "[jupyter-kernel] Sent comm_msg(update) to kernel: comm_id={}",
            comm_id
        );
        Ok(())
    }

    // ── Completions ──────────────────────────────────────────────────────

    async fn complete(
        &mut self,
        code: &str,
        cursor_pos: usize,
    ) -> Result<(Vec<CompletionItem>, usize, usize)> {
        // Clone Arc before taking &mut shell_writer to avoid borrow conflicts.
        let pending = self.pending_completions.clone();

        let shell = self
            .shell_writer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("No kernel running"))?;

        let request = CompleteRequest {
            code: code.to_string(),
            cursor_pos,
        };
        let message: JupyterMessage = request.into();
        let msg_id = message.header.msg_id.clone();

        // Register pending request
        let (tx, rx) = oneshot::channel();
        pending
            .lock()
            .map_err(|_| anyhow::anyhow!("Lock poisoned"))?
            .insert(msg_id.clone(), tx);

        if let Err(e) = shell.send(message).await {
            if let Ok(mut guard) = pending.lock() {
                guard.remove(&msg_id);
            }
            return Err(e.into());
        }
        debug!("[jupyter-kernel] Sent complete_request: msg_id={}", msg_id);

        // Wait for response with timeout (shell reader task will resolve via pending_completions)
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(result)) => Ok(result),
            Ok(Err(_)) => Err(anyhow::anyhow!("Completion request cancelled")),
            Err(_) => {
                if let Ok(mut guard) = pending.lock() {
                    guard.remove(&msg_id);
                }
                Err(anyhow::anyhow!("Completion request timed out"))
            }
        }
    }

    // ── History ──────────────────────────────────────────────────────────

    async fn get_history(
        &mut self,
        pattern: Option<&str>,
        n: i32,
        unique: bool,
    ) -> Result<Vec<HistoryEntry>> {
        // Clone Arc before taking &mut shell_writer to avoid borrow conflicts.
        let pending = self.pending_history.clone();

        let shell = self
            .shell_writer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("No kernel running"))?;

        let glob_pattern = escape_glob_pattern(pattern);
        let request = HistoryRequest::Search {
            pattern: glob_pattern,
            unique,
            output: false,
            raw: true,
            n,
        };

        let message: JupyterMessage = request.into();
        let msg_id = message.header.msg_id.clone();

        let (tx, rx) = oneshot::channel();
        pending
            .lock()
            .map_err(|_| anyhow::anyhow!("Lock poisoned"))?
            .insert(msg_id.clone(), tx);

        shell.send(message).await?;
        debug!("[jupyter-kernel] Sent history_request: msg_id={}", msg_id);

        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(entries)) => Ok(entries),
            Ok(Err(_)) => Err(anyhow::anyhow!("History request cancelled")),
            Err(_) => {
                if let Ok(mut guard) = pending.lock() {
                    guard.remove(&msg_id);
                }
                Err(anyhow::anyhow!("History request timed out"))
            }
        }
    }

    // ── Read-only metadata accessors ─────────────────────────────────────

    fn kernel_type(&self) -> &str {
        &self.kernel_type
    }

    fn env_source(&self) -> &str {
        &self.env_source
    }

    fn launched_config(&self) -> &LaunchedEnvConfig {
        &self.launched_config
    }

    fn env_path(&self) -> Option<&PathBuf> {
        self.env_path.as_ref()
    }

    fn is_connected(&self) -> bool {
        self.shell_writer.is_some()
    }

    // ── Mutable metadata update ──────────────────────────────────────────

    fn update_launched_uv_deps(&mut self, deps: Vec<String>) {
        self.launched_config.uv_deps = Some(deps);
    }
}

impl JupyterKernel {
    /// Get an InterruptHandle for concurrent interrupt without &mut self.
    pub fn interrupt_handle(&self) -> Option<InterruptHandle> {
        self.connection_info.as_ref().map(|ci| InterruptHandle {
            connection_info: ci.clone(),
            session_id: self.session_id.clone(),
        })
    }
}

impl Drop for JupyterKernel {
    fn drop(&mut self) {
        // Abort any running tasks
        if let Some(task) = self.iopub_task.take() {
            task.abort();
        }
        if let Some(task) = self.shell_reader_task.take() {
            task.abort();
        }
        if let Some(task) = self.process_watcher_task.take() {
            task.abort();
        }
        if let Some(task) = self.heartbeat_task.take() {
            task.abort();
        }
        self.comm_coalesce_tx.take();
        if let Some(task) = self.comm_coalesce_task.take() {
            task.abort();
        }

        // Kill kernel process on Unix
        #[cfg(unix)]
        if let Some(pid) = self.kernel_pid.take() {
            use nix::sys::signal::{kill, Signal};
            use nix::unistd::Pid;
            let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        }

        // Clean up connection file
        if let Some(ref path) = self.connection_file {
            let _ = std::fs::remove_file(path);
        }

        #[cfg(unix)]
        if let Some(ref prefix) = self.ipc_prefix {
            cleanup_ipc_sockets(prefix);
        }

        info!("[jupyter-kernel] JupyterKernel dropped - resources cleaned up");
    }
}

/// Wait for a process to exit by polling `kill(pid, 0)`.
#[cfg(unix)]
async fn wait_for_pid_exit(pid: i32, timeout: std::time::Duration) -> bool {
    use nix::sys::signal::kill;
    use nix::unistd::Pid;

    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        match kill(Pid::from_raw(pid), None) {
            Err(nix::errno::Errno::ESRCH) => return true,
            Err(_) => return true,
            Ok(()) => {
                if tokio::time::Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        }
    }
}

fn prepend_to_path(dir: &std::path::Path) -> String {
    let dir_str = dir.to_string_lossy();
    match std::env::var("PATH") {
        Ok(existing) => format!("{}:{}", dir_str, existing),
        Err(_) => dir_str.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_kernel_port_listeners_uses_provided_ports() {
        let ports = KernelPorts {
            stdin: 19100,
            control: 19101,
            hb: 19102,
            shell: 19103,
            iopub: 19104,
        };

        let listeners = bind_kernel_port_listeners(IpAddr::V4(Ipv4Addr::LOCALHOST), ports)
            .await
            .expect("bind provided ports");
        let bound_ports: Vec<_> = listeners
            .iter()
            .map(|listener| listener.local_addr().expect("local addr").port())
            .collect();

        assert_eq!(bound_ports, vec![19100, 19101, 19102, 19103, 19104]);
    }
}
