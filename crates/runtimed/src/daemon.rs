//! Pool daemon server implementation.
//!
//! The daemon manages prewarmed environment pools and handles requests from
//! notebook windows via IPC (Unix domain sockets on Unix, named pipes on Windows).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use log::{debug, error, info, warn};
use notify_debouncer_mini::DebounceEventResult;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, Notify};

#[cfg(unix)]
use tokio::net::UnixListener;

#[cfg(windows)]
use tokio::net::windows::named_pipe::ServerOptions;

use tokio::sync::RwLock;

use crate::blob_server;
use crate::blob_store::BlobStore;
use crate::connection::{self, Handshake};
use crate::notebook_sync_server::NotebookRooms;
use crate::protocol::{BlobRequest, BlobResponse, Request, Response};
use crate::settings_doc::SettingsDoc;
use crate::singleton::DaemonLock;
use crate::{default_blob_store_dir, default_cache_dir, default_socket_path, EnvType, PooledEnv};
use runtimed_client::singleton::DaemonInfo;

/// Configuration for the pool daemon.
#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// Socket path for the unified IPC socket.
    pub socket_path: PathBuf,
    /// Cache directory for environments.
    pub cache_dir: PathBuf,
    /// Directory for the content-addressed blob store.
    pub blob_store_dir: PathBuf,
    /// Directory for persisted notebook Automerge documents.
    pub notebook_docs_dir: PathBuf,
    /// Target number of UV environments to maintain.
    pub uv_pool_size: usize,
    /// Target number of Conda environments to maintain.
    pub conda_pool_size: usize,
    /// Target number of Pixi environments to maintain.
    pub pixi_pool_size: usize,
    /// Maximum age (in seconds) before an environment is considered stale.
    pub max_age_secs: u64,
    /// Optional custom directory for lock files (used in tests).
    pub lock_dir: Option<PathBuf>,
    /// Override for room eviction delay (milliseconds). Used in tests.
    /// If None, uses the user's `keep_alive_secs` setting.
    pub room_eviction_delay_ms: Option<u64>,
    /// Maximum age (in seconds) for content-addressed cached environments.
    /// Environments older than this are evicted by the GC loop.
    pub env_cache_max_age_secs: u64,
    /// Maximum number of content-addressed cached environments per cache directory.
    pub env_cache_max_count: usize,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: default_socket_path(),
            cache_dir: default_cache_dir(),
            blob_store_dir: default_blob_store_dir(),
            notebook_docs_dir: crate::default_notebook_docs_dir(),
            uv_pool_size: 3,
            conda_pool_size: 3,
            pixi_pool_size: 2,
            max_age_secs: 172800, // 2 days
            lock_dir: None,
            room_eviction_delay_ms: None,
            env_cache_max_age_secs: 604800, // 7 days
            env_cache_max_count: 10,
        }
    }
}

/// A prewarmed environment in the pool.
struct PoolEntry {
    env: PooledEnv,
    created_at: Instant,
}

/// Failure tracking for exponential backoff.
#[derive(Debug, Clone, Default)]
struct FailureState {
    /// Number of consecutive failures.
    consecutive_failures: u32,
    /// Time of last failure.
    last_failure: Option<Instant>,
    /// Last error message (for logging/status).
    last_error: Option<String>,
    /// Failed package name if identified.
    failed_package: Option<String>,
}

/// Result of parsing a package installation error.
#[derive(Debug, Clone)]
struct PackageInstallError {
    /// The package that failed (if identifiable).
    failed_package: Option<String>,
    /// Full error message from uv.
    error_message: String,
}

/// Parse UV stderr to identify the failed package.
///
/// UV outputs errors in various formats. This function tries to extract
/// the package name that caused the failure.
fn parse_uv_error(stderr: &str) -> Option<PackageInstallError> {
    // Pattern 1: "No solution found when resolving dependencies:
    //   ╰─▶ Because foo was not found..."
    // Pattern 2: "error: Package `foo` not found"
    // Pattern 3: "error: Failed to download `foo`"
    // Pattern 4: "No matching distribution found for foo"

    let stderr_lower = stderr.to_lowercase();

    // Look for "package `name`" or "package 'name'" pattern
    let pkg_patterns = [
        (r"package `([^`]+)`", '`'),
        (r"package '([^']+)'", '\''),
        (r"because ([a-z0-9_-]+) was not found", ' '),
        (r"no matching distribution found for ([a-z0-9_-]+)", ' '),
        (r"failed to download `([^`]+)`", '`'),
        (r"failed to download '([^']+)'", '\''),
    ];

    for (pattern, _) in &pkg_patterns {
        if let Ok(re) = regex::Regex::new(pattern) {
            if let Some(caps) = re.captures(&stderr_lower) {
                if let Some(pkg) = caps.get(1) {
                    let package_name = pkg.as_str().to_string();
                    // Skip if it's a core package name we're definitely installing
                    if package_name != "ipykernel"
                        && package_name != "ipywidgets"
                        && package_name != "anywidget"
                    {
                        return Some(PackageInstallError {
                            failed_package: Some(package_name),
                            error_message: stderr.to_string(),
                        });
                    }
                }
            }
        }
    }

    // If we couldn't identify the specific package, return a generic error
    if stderr.contains("error") || stderr.contains("failed") || stderr.contains("not found") {
        return Some(PackageInstallError {
            failed_package: None,
            error_message: stderr.to_string(),
        });
    }

    None
}

/// Spawn background deletion of environment directories.
fn spawn_env_deletions(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    tokio::spawn(async move {
        for path in &paths {
            if let Err(e) = tokio::fs::remove_dir_all(path).await {
                warn!("[runtimed] Failed to delete stale env {:?}: {}", path, e);
            }
        }
        info!(
            "[runtimed] Cleaned up {} stale/invalid env directories",
            paths.len()
        );
    });
}

/// Internal pool state.
struct Pool {
    /// Available environments ready for use.
    available: VecDeque<PoolEntry>,
    /// Number currently being created.
    warming: usize,
    /// Target pool size.
    target: usize,
    /// Maximum age in seconds.
    max_age_secs: u64,
    /// Failure tracking for exponential backoff.
    failure_state: FailureState,
}

const MIN_WARM_BASES: usize = 2;

fn uv_prewarmed_packages(extra: &[String]) -> Vec<String> {
    let mut packages = vec![
        "ipykernel".to_string(),
        "ipywidgets".to_string(),
        "anywidget".to_string(),
        "nbformat".to_string(),
        "uv".to_string(),
    ];
    packages.extend(extra.iter().cloned());
    packages
}

fn conda_prewarmed_packages(extra: &[String]) -> Vec<String> {
    let mut packages = vec![
        "ipykernel".to_string(),
        "ipywidgets".to_string(),
        "anywidget".to_string(),
        "nbformat".to_string(),
    ];
    packages.extend(extra.iter().cloned());
    packages
}

fn pixi_prewarmed_packages(extra: &[String]) -> Vec<String> {
    let mut packages = vec![
        "ipykernel".to_string(),
        "ipywidgets".to_string(),
        "anywidget".to_string(),
        "nbformat".to_string(),
    ];
    packages.extend(extra.iter().cloned());
    packages
}

fn package_import_name(pkg: &str) -> Option<String> {
    let name = pkg
        .split("::")
        .last()?
        .split([
            '<', '>', '=', '!', '~', ' ', '[', '"', '\'', '\\', '\n', '\r',
        ])
        .next()?
        .trim();
    if name.is_empty() {
        return None;
    }
    let candidate = name.replace('-', "_");
    if candidate.split('.').all(is_valid_python_identifier) {
        Some(candidate)
    } else {
        None
    }
}

