//! Pool daemon server implementation.
//!
//! The daemon manages prewarmed environment pools and handles requests from
//! notebook windows via IPC (Unix domain sockets on Unix, named pipes on Windows).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use log::{error, info, warn};
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
use crate::protocol::{BlobRequest, BlobResponse, DaemonBroadcast, Request, Response};
use crate::settings_doc::SettingsDoc;
use crate::singleton::{DaemonInfo, DaemonLock};
use crate::{
    default_blob_store_dir, default_cache_dir, default_socket_path, EnvType, PoolError, PoolStats,
    PooledEnv,
};

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
    /// Maximum age (in seconds) before an environment is considered stale.
    pub max_age_secs: u64,
    /// Optional custom directory for lock files (used in tests).
    pub lock_dir: Option<PathBuf>,
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
            max_age_secs: 172800, // 2 days
            lock_dir: None,
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
                    if package_name != "ipykernel" && package_name != "ipywidgets" {
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

    /// Prune stale environments.
    fn prune_stale(&mut self) {
        let max_age = std::time::Duration::from_secs(self.max_age_secs);
        let before = self.available.len();
        self.available.retain(|e| e.created_at.elapsed() < max_age);
        let removed = before - self.available.len();
        if removed > 0 {
            info!("[runtimed] Pruned {} stale environments", removed);
        }
    }

    /// Take an environment from the pool.
    fn take(&mut self) -> Option<PooledEnv> {
        self.prune_stale();

        // Try to get a valid environment, skipping any with missing paths
        while let Some(entry) = self.available.pop_front() {
            if entry.env.venv_path.exists() && entry.env.python_path.exists() {
                return Some(entry.env);
            }
            warn!(
                "[runtimed] Skipping env with missing path: {:?}",
                entry.env.venv_path
            );
        }

        None
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

    /// Get error info for status reporting.
    fn get_error(&self) -> Option<PoolError> {
        if self.failure_state.consecutive_failures == 0 {
            return None;
        }

        let retry_in_secs = self
            .failure_state
            .last_failure
            .map(|last| {
                self.backoff_delay()
                    .saturating_sub(last.elapsed())
                    .as_secs()
            })
            .unwrap_or(0);

        Some(PoolError {
            message: self
                .failure_state
                .last_error
                .clone()
                .unwrap_or_else(|| "Unknown error".to_string()),
            failed_package: self.failure_state.failed_package.clone(),
            consecutive_failures: self.failure_state.consecutive_failures,
            retry_in_secs,
        })
    }
}

/// The pool daemon state.
pub struct Daemon {
    config: DaemonConfig,
    uv_pool: Mutex<Pool>,
    conda_pool: Mutex<Pool>,
    shutdown: Arc<Mutex<bool>>,
    /// Notifier to wake up accept loops on shutdown.
    shutdown_notify: Arc<Notify>,
    /// Singleton lock - kept alive while daemon is running.
    _lock: DaemonLock,
    /// Shared Automerge settings document.
    settings: Arc<RwLock<SettingsDoc>>,
    /// Broadcast channel to notify sync connections of settings changes.
    settings_changed: tokio::sync::broadcast::Sender<()>,
    /// Broadcast channel to notify clients of pool state changes (errors, recovery).
    pool_state_changed: tokio::sync::broadcast::Sender<DaemonBroadcast>,
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
        let (pool_state_changed, _) = tokio::sync::broadcast::channel(16);

        let blob_store = Arc::new(BlobStore::new(config.blob_store_dir.clone()));

        Ok(Arc::new(Self {
            uv_pool: Mutex::new(Pool::new(config.uv_pool_size, config.max_age_secs)),
            conda_pool: Mutex::new(Pool::new(config.conda_pool_size, config.max_age_secs)),
            config,
            shutdown: Arc::new(Mutex::new(false)),
            shutdown_notify: Arc::new(Notify::new()),
            _lock: lock,
            settings: Arc::new(RwLock::new(settings)),
            settings_changed,
            pool_state_changed,
            blob_store,
            blob_port: Mutex::new(None),
            notebook_rooms: Arc::new(Mutex::new(HashMap::new())),
        }))
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

            // Remove stale socket file
            if self.config.socket_path.exists() {
                tokio::fs::remove_file(&self.config.socket_path).await?;
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

        // Write daemon info so clients can discover us
        if let Err(e) = self
            ._lock
            .write_info(&self.config.socket_path.to_string_lossy(), blob_port)
        {
            error!("[runtimed] Failed to write daemon info: {}", e);
        }

        // Find and reuse existing environments from previous runs
        self.find_existing_environments().await;

        // Spawn the warming loops
        let uv_daemon = self.clone();
        tokio::spawn(async move {
            uv_daemon.uv_warming_loop().await;
        });

        let conda_daemon = self.clone();
        tokio::spawn(async move {
            conda_daemon.conda_warming_loop().await;
        });

        // Spawn the settings.json file watcher
        let watcher_daemon = self.clone();
        tokio::spawn(async move {
            watcher_daemon.watch_settings_json().await;
        });

        // Platform-specific accept loop
        #[cfg(unix)]
        {
            self.run_unix_server().await?;
        }

        #[cfg(windows)]
        {
            self.run_windows_server().await?;
        }

        // Cleanup socket (Unix only - named pipes don't need cleanup)
        #[cfg(unix)]
        tokio::fs::remove_file(&self.config.socket_path).await.ok();

        Ok(())
    }

    /// Unix-specific server loop using Unix domain sockets.
    #[cfg(unix)]
    async fn run_unix_server(self: &Arc<Self>) -> anyhow::Result<()> {
        let listener = UnixListener::bind(&self.config.socket_path)?;
        info!("[runtimed] Listening on {:?}", self.config.socket_path);

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
                                    self.broadcast_pool_state().await;
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

        let mut uv_found = 0;
        let mut conda_found = 0;

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
                            },
                            created_at: Instant::now(),
                        });
                        uv_found += 1;
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
                            },
                            created_at: Instant::now(),
                        });
                        conda_found += 1;
                    }
                } else {
                    // Invalid env, clean up
                    tokio::fs::remove_dir_all(&env_path).await.ok();
                }
            }
        }

        if uv_found > 0 || conda_found > 0 {
            info!(
                "[runtimed] Found {} existing UV and {} existing Conda environments",
                uv_found, conda_found
            );
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
        // Read the handshake with the control frame limit (64 KiB) so that
        // an oversized first frame can't force a large allocation before we
        // know which channel the connection belongs to.
        let handshake_bytes = connection::recv_control_frame(&mut stream)
            .await?
            .ok_or_else(|| anyhow::anyhow!("connection closed before handshake"))?;
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
            } => {
                let use_typed_frames = protocol.as_deref() == Some(connection::PROTOCOL_V2);
                info!(
                    "[runtimed] NotebookSync requested for {} (protocol: {})",
                    notebook_id,
                    protocol.as_deref().unwrap_or("v1")
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
                // Get user's default Python env preference for auto-launch
                let default_python_env = self.settings.read().await.get_all().default_python_env;
                crate::notebook_sync_server::handle_notebook_sync_connection(
                    reader,
                    writer,
                    room,
                    self.notebook_rooms.clone(),
                    notebook_id,
                    use_typed_frames,
                    default_python_env,
                    self.clone(),
                )
                .await
            }
            Handshake::Blob => self.handle_blob_connection(stream).await,
            Handshake::PoolStateSubscribe => self.handle_pool_state_subscription(stream).await,
        }
    }

    /// Handle a pool state subscription connection.
    ///
    /// Sends the current pool state immediately, then forwards all broadcasts
    /// until the client disconnects or the daemon shuts down.
    async fn handle_pool_state_subscription<S>(self: Arc<Self>, mut stream: S) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
        info!("[runtimed] Pool state subscriber connected");

        // Subscribe to pool state changes
        let mut rx = self.pool_state_changed.subscribe();

        // Send current state immediately
        let current_state = DaemonBroadcast::PoolState {
            uv_error: self.uv_pool.lock().await.get_error(),
            conda_error: self.conda_pool.lock().await.get_error(),
        };
        connection::send_json_frame(&mut stream, &current_state).await?;

        // Forward broadcasts until disconnect or shutdown
        loop {
            tokio::select! {
                result = rx.recv() => {
                    match result {
                        Ok(broadcast) => {
                            if connection::send_json_frame(&mut stream, &broadcast).await.is_err() {
                                break; // Client disconnected
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("[runtimed] Pool state subscriber lagged {} messages", n);
                            // Send current state to catch up
                            let state = DaemonBroadcast::PoolState {
                                uv_error: self.uv_pool.lock().await.get_error(),
                                conda_error: self.conda_pool.lock().await.get_error(),
                            };
                            if connection::send_json_frame(&mut stream, &state).await.is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                            break; // Sender dropped (daemon shutting down)
                        }
                    }
                }
                _ = self.shutdown_notify.notified() => {
                    if *self.shutdown.lock().await {
                        break;
                    }
                }
            }
        }

        info!("[runtimed] Pool state subscriber disconnected");
        Ok(())
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
        let env = self.uv_pool.lock().await.take();
        if let Some(ref e) = env {
            info!(
                "[runtimed] Took UV env for kernel launch: {:?}",
                e.venv_path
            );
            // Spawn replenishment
            let daemon = self.clone();
            tokio::spawn(async move {
                daemon.create_uv_env().await;
            });
        }
        env
    }

    /// Take a Conda environment from the pool for kernel launching.
    ///
    /// Returns `Some(PooledEnv)` if an environment is available, `None` otherwise.
    /// Automatically triggers replenishment when an environment is taken.
    pub async fn take_conda_env(self: &Arc<Self>) -> Option<PooledEnv> {
        let env = self.conda_pool.lock().await.take();
        if let Some(ref e) = env {
            info!(
                "[runtimed] Took Conda env for kernel launch: {:?}",
                e.venv_path
            );
            // Spawn replenishment
            let daemon = self.clone();
            tokio::spawn(async move {
                daemon.replenish_conda_env().await;
            });
        }
        env
    }

    /// Handle a single request.
    async fn handle_request(self: Arc<Self>, request: Request) -> Response {
        match request {
            Request::Take { env_type } => {
                let env = match env_type {
                    EnvType::Uv => self.uv_pool.lock().await.take(),
                    EnvType::Conda => self.conda_pool.lock().await.take(),
                };

                match env {
                    Some(env) => {
                        info!("[runtimed] Took {} env: {:?}", env_type, env.venv_path);
                        // Spawn replenishment
                        let daemon = self.clone();
                        match env_type {
                            EnvType::Uv => {
                                tokio::spawn(async move {
                                    daemon.create_uv_env().await;
                                });
                            }
                            EnvType::Conda => {
                                tokio::spawn(async move {
                                    daemon.replenish_conda_env().await;
                                });
                            }
                        }
                        Response::Env { env }
                    }
                    None => {
                        info!("[runtimed] Pool miss for {}", env_type);
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
                            info!("[runtimed] Returned UV env: {:?}", env.venv_path);
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
                            info!("[runtimed] Returned Conda env: {:?}", env.venv_path);
                        } else {
                            tokio::fs::remove_dir_all(&env.venv_path).await.ok();
                        }
                    }
                }
                Response::Returned
            }

            Request::Status => {
                let (uv_available, uv_warming, uv_error) = {
                    let pool = self.uv_pool.lock().await;
                    let (avail, warm) = pool.stats();
                    (avail, warm, pool.get_error())
                };
                let (conda_available, conda_warming, conda_error) = {
                    let pool = self.conda_pool.lock().await;
                    let (avail, warm) = pool.stats();
                    (avail, warm, pool.get_error())
                };
                Response::Stats {
                    stats: PoolStats {
                        uv_available,
                        uv_warming,
                        conda_available,
                        conda_warming,
                        uv_error,
                        conda_error,
                    },
                }
            }

            Request::Ping => Response::Pong,

            Request::Shutdown => {
                *self.shutdown.lock().await = true;
                self.shutdown_notify.notify_one();
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
                        has_kernel: room.has_kernel().await,
                        kernel_type,
                        env_source,
                        kernel_status,
                    });
                }
                Response::RoomsList { rooms: room_infos }
            }
        }
    }

    /// UV warming loop - maintains the UV pool.
    async fn uv_warming_loop(&self) {
        // Check if uv is available
        if !self.check_uv_available().await {
            warn!("[runtimed] uv not available, UV warming disabled");
            return;
        }

        info!("[runtimed] Starting UV warming loop");

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
            info!(
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
            info!(
                "[runtimed] Conda pool: {}/{} available, {} warming",
                available, self.config.conda_pool_size, warming
            );

            // Wait before checking again
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
            self.broadcast_pool_state().await;
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
                self.broadcast_pool_state().await;
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

        // Build specs: python + ipykernel + ipywidgets + default packages
        let match_spec_options = ParseMatchSpecOptions::strict();
        let specs: Vec<MatchSpec> = match (|| -> anyhow::Result<Vec<MatchSpec>> {
            let mut specs = vec![
                MatchSpec::from_str("python>=3.9", match_spec_options)?,
                MatchSpec::from_str("ipykernel", match_spec_options)?,
                MatchSpec::from_str("ipywidgets", match_spec_options)?,
            ];
            for pkg in &extra_conda_packages {
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
                self.broadcast_pool_state().await;
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
                self.broadcast_pool_state().await;
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
            self.broadcast_pool_state().await;
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
                self.broadcast_pool_state().await;
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
                self.broadcast_pool_state().await;
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
                self.broadcast_pool_state().await;
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
                self.broadcast_pool_state().await;
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
            self.broadcast_pool_state().await;
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
            self.broadcast_pool_state().await;
            return;
        }

        // Run warmup script
        self.warmup_conda_env(&python_path, &env_path).await;

        // Add to pool and check if we're clearing a previous error state
        let had_errors = {
            let mut pool = self.conda_pool.lock().await;
            let had = pool.failure_state.consecutive_failures > 0;
            pool.add(PooledEnv {
                env_type: EnvType::Conda,
                venv_path: env_path.clone(),
                python_path,
            });
            had
        };

        info!(
            "[runtimed] Conda environment ready: {:?} (pool: {}/{})",
            env_path,
            self.conda_pool.lock().await.stats().0,
            self.config.conda_pool_size
        );

        // Broadcast cleared state if we recovered from errors
        if had_errors {
            info!("[runtimed] Conda pool recovered from error state");
            self.broadcast_pool_state().await;
        }
    }

    /// Warm up a conda environment by running Python to trigger .pyc compilation.
    async fn warmup_conda_env(&self, python_path: &PathBuf, env_path: &PathBuf) {
        let warmup_script = r#"
import ipykernel
import IPython
import ipywidgets
import traitlets
import zmq
from ipykernel.kernelbase import Kernel
from ipykernel.ipkernel import IPythonKernel
from ipykernel.comm import CommManager
print("warmup complete")
"#;

        let warmup_result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::process::Command::new(python_path)
                .args(["-c", warmup_script])
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
            }
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(
                    "[runtimed] Conda warmup failed for {:?}: {}",
                    env_path,
                    stderr.lines().take(3).collect::<Vec<_>>().join(" | ")
                );
            }
            Ok(Err(e)) => {
                warn!("[runtimed] Failed to run conda warmup: {}", e);
            }
            Err(_) => {
                warn!("[runtimed] Conda warmup timed out");
            }
        }
    }

    /// Replenish a single Conda environment.
    async fn replenish_conda_env(&self) {
        self.conda_pool.lock().await.mark_warming(1);
        self.create_conda_env().await;
    }

    /// Broadcast current pool state to all subscribed clients.
    ///
    /// Called when pool error state changes (new error, error cleared, etc.).
    async fn broadcast_pool_state(&self) {
        let uv_error = self.uv_pool.lock().await.get_error();
        let conda_error = self.conda_pool.lock().await.get_error();

        // Only broadcast if there's something to report or if we're clearing errors
        let broadcast = DaemonBroadcast::PoolState {
            uv_error,
            conda_error,
        };

        // Send to all subscribers (ignore errors if no subscribers)
        let _ = self.pool_state_changed.send(broadcast);
    }

    /// Check if uv is available on PATH.
    async fn check_uv_available(&self) -> bool {
        tokio::process::Command::new("uv")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .map(|s| s.success())
            .unwrap_or(false)
    }

    /// Create a single UV environment and add it to the pool.
    async fn create_uv_env(&self) {
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
            self.broadcast_pool_state().await;
            return;
        }

        // Create venv (60 second timeout)
        let venv_result = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            tokio::process::Command::new("uv")
                .arg("venv")
                .arg(&venv_path)
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
                self.broadcast_pool_state().await;
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
                self.broadcast_pool_state().await;
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
                self.broadcast_pool_state().await;
                return;
            }
        }

        // Build install args: ipykernel + ipywidgets + default packages from settings
        let mut install_packages = vec!["ipykernel".to_string(), "ipywidgets".to_string()];

        // Read default uv packages from synced settings
        {
            let settings = self.settings.read().await;
            let synced = settings.get_all();
            let extra = synced.uv.default_packages;
            if !extra.is_empty() {
                info!("[runtimed] Including default uv packages: {:?}", extra);
                install_packages.extend(extra);
            }
        }

        // Install packages (120 second timeout)
        let mut install_args = vec![
            "pip".to_string(),
            "install".to_string(),
            "--python".to_string(),
            python_path.to_string_lossy().to_string(),
        ];
        install_args.extend(install_packages.clone());

        let install_result = tokio::time::timeout(
            std::time::Duration::from_secs(120),
            tokio::process::Command::new("uv")
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
                        // Check if this is a user-specified package (not ipykernel/ipywidgets)
                        let is_user_package = install_packages
                            .iter()
                            .skip(2) // Skip ipykernel and ipywidgets
                            .any(|p| p == pkg);

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
                self.broadcast_pool_state().await;
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
                self.broadcast_pool_state().await;
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
                self.broadcast_pool_state().await;
                return;
            }
        }

        // Warm up the environment (30 second timeout)
        let warmup_script = r#"