fn is_valid_python_identifier(segment: &str) -> bool {
    let mut chars = segment.chars();
    match chars.next() {
        Some(first) if first == '_' || first.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn build_python_warmup_script(extra_packages: &[String], include_conda_runtime: bool) -> String {
    let mut script = r#"
import ipykernel
import IPython
import ipywidgets
import anywidget
import nbformat
"#
    .to_string();

    if include_conda_runtime {
        script.push_str(
            r#"
import traitlets
import zmq
"#,
        );
    }

    let mut modules = extra_packages
        .iter()
        .filter_map(|pkg| package_import_name(pkg))
        .collect::<Vec<_>>();
    modules.sort();
    modules.dedup();

    if !modules.is_empty() {
        if let Ok(modules_json) = serde_json::to_string(&modules) {
            script.push('\n');
            script.push_str("for module_name in ");
            script.push_str(&modules_json);
            script.push_str(":\n");
            script.push_str("    try:\n");
            script.push_str("        __import__(module_name)\n");
            script.push_str("    except Exception:\n");
            script.push_str("        pass\n");
        }
    }

    script.push_str(
        r#"
from ipykernel.kernelbase import Kernel
from ipykernel.ipkernel import IPythonKernel
"#,
    );
    if include_conda_runtime {
        script.push_str("from ipykernel.comm import CommManager\n");
    }
    script.push_str("print(\"warmup complete\")\n");
    script
}

impl Pool {
    fn new(target: usize, max_age_secs: u64) -> Self {
        Self {
            available: VecDeque::new(),
            warming: 0,
            target,
            max_age_secs,
            failure_state: FailureState::default(),
        }
    }

    fn min_available(&self) -> usize {
        self.target.min(MIN_WARM_BASES)
    }

    /// Prune stale environments, returning paths that should be deleted from disk.
    fn prune_stale(&mut self) -> Vec<PathBuf> {
        let max_age = std::time::Duration::from_secs(self.max_age_secs);
        let mut removed_paths = Vec::new();
        let mut healthy = VecDeque::new();
        for entry in self.available.drain(..) {
            if entry.env.venv_path.exists()
                && entry.env.python_path.exists()
                && entry.env.venv_path.join(".warmed").exists()
            {
                healthy.push_back(entry);
            } else {
                removed_paths.push(entry.env.venv_path.clone());
            }
        }

        let mut healthy_count = healthy.len();
        let mut kept = VecDeque::new();
        for entry in healthy {
            let stale = entry.created_at.elapsed() >= max_age;
            if stale && healthy_count > self.min_available() {
                removed_paths.push(entry.env.venv_path.clone());
                healthy_count -= 1;
            } else {
                kept.push_back(entry);
            }
        }

        self.available = kept;
        if !removed_paths.is_empty() {
            info!(
                "[runtimed] Pruned {} stale/invalid environments",
                removed_paths.len()
            );
        }
        removed_paths
    }

    /// Take an environment from the pool.
    fn take(&mut self) -> (Option<PooledEnv>, Vec<PathBuf>) {
        let stale_paths = self.prune_stale();

        // Try to get a valid environment, skipping any with missing paths or missing warmup
        let mut invalid_paths = Vec::new();
        while let Some(entry) = self.available.pop_front() {
            if entry.env.venv_path.exists()
                && entry.env.python_path.exists()
                && entry.env.venv_path.join(".warmed").exists()
            {
                let mut all_paths = stale_paths;
                all_paths.extend(invalid_paths);
                return (Some(entry.env), all_paths);
            }
            warn!(
                "[runtimed] Skipping env with missing path or warmup marker: {:?}",
                entry.env.venv_path
            );
            invalid_paths.push(entry.env.venv_path);
        }

        let mut all_paths = stale_paths;
        all_paths.extend(invalid_paths);
        (None, all_paths)
    }

    /// Add an environment to the pool (success case).
    fn add(&mut self, env: PooledEnv) {
        self.available.push_back(PoolEntry {
            env,
            created_at: Instant::now(),
        });
        self.warming = self.warming.saturating_sub(1);
        // Reset failure state on success
        self.failure_state = FailureState::default();
    }

    /// Mark that warming failed with error details.
    fn warming_failed_with_error(&mut self, error: Option<PackageInstallError>) {
        self.warming = self.warming.saturating_sub(1);
        self.failure_state.consecutive_failures += 1;
        self.failure_state.last_failure = Some(Instant::now());

        if let Some(err) = error {
            self.failure_state.last_error = Some(err.error_message);
            self.failure_state.failed_package = err.failed_package;
        }
    }

    /// Reset failure state (called on settings change).
    fn reset_failure_state(&mut self) {
        self.failure_state = FailureState::default();
    }

    /// Calculate backoff delay based on consecutive failures.
    ///
    /// Returns Duration::ZERO if no failures, otherwise exponential backoff:
    /// 30s, 60s, 120s, 240s, max 300s (5 min).
    fn backoff_delay(&self) -> std::time::Duration {
        if self.failure_state.consecutive_failures == 0 {
            return std::time::Duration::ZERO;
        }

        // Exponential backoff: 30s * 2^(failures-1), capped at 300s
        let base_secs = 30u64;
        let exponent = self
            .failure_state
            .consecutive_failures
            .saturating_sub(1)
            .min(4);
        let multiplier = 2u64.pow(exponent);
        let delay_secs = (base_secs * multiplier).min(300);

        std::time::Duration::from_secs(delay_secs)
    }

    /// Check if enough time has passed since last failure to retry.
    fn should_retry(&self) -> bool {
        match self.failure_state.last_failure {
            Some(last) => last.elapsed() >= self.backoff_delay(),
            None => true,
        }
    }

    /// Calculate deficit (how many more we need).
    fn deficit(&self) -> usize {
        let current = self.available.len() + self.warming;
        self.target.saturating_sub(current)
    }

    /// Mark that we're starting to create N environments.
    fn mark_warming(&mut self, count: usize) {
        self.warming += count;
    }

    /// Get current stats.
    fn stats(&self) -> (usize, usize) {
        (self.available.len(), self.warming)
    }

    /// Seconds until the next retry (0 if healthy or retry is imminent).
    fn retry_in_secs(&self) -> u64 {
        self.failure_state
            .last_failure
            .map(|last| {
                self.backoff_delay()
                    .saturating_sub(last.elapsed())
                    .as_secs()
            })
            .unwrap_or(0)
    }
}

/// The pool daemon state.
pub struct Daemon {
    config: DaemonConfig,
    uv_pool: Mutex<Pool>,
    conda_pool: Mutex<Pool>,
    pixi_pool: Mutex<Pool>,
    shutdown: Arc<Mutex<bool>>,
    /// Notifier to wake up accept loops on shutdown.
    shutdown_notify: Arc<Notify>,
    /// Singleton lock - kept alive while daemon is running.
    _lock: DaemonLock,
    /// Shared Automerge settings document.
    settings: Arc<RwLock<SettingsDoc>>,
    /// Broadcast channel to notify sync connections of settings changes.
    settings_changed: tokio::sync::broadcast::Sender<()>,
    /// Global Automerge pool state document (daemon-authoritative, ephemeral).
    pub(crate) pool_doc: Arc<RwLock<notebook_doc::pool_state::PoolDoc>>,
    /// Broadcast channel to notify sync connections of pool doc changes.
    pub(crate) pool_doc_changed: tokio::sync::broadcast::Sender<()>,
    /// Notifiers for pool env readiness (wakes waiters in take_*_env).
    pool_ready_uv: Notify,
    pool_ready_conda: Notify,
    pool_ready_pixi: Notify,
    /// Content-addressed blob store.
    blob_store: Arc<BlobStore>,
    /// HTTP port for the blob server (set after startup).
    blob_port: Mutex<Option<u16>>,
    /// Per-notebook Automerge sync rooms.
    notebook_rooms: NotebookRooms,
}

/// Error returned when another daemon is already running.
#[derive(Debug, thiserror::Error)]
#[error("Another daemon is already running: {info:?}")]
pub struct DaemonAlreadyRunning {
    pub info: DaemonInfo,
}

impl Daemon {
    /// Get the daemon's Unix socket path.
    pub fn socket_path(&self) -> &PathBuf {
        &self.config.socket_path
    }

    /// Get the default Python environment type from settings.
    pub async fn default_python_env(&self) -> crate::settings_doc::PythonEnvType {
        self.settings.read().await.get_all().default_python_env
    }

    /// Get the default pixi packages from settings.
    pub async fn default_pixi_packages(&self) -> Vec<String> {
        self.settings.read().await.get_all().pixi.default_packages
    }

    /// Create a new daemon with the given configuration.
    ///
    /// Returns an error if another daemon is already running.
    pub fn new(config: DaemonConfig) -> Result<Arc<Self>, DaemonAlreadyRunning> {
        // Try to acquire the singleton lock
        let lock = DaemonLock::try_acquire(config.lock_dir.as_ref())
            .map_err(|info| DaemonAlreadyRunning { info })?;

        // Load or create the settings document
        let automerge_path = crate::default_settings_doc_path();
        let json_path = crate::settings_json_path();
        let settings = SettingsDoc::load_or_create(&automerge_path, Some(&json_path));

        // Write the settings JSON Schema for editor autocomplete
        if let Err(e) = crate::settings_doc::write_settings_schema() {
            log::warn!("[settings] Failed to write schema file: {}", e);
        }

        let (settings_changed, _) = tokio::sync::broadcast::channel(16);
        let (pool_doc_changed, _) = tokio::sync::broadcast::channel(16);
        let pool_doc = Arc::new(RwLock::new(notebook_doc::pool_state::PoolDoc::new()));

        let blob_store = Arc::new(BlobStore::new(config.blob_store_dir.clone()));

        Ok(Arc::new(Self {
            uv_pool: Mutex::new(Pool::new(config.uv_pool_size, config.max_age_secs)),
            conda_pool: Mutex::new(Pool::new(config.conda_pool_size, config.max_age_secs)),
            pixi_pool: Mutex::new(Pool::new(config.pixi_pool_size, config.max_age_secs)),
            config,
            shutdown: Arc::new(Mutex::new(false)),
            shutdown_notify: Arc::new(Notify::new()),
            pool_ready_uv: Notify::new(),
            pool_ready_conda: Notify::new(),
            pool_ready_pixi: Notify::new(),
            _lock: lock,
            settings: Arc::new(RwLock::new(settings)),
            settings_changed,
            pool_doc,
            pool_doc_changed,
            blob_store,
            blob_port: Mutex::new(None),
            notebook_rooms: Arc::new(Mutex::new(HashMap::new())),
        }))
    }

    /// Trigger a graceful shutdown of the daemon.
    ///
    /// Sets the shutdown flag and notifies all waiting tasks.
    /// Used by both signal handlers and the RPC shutdown command.
    pub async fn trigger_shutdown(&self) {
        *self.shutdown.lock().await = true;
        self.shutdown_notify.notify_waiters();
    }

    /// Get the room eviction delay.
    ///
    /// Returns the eviction delay duration.
    ///
    /// Uses the config override if set (for tests), otherwise reads from
    /// the user's `keep_alive_secs` setting. Clamps to valid range (5s to 7 days)
    /// to prevent accidental instant eviction or extreme values.
    pub async fn room_eviction_delay(&self) -> std::time::Duration {
        // Test override for predictable eviction in tests
        if let Some(ms) = self.config.room_eviction_delay_ms {
            return std::time::Duration::from_millis(ms);
        }
        let settings = self.settings.read().await;
        let secs = settings
            .get_u64("keep_alive_secs")
            .unwrap_or(crate::settings_doc::DEFAULT_KEEP_ALIVE_SECS)
            .clamp(
                crate::settings_doc::MIN_KEEP_ALIVE_SECS,
                crate::settings_doc::MAX_KEEP_ALIVE_SECS,
            );
        std::time::Duration::from_secs(secs)
    }

    /// Run the daemon server.
    pub async fn run(self: Arc<Self>) -> anyhow::Result<()> {
        // Platform-specific setup
        #[cfg(unix)]
        {
            // Ensure socket directory exists
            if let Some(parent) = self.config.socket_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }

            // Remove stale socket file — singleton lock guarantees exclusivity,
            // so NotFound is fine; log other errors for diagnostics.
            if self.config.socket_path.exists() {
                if let Err(e) = tokio::fs::remove_file(&self.config.socket_path).await {
                    if e.kind() != std::io::ErrorKind::NotFound {
                        warn!(
                            "[runtimed] Failed to remove stale socket {:?}: {}",
                            self.config.socket_path, e
                        );
                    }
                }
            }

            // Clean up obsolete sync socket from pre-unification daemons
            let sync_sock = self.config.socket_path.with_file_name("runtimed-sync.sock");
            if sync_sock.exists() {
                info!("[runtimed] Removing obsolete sync socket: {:?}", sync_sock);
                tokio::fs::remove_file(&sync_sock).await.ok();
            }
        }

        // Start the blob HTTP server
        let blob_port = match blob_server::start_blob_server(self.blob_store.clone()).await {
            Ok(port) => {
                info!("[runtimed] Blob server started on port {}", port);
                *self.blob_port.lock().await = Some(port);
                Some(port)
            }
            Err(e) => {
                error!("[runtimed] Failed to start blob server: {}", e);
                None
            }
        };

        // Bind the Unix socket early so clients can connect (and ping) while
        // the rest of initialisation finishes.  The accept loop runs later.
        #[cfg(unix)]
        let unix_listener = {
            let listener = UnixListener::bind(&self.config.socket_path)?;
            // Restrict socket permissions to owner-only (0600) so other users
            // cannot connect to the daemon.
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(
                    &self.config.socket_path,
                    std::fs::Permissions::from_mode(0o600),
                )?;
            }
            info!("[runtimed] Listening on {:?}", self.config.socket_path);
            listener
        };

        // Write daemon info so clients can discover us
        if let Err(e) = self
            ._lock
            .write_info(&self.config.socket_path.to_string_lossy(), blob_port)
        {
            error!("[runtimed] Failed to write daemon info: {}", e);
        }

        // Reap any orphaned kernel process groups from a previous crash
        #[cfg(unix)]
        {
            let reaped = crate::kernel_pids::reap_orphaned_kernels();
            if reaped > 0 {
                info!(
                    "[runtimed] Reaped {} orphaned kernel process group(s)",
                    reaped
                );
            }
        }

        // Find and reuse existing environments from previous runs
        self.find_existing_environments().await;

        // Seed PoolDoc with initial state (pool sizes, any recovered envs)
        self.update_pool_doc().await;

        // Spawn the warming loops
        let uv_daemon = self.clone();
        tokio::spawn(async move {
            uv_daemon.uv_warming_loop().await;
        });

        let conda_daemon = self.clone();
        tokio::spawn(async move {
            conda_daemon.conda_warming_loop().await;
        });

        let pixi_daemon = self.clone();
        tokio::spawn(async move {
            pixi_daemon.pixi_warming_loop().await;
        });

        // Spawn the environment GC loop
        let gc_daemon = self.clone();
        tokio::spawn(async move {
            gc_daemon.env_gc_loop().await;
        });

        // Spawn the settings.json file watcher
        let watcher_daemon = self.clone();
        tokio::spawn(async move {
            watcher_daemon.watch_settings_json().await;
        });

        // Platform-specific accept loop
        #[cfg(unix)]
        {
            self.run_unix_server(unix_listener).await?;
        }

        #[cfg(windows)]
        {
            self.run_windows_server().await?;
        }

        // Shut down all running kernels before exiting.
        //
        // Kernels are spawned in their own process group (process_group(0)),
        // so they do NOT receive the SIGINT/SIGTERM that the daemon receives.
        // Without explicit shutdown here, kernel processes become orphans.
        // We cannot rely on Drop alone because:
        //   1. RoomKernel is behind Arc<Mutex<Option<...>>> inside Arc<NotebookRoom>
        //      — multiple spawned tasks hold Arc clones that may not all unwind
        //      during tokio runtime teardown.
        //   2. A second ctrl-c or SIGKILL skips destructors entirely.
        //
        // To avoid holding the notebook_rooms lock across .await points, first
        // drain the map into an owned collection, then shut down kernels.
        let drained_rooms = {
            let mut rooms = self.notebook_rooms.lock().await;
            rooms.drain().collect::<Vec<_>>()
        };

        for (notebook_id, room) in drained_rooms {
            // Shut down runtime agent via RPC before dropping handle
            {
                let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
                if has_runtime_agent {
                    info!(
                        "[runtimed] Shutting down runtime agent for notebook on exit: {}",
                        notebook_id
                    );
                    let _ = crate::notebook_sync_server::send_runtime_agent_request(
                        &room,
                        notebook_protocol::protocol::RuntimeAgentRequest::ShutdownKernel,
                    )
                    .await;
                }
                let mut ra_guard = room.runtime_agent_handle.lock().await;
                *ra_guard = None;
                let mut tx = room.runtime_agent_request_tx.lock().await;
                *tx = None;
            }
        }

        // Cleanup socket (Unix only - named pipes don't need cleanup)
        #[cfg(unix)]
        tokio::fs::remove_file(&self.config.socket_path).await.ok();

        Ok(())
    }

    /// Unix-specific server loop using a pre-bound Unix domain socket.
    #[cfg(unix)]
    async fn run_unix_server(
        self: &Arc<Self>,
        listener: tokio::net::UnixListener,
    ) -> anyhow::Result<()> {
        loop {
            tokio::select! {
                accept_result = listener.accept() => {
                    match accept_result {
                        Ok((stream, _)) => {
                            let daemon = self.clone();
                            tokio::spawn(async move {
                                if let Err(e) = daemon.route_connection(stream).await {
                                    if !crate::sync_server::is_connection_closed(&e) {
                                        error!("[runtimed] Connection error: {}", e);
                                    }
                                }
                            });
                        }
                        Err(e) => {
                            error!("[runtimed] Accept error: {}", e);
                        }
                    }
                }
                _ = self.shutdown_notify.notified() => {
                    info!("[runtimed] Shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Windows-specific server loop using named pipes.
    #[cfg(windows)]
    async fn run_windows_server(self: &Arc<Self>) -> anyhow::Result<()> {
        let pipe_name = self.config.socket_path.to_string_lossy().to_string();
        info!("[runtimed] Listening on {}", pipe_name);

        // Create the first pipe server instance
        let mut server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&pipe_name)?;

        loop {
            tokio::select! {
                // Wait for a client to connect
                connect_result = server.connect() => {
                    if let Err(e) = connect_result {
                        error!("[runtimed] Pipe connect error: {}", e);
                        continue;
                    }

                    // The current server instance is now connected - swap it out
                    let connected = server;

                    // Create a new server instance BEFORE spawning the handler
                    // This allows new clients to connect while we handle the current one
                    server = match ServerOptions::new().create(&pipe_name) {
                        Ok(s) => s,
                        Err(e) => {
                            error!("[runtimed] Failed to create new pipe server: {}", e);
                            // Try to recover by creating a new first instance
                            match ServerOptions::new().first_pipe_instance(true).create(&pipe_name) {
                                Ok(s) => s,
                                Err(e) => {
                                    error!("[runtimed] Fatal: cannot create pipe server: {}", e);
                                    break;
                                }
                            }
                        }
                    };

                    // Handle the connection
                    let daemon = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = daemon.route_connection(connected).await {
                            if !crate::sync_server::is_connection_closed(&e) {
                                error!("[runtimed] Connection error: {}", e);
                            }
                        }
                    });
                }
                _ = self.shutdown_notify.notified() => {
                    info!("[runtimed] Shutting down");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Watch `settings.json` for external changes and apply them to the Automerge doc.
    ///
    /// Uses the `notify` crate with a 500ms debouncer. When changes are detected,
    /// reads the file, parses it, and selectively applies any differences to the
    /// Automerge settings document. Self-writes (from `persist_settings`) are
    /// automatically skipped because the file contents match the doc state.
    async fn watch_settings_json(self: Arc<Self>) {
        let json_path = crate::settings_json_path();

        // Determine which path to watch: the file itself if it exists,
        // or the parent directory if it doesn't exist yet.
        let watch_path = if json_path.exists() {
            json_path.clone()
        } else if let Some(parent) = json_path.parent() {
            // Watch parent directory; we'll filter for our file in the handler
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    error!("[settings-watch] Failed to create config dir: {}", e);
                    return;
                }
            }
            parent.to_path_buf()
        } else {
            error!(
                "[settings-watch] Cannot determine watch path for {:?}",
                json_path
            );
            return;
        };

        // Create a tokio mpsc channel to bridge from the notify callback thread
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DebounceEventResult>(16);

        // Create debouncer with 500ms window
        let debouncer_result = notify_debouncer_mini::new_debouncer(
            std::time::Duration::from_millis(500),
            move |res: DebounceEventResult| {
                let _ = tx.blocking_send(res);
            },
        );

        let mut debouncer = match debouncer_result {
            Ok(d) => d,
            Err(e) => {
                error!("[settings-watch] Failed to create file watcher: {}", e);
                return;
            }
        };

        if let Err(e) = debouncer
            .watcher()
            .watch(&watch_path, notify::RecursiveMode::NonRecursive)
        {
            error!("[settings-watch] Failed to watch {:?}: {}", watch_path, e);
            return;
        }

        info!(
            "[settings-watch] Watching {:?} for external changes",
            watch_path
        );

        loop {
            tokio::select! {
                Some(result) = rx.recv() => {
                    match result {
                        Ok(events) => {
                            // Check if any event is for our settings file
                            let relevant = events.iter().any(|e| e.path == json_path);
                            if !relevant {
                                continue;
                            }

                            // Read and parse the file
                            let contents = match tokio::fs::read_to_string(&json_path).await {
                                Ok(c) => c,
                                Err(e) => {
                                    // File may have been deleted or is being written
                                    warn!("[settings-watch] Cannot read settings.json: {}", e);
                                    continue;
                                }
                            };

                            let json: serde_json::Value = match serde_json::from_str(&contents) {
                                Ok(j) => j,
                                Err(e) => {
                                    // Partial write or invalid JSON — try again next event
                                    warn!("[settings-watch] Cannot parse settings.json: {}", e);
                                    continue;
                                }
                            };

                            // Apply changes to the Automerge doc
                            let changed = {
                                let mut doc = self.settings.write().await;
                                let changed = doc.apply_json_changes(&json);
                                if changed {
                                    // Only persist the Automerge binary — do NOT write
                                    // the JSON mirror back, as serde_json formatting
                                    // differs from editors (e.g. arrays expand to one
                                    // element per line) which causes unwanted churn.
                                    let automerge_path = crate::default_settings_doc_path();
                                    if let Err(e) = doc.save_to_file(&automerge_path) {
                                        warn!("[settings-watch] Failed to save Automerge doc: {}", e);
                                    }
                                }
                                changed
                            };

                            if changed {
                                info!("[settings-watch] Applied external settings.json changes");
                                let _ = self.settings_changed.send(());

                                // Reset pool failure states so they retry immediately
                                // with the new settings (user may have fixed a typo)
                                let mut had_errors = false;
                                {
                                    let mut uv_pool = self.uv_pool.lock().await;
                                    if uv_pool.failure_state.consecutive_failures > 0 {
                                        info!(
                                            "[settings-watch] Resetting UV pool backoff (was {} failures)",
                                            uv_pool.failure_state.consecutive_failures
                                        );
                                        uv_pool.reset_failure_state();
                                        had_errors = true;
                                    }
                                }
                                {
                                    let mut conda_pool = self.conda_pool.lock().await;
                                    if conda_pool.failure_state.consecutive_failures > 0 {
                                        info!(
                                            "[settings-watch] Resetting Conda pool backoff (was {} failures)",
                                            conda_pool.failure_state.consecutive_failures
                                        );
                                        conda_pool.reset_failure_state();
                                        had_errors = true;
                                    }
                                }

                                // Broadcast cleared state if we had errors
                                if had_errors {
                                    self.update_pool_doc().await;
                                }
                            }
                        }
                        Err(errs) => {
                            warn!("[settings-watch] Watch error: {:?}", errs);
                        }
                    }
                }
                _ = self.shutdown_notify.notified() => {
                    if *self.shutdown.lock().await {
                        info!("[settings-watch] Shutting down");
                        break;
                    }
                }
            }
        }
    }

    /// Find and reuse existing runtimed environments from previous runs.
    async fn find_existing_environments(&self) {
        let cache_dir = &self.config.cache_dir;

        if !cache_dir.exists() {
            return;
        }

        let mut entries = match tokio::fs::read_dir(cache_dir).await {
            Ok(e) => e,
            Err(_) => return,
        };

        // Build the known prewarmed package lists so reused envs carry metadata.
        // These match the packages installed by create_uv_env/create_conda_env.
        let (uv_prewarmed, conda_prewarmed, pixi_prewarmed) = {
            let settings = self.settings.read().await;
            let synced = settings.get_all();

            let uv_pkgs = uv_prewarmed_packages(&synced.uv.default_packages);
            let conda_pkgs = conda_prewarmed_packages(&synced.conda.default_packages);
            let pixi_pkgs = pixi_prewarmed_packages(&synced.pixi.default_packages);

            (uv_pkgs, conda_pkgs, pixi_pkgs)
        };

        let mut uv_found = 0;
        let mut conda_found = 0;
        let mut pixi_found = 0;
        let mut orphans: Vec<PathBuf> = Vec::new();

        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            let env_path = entry.path();

            // Check for runtimed-uv-* directories
            if name.starts_with("runtimed-uv-") {
                #[cfg(target_os = "windows")]
                let python_path = env_path.join("Scripts").join("python.exe");
                #[cfg(not(target_os = "windows"))]
                let python_path = env_path.join("bin").join("python");

                if python_path.exists() {
                    let mut pool = self.uv_pool.lock().await;
                    if pool.available.len() < pool.target {
                        pool.available.push_back(PoolEntry {
                            env: PooledEnv {
                                env_type: EnvType::Uv,
                                venv_path: env_path.clone(),
                                python_path,
                                prewarmed_packages: uv_prewarmed.clone(),
                            },
                            created_at: Instant::now(),
                        });
                        uv_found += 1;
                    } else {
                        // Pool is full — this env is an orphan from a previous daemon run
                        orphans.push(env_path);
                    }
                } else {
                    // Invalid env, clean up
                    tokio::fs::remove_dir_all(&env_path).await.ok();
                }
            }
            // Check for runtimed-conda-* directories
            else if name.starts_with("runtimed-conda-") {
                #[cfg(target_os = "windows")]
                let python_path = env_path.join("python.exe");
                #[cfg(not(target_os = "windows"))]
                let python_path = env_path.join("bin").join("python");

                if python_path.exists() {
                    let mut pool = self.conda_pool.lock().await;
                    if pool.available.len() < pool.target {
                        pool.available.push_back(PoolEntry {
                            env: PooledEnv {
                                env_type: EnvType::Conda,
                                venv_path: env_path.clone(),
                                python_path,
                                prewarmed_packages: conda_prewarmed.clone(),
                            },
                            created_at: Instant::now(),
                        });
                        conda_found += 1;
                    } else {
                        orphans.push(env_path);
                    }
                } else {
                    tokio::fs::remove_dir_all(&env_path).await.ok();
                }
            }
            // Check for runtimed-pixi-* directories
            else if name.starts_with("runtimed-pixi-") {
                let venv_path = env_path.join(".pixi").join("envs").join("default");
                #[cfg(target_os = "windows")]
                let python_path = venv_path.join("python.exe");
                #[cfg(not(target_os = "windows"))]
                let python_path = venv_path.join("bin").join("python");

                if python_path.exists() && venv_path.join(".warmed").exists() {
                    let mut pool = self.pixi_pool.lock().await;
                    if pool.available.len() < pool.target {
                        pool.available.push_back(PoolEntry {
                            env: PooledEnv {
                                env_type: EnvType::Pixi,
                                venv_path,
                                python_path,
                                prewarmed_packages: pixi_prewarmed.clone(),
                            },
                            created_at: Instant::now(),
                        });
                        pixi_found += 1;
                    } else {
                        orphans.push(env_path);
                    }
                } else {
                    tokio::fs::remove_dir_all(&env_path).await.ok();
                }
            }
        }

        if uv_found > 0 || conda_found > 0 || pixi_found > 0 {
            info!(
                "[runtimed] Found {} existing UV, {} Conda, {} Pixi environments",
                uv_found, conda_found, pixi_found
            );
        }

        // Clean up orphaned pool envs in a background task so startup
        // isn't blocked when there are hundreds of stale directories.
        if !orphans.is_empty() {
            info!(
                "[runtimed] Scheduling cleanup of {} orphaned pool environments",
                orphans.len()
            );
            spawn_env_deletions(orphans);
        }
    }

    /// Route a connection based on its handshake frame.
    ///
    /// Every connection sends a JSON handshake as its first frame to declare
    /// which channel it wants. The daemon then dispatches to the appropriate
    /// handler.
    async fn route_connection<S>(self: Arc<Self>, mut stream: S) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    {
        // Read preamble + handshake with a timeout so that idle/stalled
        // connections don't hold resources.
        //
        // Backward compatibility: old clients (pre-2.0.0) send a length-prefixed
        // JSON handshake without the magic bytes preamble. We detect this by
        // peeking at the first byte: 0xC0 = new protocol (magic preamble),
        // 0x00 = old protocol (4-byte big-endian length prefix for any
        // reasonable handshake size). This allows the daemon upgrade to succeed
        // even when the old app is still verifying daemon health.
        let handshake_bytes = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            // Peek at first byte to detect protocol version
            let mut first_byte = [0u8; 1];
            tokio::io::AsyncReadExt::read_exact(&mut stream, &mut first_byte)
                .await
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::UnexpectedEof {
                        anyhow::anyhow!("connection closed before preamble")
                    } else {
                        anyhow::anyhow!("preamble read: {}", e)
                    }
                })?;

            if first_byte[0] == connection::MAGIC[0] {
                // New protocol: read remaining 4 bytes of preamble (3 more magic + 1 version)
                let mut rest = [0u8; 4];
                tokio::io::AsyncReadExt::read_exact(&mut stream, &mut rest)
                    .await
                    .map_err(|e| anyhow::anyhow!("preamble: {}", e))?;

                if rest[..3] != connection::MAGIC[1..] {
                    anyhow::bail!(
                        "invalid magic bytes: expected {:02X?}, got {:02X?}",
                        connection::MAGIC,
                        [&first_byte[..], &rest[..3]].concat()
                    );
                }

                let version = rest[3];
                if version != connection::PROTOCOL_VERSION as u8 {
                    anyhow::bail!(
                        "protocol version mismatch: expected {}, got {}",
                        connection::PROTOCOL_VERSION,
                        version
                    );
                }

                // Read the JSON handshake frame
                connection::recv_control_frame(&mut stream)
                    .await
                    .context("handshake read error")?
                    .ok_or_else(|| anyhow::anyhow!("connection closed before handshake"))
            } else {
                // Legacy protocol (pre-2.0.0): first byte is part of a 4-byte
                // big-endian length prefix. Read remaining 3 length bytes.
                log::debug!("[runtimed] Legacy client detected (no magic preamble)");
                let mut len_rest = [0u8; 3];
                tokio::io::AsyncReadExt::read_exact(&mut stream, &mut len_rest)
                    .await
                    .map_err(|e| anyhow::anyhow!("legacy length read: {}", e))?;

                let len_bytes = [first_byte[0], len_rest[0], len_rest[1], len_rest[2]];
                let len = u32::from_be_bytes(len_bytes) as usize;

                if len > 64 * 1024 {
                    anyhow::bail!("legacy handshake frame too large: {} bytes", len);
                }

                let mut buf = vec![0u8; len];
                tokio::io::AsyncReadExt::read_exact(&mut stream, &mut buf)
                    .await
                    .map_err(|e| anyhow::anyhow!("legacy handshake read: {}", e))?;

                Ok(buf)
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("handshake timeout (10s)"))??;
        let handshake: Handshake = serde_json::from_slice(&handshake_bytes)?;

        match handshake {
            Handshake::Pool => self.handle_pool_connection(stream).await,
            Handshake::SettingsSync => {
                let (reader, writer) = tokio::io::split(stream);
                let changed_tx = self.settings_changed.clone();
                let changed_rx = self.settings_changed.subscribe();
                crate::sync_server::handle_settings_sync_connection(
                    reader,
                    writer,
                    self.settings.clone(),
                    changed_tx,
                    changed_rx,
                )
                .await
            }
            Handshake::NotebookSync {
                notebook_id,
                protocol,
                working_dir,
                initial_metadata,
            } => {
                info!(
                    "[runtimed] NotebookSync requested for {} (protocol: {}, working_dir: {:?})",
                    notebook_id,
                    protocol.as_deref().unwrap_or("v2"),
                    working_dir
                );
                let docs_dir = self.config.notebook_docs_dir.clone();
                let room = {
                    let mut rooms = self.notebook_rooms.lock().await;
                    crate::notebook_sync_server::get_or_create_room(
                        &mut rooms,
                        &notebook_id,
                        &docs_dir,
                        self.blob_store.clone(),
                    )
                };
                let (reader, writer) = tokio::io::split(stream);
                // Get user's default runtime and Python env preference for auto-launch
                let settings = self.settings.read().await.get_all();
                let default_runtime = settings.default_runtime;
                let default_python_env = settings.default_python_env;
                // Convert working_dir String to PathBuf
                let working_dir_path = working_dir.map(std::path::PathBuf::from);
                crate::notebook_sync_server::handle_notebook_sync_connection(
                    reader,
                    writer,
                    room,
                    self.notebook_rooms.clone(),
                    notebook_id,
                    default_runtime,
                    default_python_env,
                    self.clone(),
                    working_dir_path,
                    initial_metadata,
                    false, // Send ProtocolCapabilities for legacy NotebookSync handshake
                    None,  // No streaming load for legacy handshake
                    false, // Not a newly-created notebook at path
                )
                .await
            }
            Handshake::Blob => self.handle_blob_connection(stream).await,
            Handshake::OpenNotebook { path } => self.handle_open_notebook(stream, path).await,
            Handshake::CreateNotebook {
                runtime,
                working_dir,
                notebook_id,
            } => {
                self.handle_create_notebook(stream, runtime, working_dir, notebook_id)
                    .await
            }
            Handshake::RuntimeAgent {
                notebook_id,
                runtime_agent_id,
                blob_root: _,
            } => {
                info!(
                    "[runtimed] Runtime agent connecting via socket: notebook={} runtime_agent={}",
                    notebook_id, runtime_agent_id
                );
                let room = {
                    let rooms = self.notebook_rooms.lock().await;
                    rooms.get(&notebook_id).cloned()
                };
                match room {
                    Some(room) => {
                        let (reader, writer) = tokio::io::split(stream);
                        crate::notebook_sync_server::handle_runtime_agent_sync_connection(
                            reader,
                            writer,
                            room,
                            notebook_id,
                            runtime_agent_id,
                        )
                        .await;
                        Ok(())
                    }
                    None => {
                        warn!(
                            "[runtimed] Agent connected to unknown room: {}",
                            notebook_id
                        );
                        Ok(())
                    }
                }
            }
        }
    }

    /// Handle an OpenNotebook connection.
    ///
    /// Daemon loads the .ipynb file, derives notebook_id, creates room, populates doc.
    /// If the file doesn't exist, creates a new empty notebook at that path.
    /// Returns NotebookConnectionInfo, then continues as normal notebook sync.
    async fn handle_open_notebook<S>(self: Arc<Self>, stream: S, path: String) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        use crate::connection::{
            send_json_frame, NotebookConnectionInfo, PROTOCOL_V2, PROTOCOL_VERSION,
        };

        info!("[runtimed] OpenNotebook requested for {}", path);

        // Helper to send error response to client
        async fn send_error_response<W: AsyncWrite + Unpin>(
            writer: &mut W,
            error: String,
        ) -> anyhow::Result<()> {
            let response = NotebookConnectionInfo {
                protocol: PROTOCOL_V2.to_string(),
                protocol_version: Some(PROTOCOL_VERSION),
                daemon_version: Some(crate::daemon_version().to_string()),
                notebook_id: String::new(),
                cell_count: 0,
                needs_trust_approval: false,
                error: Some(error),
            };
            send_json_frame(writer, &response).await?;
            Ok(())
        }

        // Check if file exists before canonicalizing (canonicalize fails for non-existent paths)
        let mut path_buf = std::path::PathBuf::from(&path);
        let file_exists = match tokio::fs::metadata(&path_buf).await {
            Ok(meta) if meta.is_dir() => {
                // Directory path — create untitled notebook with this as working dir
                info!(
                    "[runtimed] Path {} is a directory, creating untitled notebook with working_dir",
                    path
                );
                let dir_path = match path_buf.canonicalize() {
                    Ok(p) => p.to_string_lossy().to_string(),
                    Err(_) => path_buf.to_string_lossy().to_string(),
                };
                // Verify directory is writable using a uniquely-named temp file.
                // create_new uses O_EXCL so it can never clobber an existing file.
                let probe = path_buf.join(format!(".runtimed_probe_{}", uuid::Uuid::new_v4()));
                match std::fs::File::create_new(&probe) {
                    Ok(_) => {
                        let _ = std::fs::remove_file(&probe);
                    }
                    Err(e) => {
                        let (_reader, mut writer) = tokio::io::split(stream);
                        send_error_response(
                            &mut writer,
                            format!("Directory '{}' is not writable: {}", path, e),
                        )
                        .await?;
                        return Ok(());
                    }
                }
                let settings = self.settings.read().await.get_all();
                return self
                    .handle_create_notebook(
                        stream,
                        settings.default_runtime.to_string(),
                        Some(dir_path),
                        None,
                    )
                    .await;
            }
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // For new files, ensure .ipynb extension
                if path_buf.extension().is_none_or(|ext| ext != "ipynb") {
                    let mut new_path = path_buf.as_os_str().to_owned();
                    new_path.push(".ipynb");
                    path_buf = std::path::PathBuf::from(new_path);
                    info!(
                        "[runtimed] File {} does not exist, will create new notebook at {}",
                        path,
                        path_buf.display()
                    );
                } else {
                    info!(
                        "[runtimed] File {} does not exist, will create new notebook",
                        path
                    );
                }
                false
            }
            Err(e) => {
                // Permission denied, I/O error, etc. - return error to client
                let (_reader, mut writer) = tokio::io::split(stream);
                send_error_response(
                    &mut writer,
                    format!("Cannot access notebook '{}': {}", path, e),
                )
                .await?;
                return Ok(());
            }
        };

        // Derive notebook_id from path
        // For existing files: canonicalize for stable cross-process identity
        // For new files: use absolute path (canonicalize would fail)
        let notebook_id = if file_exists {
            match path_buf.canonicalize() {
                Ok(canonical) => canonical.to_string_lossy().to_string(),
                Err(e) => {
                    // Canonicalize failed even though file exists (permission/symlink issues)
                    let (_reader, mut writer) = tokio::io::split(stream);
                    send_error_response(
                        &mut writer,
                        format!("Cannot resolve notebook path '{}': {}", path, e),
                    )
                    .await?;
                    return Ok(());
                }
            }
        } else {
            std::path::absolute(&path_buf)
                .unwrap_or_else(|_| path_buf.clone())
                .to_string_lossy()
                .to_string()
        };

        // Get or create room for this notebook.
        // First check if an existing room (including UUID-keyed ephemeral rooms)
        // already owns this path — prevents duplicate rooms and re-key collisions.
        let docs_dir = self.config.notebook_docs_dir.clone();
        let room = {
            let mut rooms = self.notebook_rooms.lock().await;
            if let Some(existing) =
                crate::notebook_sync_server::find_room_by_notebook_path(&rooms, &notebook_id)
            {
                existing
            } else {
                crate::notebook_sync_server::get_or_create_room(
                    &mut rooms,
                    &notebook_id,
                    &docs_dir,
                    self.blob_store.clone(),
                )
            }
        };

        // Get settings for sync and auto-launch (needed for both new and existing notebooks)
        let settings = self.settings.read().await.get_all();
        let default_runtime = settings.default_runtime;
        let default_python_env = settings.default_python_env;

        // Check whether this connection needs to stream-load the notebook
        // from disk, or create a new empty notebook.
        // Track if we created a new notebook at this path (for auto-launch logic)
        let mut created_new_at_path = false;
        let (cell_count, needs_load) = if !file_exists {
            // File doesn't exist - create empty notebook in the doc
            let mut doc = room.doc.write().await;
            if doc.cell_count() == 0 {
                match crate::notebook_sync_server::create_empty_notebook(
                    &mut doc,
                    &default_runtime.to_string(),
                    default_python_env.clone(),
                    Some(&notebook_id),
                ) {
                    Ok(_cell_id) => {
                        info!("[runtimed] Created new notebook at {}", path);
                        created_new_at_path = true;
                    }
                    Err(e) => {
                        error!(
                            "[runtimed] Failed to create new notebook at {}: {}",
                            path, e
                        );
                        drop(doc);
                        let (_reader, mut writer) = tokio::io::split(stream);
                        send_error_response(
                            &mut writer,
                            format!("Failed to create notebook '{}': {}", path, e),
                        )
                        .await?;
                        return Ok(());
                    }
                }
            }
            (doc.cell_count(), None) // No streaming load needed
        } else {
            let doc = room.doc.read().await;
            let existing_count = doc.cell_count();
            if existing_count == 0 && !room.is_loading.load(std::sync::atomic::Ordering::Acquire) {
                // Room is empty and nobody is loading yet — this connection
                // will do the streaming load inside the sync loop.
                info!(
                    "[runtimed] Room for {} is empty, deferring streaming load",
                    path
                );
                (0, Some(path_buf.clone()))
            } else {
                info!(
                    "[runtimed] Room for {} has {} cells (joining existing{})",
                    path,
                    existing_count,
                    if room.is_loading.load(std::sync::atomic::Ordering::Acquire) {
                        ", load in progress"
                    } else {
                        ""
                    }
                );
                (existing_count, None)
            }
        };

        // Get trust state (already verified during room creation)
        let trust_state = room.trust_state.read().await;
        let needs_trust_approval = !matches!(
            trust_state.status,
            runt_trust::TrustStatus::Trusted | runt_trust::TrustStatus::NoDependencies
        );

        // Send NotebookConnectionInfo response
        let (reader, mut writer) = tokio::io::split(stream);
        let response = NotebookConnectionInfo {
            protocol: PROTOCOL_V2.to_string(),
            protocol_version: Some(PROTOCOL_VERSION),
            daemon_version: Some(crate::daemon_version().to_string()),
            notebook_id: notebook_id.clone(),
            cell_count,
            needs_trust_approval,
            error: None,
        };
        send_json_frame(&mut writer, &response).await?;

        // working_dir derived from path's parent directory
        let working_dir_path = path_buf.parent().map(|p| p.to_path_buf());

        // Drop the trust_state lock before continuing
        drop(trust_state);

        // Continue with normal notebook sync (handles auto-launch internally).
        // If needs_load is Some, the sync loop will stream cells from disk
        // before entering the steady-state select loop.
        crate::notebook_sync_server::handle_notebook_sync_connection(
            reader,
            writer,
            room,
            self.notebook_rooms.clone(),
            notebook_id,
            default_runtime,
            default_python_env,
            self.clone(),
            working_dir_path,
            None, // No initial_metadata - doc is already populated
            true, // Skip ProtocolCapabilities - already sent in NotebookConnectionInfo
            needs_load,
            created_new_at_path, // Enable auto-launch for notebooks created at non-existent paths
        )
        .await
    }

    /// Handle a CreateNotebook connection.
    ///
    /// Daemon creates empty room with zero cells, generates env_id as notebook_id.
    /// Returns NotebookConnectionInfo, then continues as normal notebook sync.
    async fn handle_create_notebook<S>(
        self: Arc<Self>,
        stream: S,
        runtime: String,
        working_dir: Option<String>,
        notebook_id_hint: Option<String>,
    ) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        use crate::connection::{
            send_json_frame, NotebookConnectionInfo, PROTOCOL_V2, PROTOCOL_VERSION,
        };

        info!(
            "[runtimed] CreateNotebook requested (runtime={}, working_dir={:?}, notebook_id_hint={:?})",
            runtime, working_dir, notebook_id_hint
        );

        // Get settings for default Python env preference
        let settings = self.settings.read().await.get_all();
        let default_python_env = settings.default_python_env;
        let default_runtime = settings.default_runtime;

        // Use provided notebook_id (session restore) or generate a new UUID
        let notebook_id = notebook_id_hint.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        // Create room for this notebook
        let docs_dir = self.config.notebook_docs_dir.clone();
        let room = {
            let mut rooms = self.notebook_rooms.lock().await;
            crate::notebook_sync_server::get_or_create_room(
                &mut rooms,
                &notebook_id,
                &docs_dir,
                self.blob_store.clone(),
            )
        };

        // Populate the room's doc with the empty notebook content — but only if the
        // room is empty. If a persisted doc was loaded (session restore with notebook_id
        // hint), the room already has cells and we skip creation.
        let cell_count = {
            let mut doc = room.doc.write().await;
            if doc.cell_count() > 0 {
                // Room already has content (loaded from persisted doc)
                info!(
                    "[runtimed] Room {} already has {} cells (restored from persisted doc)",
                    notebook_id,
                    doc.cell_count()
                );
                doc.cell_count()
            } else {
                match crate::notebook_sync_server::create_empty_notebook(
                    &mut doc,
                    &runtime,
                    default_python_env.clone(),
                    Some(&notebook_id),
                ) {
                    Ok(_) => doc.cell_count(),
                    Err(e) => {
                        // Drop the doc lock before removing room
                        drop(doc);
                        // Remove the room to prevent stale state (consistency with OpenNotebook)
                        {
                            let mut rooms = self.notebook_rooms.lock().await;
                            rooms.remove(&notebook_id);
                            info!(
                                "[runtimed] Removed room {} after create failure",
                                notebook_id
                            );
                        }
                        let (mut reader, mut writer) = tokio::io::split(stream);
                        let response = NotebookConnectionInfo {
                            protocol: PROTOCOL_V2.to_string(),
                            protocol_version: Some(PROTOCOL_VERSION),
                            daemon_version: Some(crate::daemon_version().to_string()),
                            notebook_id: String::new(),
                            cell_count: 0,
                            needs_trust_approval: false,
                            error: Some(format!("Failed to create notebook: {}", e)),
                        };
                        send_json_frame(&mut writer, &response).await?;
                        let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
                        return Ok(());
                    }
                }
            }
        };

        // Send NotebookConnectionInfo response
        // New notebooks have no deps, so no trust approval needed
        let (reader, mut writer) = tokio::io::split(stream);
        let response = NotebookConnectionInfo {
            protocol: PROTOCOL_V2.to_string(),
            protocol_version: Some(PROTOCOL_VERSION),
            daemon_version: Some(crate::daemon_version().to_string()),
            notebook_id: notebook_id.clone(),
            cell_count,
            needs_trust_approval: false,
            error: None,
        };
        send_json_frame(&mut writer, &response).await?;

        // working_dir for untitled notebooks (used for project file detection)
        let working_dir_path = working_dir.map(std::path::PathBuf::from);

        // Use the explicitly requested runtime for auto-launch, not the system default.
        // This ensures create_notebook(runtime="deno") actually launches a Deno kernel.
        let requested_runtime: crate::runtime::Runtime = runtime.parse().unwrap_or(default_runtime);

        // Continue with normal notebook sync
        crate::notebook_sync_server::handle_notebook_sync_connection(
            reader,
            writer,
            room,
            self.notebook_rooms.clone(),
            notebook_id,
            requested_runtime,
            default_python_env,
            self.clone(),
            working_dir_path,
            None,  // No initial_metadata - doc is already populated
            true,  // Skip ProtocolCapabilities - already sent in NotebookConnectionInfo
            None,  // No streaming load - doc was just created with empty cell
            false, // UUID-based new notebook, handled by is_new_notebook check
        )
        .await
    }

    /// Handle a pool channel connection (framed JSON request/response).
    async fn handle_pool_connection<S>(self: Arc<Self>, mut stream: S) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        loop {
            let request: Request = match connection::recv_json_frame(&mut stream).await? {
                Some(req) => req,
                None => break, // Connection closed
            };

            let response = self.clone().handle_request(request).await;
            connection::send_json_frame(&mut stream, &response).await?;
        }

        Ok(())
    }

    /// Handle a blob channel connection.
    ///
    /// Protocol:
    /// - `{"action":"store","media_type":"..."}` followed by a raw binary frame
    ///   -> `{"hash":"..."}`
    /// - `{"action":"get_port"}` -> `{"port":N}`
    async fn handle_blob_connection<S>(self: Arc<Self>, mut stream: S) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        loop {
            let request: BlobRequest = match connection::recv_json_frame(&mut stream).await? {
                Some(req) => req,
                None => break,
            };

            match request {
                BlobRequest::Store { media_type } => {
                    // Next frame is the raw binary blob data
                    let data = match connection::recv_frame(&mut stream).await? {
                        Some(d) => d,
                        None => break,
                    };

                    let response = match self.blob_store.put(&data, &media_type).await {
                        Ok(hash) => BlobResponse::Stored { hash },
                        Err(e) => BlobResponse::Error {
                            error: e.to_string(),
                        },
                    };
                    connection::send_json_frame(&mut stream, &response).await?;
                }
                BlobRequest::GetPort => {
                    let port = self.blob_port.lock().await;
                    let response = match *port {
                        Some(p) => BlobResponse::Port { port: p },
                        None => BlobResponse::Error {
                            error: "blob server not running".to_string(),
                        },
                    };
                    connection::send_json_frame(&mut stream, &response).await?;
                }
            }
        }

        Ok(())
    }

    /// Take a UV environment from the pool for kernel launching.
    ///
    /// Returns `Some(PooledEnv)` if an environment is available, `None` otherwise.
    /// Automatically triggers replenishment when an environment is taken.
    pub async fn take_uv_env(self: &Arc<Self>) -> Option<PooledEnv> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);

        loop {
            let (env, stale_paths) = self.uv_pool.lock().await.take();
            spawn_env_deletions(stale_paths);

            if let Some(e) = env {
                info!(
                    "[runtimed] Took UV env for kernel launch: {:?}",
                    e.venv_path
                );
                let daemon = self.clone();
                tokio::spawn(async move {
                    daemon.create_uv_env().await;
                });
                return Some(e);
            }

            if self.config.uv_pool_size == 0 {
                return None;
            }

            let (warming, can_retry, retry_in_secs) = {
                let mut pool = self.uv_pool.lock().await;
                let (_, warming) = pool.stats();
                let can_retry = pool.should_retry();
                let retry_in_secs = pool.retry_in_secs();

                if warming == 0 && can_retry {
                    pool.mark_warming(1);
                    let daemon = self.clone();
                    tokio::spawn(async move {
                        daemon.create_uv_env().await;
                    });
                    (1, can_retry, retry_in_secs)
                } else {
                    (warming, can_retry, retry_in_secs)
                }
            };

            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                warn!("[runtimed] Timed out waiting for UV pool env");
                return None;
            }

            let wait_for = if warming > 0 {
                remaining
            } else {
                remaining.min(std::time::Duration::from_secs(retry_in_secs.max(1)))
            };

            info!("[runtimed] UV pool empty, waiting for warming ({warming} in progress, retry ready: {can_retry})...");

            tokio::select! {
                _ = tokio::time::sleep(wait_for) => {
                    if wait_for == remaining {
                        warn!("[runtimed] Timed out waiting for UV pool env");
                        return None;
                    }
                    continue;
                }
                _ = self.pool_ready_uv.notified() => continue,
                _ = self.shutdown_notify.notified() => return None,
            }
        }
    }

    /// Take a Conda environment from the pool for kernel launching.
    ///
    /// Returns `Some(PooledEnv)` if an environment is available, `None` otherwise.
    /// Automatically triggers replenishment when an environment is taken.
    pub async fn take_conda_env(self: &Arc<Self>) -> Option<PooledEnv> {
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(90);

        loop {
            let (env, stale_paths) = self.conda_pool.lock().await.take();
            spawn_env_deletions(stale_paths);

            if let Some(e) = env {
                info!(
                    "[runtimed] Took Conda env for kernel launch: {:?}",
                    e.venv_path
                );
                let daemon = self.clone();
                tokio::spawn(async move {
                    daemon.replenish_conda_env().await;
                });
                return Some(e);
            }

            if self.config.conda_pool_size == 0 {
                return None;
            }

            let (warming, can_retry, retry_in_secs) = {
                let mut pool = self.conda_pool.lock().await;
                let (_, warming) = pool.stats();
                let can_retry = pool.should_retry();
                let retry_in_secs = pool.retry_in_secs();

                if warming == 0 && can_retry {
                    pool.mark_warming(1);
                    let daemon = self.clone();
                    tokio::spawn(async move {
                        daemon.create_conda_env().await;
                    });
                    (1, can_retry, retry_in_secs)
                } else {
                    (warming, can_retry, retry_in_secs)
                }
            };

            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                warn!("[runtimed] Timed out waiting for Conda pool env");
                return None;
            }

            let wait_for = if warming > 0 {
                remaining
            } else {
                remaining.min(std::time::Duration::from_secs(retry_in_secs.max(1)))
            };

            info!("[runtimed] Conda pool empty, waiting for warming ({warming} in progress, retry ready: {can_retry})...");

            tokio::select! {
                _ = tokio::time::sleep(wait_for) => {
                    if wait_for == remaining {
                        warn!("[runtimed] Timed out waiting for Conda pool env");
                        return None;
                    }
                    continue;
                }
                _ = self.pool_ready_conda.notified() => continue,
                _ = self.shutdown_notify.notified() => return None,
            }
        }
    }

    /// Take a Pixi environment from the pool for kernel launching.
    ///
    /// Returns `Some(PooledEnv)` if an environment is available, `None` otherwise.
    pub async fn take_pixi_env(self: &Arc<Self>) -> Option<PooledEnv> {
        let (env, stale_paths) = self.pixi_pool.lock().await.take();
        spawn_env_deletions(stale_paths);
        if let Some(ref e) = env {
            info!(
                "[runtimed] Took Pixi env for kernel launch: {:?}",
                e.venv_path
            );
            let daemon = self.clone();
            tokio::spawn(async move {
                daemon.replenish_pixi_env().await;
            });
        }
        env
    }

    /// Handle a single request.
    async fn handle_request(self: Arc<Self>, request: Request) -> Response {
        match request {
            Request::Take { env_type } => {
                let env = match env_type {
                    EnvType::Uv => self.take_uv_env().await,
                    EnvType::Conda => self.take_conda_env().await,
                    EnvType::Pixi => self.take_pixi_env().await,
                };

                match env {
                    Some(env) => {
                        self.update_pool_doc().await;
                        Response::Env { env }
                    }
                    None => {
                        debug!("[runtimed] Pool miss for {}", env_type);
                        Response::Empty
                    }
                }
            }

            Request::Return { env } => {
                // Return an environment to the pool (e.g., if notebook closed without using it)
                match env.env_type {
                    EnvType::Uv => {
                        let mut pool = self.uv_pool.lock().await;
                        if pool.available.len() < pool.target {
                            pool.available.push_back(PoolEntry {
                                env: env.clone(),
                                created_at: Instant::now(),
                            });
                            debug!("[runtimed] Returned UV env: {:?}", env.venv_path);
                        } else {
                            // Pool is full, clean up
                            tokio::fs::remove_dir_all(&env.venv_path).await.ok();
                        }
                    }
                    EnvType::Conda => {
                        let mut pool = self.conda_pool.lock().await;
                        if pool.available.len() < pool.target {
                            pool.available.push_back(PoolEntry {
                                env: env.clone(),
                                created_at: Instant::now(),
                            });
                            debug!("[runtimed] Returned Conda env: {:?}", env.venv_path);
                        } else {
                            tokio::fs::remove_dir_all(&env.venv_path).await.ok();
                        }
                    }
                    EnvType::Pixi => {
                        let mut pool = self.pixi_pool.lock().await;
                        if pool.available.len() < pool.target {
                            pool.available.push_back(PoolEntry {
                                env: env.clone(),
                                created_at: Instant::now(),
                            });
                            debug!("[runtimed] Returned Pixi env: {:?}", env.venv_path);
                        } else {
                            tokio::fs::remove_dir_all(&env.venv_path).await.ok();
                        }
                    }
                }
                self.update_pool_doc().await;
                Response::Returned
            }

            Request::Status => {
                let state = self.pool_doc.read().await.read_state();
                Response::Stats { state }
            }

            Request::Ping => Response::Pong {
                protocol_version: Some(notebook_protocol::connection::PROTOCOL_VERSION),
                daemon_version: Some(crate::daemon_version().to_string()),
            },

            Request::Shutdown => {
                self.trigger_shutdown().await;
                Response::ShuttingDown
            }

            Request::FlushPool => {
                info!("[runtimed] Flushing all pooled environments");

                // Drain UV pool and delete env directories
                {
                    let mut pool = self.uv_pool.lock().await;
                    let entries: Vec<_> = pool.available.drain(..).collect();
                    for entry in entries {
                        info!("[runtimed] Removing UV env: {:?}", entry.env.venv_path);
                        tokio::fs::remove_dir_all(&entry.env.venv_path).await.ok();
                    }
                }

                // Drain Conda pool and delete env directories
                {
                    let mut pool = self.conda_pool.lock().await;
                    let entries: Vec<_> = pool.available.drain(..).collect();
                    for entry in entries {
                        info!("[runtimed] Removing Conda env: {:?}", entry.env.venv_path);
                        tokio::fs::remove_dir_all(&entry.env.venv_path).await.ok();
                    }
                }

                // Warming loops will detect the deficit and rebuild on their next iteration
                self.update_pool_doc().await;
                Response::Flushed
            }

            Request::InspectNotebook { notebook_id } => {
                info!("[runtimed] Inspecting notebook: {}", notebook_id);

                // First try to get from an active room
                let rooms = self.notebook_rooms.lock().await;
                if let Some(room) = rooms.get(&notebook_id) {
                    let doc = room.doc.read().await;
                    let cells = doc.get_cells();
                    let kernel_info = room.kernel_info().await.map(|(kt, es, status)| {
                        crate::protocol::NotebookKernelInfo {
                            kernel_type: kt,
                            env_source: es,
                            status,
                        }
                    });
                    Response::NotebookState {
                        notebook_id,
                        cells,
                        source: "live_room".to_string(),
                        kernel_info,
                    }
                } else {
                    // No active room - try to load from persisted file
                    drop(rooms); // Release lock before disk I/O
                    let filename = crate::notebook_doc::notebook_doc_filename(&notebook_id);
                    let persist_path = self.config.notebook_docs_dir.join(filename);
                    if persist_path.exists() {
                        match std::fs::read(&persist_path) {
                            Ok(data) => match crate::notebook_doc::NotebookDoc::load(&data) {
                                Ok(doc) => {
                                    let cells = doc.get_cells();
                                    Response::NotebookState {
                                        notebook_id,
                                        cells,
                                        source: "persisted_file".to_string(),
                                        kernel_info: None,
                                    }
                                }
                                Err(e) => Response::Error {
                                    message: format!("Failed to parse Automerge doc: {}", e),
                                },
                            },
                            Err(e) => Response::Error {
                                message: format!("Failed to read persisted file: {}", e),
                            },
                        }
                    } else {
                        Response::Error {
                            message: format!(
                                "Notebook not found: no active room and no persisted file at {:?}",
                                persist_path
                            ),
                        }
                    }
                }
            }

            Request::ListRooms => {
                let rooms = self.notebook_rooms.lock().await;
                let mut room_infos = Vec::new();
                for (notebook_id, room) in rooms.iter() {
                    // Get kernel info if available
                    let (kernel_type, env_source, kernel_status) = room
                        .kernel_info()
                        .await
                        .map(|(kt, es, st)| (Some(kt), Some(es), Some(st)))
                        .unwrap_or((None, None, None));

                    room_infos.push(crate::protocol::RoomInfo {
                        notebook_id: notebook_id.clone(),
                        active_peers: room.active_peers.load(std::sync::atomic::Ordering::Relaxed),
                        had_peers: room.had_peers.load(std::sync::atomic::Ordering::Relaxed),
                        has_kernel: room.has_kernel().await,
                        kernel_type,
                        env_source,
                        kernel_status,
                    });
                }
                Response::RoomsList { rooms: room_infos }
            }

            Request::ShutdownNotebook { notebook_id } => {
                let mut rooms = self.notebook_rooms.lock().await;
                if let Some(room) = rooms.remove(&notebook_id) {
                    // Shut down runtime agent via RPC before dropping handle.
                    // RuntimeAgentHandle doesn't own the Child (it's in a background
                    // task), so dropping the handle alone doesn't kill it.
                    {
                        let has_runtime_agent =
                            room.runtime_agent_request_tx.lock().await.is_some();
                        if has_runtime_agent {
                            info!(
                                "[runtimed] Shutting down runtime agent for notebook: {}",
                                notebook_id
                            );
                            let _ = crate::notebook_sync_server::send_runtime_agent_request(
                                &room,
                                notebook_protocol::protocol::RuntimeAgentRequest::ShutdownKernel,
                            )
                            .await;
                        }
                        let mut ra_guard = room.runtime_agent_handle.lock().await;
                        *ra_guard = None;
                        let mut tx = room.runtime_agent_request_tx.lock().await;
                        *tx = None;
                    }
                    info!("[runtimed] Evicted room for notebook: {}", notebook_id);
                    Response::NotebookShutdown { found: true }
                } else {
                    Response::NotebookShutdown { found: false }
                }
            }

            Request::ActiveEnvPaths => {
                let paths: Vec<PathBuf> =
                    self.collect_active_env_paths().await.into_iter().collect();
                Response::ActiveEnvPaths { paths }
            }
        }
    }

    /// Collect env paths from all running kernels to protect from GC eviction.
    async fn collect_active_env_paths(&self) -> std::collections::HashSet<PathBuf> {
        let mut paths = std::collections::HashSet::new();
        let rooms = self.notebook_rooms.lock().await;
        for room in rooms.values() {
            // Check runtime-agent-backed kernel
            if let Some(ref env_path) = *room.runtime_agent_env_path.read().await {
                paths.insert(env_path.clone());
            }
        }
        paths
    }

    /// Background GC loop for content-addressed environment caches.
    ///
    /// Runs once after a 60-second startup delay, then every 6 hours.
    /// Evicts stale cached environments from the global UV, Conda, and inline-env
    /// cache directories based on `env_cache_max_age_secs` and `env_cache_max_count`.
    async fn env_gc_loop(&self) {
        // Wait for warming loops to settle before first GC run
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;

        let max_age = std::time::Duration::from_secs(self.config.env_cache_max_age_secs);
        let max_count = self.config.env_cache_max_count;

        // Directories to GC. These are the global content-addressed caches
        // used by kernel-env (not per-worktree pool dirs).
        let cache_dirs = [
            kernel_env::uv::default_cache_dir_uv(),
            kernel_env::conda::default_cache_dir_conda(),
            // inline-envs: same parent as uv cache but under "inline-envs"
            dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join("runt")
                .join("inline-envs"),
        ];

        loop {
            // Collect env paths from all running kernels so GC won't evict them
            let in_use = self.collect_active_env_paths().await;

            let mut total_evicted = 0;
            for dir in &cache_dirs {
                match kernel_env::gc::evict_stale_envs(dir, max_age, max_count, &in_use).await {
                    Ok(deleted) => total_evicted += deleted.len(),
                    Err(e) => {
                        warn!("[runtimed] GC failed for {:?}: {}", dir, e);
                    }
                }
            }
            if total_evicted > 0 {
                info!(
                    "[runtimed] GC cycle complete: evicted {} cached environments",
                    total_evicted
                );
            }

            // Clean up stale worktree state directories
            let worktrees_dir = dirs::cache_dir()
                .unwrap_or_else(|| PathBuf::from("/tmp"))
                .join(crate::cache_namespace())
                .join("worktrees");

            if let Ok(total_cleaned) = Self::cleanup_stale_worktrees(&worktrees_dir).await {
                if total_cleaned > 0 {
                    info!(
                        "[runtimed] Cleaned up {} stale worktree directories",
                        total_cleaned
                    );
                }
            }

            // Run every 6 hours
            tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
        }
    }

    /// Clean up worktree state directories where the original git worktree
    /// path no longer exists and the daemon.json is older than 7 days.
    async fn cleanup_stale_worktrees(worktrees_dir: &std::path::Path) -> anyhow::Result<usize> {
        if !worktrees_dir.exists() {
            return Ok(0);
        }

        let mut entries = tokio::fs::read_dir(worktrees_dir).await?;
        let mut cleaned = 0;
        let grace_period = std::time::Duration::from_secs(7 * 24 * 3600); // 7 days

        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            let daemon_json = path.join("daemon.json");
            if !daemon_json.exists() {
                continue;
            }

            // Check if daemon.json is old enough (grace period)
            let mtime = tokio::fs::metadata(&daemon_json)
                .await
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let age = std::time::SystemTime::now()
                .duration_since(mtime)
                .unwrap_or_default();
            if age < grace_period {
                continue;
            }

            // Read daemon.json to get the worktree_path
            let contents = match tokio::fs::read_to_string(&daemon_json).await {
                Ok(c) => c,
                Err(_) => continue,
            };
            let info: serde_json::Value = match serde_json::from_str(&contents) {
                Ok(v) => v,
                Err(_) => continue,
            };

            // Check if the original worktree path still exists
            if let Some(wt_path) = info.get("worktree_path").and_then(|v| v.as_str()) {
                if std::path::Path::new(wt_path).exists() {
                    continue; // Worktree still exists, skip
                }
            } else {
                continue; // No worktree_path field, not a dev worktree
            }

            // Worktree path is gone and daemon.json is old — safe to delete
            info!("[runtimed] Removing stale worktree state: {:?}", path);
            if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                warn!(
                    "[runtimed] Failed to remove stale worktree {:?}: {}",
                    path, e
                );
            } else {
                cleaned += 1;
            }
        }

        Ok(cleaned)
    }

    /// UV warming loop - maintains the UV pool.
    async fn uv_warming_loop(&self) {
        // Bootstrap uv via rattler if not on PATH (this is cached via OnceCell)
        let uv_path = match kernel_launch::tools::get_uv_path().await {
            Ok(path) => {
                info!("[runtimed] UV warming using uv at: {:?}", path);
                path
            }
            Err(e) => {
                warn!(
                    "[runtimed] Failed to bootstrap uv: {}, UV warming disabled",
                    e
                );
                return;
            }
        };

        info!("[runtimed] Starting UV warming loop");
        // Store uv_path for use in create_uv_env - it's cached so get_uv_path() is instant
        let _ = uv_path;

        loop {
            if *self.shutdown.lock().await {
                break;
            }

            let (deficit, should_retry, backoff_info) = {
                let mut pool = self.uv_pool.lock().await;
                let d = pool.deficit();
                let retry = pool.should_retry();
                let info = if pool.failure_state.consecutive_failures > 0 {
                    Some((
                        pool.failure_state.consecutive_failures,
                        pool.backoff_delay().as_secs(),
                        pool.failure_state.failed_package.clone(),
                    ))
                } else {
                    None
                };

                if d > 0 && retry {
                    pool.mark_warming(d);
                }
                (d, retry, info)
            };

            if deficit > 0 {
                if should_retry {
                    self.update_pool_doc().await;
                    info!("[runtimed] Creating {} UV environments", deficit);
                    for _ in 0..deficit {
                        self.create_uv_env().await;
                    }
                } else if let Some((failures, backoff_secs, failed_pkg)) = backoff_info {
                    // In backoff period - log why we're waiting
                    if let Some(pkg) = failed_pkg {
                        warn!(
                            "[runtimed] UV pool in backoff: {} consecutive failures installing '{}', \
                             waiting {}s before retry. Check uv.default_packages in settings.",
                            failures, pkg, backoff_secs
                        );
                    } else {
                        warn!(
                            "[runtimed] UV pool in backoff: {} consecutive failures, \
                             waiting {}s before retry",
                            failures, backoff_secs
                        );
                    }
                }
            }

            // Log status
            let (available, warming) = self.uv_pool.lock().await.stats();
            debug!(
                "[runtimed] UV pool: {}/{} available, {} warming",
                available, self.config.uv_pool_size, warming
            );

            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    }

    /// Conda warming loop - maintains the Conda pool using rattler.
    async fn conda_warming_loop(&self) {
        // Check if we should even try (pool size > 0)
        if self.config.conda_pool_size == 0 {
            info!("[runtimed] Conda pool size is 0, skipping warming");
            return;
        }

        info!(
            "[runtimed] Starting conda warming loop (target: {})",
            self.config.conda_pool_size
        );

        loop {
            // Check shutdown
            if *self.shutdown.lock().await {
                break;
            }

            let (deficit, should_retry, backoff_info) = {
                let mut pool = self.conda_pool.lock().await;
                let d = pool.deficit();
                let retry = pool.should_retry();
                let info = if pool.failure_state.consecutive_failures > 0 {
                    Some((
                        pool.failure_state.consecutive_failures,
                        pool.backoff_delay().as_secs(),
                        pool.failure_state.last_error.clone(),
                    ))
                } else {
                    None
                };

                if d > 0 && retry {
                    pool.mark_warming(d);
                }
                (d, retry, info)
            };

            if deficit > 0 {
                if should_retry {
                    self.update_pool_doc().await;
                    info!(
                        "[runtimed] Conda pool deficit: {}, creating {} envs",
                        deficit, deficit
                    );

                    // Create environments one at a time (rattler is already efficient)
                    for _ in 0..deficit {
                        if *self.shutdown.lock().await {
                            break;
                        }
                        self.create_conda_env().await;
                    }
                } else if let Some((failures, backoff_secs, last_error)) = backoff_info {
                    // In backoff period - log why we're waiting
                    if let Some(err) = last_error {
                        warn!(
                            "[runtimed] Conda pool in backoff: {} consecutive failures ({}), \
                             waiting {}s before retry. Check conda.default_packages in settings.",
                            failures,
                            err.chars().take(80).collect::<String>(),
                            backoff_secs
                        );
                    } else {
                        warn!(
                            "[runtimed] Conda pool in backoff: {} consecutive failures, \
                             waiting {}s before retry",
                            failures, backoff_secs
                        );
                    }
                }
            }

            // Log status
            let (available, warming) = self.conda_pool.lock().await.stats();
            debug!(
                "[runtimed] Conda pool: {}/{} available, {} warming",
                available, self.config.conda_pool_size, warming
            );

            // Wait before checking again
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    }

    /// Background loop that keeps the pixi environment pool at its target size.
    async fn pixi_warming_loop(&self) {
        if self.config.pixi_pool_size == 0 {
            info!("[runtimed] Pixi pool size is 0, skipping warming");
            return;
        }

        info!(
            "[runtimed] Starting pixi warming loop (target: {})",
            self.config.pixi_pool_size
        );

        loop {
            if *self.shutdown.lock().await {
                break;
            }

            let (deficit, should_retry, backoff_info) = {
                let mut pool = self.pixi_pool.lock().await;
                let d = pool.deficit();
                let retry = pool.should_retry();
                let info = if pool.failure_state.consecutive_failures > 0 {
                    Some((
                        pool.failure_state.consecutive_failures,
                        pool.backoff_delay().as_secs(),
                        pool.failure_state.last_error.clone(),
                    ))
                } else {
                    None
                };

                if d > 0 && retry {
                    pool.mark_warming(d);
                }
                (d, retry, info)
            };

            if deficit > 0 {
                if should_retry {
                    self.update_pool_doc().await;
                    info!(
                        "[runtimed] Pixi pool deficit: {}, creating {} envs",
                        deficit, deficit
                    );
                    for _ in 0..deficit {
                        if *self.shutdown.lock().await {
                            break;
                        }
                        self.create_pixi_env().await;
                    }
                } else if let Some((failures, backoff_secs, last_error)) = backoff_info {
                    if let Some(err) = last_error {
                        warn!(
                            "[runtimed] Pixi pool in backoff: {} consecutive failures ({}), \
                             waiting {}s before retry. Check pixi.default_packages in settings.",
                            failures,
                            err.chars().take(80).collect::<String>(),
                            backoff_secs
                        );
                    } else {
                        warn!(
                            "[runtimed] Pixi pool in backoff: {} consecutive failures, \
                             waiting {}s before retry",
                            failures, backoff_secs
                        );
                    }
                }
            }

            let (available, warming) = self.pixi_pool.lock().await.stats();
            debug!(
                "[runtimed] Pixi pool: {}/{} available, {} warming",
                available, self.config.pixi_pool_size, warming
            );

            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    }

    /// Create a single Conda environment using rattler and add it to the pool.
    async fn create_conda_env(&self) {
        use rattler::{default_cache_dir, install::Installer, package_cache::PackageCache};
        use rattler_conda_types::{
            Channel, ChannelConfig, GenericVirtualPackage, MatchSpec, ParseMatchSpecOptions,
            Platform,
        };
        use rattler_repodata_gateway::Gateway;
        use rattler_solve::{resolvo, SolverImpl, SolverTask};

        let temp_id = format!("runtimed-conda-{}", uuid::Uuid::new_v4());
        let env_path = self.config.cache_dir.join(&temp_id);

        #[cfg(target_os = "windows")]
        let python_path = env_path.join("python.exe");
        #[cfg(not(target_os = "windows"))]
        let python_path = env_path.join("bin").join("python");

        info!("[runtimed] Creating Conda environment at {:?}", env_path);

        // Ensure cache directory exists
        if let Err(e) = tokio::fs::create_dir_all(&self.config.cache_dir).await {
            error!("[runtimed] Failed to create cache dir: {}", e);
            self.conda_pool
                .lock()
                .await
                .warming_failed_with_error(Some(PackageInstallError {
                    failed_package: None,
                    error_message: format!("Failed to create cache dir: {}", e),
                }));
            self.update_pool_doc().await;
            return;
        }

        // Setup channel configuration
        let channel_config = ChannelConfig::default_with_root_dir(self.config.cache_dir.clone());

        // Parse channels
        let channels = match Channel::from_str("conda-forge", &channel_config) {
            Ok(ch) => vec![ch],
            Err(e) => {
                error!("[runtimed] Failed to parse conda-forge channel: {}", e);
                self.conda_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to parse conda-forge channel: {}", e),
                    }));
                self.update_pool_doc().await;
                return;
            }
        };

        // Read default conda packages from synced settings
        let extra_conda_packages: Vec<String> = {
            let settings = self.settings.read().await;
            let synced = settings.get_all();
            synced.conda.default_packages
        };

        if !extra_conda_packages.is_empty() {
            info!(
                "[runtimed] Including default conda packages: {:?}",
                extra_conda_packages
            );
        }

        // Build specs: python + notebook essentials + user-configured defaults
        let conda_install_packages = conda_prewarmed_packages(&extra_conda_packages);

        let match_spec_options = ParseMatchSpecOptions::strict();
        let specs: Vec<MatchSpec> = match (|| -> anyhow::Result<Vec<MatchSpec>> {
            let mut specs = vec![MatchSpec::from_str("python>=3.13", match_spec_options)?];
            for pkg in &conda_install_packages {
                specs.push(MatchSpec::from_str(pkg, match_spec_options)?);
            }
            Ok(specs)
        })() {
            Ok(s) => s,
            Err(e) => {
                error!("[runtimed] Failed to parse match specs: {}", e);
                self.conda_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to parse match specs: {}", e),
                    }));
                self.update_pool_doc().await;
                return;
            }
        };

        // Find rattler cache directory
        let rattler_cache_dir = match default_cache_dir() {
            Ok(dir) => dir,
            Err(e) => {
                error!(
                    "[runtimed] Could not determine rattler cache directory: {}",
                    e
                );
                self.conda_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!(
                            "Could not determine rattler cache directory: {}",
                            e
                        ),
                    }));
                self.update_pool_doc().await;
                return;
            }
        };

        if let Err(e) = rattler_cache::ensure_cache_dir(&rattler_cache_dir) {
            error!("[runtimed] Could not create rattler cache directory: {}", e);
            self.conda_pool
                .lock()
                .await
                .warming_failed_with_error(Some(PackageInstallError {
                    failed_package: None,
                    error_message: format!("Could not create rattler cache directory: {}", e),
                }));
            self.update_pool_doc().await;
            return;
        }

        // Create HTTP client
        let download_client = match reqwest::Client::builder().build() {
            Ok(c) => reqwest_middleware::ClientBuilder::new(c).build(),
            Err(e) => {
                error!("[runtimed] Failed to create HTTP client: {}", e);
                self.conda_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to create HTTP client: {}", e),
                    }));
                self.update_pool_doc().await;
                return;
            }
        };

        // Create gateway for fetching repodata
        let gateway = Gateway::builder()
            .with_cache_dir(rattler_cache_dir.join(rattler_cache::REPODATA_CACHE_DIR))
            .with_package_cache(PackageCache::new(
                rattler_cache_dir.join(rattler_cache::PACKAGE_CACHE_DIR),
            ))
            .with_client(download_client.clone())
            .finish();

        // Query repodata
        let install_platform = Platform::current();
        let platforms = vec![install_platform, Platform::NoArch];

        info!("[runtimed] Fetching conda repodata from conda-forge...");
        let repo_data = match gateway
            .query(channels.clone(), platforms.clone(), specs.clone())
            .recursive(true)
            .await
        {
            Ok(data) => data,
            Err(e) => {
                error!("[runtimed] Failed to fetch repodata: {}", e);
                self.conda_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to fetch repodata: {}", e),
                    }));
                self.update_pool_doc().await;
                return;
            }
        };

        info!("[runtimed] Repodata fetched, solving dependencies...");

        // Detect virtual packages
        let virtual_packages = match rattler_virtual_packages::VirtualPackage::detect(
            &rattler_virtual_packages::VirtualPackageOverrides::default(),
        ) {
            Ok(vps) => vps
                .iter()
                .map(|vpkg| GenericVirtualPackage::from(vpkg.clone()))
                .collect::<Vec<_>>(),
            Err(e) => {
                error!("[runtimed] Failed to detect virtual packages: {}", e);
                self.conda_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to detect virtual packages: {}", e),
                    }));
                self.update_pool_doc().await;
                return;
            }
        };

        // Solve dependencies
        let solver_task = SolverTask {
            virtual_packages,
            specs,
            ..SolverTask::from_iter(&repo_data)
        };

        let required_packages = match resolvo::Solver.solve(solver_task) {
            Ok(result) => result.records,
            Err(e) => {
                error!("[runtimed] Failed to solve dependencies: {}", e);
                self.conda_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to solve dependencies: {}", e),
                    }));
                self.update_pool_doc().await;
                return;
            }
        };

        info!(
            "[runtimed] Solved: {} packages to install",
            required_packages.len()
        );

        // Install packages
        let install_result = Installer::new()
            .with_download_client(download_client)
            .with_target_platform(install_platform)
            .install(&env_path, required_packages)
            .await;

        if let Err(e) = install_result {
            error!("[runtimed] Failed to install packages: {}", e);
            tokio::fs::remove_dir_all(&env_path).await.ok();
            self.conda_pool
                .lock()
                .await
                .warming_failed_with_error(Some(PackageInstallError {
                    failed_package: None,
                    error_message: format!("Failed to install packages: {}", e),
                }));
            self.update_pool_doc().await;
            return;
        }

        // Verify python exists
        if !python_path.exists() {
            error!(
                "[runtimed] Python not found at {:?} after install",
                python_path
            );
            tokio::fs::remove_dir_all(&env_path).await.ok();
            self.conda_pool
                .lock()
                .await
                .warming_failed_with_error(Some(PackageInstallError {
                    failed_package: None,
                    error_message: format!("Python not found at {:?} after install", python_path),
                }));
            self.update_pool_doc().await;
            return;
        }

        // Run warmup script
        let warmup_ok = self
            .warmup_conda_env(&python_path, &env_path, &extra_conda_packages)
            .await;

        if warmup_ok {
            {
                let mut pool = self.conda_pool.lock().await;
                pool.add(PooledEnv {
                    env_type: EnvType::Conda,
                    venv_path: env_path.clone(),
                    python_path,
                    prewarmed_packages: conda_install_packages,
                });
            }

            info!(
                "[runtimed] Conda environment ready: {:?} (pool: {}/{})",
                env_path,
                self.conda_pool.lock().await.stats().0,
                self.config.conda_pool_size
            );

            self.update_pool_doc().await;
        } else {
            // Clean up failed env and record failure
            let _ = tokio::fs::remove_dir_all(&env_path).await;
            self.conda_pool
                .lock()
                .await
                .warming_failed_with_error(Some(PackageInstallError {
                    failed_package: None,
                    error_message: "Conda warmup script failed (ipykernel may not be installed)"
                        .into(),
                }));
            self.update_pool_doc().await;
        }
    }

    /// Warm up a conda environment by running Python to trigger .pyc compilation.
    /// Returns `true` if warmup succeeded (ipykernel imports work).
    async fn warmup_conda_env(
        &self,
        python_path: &PathBuf,
        env_path: &PathBuf,
        extra_packages: &[String],
    ) -> bool {
        let warmup_script = build_python_warmup_script(extra_packages, true);

        let warmup_result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::process::Command::new(python_path)
                .args(["-c", &warmup_script])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        match warmup_result {
            Ok(Ok(output)) if output.status.success() => {
                // Create marker file
                tokio::fs::write(env_path.join(".warmed"), "").await.ok();
                info!("[runtimed] Conda warmup complete for {:?}", env_path);
                true
            }
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!(
                    "[runtimed] Conda warmup failed for {:?}: {}",
                    env_path,
                    stderr.lines().take(3).collect::<Vec<_>>().join(" | ")
                );
                false
            }
            Ok(Err(e)) => {
                error!("[runtimed] Failed to run conda warmup: {}", e);
                false
            }
            Err(_) => {
                error!("[runtimed] Conda warmup timed out");
                false
            }
        }
    }

    /// Replenish a single Conda environment.
    async fn replenish_conda_env(&self) {
        self.conda_pool.lock().await.mark_warming(1);
        self.create_conda_env().await;
    }

    /// Create a pixi environment using rattler (no subprocess).
    ///
    /// Creates a pixi-compatible project directory with ipykernel and default
    /// packages, solved and installed via rattler. Replaces the old
    /// `pixi init` + `pixi add` subprocess approach.
    async fn create_pixi_env(&self) {
        let cache_dir = self.config.cache_dir.clone();
        let env_id = uuid::Uuid::new_v4().to_string();
        let project_dir = cache_dir.join(format!("runtimed-pixi-{}", env_id));

        info!("[runtimed] Creating Pixi environment at {:?}", project_dir);

        // Build package list
        let mut packages = pixi_prewarmed_packages(&[]);
        {
            let pixi_defaults = self.default_pixi_packages().await;
            if !pixi_defaults.is_empty() {
                info!(
                    "[runtimed] Including default pixi packages: {:?}",
                    pixi_defaults
                );
                packages.extend(pixi_defaults);
            }
        }
        let prewarmed_packages = packages.clone();

        // Create environment using rattler (pixi-compatible layout)
        let handler = std::sync::Arc::new(kernel_env::LogHandler);
        match kernel_env::pixi::create_pixi_environment(
            &project_dir,
            &packages,
            &["conda-forge".to_string()],
            handler.clone(),
        )
        .await
        {
            Ok(env) => {
                // Warm up the environment (.pyc compilation)
                if let Err(e) = kernel_env::pixi::warmup_environment(&env).await {
                    warn!("[runtimed] Pixi warmup failed (non-fatal): {}", e);
                }

                info!("[runtimed] Pixi environment ready at {:?}", env.project_dir);
                {
                    let mut pool = self.pixi_pool.lock().await;
                    pool.add(PooledEnv {
                        env_type: EnvType::Pixi,
                        venv_path: env.venv_path,
                        python_path: env.python_path,
                        prewarmed_packages,
                    });
                }
                self.update_pool_doc().await;
            }
            Err(e) => {
                error!("[runtimed] Pixi environment creation failed: {}", e);
                let _ = tokio::fs::remove_dir_all(&project_dir).await;
                self.pixi_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        error_message: format!("{}", e),
                        failed_package: None,
                    }));
                self.update_pool_doc().await;
            }
        }
    }

    /// Mark pixi pool as warming and create a pixi environment.
    async fn replenish_pixi_env(&self) {
        self.pixi_pool.lock().await.mark_warming(1);
        self.create_pixi_env().await;
    }

    /// Update the PoolDoc with current pool state and notify sync connections.
    ///
    /// Called when pool state changes (new error, error cleared, warming, etc.).
    async fn update_pool_doc(&self) {
        use notebook_doc::pool_state::{PoolState, RuntimePoolState};

        let uv = {
            let pool = self.uv_pool.lock().await;
            let (available, warming) = pool.stats();
            RuntimePoolState {
                available: available as u64,
                warming: warming as u64,
                pool_size: self.config.uv_pool_size as u64,
                error: pool.failure_state.last_error.clone(),
                failed_package: pool.failure_state.failed_package.clone(),
                consecutive_failures: pool.failure_state.consecutive_failures,
                retry_in_secs: pool.retry_in_secs(),
            }
        };
        let conda = {
            let pool = self.conda_pool.lock().await;
            let (available, warming) = pool.stats();
            RuntimePoolState {
                available: available as u64,
                warming: warming as u64,
                pool_size: self.config.conda_pool_size as u64,
                error: pool.failure_state.last_error.clone(),
                failed_package: pool.failure_state.failed_package.clone(),
                consecutive_failures: pool.failure_state.consecutive_failures,
                retry_in_secs: pool.retry_in_secs(),
            }
        };

        let pixi = {
            let pool = self.pixi_pool.lock().await;
            let (available, warming) = pool.stats();
            RuntimePoolState {
                available: available as u64,
                warming: warming as u64,
                pool_size: self.config.pixi_pool_size as u64,
                error: pool.failure_state.last_error.clone(),
                failed_package: pool.failure_state.failed_package.clone(),
                consecutive_failures: pool.failure_state.consecutive_failures,
                retry_in_secs: pool.retry_in_secs(),
            }
        };

        let changed = self
            .pool_doc
            .write()
            .await
            .update(&PoolState { uv, conda, pixi });
        if changed {
            let _ = self.pool_doc_changed.send(());
        }

        // Wake any take_*_env() waiters so they can retry after pool state changes
        self.pool_ready_uv.notify_waiters();
        self.pool_ready_conda.notify_waiters();
        self.pool_ready_pixi.notify_waiters();
    }

    /// Create a single UV environment and add it to the pool.
    async fn create_uv_env(&self) {
        // Get uv path (cached via OnceCell, so this is instant after initial bootstrap)
        let uv_path = match kernel_launch::tools::get_uv_path().await {
            Ok(path) => path,
            Err(e) => {
                error!("[runtimed] Failed to get uv path: {}", e);
                self.uv_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to get uv path: {}", e),
                    }));
                self.update_pool_doc().await;
                return;
            }
        };

        let temp_id = format!("runtimed-uv-{}", uuid::Uuid::new_v4());
        let venv_path = self.config.cache_dir.join(&temp_id);

        #[cfg(target_os = "windows")]
        let python_path = venv_path.join("Scripts").join("python.exe");
        #[cfg(not(target_os = "windows"))]
        let python_path = venv_path.join("bin").join("python");

        info!("[runtimed] Creating UV environment at {:?}", venv_path);

        // Ensure cache directory exists
        if let Err(e) = tokio::fs::create_dir_all(&self.config.cache_dir).await {
            error!("[runtimed] Failed to create cache dir: {}", e);
            self.uv_pool
                .lock()
                .await
                .warming_failed_with_error(Some(PackageInstallError {
                    failed_package: None,
                    error_message: format!("Failed to create cache dir: {}", e),
                }));
            self.update_pool_doc().await;
            return;
        }

        // Create venv (60 second timeout)
        let venv_result = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            tokio::process::Command::new(&uv_path)
                .arg("venv")
                .arg(&venv_path)
                .arg("--python")
                .arg("3.13")
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        match venv_result {
            Ok(Ok(output)) if output.status.success() => {}
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!("[runtimed] Failed to create venv: {}", stderr);
                self.uv_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to create venv: {}", stderr),
                    }));
                self.update_pool_doc().await;
                return;
            }
            Ok(Err(e)) => {
                error!("[runtimed] Failed to create venv: {}", e);
                self.uv_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to create venv: {}", e),
                    }));
                self.update_pool_doc().await;
                return;
            }
            Err(_) => {
                error!("[runtimed] Timeout creating venv");
                tokio::fs::remove_dir_all(&venv_path).await.ok();
                self.uv_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: "Timeout creating venv after 60 seconds".to_string(),
                    }));
                self.update_pool_doc().await;
                return;
            }
        }

        // Read default uv packages from synced settings
        let user_default_packages = {
            let settings = self.settings.read().await;
            let synced = settings.get_all();
            let configured = synced.uv.default_packages;
            if !configured.is_empty() {
                info!("[runtimed] Including default uv packages: {:?}", configured);
            }
            configured
        };
        let install_packages = uv_prewarmed_packages(&user_default_packages);

        // Install packages (120 second timeout)
        // Use hardlink mode to share files from uv's global cache,
        // dramatically reducing per-env disk usage. uv falls back to
        // copies automatically if hardlinks aren't supported.
        let mut install_args = vec![
            "pip".to_string(),
            "install".to_string(),
            "--link-mode".to_string(),
            "hardlink".to_string(),
            "--python".to_string(),
            python_path.to_string_lossy().to_string(),
        ];
        install_args.extend(install_packages.clone());

        let install_result = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::process::Command::new(&uv_path)
                .args(&install_args)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .output(),
        )
        .await;

        match install_result {
            Ok(Ok(output)) if output.status.success() => {}
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let parsed_error = parse_uv_error(&stderr);

                if let Some(ref err) = parsed_error {
                    if let Some(pkg) = &err.failed_package {
                        // Check if this is a user-specified package (not ipykernel/ipywidgets/anywidget)
                        let is_user_package = user_default_packages
                            .iter()
                            .any(|configured| configured == pkg);

                        if is_user_package {
                            error!(
                                "[runtimed] Failed to install user package '{}' from default_packages setting. \
                                 Check uv.default_packages in settings for typos.",
                                pkg
                            );
                        } else {
                            error!(
                                "[runtimed] Failed to install package '{}': {}",
                                pkg,
                                stderr.lines().take(3).collect::<Vec<_>>().join(" ")
                            );
                        }
                    } else {
                        error!(
                            "[runtimed] Package installation failed: {}",
                            stderr.lines().take(5).collect::<Vec<_>>().join(" ")
                        );
                    }
                } else {
                    error!(
                        "[runtimed] Package installation failed: {}",
                        stderr.lines().take(5).collect::<Vec<_>>().join(" ")
                    );
                }

                tokio::fs::remove_dir_all(&venv_path).await.ok();
                self.uv_pool
                    .lock()
                    .await
                    .warming_failed_with_error(parsed_error);
                self.update_pool_doc().await;
                return;
            }
            Ok(Err(e)) => {
                error!("[runtimed] Failed to run uv pip install: {}", e);
                tokio::fs::remove_dir_all(&venv_path).await.ok();
                self.uv_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: e.to_string(),
                    }));
                self.update_pool_doc().await;
                return;
            }
            Err(_) => {
                error!("[runtimed] Timeout installing packages (120s)");
                tokio::fs::remove_dir_all(&venv_path).await.ok();
                self.uv_pool
                    .lock()
                    .await
                    .warming_failed_with_error(Some(PackageInstallError {
                        failed_package: None,
                        error_message: "Timeout after 120 seconds".to_string(),
                    }));
                self.update_pool_doc().await;
                return;
            }
        }

        // Warm up the environment (30 second timeout)
        let warmup_script = build_python_warmup_script(&user_default_packages, false);

        let warmup_result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::process::Command::new(&python_path)
                .args(["-c", &warmup_script])
                .output(),
        )
        .await;

        let warmup_ok = match warmup_result {
            Ok(Ok(output)) if output.status.success() => {
                // Create marker file
                tokio::fs::write(venv_path.join(".warmed"), "").await.ok();
                true
            }
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!(
                    "[runtimed] UV warmup failed, NOT adding to pool: {}",
                    stderr.lines().take(3).collect::<Vec<_>>().join(" | ")
                );
                false
            }
            Ok(Err(e)) => {
                error!("[runtimed] Failed to run UV warmup: {}", e);
                false
            }
            Err(_) => {
                error!("[runtimed] UV warmup timed out, NOT adding to pool");
                false
            }
        };

        if warmup_ok {
            info!("[runtimed] UV environment ready at {:?}", venv_path);

            {
                let mut pool = self.uv_pool.lock().await;
                pool.add(PooledEnv {
                    env_type: EnvType::Uv,
                    venv_path,
                    python_path,
                    prewarmed_packages: install_packages,
                });
            }
            self.update_pool_doc().await;
        } else {
            // Clean up failed env and record failure
            let _ = tokio::fs::remove_dir_all(&venv_path).await;
            self.uv_pool
                .lock()
                .await
                .warming_failed_with_error(Some(PackageInstallError {
                    failed_package: None,
                    error_message: "Warmup script failed (ipykernel may not be installed)".into(),
                }));
            self.update_pool_doc().await;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_test_env(temp_dir: &TempDir, name: &str) -> PooledEnv {
        let venv_path = temp_dir.path().join(name);
        std::fs::create_dir_all(&venv_path).unwrap();

        #[cfg(windows)]
        let python_path = venv_path.join("Scripts").join("python.exe");
        #[cfg(not(windows))]
        let python_path = venv_path.join("bin").join("python");

        // Create the python file so it "exists"
        if let Some(parent) = python_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&python_path, "").unwrap();

        // Create warmup marker so take() accepts this env
        std::fs::write(venv_path.join(".warmed"), "").unwrap();

        PooledEnv {
            env_type: EnvType::Uv,
            venv_path,
            python_path,
            prewarmed_packages: vec![],
        }
    }

    #[test]
    fn test_pool_new() {
        let pool = Pool::new(3, 3600);
        assert_eq!(pool.target, 3);
        assert_eq!(pool.max_age_secs, 3600);
        assert_eq!(pool.available.len(), 0);
        assert_eq!(pool.warming, 0);
    }

    #[test]
    fn test_pool_add_and_take() {
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(3, 3600);

        let env = create_test_env(&temp_dir, "test-env");
        pool.add(env.clone());

        assert_eq!(pool.available.len(), 1);

        let (taken, stale) = pool.take();
        assert!(taken.is_some());
        assert_eq!(taken.unwrap().venv_path, env.venv_path);
        assert_eq!(pool.available.len(), 0);
        assert!(stale.is_empty());
    }

    #[test]
    fn test_pool_prune_stale_keeps_minimum_warm_bases() {
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(3, 0);

        let env1 = create_test_env(&temp_dir, "env1");
        let env2 = create_test_env(&temp_dir, "env2");
        let env3 = create_test_env(&temp_dir, "env3");
        pool.add(env1);
        pool.add(env2);
        pool.add(env3);

        let stale = pool.prune_stale();
        assert_eq!(pool.available.len(), 2);
        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn test_pool_prune_stale_drops_invalid_even_at_minimum() {
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(2, 3600);

        let env1 = create_test_env(&temp_dir, "env1");
        let env2 = create_test_env(&temp_dir, "env2");
        std::fs::remove_file(env2.venv_path.join(".warmed")).unwrap();

        pool.add(env1.clone());
        pool.add(env2.clone());

        let stale = pool.prune_stale();
        assert_eq!(pool.available.len(), 1);
        assert_eq!(
            pool.available.front().unwrap().env.venv_path,
            env1.venv_path
        );
        assert_eq!(stale, vec![env2.venv_path]);
    }

    #[test]
    fn test_pool_take_empty() {
        let mut pool = Pool::new(3, 3600);
        let (taken, stale) = pool.take();
        assert!(taken.is_none());
        assert!(stale.is_empty());
    }

    #[test]
    fn test_pool_take_skips_missing_paths() {
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(3, 3600);

        // Add an env with a path that doesn't exist
        let missing_env = PooledEnv {
            env_type: EnvType::Uv,
            venv_path: PathBuf::from("/nonexistent/path"),
            python_path: PathBuf::from("/nonexistent/path/bin/python"),
            prewarmed_packages: vec![],
        };
        pool.available.push_back(PoolEntry {
            env: missing_env,
            created_at: Instant::now(),
        });

        // Add a valid env
        let valid_env = create_test_env(&temp_dir, "valid-env");
        pool.add(valid_env.clone());

        // Take should skip the missing one and return the valid one
        let (taken, stale) = pool.take();
        assert!(taken.is_some());
        assert_eq!(taken.unwrap().venv_path, valid_env.venv_path);
        // The missing env path should be in the stale list for cleanup
        assert!(stale.contains(&PathBuf::from("/nonexistent/path")));
    }

    #[test]
    fn test_pool_deficit() {
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(3, 3600);

        // Initially, deficit is 3 (need 3, have 0)
        assert_eq!(pool.deficit(), 3);

        // Add one env directly, deficit is 2
        let env1 = create_test_env(&temp_dir, "env1");
        pool.add(env1);
        // Note: add() decrements warming, but it was 0 so stays 0
        assert_eq!(pool.available.len(), 1);
        assert_eq!(pool.warming, 0);
        assert_eq!(pool.deficit(), 2);

        // Mark that we're warming 1 more, deficit is 1
        pool.mark_warming(1);
        assert_eq!(pool.warming, 1);
        assert_eq!(pool.deficit(), 1); // 1 available + 1 warming = 2, need 1 more

        // Add another (simulating warming completion), deficit is 1
        // add() decrements warming: 1 -> 0
        let env2 = create_test_env(&temp_dir, "env2");
        pool.add(env2);
        assert_eq!(pool.available.len(), 2);
        assert_eq!(pool.warming, 0);
        assert_eq!(pool.deficit(), 1); // 2 available, need 1 more

        // Mark warming for the last one
        pool.mark_warming(1);
        assert_eq!(pool.deficit(), 0); // 2 available + 1 warming = 3 = target

        // Add the last one
        let env3 = create_test_env(&temp_dir, "env3");
        pool.add(env3);
        assert_eq!(pool.available.len(), 3);
        assert_eq!(pool.warming, 0);
        assert_eq!(pool.deficit(), 0); // 3 available = target

        // Taking one should increase deficit
        let _ = pool.take();
        assert_eq!(pool.available.len(), 2);
        assert_eq!(pool.deficit(), 1);
    }

    #[test]
    fn test_pool_warming_failed() {
        let mut pool = Pool::new(3, 3600);

        pool.mark_warming(2);
        assert_eq!(pool.warming, 2);

        pool.warming_failed_with_error(None);
        assert_eq!(pool.warming, 1);

        pool.warming_failed_with_error(None);
        assert_eq!(pool.warming, 0);

        // Should not go negative
        pool.warming_failed_with_error(None);
        assert_eq!(pool.warming, 0);
    }

    #[test]
    fn test_pool_stats() {
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(3, 3600);

        let (available, warming) = pool.stats();
        assert_eq!(available, 0);
        assert_eq!(warming, 0);

        let env = create_test_env(&temp_dir, "env1");
        pool.add(env);
        pool.mark_warming(2);

        let (available, warming) = pool.stats();
        assert_eq!(available, 1);
        assert_eq!(warming, 2);
    }

    #[test]
    fn test_daemon_config_default() {
        let config = DaemonConfig::default();
        assert_eq!(config.uv_pool_size, 3);
        assert_eq!(config.conda_pool_size, 3);
        #[cfg(unix)]
        assert!(config
            .socket_path
            .to_string_lossy()
            .contains("runtimed.sock"));
        #[cfg(windows)]
        assert!(config
            .socket_path
            .to_string_lossy()
            .contains(r"\\.\pipe\runtimed"));
        assert!(config.blob_store_dir.to_string_lossy().contains("blobs"));
    }

    #[test]
    fn test_env_type_display() {
        assert_eq!(format!("{}", EnvType::Uv), "uv");
        assert_eq!(format!("{}", EnvType::Conda), "conda");
    }

    // =========================================================================
    // Backoff and error handling tests
    // =========================================================================

    #[test]
    fn test_pool_backoff_exponential() {
        let mut pool = Pool::new(3, 3600);

        // No failures = no backoff
        assert_eq!(pool.backoff_delay(), std::time::Duration::ZERO);
        assert!(pool.should_retry());

        // First failure = 30s backoff
        pool.warming_failed_with_error(None);
        assert_eq!(pool.backoff_delay(), std::time::Duration::from_secs(30));
        assert_eq!(pool.failure_state.consecutive_failures, 1);

        // Second failure = 60s
        pool.warming_failed_with_error(None);
        assert_eq!(pool.backoff_delay(), std::time::Duration::from_secs(60));
        assert_eq!(pool.failure_state.consecutive_failures, 2);

        // Third = 120s
        pool.warming_failed_with_error(None);
        assert_eq!(pool.backoff_delay(), std::time::Duration::from_secs(120));

        // Fourth = 240s
        pool.warming_failed_with_error(None);
        assert_eq!(pool.backoff_delay(), std::time::Duration::from_secs(240));

        // Fifth and beyond = max 300s (5 min)
        pool.warming_failed_with_error(None);
        assert_eq!(pool.backoff_delay(), std::time::Duration::from_secs(300));

        // Even more failures should stay at max
        for _ in 0..10 {
            pool.warming_failed_with_error(None);
        }
        assert_eq!(pool.backoff_delay(), std::time::Duration::from_secs(300));
    }

    #[test]
    fn test_pool_reset_on_success() {
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(3, 3600);

        // Simulate some failures
        pool.warming_failed_with_error(Some(PackageInstallError {
            failed_package: Some("bad-pkg".to_string()),
            error_message: "not found".to_string(),
        }));
        pool.warming_failed_with_error(None);
        assert_eq!(pool.failure_state.consecutive_failures, 2);
        assert!(pool.failure_state.last_error.is_some());

        // Adding an env should reset failure state
        let env = create_test_env(&temp_dir, "env1");
        pool.add(env);
        assert_eq!(pool.failure_state.consecutive_failures, 0);
        assert!(pool.failure_state.last_error.is_none());
        assert!(pool.failure_state.failed_package.is_none());
    }

    #[test]
    fn test_pool_reset_failure_state() {
        let mut pool = Pool::new(3, 3600);

        pool.warming_failed_with_error(Some(PackageInstallError {
            failed_package: Some("scitkit-learn".to_string()),
            error_message: "Package not found".to_string(),
        }));
        assert_eq!(pool.failure_state.consecutive_failures, 1);

        pool.reset_failure_state();
        assert_eq!(pool.failure_state.consecutive_failures, 0);
        assert!(pool.failure_state.last_error.is_none());
        assert!(pool.failure_state.failed_package.is_none());
        assert!(pool.failure_state.last_failure.is_none());
    }

    #[test]
    fn test_pool_take_skips_unwarmed() {
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(3, 3600);

        // Create an env with valid paths but NO .warmed marker
        let venv_path = temp_dir.path().join("unwarmed-env");
        std::fs::create_dir_all(&venv_path).unwrap();
        #[cfg(windows)]
        let python_path = venv_path.join("Scripts").join("python.exe");
        #[cfg(not(windows))]
        let python_path = venv_path.join("bin").join("python");
        if let Some(parent) = python_path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&python_path, "").unwrap();

        let unwarmed_env = PooledEnv {
            env_type: EnvType::Uv,
            venv_path,
            python_path,
            prewarmed_packages: vec![],
        };
        pool.add(unwarmed_env);

        // take() should skip the unwarmed env
        let (taken, _stale) = pool.take();
        assert!(taken.is_none());

        // Add a properly warmed env
        let warmed_env = create_test_env(&temp_dir, "warmed-env");
        pool.add(warmed_env.clone());

        // take() should return the warmed env
        let (taken, _stale) = pool.take();
        assert!(taken.is_some());
        assert_eq!(taken.unwrap().venv_path, warmed_env.venv_path);
    }

    #[test]
    fn test_parse_uv_error_package_not_found() {
        let stderr = r#"error: No solution found when resolving dependencies:
  ╰─▶ Because scitkit-learn was not found in the package registry and you require scitkit-learn, we can conclude that your requirements are unsatisfiable."#;

        let result = parse_uv_error(stderr);
        assert!(result.is_some());
        let err = result.unwrap();
        assert_eq!(err.failed_package, Some("scitkit-learn".to_string()));
    }

    #[test]
    fn test_parse_uv_error_backtick_format() {
        let stderr = "error: Package `nonexistent-pkg` not found in registry";

        let result = parse_uv_error(stderr);
        assert!(result.is_some());
        let err = result.unwrap();
        assert_eq!(err.failed_package, Some("nonexistent-pkg".to_string()));
    }

    #[test]
    fn test_parse_uv_error_no_matching_distribution() {
        let stderr = "error: No matching distribution found for bad-package-name";

        let result = parse_uv_error(stderr);
        assert!(result.is_some());
        let err = result.unwrap();
        assert_eq!(err.failed_package, Some("bad-package-name".to_string()));
    }

    #[test]
    fn test_parse_uv_error_generic_error() {
        let stderr = "error: Failed to resolve dependencies";

        let result = parse_uv_error(stderr);
        assert!(result.is_some());
        let err = result.unwrap();
        // Generic error without specific package
        assert!(err.failed_package.is_none());
        assert!(err.error_message.contains("error"));
    }

    #[test]
    fn test_parse_uv_error_no_error() {
        let stderr = "Successfully installed packages";

        let result = parse_uv_error(stderr);
        assert!(result.is_none());
    }
}