import ipykernel
import IPython
import ipywidgets
from ipykernel.kernelbase import Kernel
from ipykernel.ipkernel import IPythonKernel
print("warmup complete")
"#;

        let warmup_result = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            tokio::process::Command::new(&python_path)
                .args(["-c", warmup_script])
                .output(),
        )
        .await;

        match warmup_result {
            Ok(Ok(output)) if output.status.success() => {
                // Create marker file
                tokio::fs::write(venv_path.join(".warmed"), "").await.ok();
            }
            Ok(_) => {
                warn!("[runtimed] Warmup script failed, continuing anyway");
            }
            Err(_) => {
                warn!("[runtimed] Warmup script timed out, continuing anyway");
            }
        }

        info!("[runtimed] UV environment ready at {:?}", venv_path);

        // Add to pool and check if we're clearing a previous error state
        let had_errors = {
            let mut pool = self.uv_pool.lock().await;
            let had = pool.failure_state.consecutive_failures > 0;
            pool.add(PooledEnv {
                env_type: EnvType::Uv,
                venv_path,
                python_path,
            });
            had
        };

        // Broadcast cleared state if we recovered from errors
        if had_errors {
            info!("[runtimed] UV pool recovered from error state");
            self.broadcast_pool_state().await;
        }
    }
}

#[cfg(test)]
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

        PooledEnv {
            env_type: EnvType::Uv,
            venv_path,
            python_path,
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

        let taken = pool.take();
        assert!(taken.is_some());
        assert_eq!(taken.unwrap().venv_path, env.venv_path);
        assert_eq!(pool.available.len(), 0);
    }

    #[test]
    fn test_pool_take_empty() {
        let mut pool = Pool::new(3, 3600);
        let taken = pool.take();
        assert!(taken.is_none());
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
        };
        pool.available.push_back(PoolEntry {
            env: missing_env,
            created_at: Instant::now(),
        });

        // Add a valid env
        let valid_env = create_test_env(&temp_dir, "valid-env");
        pool.add(valid_env.clone());

        // Take should skip the missing one and return the valid one
        let taken = pool.take();
        assert!(taken.is_some());
        assert_eq!(taken.unwrap().venv_path, valid_env.venv_path);
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
        pool.take();
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
        assert!(config
            .socket_path
            .to_string_lossy()
            .contains("runtimed.sock"));
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
    fn test_pool_get_error() {
        let mut pool = Pool::new(3, 3600);

        // No error initially
        assert!(pool.get_error().is_none());

        // After failure, should have error
        pool.warming_failed_with_error(Some(PackageInstallError {
            failed_package: Some("scitkit-learn".to_string()),
            error_message: "Package scitkit-learn not found".to_string(),
        }));

        let err = pool.get_error().unwrap();
        assert_eq!(err.failed_package, Some("scitkit-learn".to_string()));
        assert_eq!(err.consecutive_failures, 1);
        assert!(err.message.contains("scitkit-learn"));
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
