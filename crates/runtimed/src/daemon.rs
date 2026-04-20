//! Pool daemon server implementation.
//!
//! The daemon manages prewarmed environment pools and handles requests from
//! notebook windows via IPC (Unix domain sockets on Unix, named pipes on Windows).

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use notify_debouncer_mini::DebounceEventResult;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{Mutex, Notify};
use tracing::{debug, error, info, warn};

#[cfg(unix)]
use tokio::net::UnixListener;

#[cfg(windows)]
use tokio::net::windows::named_pipe::ServerOptions;

use tokio::sync::RwLock;

use crate::blob_server;
use crate::blob_store::BlobStore;
use crate::connection::{self, Handshake};
use crate::notebook_sync_server::{NotebookRooms, PathIndex};
use crate::protocol::{BlobRequest, BlobResponse, Request, Response};
use crate::settings_doc::SettingsDoc;
use crate::singleton::DaemonLock;
use crate::task_supervisor::{spawn_best_effort, spawn_supervised};
use crate::{
    default_blob_store_dir, default_cache_dir, default_socket_path, is_pool_env_dir,
    is_within_cache_dir, pool_env_root, EnvType, PooledEnv,
};
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
            env_cache_max_age_secs: 86400, // 1 day
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
    /// Error classification for the frontend banner.
    error_kind: Option<String>,
    /// Whether the last failure was a network error (for shorter backoff).
    is_network_failure: bool,
}

/// Classify whether an error message indicates a network failure.
///
/// Network failures get shorter backoff since kernel-env's offline-first
/// path may succeed without network access. We use specific substrings
/// rather than broad terms to avoid false positives (e.g., a local socket
/// "connection" or a subprocess "timeout" is not a network failure).
fn is_network_error(error_msg: &str) -> bool {
    let lower = error_msg.to_lowercase();
    lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("connection timed out")
        || lower.contains("request timed out")
        || lower.contains("connect timed out")
        || lower.contains("dns")
        || lower.contains("network is unreachable")
        || lower.contains("network unreachable")
        || lower.contains("failed to fetch")
        || lower.contains("could not resolve")
        || lower.contains("no cached repodata")
}

/// Result of parsing a package installation error.
#[derive(Debug, Clone)]
struct PackageInstallError {
    /// The package that failed (if identifiable).
    failed_package: Option<String>,
    /// Full error message from uv.
    error_message: String,
    /// Error classification: "timeout", "invalid_package", "import_error", "setup_failed".
    error_kind: String,
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
                            error_kind: "invalid_package".to_string(),
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
            error_kind: "setup_failed".to_string(),
        });
    }

    None
}

/// Outcome of a warmup script execution.
enum WarmupOutcome {
    /// Warmup succeeded — environment is ready.
    Ok,
    /// Warmup timed out.
    Timeout,
    /// Warmup script failed (import error or process failure).
    ImportError(String),
}

/// Spawn background deletion of environment directories.
fn spawn_env_deletions(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    spawn_best_effort("env-deletions", async move {
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
    /// Number currently being created (reservation counter for deficit math).
    warming: usize,
    /// Paths of environments currently being warmed up (for GC protection).
    /// Populated when the env directory is created, removed on `add()` or failure.
    warming_paths: std::collections::HashSet<PathBuf>,
    /// Target pool size.
    target: usize,
    /// Maximum age in seconds.
    max_age_secs: u64,
    /// Failure tracking for exponential backoff.
    failure_state: FailureState,
}

const MIN_WARM_BASES: usize = 2;

fn uv_prewarmed_packages(
    extra: &[String],
    feature_flags: notebook_protocol::protocol::FeatureFlags,
) -> Vec<String> {
    let mut packages = vec![
        "ipykernel".to_string(),
        "ipywidgets".to_string(),
        "anywidget".to_string(),
        "nbformat".to_string(),
        "uv".to_string(),
    ];
    if feature_flags.bootstrap_dx {
        packages.push("nteract-kernel-launcher".to_string());
        packages.push("dx".to_string());
    }
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

impl Pool {
    fn new(target: usize, max_age_secs: u64) -> Self {
        Self {
            available: VecDeque::new(),
            warming: 0,
            warming_paths: std::collections::HashSet::new(),
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

    /// Evict pool entries whose installed package set no longer matches the
    /// current expected list, returning the pool-root paths so the caller can
    /// delete them from disk.
    ///
    /// Compares `PooledEnv::prewarmed_packages` as a sorted list against the
    /// caller-provided expected list. This catches changes to `uv.default_packages`,
    /// `conda.default_packages`, `pixi.default_packages`, and feature flags that
    /// affect the install set (e.g. `bootstrap_dx` adding `nteract-kernel-launcher`
    /// to UV envs).
    ///
    /// Returned paths are normalised via [`pool_env_root`] so callers delete the
    /// top-level pool directory (Pixi envs are nested under `.pixi/envs/default`
    /// but GC and eviction must both operate on the `runtimed-pixi-*` root).
    ///
    /// Note: envs that are still warming are not affected. Their `prewarmed_packages`
    /// is the snapshot the warming task captured at install time; once they finish,
    /// the next sweep here will evict them if the expected list has drifted again.
    fn evict_mismatched_packages(&mut self, expected: &[String]) -> Vec<PathBuf> {
        let mut expected_sorted: Vec<String> = expected.to_vec();
        expected_sorted.sort();
        let mut evicted = Vec::new();
        let mut kept = VecDeque::new();
        for entry in self.available.drain(..) {
            let mut entry_pkgs = entry.env.prewarmed_packages.clone();
            entry_pkgs.sort();
            if entry_pkgs == expected_sorted {
                kept.push_back(entry);
            } else {
                evicted.push(pool_env_root(&entry.env.venv_path));
            }
        }
        self.available = kept;
        evicted
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
        self.warming_paths.remove(&pool_env_root(&env.venv_path));
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
            self.failure_state.is_network_failure = is_network_error(&err.error_message);
            self.failure_state.last_error = Some(err.error_message);
            self.failure_state.failed_package = err.failed_package;
            self.failure_state.error_kind = Some(err.error_kind);
        } else {
            self.failure_state.is_network_failure = false;
        }
    }

    /// Mark that warming failed for a specific path (unregisters the path and records the error).
    fn warming_failed_for_path(&mut self, path: &Path, error: Option<PackageInstallError>) {
        self.warming_paths.remove(path);
        self.warming_failed_with_error(error);
    }

    /// Reset failure state (called on settings change).
    fn reset_failure_state(&mut self) {
        self.failure_state = FailureState::default();
    }

    /// Calculate backoff delay based on consecutive failures.
    ///
    /// Returns Duration::ZERO if no failures, otherwise exponential backoff:
    /// - Network failures: 10s, 20s, 40s, max 60s (shorter — offline-first may succeed)
    /// - Other failures: 30s, 60s, 120s, 240s, max 300s (5 min)
    fn backoff_delay(&self) -> std::time::Duration {
        if self.failure_state.consecutive_failures == 0 {
            return std::time::Duration::ZERO;
        }

        if self.failure_state.is_network_failure {
            // Network failures get shorter backoff: 10s base, 60s cap.
            // kernel-env's offline-first path may succeed without network.
            let base = std::time::Duration::from_secs(10);
            let max = std::time::Duration::from_secs(60);
            let delay = base
                * 2u32.saturating_pow(
                    self.failure_state
                        .consecutive_failures
                        .saturating_sub(1)
                        .min(4),
                );
            std::cmp::min(delay, max)
        } else {
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

    fn set_target(&mut self, target: usize) {
        self.target = target;
    }

    fn target(&self) -> usize {
        self.target
    }

    /// Mark that we're starting to create N environments.
    fn mark_warming(&mut self, count: usize) {
        self.warming += count;
    }

    /// Register a warming path so GC won't delete it while it's being set up.
    fn register_warming_path(&mut self, path: PathBuf) {
        self.warming_paths.insert(path);
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

/// Which environment pool a `WarmingGuard` protects.
#[derive(Clone, Copy)]
enum PoolKind {
    Uv,
    Conda,
    Pixi,
}

/// RAII guard for pool warming paths. On drop (including panic unwind), rolls
/// back the warming counter and unregisters the path. Call `commit()` on
/// success to suppress the rollback, or `fail_with()` to record a specific
/// error before consuming the guard.
struct WarmingGuard {
    inner: Option<WarmingGuardInner>,
}

struct WarmingGuardInner {
    daemon: Arc<Daemon>,
    path: PathBuf,
    kind: PoolKind,
}

impl WarmingGuard {
    fn new(daemon: Arc<Daemon>, path: PathBuf, kind: PoolKind) -> Self {
        Self {
            inner: Some(WarmingGuardInner { daemon, path, kind }),
        }
    }

    /// Consume the guard on success — suppresses the Drop rollback.
    fn commit(&mut self) {
        self.inner.take();
    }

    /// Record a specific error and consume the guard. Caller must be in an
    /// async context (this is not used from Drop).
    async fn fail_with(&mut self, error: Option<PackageInstallError>) {
        if let Some(inner) = self.inner.take() {
            inner.rollback(error).await;
        }
    }
}

impl WarmingGuardInner {
    async fn rollback(self, error: Option<PackageInstallError>) {
        let pool = match self.kind {
            PoolKind::Uv => &self.daemon.uv_pool,
            PoolKind::Conda => &self.daemon.conda_pool,
            PoolKind::Pixi => &self.daemon.pixi_pool,
        };
        pool.lock().await.warming_failed_for_path(&self.path, error);
        match self.kind {
            PoolKind::Uv => self.daemon.pool_ready_uv.notify_waiters(),
            PoolKind::Conda => self.daemon.pool_ready_conda.notify_waiters(),
            PoolKind::Pixi => self.daemon.pool_ready_pixi.notify_waiters(),
        }
        self.daemon.update_pool_doc().await;
    }
}

impl Drop for WarmingGuard {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            // Panic or early exit without commit/fail_with — roll back
            // asynchronously. If the runtime is shutting down, the spawned
            // task may not execute, but pool accounting is irrelevant then.
            tokio::spawn(async move {
                inner.rollback(None).await;
            });
        }
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
    /// When the daemon process began. Reported via Ping for diagnostics.
    started_at: chrono::DateTime<chrono::Utc>,
    /// Per-notebook Automerge sync rooms.
    notebook_rooms: NotebookRooms,
    /// Secondary index: canonical .ipynb path → room UUID.
    /// Kept in sync with `notebook_rooms` by `get_or_create_room` (insert)
    /// and the eviction loop (remove). Queried by `find_room_by_path`.
    pub(crate) path_index: Arc<tokio::sync::Mutex<PathIndex>>,
    /// Set to `true` the first time any client causes a room to be
    /// acquired in `notebook_rooms` (via `get_or_create_room`). Used by
    /// the zero-room sweep-skip guard to distinguish "post-restart, no
    /// client has reconnected yet" (skip, we don't yet know what refs
    /// are needed) from "idle daemon whose user closed every notebook"
    /// (sweep — refs legitimately empty, persisted-doc walk still
    /// gathers anything the user might reopen).
    ///
    /// Flipped at acquisition, not at GC sample time, because rooms can
    /// open and close between 30-minute GC cycles. Sampling in the GC
    /// loop would miss short-lived sessions and pin the daemon back in
    /// the post-restart state forever.
    rooms_ever_seen: std::sync::atomic::AtomicBool,
    uv_warming_respawns: std::sync::atomic::AtomicU32,
    conda_warming_respawns: std::sync::atomic::AtomicU32,
    pixi_warming_respawns: std::sync::atomic::AtomicU32,
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

    /// Get the full list of UV pool packages (base + user default_packages).
    pub async fn uv_pool_packages(&self) -> Vec<String> {
        let settings = self.settings.read().await;
        let synced = settings.get_all();
        uv_prewarmed_packages(&synced.uv.default_packages, synced.feature_flags())
    }

    /// Snapshot the user's feature-flag settings.
    pub async fn feature_flags(&self) -> notebook_protocol::protocol::FeatureFlags {
        self.settings.read().await.get_all().feature_flags()
    }

    /// Get the full list of Conda pool packages (base + user default_packages).
    pub async fn conda_pool_packages(&self) -> Vec<String> {
        let settings = self.settings.read().await;
        let synced = settings.get_all();
        conda_prewarmed_packages(&synced.conda.default_packages)
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

        // Pool sizes now come from settings.json (imported via apply_json_changes)
        // or from SettingsDoc defaults if not set in JSON.

        // Write the settings JSON Schema for editor autocomplete
        if let Err(e) = crate::settings_doc::write_settings_schema() {
            tracing::warn!("[settings] Failed to write schema file: {}", e);
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
            started_at: chrono::Utc::now(),
            notebook_rooms: Arc::new(Mutex::new(HashMap::new())),
            path_index: Arc::new(tokio::sync::Mutex::new(PathIndex::new())),
            rooms_ever_seen: std::sync::atomic::AtomicBool::new(false),
            uv_warming_respawns: std::sync::atomic::AtomicU32::new(0),
            conda_warming_respawns: std::sync::atomic::AtomicU32::new(0),
            pixi_warming_respawns: std::sync::atomic::AtomicU32::new(0),
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

    /// Build the `DaemonInfo` response from live daemon state.
    ///
    /// This carries every field that `daemon.json` used to — `pid`,
    /// `version`, `started_at`, `blob_port`, plus the dev-mode worktree
    /// fields — so clients can query the daemon directly over the socket
    /// instead of reading a sidecar file. Keeping it in its own message
    /// (not overloaded onto `Pong`) means the frequent liveness-check
    /// path stays tiny and the one-shot discovery path carries the full
    /// payload.
    async fn build_daemon_info(&self) -> Response {
        let blob_port = *self.blob_port.lock().await;
        let (worktree_path, workspace_description) = if crate::is_dev_mode() {
            (
                crate::get_workspace_path().map(|p| p.to_string_lossy().to_string()),
                crate::get_workspace_name(),
            )
        } else {
            (None, None)
        };
        Response::DaemonInfo {
            protocol_version: notebook_protocol::connection::PROTOCOL_VERSION,
            daemon_version: crate::daemon_version().to_string(),
            pid: std::process::id(),
            started_at: self.started_at,
            blob_port,
            worktree_path,
            workspace_description,
        }
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

        // Start the blob HTTP server (also serves renderer plugin assets)
        let blob_port =
            match blob_server::start_blob_server(self.blob_store.clone(), Some(self.clone())).await
            {
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

        // Write `daemon.json` so older clients can still discover us.
        // Retained as a one-release compatibility shim for stale
        // `runt-mcp` / `runt-proxy` proxies that predate `GetDaemonInfo`.
        // New consumers go through the socket (see
        // `runtimed_client::daemon_connection`). Target v2.4 for removal.
        if let Err(e) = self
            ._lock
            .write_info(&self.config.socket_path.to_string_lossy(), blob_port)
        {
            error!("[runtimed] Failed to write daemon info: {}", e);
        }

        // Reap any orphaned agent process groups from a previous crash
        #[cfg(unix)]
        {
            let reaped = crate::process_groups::reap_orphaned_agents();
            if reaped > 0 {
                info!(
                    "[runtimed] Reaped {} orphaned agent process group(s)",
                    reaped
                );
            }
        }

        // Register global shutdown trigger for notebook_sync_server debouncers.
        {
            let shutdown_daemon = self.clone();
            crate::notebook_sync_server::register_shutdown_trigger(Arc::new(move || {
                let d = shutdown_daemon.clone();
                tokio::spawn(async move { d.trigger_shutdown().await });
            }));
        }

        // Find and reuse existing environments from previous runs
        self.find_existing_environments().await;

        // Seed PoolDoc with initial state (pool sizes, any recovered envs)
        self.update_pool_doc().await;

        // Spawn the warming loops
        let uv_daemon = self.clone();
        let uv_panic_daemon = self.clone();
        spawn_supervised(
            "uv-warming-loop",
            async move { uv_daemon.uv_warming_loop().await },
            move |_| {
                use std::sync::atomic::Ordering;
                if uv_panic_daemon
                    .uv_warming_respawns
                    .compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed)
                    .is_ok()
                {
                    let d = uv_panic_daemon.clone();
                    let d2 = uv_panic_daemon;
                    spawn_supervised(
                        "uv-warming-loop",
                        async move { d.uv_warming_loop().await },
                        move |_| {
                            tokio::spawn(async move { d2.trigger_shutdown().await });
                        },
                    );
                } else {
                    let d = uv_panic_daemon;
                    tokio::spawn(async move { d.trigger_shutdown().await });
                }
            },
        );

        let conda_daemon = self.clone();
        let conda_panic_daemon = self.clone();
        spawn_supervised(
            "conda-warming-loop",
            async move { conda_daemon.conda_warming_loop().await },
            move |_| {
                use std::sync::atomic::Ordering;
                if conda_panic_daemon
                    .conda_warming_respawns
                    .compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed)
                    .is_ok()
                {
                    let d = conda_panic_daemon.clone();
                    let d2 = conda_panic_daemon;
                    spawn_supervised(
                        "conda-warming-loop",
                        async move { d.conda_warming_loop().await },
                        move |_| {
                            tokio::spawn(async move { d2.trigger_shutdown().await });
                        },
                    );
                } else {
                    let d = conda_panic_daemon;
                    tokio::spawn(async move { d.trigger_shutdown().await });
                }
            },
        );

        let pixi_daemon = self.clone();
        let pixi_panic_daemon = self.clone();
        spawn_supervised(
            "pixi-warming-loop",
            async move { pixi_daemon.pixi_warming_loop().await },
            move |_| {
                use std::sync::atomic::Ordering;
                if pixi_panic_daemon
                    .pixi_warming_respawns
                    .compare_exchange(0, 1, Ordering::SeqCst, Ordering::Relaxed)
                    .is_ok()
                {
                    let d = pixi_panic_daemon.clone();
                    let d2 = pixi_panic_daemon;
                    spawn_supervised(
                        "pixi-warming-loop",
                        async move { d.pixi_warming_loop().await },
                        move |_| {
                            tokio::spawn(async move { d2.trigger_shutdown().await });
                        },
                    );
                } else {
                    let d = pixi_panic_daemon;
                    tokio::spawn(async move { d.trigger_shutdown().await });
                }
            },
        );

        // Spawn the environment GC loop
        let gc_daemon = self.clone();
        spawn_best_effort("env-gc-loop", async move {
            gc_daemon.env_gc_loop().await;
        });

        // Spawn the settings.json file watcher
        let watcher_daemon = self.clone();
        spawn_best_effort("watch-settings-json", async move {
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

        // Shut down all runtime agents before exiting.
        //
        // Runtime agents are spawned in their own process group (process_group(0)),
        // so they do NOT receive the SIGINT/SIGTERM that the daemon receives.
        // Their kernel subprocesses inherit the agent's PGID, so killing the
        // agent's process group kills the kernel too.
        //
        // Without explicit shutdown here, agent process groups become orphans.
        // We cannot rely on Drop alone because:
        //   1. The runtime agent handle is behind Arc<Mutex<Option<...>>> inside
        //      Arc<NotebookRoom> — multiple spawned tasks hold Arc clones that
        //      may not all unwind during tokio runtime teardown.
        //   2. A second ctrl-c or SIGKILL skips destructors entirely.
        //
        // To avoid holding the notebook_rooms lock across .await points, first
        // drain the map into an owned collection, then shut down agents.
        let drained_rooms = {
            let mut rooms = self.notebook_rooms.lock().await;
            rooms.drain().collect::<Vec<_>>()
        };

        for (notebook_uuid, room) in drained_rooms {
            // Shut down runtime agent via RPC before dropping handle
            {
                let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
                if has_runtime_agent {
                    info!(
                        "[runtimed] Shutting down runtime agent for notebook on exit: {}",
                        notebook_uuid
                    );
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(10),
                        crate::notebook_sync_server::send_runtime_agent_request(
                            &room,
                            notebook_protocol::protocol::RuntimeAgentRequest::ShutdownKernel,
                        ),
                    )
                    .await;
                }
                // Unregister from process group registry and drop handle
                {
                    let mut ra_guard = room.runtime_agent_handle.lock().await;
                    if let Some(ref handle) = *ra_guard {
                        handle.unregister();
                    }
                    *ra_guard = None;
                }
                {
                    let mut tx = room.runtime_agent_request_tx.lock().await;
                    *tx = None;
                }
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
                            spawn_best_effort("unix-connection", async move {
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
                    spawn_best_effort("pipe-connection", async move {
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

            let uv_pkgs =
                uv_prewarmed_packages(&synced.uv.default_packages, synced.feature_flags());
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
            if name.starts_with(crate::POOL_PREFIX_UV) {
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
                    if let Err(e) = tokio::fs::remove_dir_all(&env_path).await {
                        warn!(
                            "[runtimed] Failed to clean up invalid UV env {:?}: {}",
                            env_path, e
                        );
                    }
                }
            }
            // Check for runtimed-conda-* directories
            else if name.starts_with(crate::POOL_PREFIX_CONDA) {
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
                    if let Err(e) = tokio::fs::remove_dir_all(&env_path).await {
                        warn!(
                            "[runtimed] Failed to clean up invalid Conda env {:?}: {}",
                            env_path, e
                        );
                    }
                }
            }
            // Check for runtimed-pixi-* directories
            else if name.starts_with(crate::POOL_PREFIX_PIXI) {
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
                    if let Err(e) = tokio::fs::remove_dir_all(&env_path).await {
                        warn!(
                            "[runtimed] Failed to clean up invalid Pixi env {:?}: {}",
                            env_path, e
                        );
                    }
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
                tracing::debug!("[runtimed] Legacy client detected (no magic preamble)");
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
                // For the legacy NotebookSync handshake:
                // - UUID notebook_id → untitled room (path=None)
                // - Path notebook_id → file-backed room (path=Some)
                //
                // When notebook_id is a path, canonicalize and consult path_index
                // before minting a new UUID. Without this, each reconnect creates a
                // fresh UUID and a duplicate room (two file watchers, two autosave
                // debouncers, two writers on the same .ipynb — zombie rooms).
                let room = {
                    let (ns_uuid, ns_path) = if let Ok(parsed) = uuid::Uuid::parse_str(&notebook_id)
                    {
                        (parsed, None)
                    } else {
                        // notebook_id is a path — canonicalize and look up
                        // existing room.
                        let raw = PathBuf::from(&notebook_id);
                        let canonical = match tokio::fs::canonicalize(&raw).await {
                            Ok(c) => c,
                            Err(e) => {
                                warn!(
                                        "[daemon] canonicalize({}) for NotebookSync handshake failed: {}, using raw path",
                                        notebook_id, e
                                    );
                                raw
                            }
                        };
                        match crate::notebook_sync_server::find_room_by_path(
                            &self.notebook_rooms,
                            &self.path_index,
                            &canonical,
                        )
                        .await
                        {
                            Some(existing) => (existing.id, Some(canonical)),
                            None => (uuid::Uuid::new_v4(), Some(canonical)),
                        }
                    };
                    crate::notebook_sync_server::get_or_create_room(
                        &self.notebook_rooms,
                        &self.path_index,
                        ns_uuid,
                        ns_path,
                        &docs_dir,
                        self.blob_store.clone(),
                        false, // NotebookSync handshake is always persistent
                    )
                    .await
                };
                self.mark_rooms_ever_seen();
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
                ephemeral,
            } => {
                self.handle_create_notebook(stream, runtime, working_dir, notebook_id, ephemeral)
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
                    // notebook_id from runtime agent is always a UUID string
                    uuid::Uuid::parse_str(&notebook_id)
                        .ok()
                        .and_then(|uuid| rooms.get(&uuid).cloned())
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

        // Diagnostic: flag suspicious path shapes. UUID-shaped paths (no slash,
        // no extension, parses as a UUID) almost certainly indicate a bug
        // upstream — someone is passing a notebook_id string as a path and
        // the daemon would resolve it via current_dir.join(uuid), creating
        // stray `{cwd}/{uuid}.ipynb` files.
        {
            let looks_uuid_shaped = !path.contains('/')
                && !path.contains('\\')
                && !path.ends_with(".ipynb")
                && uuid::Uuid::parse_str(&path).is_ok();
            if looks_uuid_shaped {
                warn!(
                    "[runtimed] OpenNotebook received bare-UUID path {:?} — \
                     this usually means a caller passed a notebook_id as a path. \
                     The daemon will resolve it against cwd, creating a stray file.",
                    path
                );
            }
        }

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
                ephemeral: false,
            };
            send_json_frame(writer, &response).await?;
            Ok(())
        }

        if looks_like_untitled_notebook_path(&path) {
            let (_reader, mut writer) = tokio::io::split(stream);
            send_error_response(
                &mut writer,
                format!(
                    "Refusing to open bare UUID '{}' as a file path. \
                     Untitled notebooks must reconnect via notebook_id, not OpenNotebook path.",
                    path
                ),
            )
            .await?;
            return Ok(());
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
        // First check if an existing room already owns this canonical path.
        // The path_index gives O(1) lookup without scanning all rooms.
        let docs_dir = self.config.notebook_docs_dir.clone();
        let canonical_path = PathBuf::from(&notebook_id);
        let room = {
            if let Some(existing) = crate::notebook_sync_server::find_room_by_path(
                &self.notebook_rooms,
                &self.path_index,
                &canonical_path,
            )
            .await
            {
                existing
            } else {
                let uuid = uuid::Uuid::new_v4();
                let path = Some(canonical_path.clone());
                crate::notebook_sync_server::get_or_create_room(
                    &self.notebook_rooms,
                    &self.path_index,
                    uuid,
                    path,
                    &docs_dir,
                    self.blob_store.clone(),
                    false, // OpenNotebook handshake is always persistent
                )
                .await
            }
        };
        self.mark_rooms_ever_seen();

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
            let mut create_error = None;
            let count = {
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
                            create_error = Some(e);
                        }
                    }
                }
                doc.cell_count()
            }; // doc lock dropped
            if let Some(e) = create_error {
                let (_reader, mut writer) = tokio::io::split(stream);
                send_error_response(
                    &mut writer,
                    format!("Failed to create notebook '{}': {}", path, e),
                )
                .await?;
                return Ok(());
            }
            (count, None) // No streaming load needed
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

        // Get trust state (already verified during room creation).
        // Scope the read guard so it's dropped before the .await on send_json_frame.
        let needs_trust_approval = {
            let trust_state = room.trust_state.read().await;
            !matches!(
                trust_state.status,
                runt_trust::TrustStatus::Trusted | runt_trust::TrustStatus::NoDependencies
            )
        };

        // Send NotebookConnectionInfo response. The wire notebook_id is the
        // room's UUID (stable across the life of the room); the local
        // `notebook_id` variable in this handler is the canonical path string
        // used for logging and file-watcher wiring below.
        let (reader, mut writer) = tokio::io::split(stream);
        let response = NotebookConnectionInfo {
            protocol: PROTOCOL_V2.to_string(),
            protocol_version: Some(PROTOCOL_VERSION),
            daemon_version: Some(crate::daemon_version().to_string()),
            notebook_id: room.id.to_string(),
            cell_count,
            needs_trust_approval,
            error: None,
            ephemeral: false,
        };
        send_json_frame(&mut writer, &response).await?;

        // working_dir derived from path's parent directory
        let working_dir_path = path_buf.parent().map(|p| p.to_path_buf());

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
        ephemeral: Option<bool>,
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
        let ephemeral = ephemeral.unwrap_or(false);

        // Create room for this notebook. For CreateNotebook, the notebook_id is
        // always a UUID (new room) or an existing UUID (session restore).
        let docs_dir = self.config.notebook_docs_dir.clone();
        let uuid = uuid::Uuid::parse_str(&notebook_id).unwrap_or_else(|_| uuid::Uuid::new_v4());
        let room = crate::notebook_sync_server::get_or_create_room(
            &self.notebook_rooms,
            &self.path_index,
            uuid,
            None, // CreateNotebook creates untitled rooms with no file path
            &docs_dir,
            self.blob_store.clone(),
            ephemeral,
        )
        .await;
        self.mark_rooms_ever_seen();

        // Populate the room's doc with the empty notebook content — but only if the
        // room is empty. If a persisted doc was loaded (session restore with notebook_id
        // hint), the room already has cells and we skip creation.
        let (cell_count, create_error) = {
            let mut doc = room.doc.write().await;
            let mut err = None;
            if doc.cell_count() > 0 {
                // Room already has content (loaded from persisted doc)
                info!(
                    "[runtimed] Room {} already has {} cells (restored from persisted doc)",
                    notebook_id,
                    doc.cell_count()
                );
            } else {
                match crate::notebook_sync_server::create_empty_notebook(
                    &mut doc,
                    &runtime,
                    default_python_env.clone(),
                    Some(&notebook_id),
                ) {
                    Ok(_) => {}
                    Err(e) => {
                        err = Some(e);
                    }
                }
            }
            (doc.cell_count(), err)
        }; // doc lock dropped

        if let Some(e) = create_error {
            // Remove the room to prevent stale state (consistency with OpenNotebook)
            // CreateNotebook rooms have no path, so no path_index cleanup needed.
            {
                let mut rooms = self.notebook_rooms.lock().await;
                rooms.remove(&uuid);
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
                ephemeral: false,
            };
            send_json_frame(&mut writer, &response).await?;
            let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
            return Ok(());
        }

        // Send NotebookConnectionInfo response
        // New notebooks have no deps, so no trust approval needed.
        // Always send the room's UUID on the wire, even when the caller
        // provided a notebook_id_hint — room.id is the canonical source.
        let (reader, mut writer) = tokio::io::split(stream);
        let response = NotebookConnectionInfo {
            protocol: PROTOCOL_V2.to_string(),
            protocol_version: Some(PROTOCOL_VERSION),
            daemon_version: Some(crate::daemon_version().to_string()),
            notebook_id: room.id.to_string(),
            cell_count,
            needs_trust_approval: false,
            error: None,
            ephemeral,
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
                    let response = {
                        let port = self.blob_port.lock().await;
                        match *port {
                            Some(p) => BlobResponse::Port { port: p },
                            None => BlobResponse::Error {
                                error: "blob server not running".to_string(),
                            },
                        }
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
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);

        loop {
            let (env, stale_paths) = self.uv_pool.lock().await.take();
            spawn_env_deletions(stale_paths);

            if let Some(e) = env {
                info!(
                    "[runtimed] Took UV env for kernel launch: {:?}",
                    e.venv_path
                );
                let daemon = self.clone();
                spawn_best_effort("uv-replenish", async move {
                    daemon.create_uv_env().await;
                });
                return Some(e);
            }

            if self.uv_pool.lock().await.target() == 0 {
                return None;
            }

            let (warming, can_retry, retry_in_secs, should_spawn) = {
                let mut pool = self.uv_pool.lock().await;
                let (_, warming) = pool.stats();
                let can_retry = pool.should_retry();
                let retry_in_secs = pool.retry_in_secs();

                if warming == 0 && can_retry {
                    pool.mark_warming(1);
                    (1, can_retry, retry_in_secs, true)
                } else {
                    (warming, can_retry, retry_in_secs, false)
                }
            }; // pool lock dropped
            if should_spawn {
                let daemon = self.clone();
                spawn_best_effort("uv-retry", async move {
                    daemon.create_uv_env().await;
                });
            }

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
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(120);

        loop {
            let (env, stale_paths) = self.conda_pool.lock().await.take();
            spawn_env_deletions(stale_paths);

            if let Some(e) = env {
                info!(
                    "[runtimed] Took Conda env for kernel launch: {:?}",
                    e.venv_path
                );
                let daemon = self.clone();
                spawn_best_effort("conda-replenish", async move {
                    daemon.replenish_conda_env().await;
                });
                return Some(e);
            }

            if self.conda_pool.lock().await.target() == 0 {
                return None;
            }

            let (warming, can_retry, retry_in_secs, should_spawn) = {
                let mut pool = self.conda_pool.lock().await;
                let (_, warming) = pool.stats();
                let can_retry = pool.should_retry();
                let retry_in_secs = pool.retry_in_secs();

                if warming == 0 && can_retry {
                    pool.mark_warming(1);
                    (1, can_retry, retry_in_secs, true)
                } else {
                    (warming, can_retry, retry_in_secs, false)
                }
            }; // pool lock dropped
            if should_spawn {
                let daemon = self.clone();
                spawn_best_effort("conda-retry", async move {
                    daemon.create_conda_env().await;
                });
            }

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
            spawn_best_effort("pixi-replenish", async move {
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
                // Return an environment to the pool (e.g., if notebook closed without using it).
                // Check if the pool is full under the lock, then drop the lock before
                // any async filesystem cleanup.
                let should_delete = match env.env_type {
                    EnvType::Uv => {
                        let mut pool = self.uv_pool.lock().await;
                        if pool.available.len() < pool.target {
                            pool.available.push_back(PoolEntry {
                                env: env.clone(),
                                created_at: Instant::now(),
                            });
                            debug!("[runtimed] Returned UV env: {:?}", env.venv_path);
                            false
                        } else {
                            true
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
                            false
                        } else {
                            true
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
                            false
                        } else {
                            true
                        }
                    }
                }; // pool lock dropped
                if should_delete {
                    tokio::fs::remove_dir_all(&env.venv_path).await.ok();
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

            Request::GetDaemonInfo => self.build_daemon_info().await,

            Request::Shutdown => {
                self.trigger_shutdown().await;
                Response::ShuttingDown
            }

            Request::FlushPool => {
                info!("[runtimed] Flushing all pooled environments");

                // Drain pools under locks, then delete directories after locks drop
                let uv_entries: Vec<_> = {
                    let mut pool = self.uv_pool.lock().await;
                    pool.available.drain(..).collect()
                };
                for entry in uv_entries {
                    info!("[runtimed] Removing UV env: {:?}", entry.env.venv_path);
                    tokio::fs::remove_dir_all(&entry.env.venv_path).await.ok();
                }

                let conda_entries: Vec<_> = {
                    let mut pool = self.conda_pool.lock().await;
                    pool.available.drain(..).collect()
                };
                for entry in conda_entries {
                    info!("[runtimed] Removing Conda env: {:?}", entry.env.venv_path);
                    tokio::fs::remove_dir_all(&entry.env.venv_path).await.ok();
                }

                // Warming loops will detect the deficit and rebuild on their next iteration
                self.update_pool_doc().await;
                Response::Flushed
            }

            Request::InspectNotebook { notebook_id } => {
                info!("[runtimed] Inspecting notebook: {}", notebook_id);

                // First try to get from an active room.
                // Clone the Arc and drop the lock before any .await to avoid
                // holding notebook_rooms across async calls.
                let maybe_room = {
                    let rooms = self.notebook_rooms.lock().await;
                    uuid::Uuid::parse_str(&notebook_id)
                        .ok()
                        .and_then(|uuid| rooms.get(&uuid).cloned())
                };
                if let Some(room) = maybe_room {
                    let cells = {
                        let doc = room.doc.read().await;
                        doc.get_cells()
                    }; // doc read lock dropped
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
                // Snapshot room references and drop the lock before any .await
                // to avoid holding notebook_rooms across async calls (convoy deadlock).
                let snapshot: Vec<_> = {
                    let rooms = self.notebook_rooms.lock().await;
                    rooms
                        .iter()
                        .map(|(id, room)| (id.to_string(), room.clone()))
                        .collect()
                };
                let mut room_infos = Vec::new();
                for (notebook_id, room) in &snapshot {
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
                        ephemeral: room.is_ephemeral.load(std::sync::atomic::Ordering::Relaxed),
                    });
                }
                Response::RoomsList { rooms: room_infos }
            }

            Request::ShutdownNotebook { notebook_id } => {
                // Remove the room from the map and drop the lock before any .await
                // to avoid holding notebook_rooms across async teardown calls.
                //
                // Note: the room is already removed from the map so concurrent
                // get_or_create_room() calls will create a fresh room. This is
                // identical to the original behavior (remove() was always called
                // before teardown), but now the lock isn't held during the async
                // teardown that follows.
                let maybe_room = {
                    let mut rooms = self.notebook_rooms.lock().await;
                    uuid::Uuid::parse_str(&notebook_id)
                        .ok()
                        .and_then(|uuid| rooms.remove(&uuid))
                };
                // Clean up path_index if the room had a path.
                if let Some(ref room) = maybe_room {
                    let path = room.path.read().await.clone();
                    if let Some(p) = path {
                        self.path_index.lock().await.remove(&p);
                    }
                }
                if let Some(room) = maybe_room {
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
                        // Scope each lock independently to avoid cross-lock ordering.
                        {
                            let mut ra_guard = room.runtime_agent_handle.lock().await;
                            *ra_guard = None;
                        }
                        {
                            let mut tx = room.runtime_agent_request_tx.lock().await;
                            *tx = None;
                        }
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
        // Snapshot room references and drop the lock before any .await
        // to avoid holding notebook_rooms across async calls.
        let snapshot: Vec<_> = {
            let rooms = self.notebook_rooms.lock().await;
            rooms.values().cloned().collect()
        };
        let mut paths = std::collections::HashSet::new();
        for room in &snapshot {
            // Check runtime-agent-backed kernel. Normalise to the top-level
            // pool dir so that GC's top-level scan will match pixi envs
            // whose venv_path is nested (e.g. .pixi/envs/default).
            if let Some(ref env_path) = *room.runtime_agent_env_path.read().await {
                paths.insert(pool_env_root(env_path));
            }
        }
        paths
    }

    /// Background GC loop for content-addressed environment caches.
    ///
    /// Runs once after a 60-second startup delay, then every 30 minutes.
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

            // Clean up orphaned pool env directories (runtimed-uv-*, runtimed-conda-*,
            // runtimed-pixi-*) that are not tracked by the pool and not in use by
            // running kernels. These can leak when a notebook takes a pool env, mutates
            // it, and then the room is evicted without cleanup.
            {
                let cache_dir = &self.config.cache_dir;
                if cache_dir.exists() {
                    // Collect pool-tracked paths, normalised to top-level
                    // pool dirs so pixi's nested venv_path
                    // (runtimed-pixi-{uuid}/.pixi/envs/default) matches the
                    // top-level directory that the scan below sees.
                    // Also includes warming paths (mid-creation) to avoid
                    // racing with in-progress warmup tasks.
                    let mut tracked: std::collections::HashSet<PathBuf> =
                        std::collections::HashSet::new();
                    {
                        let pool = self.uv_pool.lock().await;
                        for entry in &pool.available {
                            tracked.insert(pool_env_root(&entry.env.venv_path));
                        }
                        tracked.extend(pool.warming_paths.iter().cloned());
                    }
                    {
                        let pool = self.conda_pool.lock().await;
                        for entry in &pool.available {
                            tracked.insert(pool_env_root(&entry.env.venv_path));
                        }
                        tracked.extend(pool.warming_paths.iter().cloned());
                    }
                    {
                        let pool = self.pixi_pool.lock().await;
                        for entry in &pool.available {
                            tracked.insert(pool_env_root(&entry.env.venv_path));
                        }
                        tracked.extend(pool.warming_paths.iter().cloned());
                    }

                    let mut orphans_deleted = 0;
                    if let Ok(mut entries) = tokio::fs::read_dir(cache_dir).await {
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            let name = entry.file_name().to_string_lossy().to_string();
                            if !is_pool_env_dir(&name) {
                                continue;
                            }
                            let path = entry.path();
                            if tracked.contains(&path) || in_use.contains(&path) {
                                continue;
                            }
                            if !is_within_cache_dir(&path, cache_dir) {
                                warn!(
                                    "[runtimed] GC: refusing to delete {:?} (not within cache dir)",
                                    path
                                );
                                continue;
                            }
                            info!("[runtimed] GC: removing orphaned pool env {:?}", path);
                            if let Err(e) = tokio::fs::remove_dir_all(&path).await {
                                warn!(
                                    "[runtimed] GC: failed to remove orphaned pool env {:?}: {}",
                                    path, e
                                );
                            } else {
                                orphans_deleted += 1;
                            }
                        }
                    }
                    if orphans_deleted > 0 {
                        info!(
                            "[runtimed] GC: cleaned up {} orphaned pool environments",
                            orphans_deleted
                        );
                    }
                }
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

            // Clean up orphaned notebook-docs (emergency persist files, legacy untitled docs)
            let notebook_docs_dir = self.config.notebook_docs_dir.clone();
            if notebook_docs_dir.exists() {
                let active_rooms = self.notebook_rooms.lock().await;
                let active_hashes: std::collections::HashSet<String> = active_rooms
                    .keys()
                    .map(|id| notebook_doc::notebook_doc_filename(&id.to_string()))
                    .collect();
                drop(active_rooms);

                let docs_max_age = std::time::Duration::from_secs(24 * 3600); // 24 hours
                let mut docs_cleaned = 0;
                if let Ok(mut entries) = tokio::fs::read_dir(&notebook_docs_dir).await {
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if !name.ends_with(".automerge") {
                            continue;
                        }
                        if active_hashes.contains(&name) {
                            continue;
                        }
                        let is_stale = entry
                            .metadata()
                            .await
                            .ok()
                            .and_then(|m| m.modified().ok())
                            .is_some_and(|t| t.elapsed().unwrap_or_default() > docs_max_age);
                        if is_stale && tokio::fs::remove_file(entry.path()).await.is_ok() {
                            docs_cleaned += 1;
                        }
                    }
                }
                if docs_cleaned > 0 {
                    info!(
                        "[runtimed] GC: cleaned up {} orphaned notebook-doc files",
                        docs_cleaned
                    );
                }
            }

            // Clean up orphaned blobs — mark-and-sweep across all active rooms
            // plus the persisted notebook-docs on disk. Blobs are
            // content-addressed, so the same hash may be referenced by multiple
            // rooms or persisted docs. We must scan ALL sources before
            // deleting anything.
            //
            // Batched: collect refs from rooms in batches of 10, yield between
            // batches to avoid starving other daemon tasks. Deletions are also
            // batched with yields between chunks.
            {
                // Collect (id, Arc<room>) pairs under the rooms lock, then drop
                // it before any async state_doc reads (deadlock prevention).
                let room_arcs: Vec<(String, _)> = {
                    let rooms = self.notebook_rooms.lock().await;
                    rooms
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.clone()))
                        .collect()
                };

                // Zero-rooms sweep-skip: ambiguous only right after a daemon
                // restart before any client has reconnected. `rooms_ever_seen`
                // flips the first time a room is acquired (see
                // `mark_rooms_ever_seen`), so a user who opens and closes a
                // notebook between GC ticks still arms the flag. Once armed,
                // zero rooms means "user closed everything" and the sweep is
                // safe (the persisted-doc walk below still gathers refs for
                // anything they might reopen). Without this distinction, a
                // daemon that stays open while idle would never GC again.
                let rooms_empty = room_arcs.is_empty();
                if Self::should_skip_blob_sweep(
                    rooms_empty,
                    self.rooms_ever_seen
                        .load(std::sync::atomic::Ordering::Relaxed),
                ) {
                    info!(
                        "[runtimed] GC: 0 active rooms and no room has loaded since startup; skipping blob sweep this cycle"
                    );
                } else {
                    let blob_max_age = blob_gc_grace();
                    let referenced_hashes =
                        Self::collect_blob_refs_for_gc(&room_arcs, &self.config.notebook_docs_dir)
                            .await;
                    Self::sweep_orphaned_blobs(&self.blob_store, &referenced_hashes, blob_max_age)
                        .await;
                }
            }

            // Run every 30 minutes (was 6 hours — too slow for sustained
            // workloads that create many ephemeral environments).
            tokio::time::sleep(std::time::Duration::from_secs(30 * 60)).await;
        }
    }
}

/// Extract blob hashes from an output manifest JSON value.
///
/// Walks `data` (display_data/execute_result MIME entries), `text` (stream),
/// and `traceback` (error) fields looking for `{"blob": "<hash>"}` refs.
fn collect_blob_hashes(
    manifest: &serde_json::Value,
    hashes: &mut std::collections::HashSet<String>,
) {
    // display_data / execute_result: data.{mime_type}.blob
    if let Some(data) = manifest.get("data").and_then(|d| d.as_object()) {
        for mime_data in data.values() {
            if let Some(hash) = mime_data.get("blob").and_then(|b| b.as_str()) {
                hashes.insert(hash.to_string());
            }
        }
    }
    // stream: text.blob
    if let Some(hash) = manifest
        .get("text")
        .and_then(|t| t.as_object())
        .and_then(|t| t.get("blob"))
        .and_then(|b| b.as_str())
    {
        hashes.insert(hash.to_string());
    }
    // error: traceback.blob
    if let Some(hash) = manifest
        .get("traceback")
        .and_then(|t| t.as_object())
        .and_then(|t| t.get("blob"))
        .and_then(|b| b.as_str())
    {
        hashes.insert(hash.to_string());
    }
}

/// Recursively walk a JSON value collecting blob hashes.
///
/// Handles comm state where `blob_store_large_state_values` and `store_widget_buffers`
/// produce `{"blob": "<hash>", "size": N, ...}` refs at arbitrary nesting depths.
fn collect_blob_hashes_recursive(
    value: &serde_json::Value,
    hashes: &mut std::collections::HashSet<String>,
) {
    match value {
        serde_json::Value::Object(obj) => {
            // Check if this object IS a blob ref (has "blob" + "size" keys)
            if let Some(hash) = obj.get("blob").and_then(|b| b.as_str()) {
                if obj.contains_key("size") {
                    hashes.insert(hash.to_string());
                    return;
                }
            }
            // Otherwise recurse into values
            for v in obj.values() {
                collect_blob_hashes_recursive(v, hashes);
            }
        }
        serde_json::Value::Array(arr) => {
            for v in arr {
                collect_blob_hashes_recursive(v, hashes);
            }
        }
        _ => {}
    }
}

/// Default grace period before an unreferenced blob is swept (30 days).
///
/// Rationale: after `.ipynb` save switches to external blob refs, a
/// saved-and-closed notebook relies on the blob store surviving until it
/// is re-opened. A week-long vacation shouldn't eat someone's rich outputs.
/// Disk is cheap; data loss isn't.
pub(crate) const BLOB_GC_GRACE_SECS: u64 = 30 * 24 * 3600;

/// Environment variable that overrides [`BLOB_GC_GRACE_SECS`].
///
/// Primarily for development and tests that want a short grace period to
/// exercise the sweep path without waiting 30 days.
pub(crate) const BLOB_GC_GRACE_ENV: &str = "RUNTIMED_BLOB_GC_GRACE_SECS";

/// Effective blob GC grace period.
///
/// Reads [`BLOB_GC_GRACE_ENV`] on each call so tests can flip it per scenario.
/// Invalid values fall back to the compiled default with a warning.
pub(crate) fn blob_gc_grace() -> std::time::Duration {
    match std::env::var(BLOB_GC_GRACE_ENV) {
        Ok(val) => match val.parse::<u64>() {
            Ok(secs) => std::time::Duration::from_secs(secs),
            Err(_) => {
                warn!(
                    "[runtimed] GC: ignoring invalid {}={:?}, using default",
                    BLOB_GC_GRACE_ENV, val
                );
                std::time::Duration::from_secs(BLOB_GC_GRACE_SECS)
            }
        },
        Err(_) => std::time::Duration::from_secs(BLOB_GC_GRACE_SECS),
    }
}

/// Walk a persisted notebook-doc `.automerge` file and collect blob refs.
///
/// Mirrors the in-memory mark phase: we load the saved document, read its
/// cells, and pull blob refs from `cell.outputs` (inline manifests) and
/// `cell.resolved_assets` (markdown image refs). Returns `None` if the
/// file cannot be read or decoded — the caller logs and moves on.
///
/// Note: `RuntimeStateDoc` is not persisted to disk separately; the
/// notebook doc's `cell.outputs` (legacy pre-v3 layout, plus future
/// `.ipynb`-backed outputs from spec 2) is the on-disk record of blob
/// references for a closed room.
pub(crate) async fn collect_hashes_from_persisted_doc(
    path: &Path,
    hashes: &mut std::collections::HashSet<String>,
) -> bool {
    let bytes = match tokio::fs::read(path).await {
        Ok(b) => b,
        Err(e) => {
            warn!(
                "[runtimed] GC: failed to read persisted notebook doc {:?}: {}",
                path, e
            );
            return false;
        }
    };
    let doc = match notebook_doc::NotebookDoc::load(&bytes) {
        Ok(d) => d,
        Err(e) => {
            warn!(
                "[runtimed] GC: failed to decode persisted notebook doc {:?}: {}",
                path, e
            );
            return false;
        }
    };
    for cell in doc.get_cells() {
        for output in &cell.outputs {
            collect_blob_hashes(output, hashes);
        }
        for hash in cell.resolved_assets.values() {
            hashes.insert(hash.clone());
        }
    }
    true
}

impl Daemon {
    /// Decide whether the current GC cycle should skip the blob sweep.
    ///
    /// Skip only when the rooms map is empty **and** no room has ever been
    /// loaded in this daemon process. That's the post-restart window where
    /// zero refs mean "we don't know yet" rather than "nothing is needed."
    /// Once any room has been observed, zero rooms thereafter means the user
    /// legitimately closed everything and the sweep must run to eventually
    /// reclaim the blobs whose notebooks are never reopened.
    pub(crate) fn should_skip_blob_sweep(rooms_empty: bool, rooms_ever_seen: bool) -> bool {
        rooms_empty && !rooms_ever_seen
    }

    /// Mark that at least one room has been acquired in this daemon
    /// process. Call from every code path that inserts into or fetches
    /// from `notebook_rooms` (typically right after `get_or_create_room`).
    ///
    /// This arms the zero-room GC skip guard. Flipping on acquisition
    /// (instead of on GC sampling) ensures short-lived sessions that
    /// open and close between 30-minute GC ticks still count.
    fn mark_rooms_ever_seen(&self) {
        self.rooms_ever_seen
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Collect every blob hash referenced by active rooms **and** persisted
    /// notebook-doc files the daemon owns.
    ///
    /// Scans three sources per active room (RuntimeStateDoc executions,
    /// RuntimeStateDoc comms, notebook doc resolved assets), then walks
    /// `notebook_docs_dir/*.automerge` for closed notebooks to protect their
    /// refs through the close/reopen window. Persisted docs already
    /// represented by an active room are skipped — their refs are covered
    /// by the in-memory pass.
    pub(crate) async fn collect_blob_refs_for_gc(
        rooms: &[(String, Arc<crate::notebook_sync_server::NotebookRoom>)],
        notebook_docs_dir: &Path,
    ) -> std::collections::HashSet<String> {
        /// Rooms are scanned in batches so the sweep yields back to other
        /// daemon tasks; keeping the constant local keeps it near the loop.
        const ROOM_BATCH_SIZE: usize = 10;
        /// Reading `.automerge` files is I/O-bound; yield between batches.
        const DOC_BATCH_SIZE: usize = 10;

        let mut referenced_hashes = std::collections::HashSet::new();

        // 1. In-memory: active rooms (RuntimeStateDoc + notebook doc).
        for batch in rooms.chunks(ROOM_BATCH_SIZE) {
            for (_id, room) in batch {
                {
                    let sd = room.state_doc.read().await;
                    let state = sd.read_state();
                    for exec in state.executions.values() {
                        for output in &exec.outputs {
                            collect_blob_hashes(output, &mut referenced_hashes);
                        }
                    }
                    for comm in state.comms.values() {
                        for output in &comm.outputs {
                            collect_blob_hashes(output, &mut referenced_hashes);
                        }
                        collect_blob_hashes_recursive(&comm.state, &mut referenced_hashes);
                    }
                }
                {
                    let doc = room.doc.read().await;
                    for cell in doc.get_cells() {
                        for hash in cell.resolved_assets.values() {
                            referenced_hashes.insert(hash.clone());
                        }
                    }
                }
            }
            tokio::task::yield_now().await;
        }

        // 2. On-disk: persisted notebook-doc files for closed notebooks.
        // Skip files that correspond to an active room (their refs were
        // already collected above).
        if notebook_docs_dir.exists() {
            let active_filenames: std::collections::HashSet<String> = rooms
                .iter()
                .map(|(id, _)| notebook_doc::notebook_doc_filename(id))
                .collect();

            let mut persisted_paths: Vec<PathBuf> = Vec::new();
            match tokio::fs::read_dir(notebook_docs_dir).await {
                Ok(mut entries) => {
                    while let Ok(Some(entry)) = entries.next_entry().await {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if !name.ends_with(".automerge") {
                            continue;
                        }
                        if active_filenames.contains(&name) {
                            continue;
                        }
                        persisted_paths.push(entry.path());
                    }
                }
                Err(e) => {
                    warn!(
                        "[runtimed] GC: failed to read notebook-docs dir {:?}: {}",
                        notebook_docs_dir, e
                    );
                }
            }

            if !persisted_paths.is_empty() {
                debug!(
                    "[runtimed] GC: walking {} persisted notebook-doc files for blob refs",
                    persisted_paths.len()
                );
                for batch in persisted_paths.chunks(DOC_BATCH_SIZE) {
                    for path in batch {
                        collect_hashes_from_persisted_doc(path, &mut referenced_hashes).await;
                    }
                    tokio::task::yield_now().await;
                }
            }
        }

        referenced_hashes
    }

    /// Sweep the blob store, deleting blobs that are not in
    /// `referenced_hashes` and are older than `blob_max_age`.
    pub(crate) async fn sweep_orphaned_blobs(
        blob_store: &BlobStore,
        referenced_hashes: &std::collections::HashSet<String>,
        blob_max_age: std::time::Duration,
    ) {
        const DELETE_BATCH_SIZE: usize = 50;
        match blob_store.list().await {
            Ok(all_blobs) => {
                let mut blobs_deleted = 0usize;
                let total_blobs = all_blobs.len();
                for chunk in all_blobs.chunks(DELETE_BATCH_SIZE) {
                    for hash in chunk {
                        if referenced_hashes.contains(hash) {
                            continue;
                        }
                        let is_stale =
                            blob_store
                                .get_meta(hash)
                                .await
                                .ok()
                                .flatten()
                                .is_some_and(|m| {
                                    let age_secs = chrono::Utc::now()
                                        .signed_duration_since(m.created_at)
                                        .num_seconds();
                                    // Guard against clock skew (negative age
                                    // wraps to huge u64 otherwise).
                                    age_secs > 0 && age_secs as u64 > blob_max_age.as_secs()
                                });
                        if is_stale && blob_store.delete(hash).await.unwrap_or(false) {
                            blobs_deleted += 1;
                        }
                    }
                    tokio::task::yield_now().await;
                }
                if blobs_deleted > 0 {
                    info!(
                        "[runtimed] GC: cleaned up {} orphaned blobs ({} total, {} referenced)",
                        blobs_deleted,
                        total_blobs,
                        referenced_hashes.len()
                    );
                }
            }
            Err(e) => {
                warn!("[runtimed] GC: failed to list blobs: {}", e);
            }
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
    async fn uv_warming_loop(self: &Arc<Self>) {
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
        let _ = uv_path;
        let mut settings_rx = self.settings_changed.subscribe();

        loop {
            if *self.shutdown.lock().await {
                break;
            }

            // Snapshot expected package list and target pool size from settings
            // so we can detect pool-entry drift (issue #1915) without holding
            // the settings lock across the pool lock.
            let (target, expected_packages) = {
                let settings = self.settings.read().await;
                let target = if self.config.uv_pool_size == 0 {
                    0 // Test mode: explicit 0 in config means don't warm
                } else {
                    settings
                        .get_u64("uv_pool_size")
                        .unwrap_or(runtimed_client::settings_doc::DEFAULT_UV_POOL_SIZE)
                        .min(runtimed_client::settings_doc::MAX_POOL_SIZE)
                        as usize
                };
                let synced = settings.get_all();
                let pkgs =
                    uv_prewarmed_packages(&synced.uv.default_packages, synced.feature_flags());
                (target, pkgs)
            };

            let (evicted_paths, deficit, should_retry, backoff_info) = {
                let mut pool = self.uv_pool.lock().await;
                pool.set_target(target);
                let evicted = pool.evict_mismatched_packages(&expected_packages);
                let d = pool.deficit();
                let retry = pool.should_retry();
                let info = if pool.failure_state.consecutive_failures > 0 {
                    Some((
                        pool.failure_state.consecutive_failures,
                        pool.backoff_delay().as_secs(),
                        pool.failure_state.failed_package.clone(),
                        pool.failure_state.is_network_failure,
                    ))
                } else {
                    None
                };

                if d > 0 && retry {
                    pool.mark_warming(d);
                }
                (evicted, d, retry, info)
            };

            if !evicted_paths.is_empty() {
                info!(
                    "[runtimed] UV pool: evicting {} env(s) after settings change",
                    evicted_paths.len()
                );
                spawn_env_deletions(evicted_paths);
                // Publish the post-eviction state immediately so clients don't
                // see ghost entries while the pool is in backoff or waiting
                // for the next warm tick.
                self.update_pool_doc().await;
            }

            if deficit > 0 {
                if should_retry {
                    self.update_pool_doc().await;
                    info!("[runtimed] Creating {} UV environments", deficit);
                    for _ in 0..deficit {
                        self.create_uv_env().await;
                    }
                } else if let Some((failures, backoff_secs, failed_pkg, is_network)) = backoff_info
                {
                    // In backoff period - log why we're waiting
                    if is_network {
                        warn!(
                            "[runtimed] UV pool warming offline — network unavailable, will retry in {}s",
                            backoff_secs
                        );
                    } else if let Some(pkg) = failed_pkg {
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

            let (available, warming, target) = {
                let pool = self.uv_pool.lock().await;
                let (a, w) = pool.stats();
                (a, w, pool.target())
            };
            debug!(
                "[runtimed] UV pool: {}/{} available, {} warming",
                available, target, warming
            );

            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
                _ = settings_rx.recv() => {}
            }
        }
    }

    /// Conda warming loop - maintains the Conda pool using rattler.
    async fn conda_warming_loop(self: &Arc<Self>) {
        info!("[runtimed] Starting conda warming loop");
        let mut settings_rx = self.settings_changed.subscribe();

        loop {
            if *self.shutdown.lock().await {
                break;
            }

            // Snapshot expected package list and target pool size (issue #1915).
            let (target, expected_packages) = {
                let settings = self.settings.read().await;
                let target = if self.config.conda_pool_size == 0 {
                    0 // Test mode: explicit 0 in config means don't warm
                } else {
                    settings
                        .get_u64("conda_pool_size")
                        .unwrap_or(runtimed_client::settings_doc::DEFAULT_CONDA_POOL_SIZE)
                        .min(runtimed_client::settings_doc::MAX_POOL_SIZE)
                        as usize
                };
                let synced = settings.get_all();
                let pkgs = conda_prewarmed_packages(&synced.conda.default_packages);
                (target, pkgs)
            };

            let (evicted_paths, deficit, should_retry, backoff_info) = {
                let mut pool = self.conda_pool.lock().await;
                pool.set_target(target);
                let evicted = pool.evict_mismatched_packages(&expected_packages);
                let d = pool.deficit();
                let retry = pool.should_retry();
                let info = if pool.failure_state.consecutive_failures > 0 {
                    Some((
                        pool.failure_state.consecutive_failures,
                        pool.backoff_delay().as_secs(),
                        pool.failure_state.last_error.clone(),
                        pool.failure_state.is_network_failure,
                    ))
                } else {
                    None
                };

                if d > 0 && retry {
                    pool.mark_warming(d);
                }
                (evicted, d, retry, info)
            };

            if !evicted_paths.is_empty() {
                info!(
                    "[runtimed] Conda pool: evicting {} env(s) after settings change",
                    evicted_paths.len()
                );
                spawn_env_deletions(evicted_paths);
                self.update_pool_doc().await;
            }

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
                } else if let Some((failures, backoff_secs, last_error, is_network)) = backoff_info
                {
                    // In backoff period - log why we're waiting
                    if is_network {
                        warn!(
                            "[runtimed] Conda pool warming offline — network unavailable, will retry in {}s",
                            backoff_secs
                        );
                    } else if let Some(err) = last_error {
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

            let (available, warming, target) = {
                let pool = self.conda_pool.lock().await;
                let (a, w) = pool.stats();
                (a, w, pool.target())
            };
            debug!(
                "[runtimed] Conda pool: {}/{} available, {} warming",
                available, target, warming
            );

            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
                _ = settings_rx.recv() => {}
            }
        }
    }

    /// Background loop that keeps the pixi environment pool at its target size.
    async fn pixi_warming_loop(self: &Arc<Self>) {
        info!("[runtimed] Starting pixi warming loop");
        let mut settings_rx = self.settings_changed.subscribe();

        loop {
            if *self.shutdown.lock().await {
                break;
            }

            // Snapshot expected package list and target pool size (issue #1915).
            let (target, expected_packages) = {
                let settings = self.settings.read().await;
                let target = if self.config.pixi_pool_size == 0 {
                    0 // Test mode: explicit 0 in config means don't warm
                } else {
                    settings
                        .get_u64("pixi_pool_size")
                        .unwrap_or(runtimed_client::settings_doc::DEFAULT_PIXI_POOL_SIZE)
                        .min(runtimed_client::settings_doc::MAX_POOL_SIZE)
                        as usize
                };
                let synced = settings.get_all();
                let pkgs = pixi_prewarmed_packages(&synced.pixi.default_packages);
                (target, pkgs)
            };

            let (evicted_paths, deficit, should_retry, backoff_info) = {
                let mut pool = self.pixi_pool.lock().await;
                pool.set_target(target);
                let evicted = pool.evict_mismatched_packages(&expected_packages);
                let d = pool.deficit();
                let retry = pool.should_retry();
                let info = if pool.failure_state.consecutive_failures > 0 {
                    Some((
                        pool.failure_state.consecutive_failures,
                        pool.backoff_delay().as_secs(),
                        pool.failure_state.last_error.clone(),
                        pool.failure_state.is_network_failure,
                    ))
                } else {
                    None
                };

                if d > 0 && retry {
                    pool.mark_warming(d);
                }
                (evicted, d, retry, info)
            };

            if !evicted_paths.is_empty() {
                info!(
                    "[runtimed] Pixi pool: evicting {} env(s) after settings change",
                    evicted_paths.len()
                );
                spawn_env_deletions(evicted_paths);
                self.update_pool_doc().await;
            }

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
                } else if let Some((failures, backoff_secs, last_error, is_network)) = backoff_info
                {
                    if is_network {
                        warn!(
                            "[runtimed] Pixi pool warming offline — network unavailable, will retry in {}s",
                            backoff_secs
                        );
                    } else if let Some(err) = last_error {
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

            let (available, warming, target) = {
                let pool = self.pixi_pool.lock().await;
                let (a, w) = pool.stats();
                (a, w, pool.target())
            };
            debug!(
                "[runtimed] Pixi pool: {}/{} available, {} warming",
                available, target, warming
            );

            tokio::select! {
                _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
                _ = settings_rx.recv() => {}
            }
        }
    }

    /// Create a single Conda environment using rattler and add it to the pool.
    async fn create_conda_env(self: &Arc<Self>) {
        use rattler::{default_cache_dir, install::Installer, package_cache::PackageCache};
        use rattler_conda_types::{
            Channel, ChannelConfig, GenericVirtualPackage, MatchSpec, ParseMatchSpecOptions,
            Platform,
        };
        use rattler_repodata_gateway::Gateway;
        use rattler_solve::{resolvo, SolverImpl, SolverTask};

        let temp_id = format!("{}{}", crate::POOL_PREFIX_CONDA, uuid::Uuid::new_v4());
        let env_path = self.config.cache_dir.join(&temp_id);

        // Register warming path before creating the directory so GC won't
        // delete it while packages are being installed.
        self.conda_pool
            .lock()
            .await
            .register_warming_path(env_path.clone());

        // Guard rolls back pool accounting on panic or early exit.
        let mut guard = WarmingGuard::new(self.clone(), env_path.clone(), PoolKind::Conda);

        #[cfg(target_os = "windows")]
        let python_path = env_path.join("python.exe");
        #[cfg(not(target_os = "windows"))]
        let python_path = env_path.join("bin").join("python");

        info!("[runtimed] Creating Conda environment at {:?}", env_path);

        // Ensure cache directory exists
        if let Err(e) = tokio::fs::create_dir_all(&self.config.cache_dir).await {
            error!("[runtimed] Failed to create cache dir: {}", e);
            guard
                .fail_with(Some(PackageInstallError {
                    failed_package: None,
                    error_message: format!("Failed to create cache dir: {}", e),
                    error_kind: "setup_failed".to_string(),
                }))
                .await;
            return;
        }

        // Setup channel configuration
        let channel_config = ChannelConfig::default_with_root_dir(self.config.cache_dir.clone());

        // Parse channels
        let channels = match Channel::from_str("conda-forge", &channel_config) {
            Ok(ch) => vec![ch],
            Err(e) => {
                error!("[runtimed] Failed to parse conda-forge channel: {}", e);
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to parse conda-forge channel: {}", e),
                        error_kind: "setup_failed".to_string(),
                    }))
                    .await;
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
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to parse match specs: {}", e),
                        error_kind: "setup_failed".to_string(),
                    }))
                    .await;
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
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!(
                            "Could not determine rattler cache directory: {}",
                            e
                        ),
                        error_kind: "setup_failed".to_string(),
                    }))
                    .await;
                return;
            }
        };

        if let Err(e) = rattler_cache::ensure_cache_dir(&rattler_cache_dir) {
            error!("[runtimed] Could not create rattler cache directory: {}", e);
            guard
                .fail_with(Some(PackageInstallError {
                    failed_package: None,
                    error_message: format!("Could not create rattler cache directory: {}", e),
                    error_kind: "setup_failed".to_string(),
                }))
                .await;
            return;
        }

        // Create HTTP client
        let download_client = match reqwest::Client::builder().build() {
            Ok(c) => reqwest_middleware::ClientBuilder::new(c).build(),
            Err(e) => {
                error!("[runtimed] Failed to create HTTP client: {}", e);
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to create HTTP client: {}", e),
                        error_kind: "setup_failed".to_string(),
                    }))
                    .await;
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
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to fetch repodata: {}", e),
                        error_kind: "setup_failed".to_string(),
                    }))
                    .await;
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
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to detect virtual packages: {}", e),
                        error_kind: "setup_failed".to_string(),
                    }))
                    .await;
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
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to solve dependencies: {}", e),
                        error_kind: "invalid_package".to_string(),
                    }))
                    .await;
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
            guard
                .fail_with(Some(PackageInstallError {
                    failed_package: None,
                    error_message: format!("Failed to install packages: {}", e),
                    error_kind: "setup_failed".to_string(),
                }))
                .await;
            return;
        }

        // Verify python exists
        if !python_path.exists() {
            error!(
                "[runtimed] Python not found at {:?} after install",
                python_path
            );
            tokio::fs::remove_dir_all(&env_path).await.ok();
            guard
                .fail_with(Some(PackageInstallError {
                    failed_package: None,
                    error_message: format!("Python not found at {:?} after install", python_path),
                    error_kind: "setup_failed".to_string(),
                }))
                .await;
            return;
        }

        // Run warmup script
        let warmup_outcome = self
            .warmup_conda_env(&python_path, &env_path, &extra_conda_packages)
            .await;

        match warmup_outcome {
            WarmupOutcome::Ok => {
                guard.commit();
                {
                    let mut pool = self.conda_pool.lock().await;
                    pool.add(PooledEnv {
                        env_type: EnvType::Conda,
                        venv_path: env_path.clone(),
                        python_path,
                        prewarmed_packages: conda_install_packages,
                    });
                }

                {
                    let pool = self.conda_pool.lock().await;
                    info!(
                        "[runtimed] Conda environment ready: {:?} (pool: {}/{})",
                        env_path,
                        pool.stats().0,
                        pool.target()
                    );
                }
                self.update_pool_doc().await;
            }
            WarmupOutcome::Timeout => {
                let _ = tokio::fs::remove_dir_all(&env_path).await;
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: "Conda warmup timed out".into(),
                        error_kind: "timeout".to_string(),
                    }))
                    .await;
            }
            WarmupOutcome::ImportError(msg) => {
                let _ = tokio::fs::remove_dir_all(&env_path).await;
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!(
                            "Conda warmup failed: {}",
                            msg.chars().take(200).collect::<String>()
                        ),
                        error_kind: "import_error".to_string(),
                    }))
                    .await;
            }
        }
    }

    /// Warm up a conda environment by running Python to trigger .pyc compilation.
    async fn warmup_conda_env(
        &self,
        python_path: &PathBuf,
        env_path: &PathBuf,
        extra_packages: &[String],
    ) -> WarmupOutcome {
        let site_packages = {
            let lib_dir = env_path.join("lib");
            std::fs::read_dir(&lib_dir).ok().and_then(|entries| {
                entries.flatten().find_map(|entry| {
                    let path = entry.path();
                    let name = path.file_name()?.to_str()?;
                    if name.starts_with("python") {
                        let sp = path.join("site-packages");
                        sp.is_dir().then(|| sp.to_string_lossy().into_owned())
                    } else {
                        None
                    }
                })
            })
        };
        let warmup_script = kernel_env::warmup::build_warmup_command(
            extra_packages,
            true,
            site_packages.as_deref(),
        );

        let warmup_result = tokio::time::timeout(
            std::time::Duration::from_secs(180),
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
                WarmupOutcome::Ok
            }
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!(
                    "[runtimed] Conda warmup failed for {:?}: {}",
                    env_path,
                    stderr.lines().take(3).collect::<Vec<_>>().join(" | ")
                );
                WarmupOutcome::ImportError(stderr.to_string())
            }
            Ok(Err(e)) => {
                error!("[runtimed] Failed to run conda warmup: {}", e);
                WarmupOutcome::ImportError(e.to_string())
            }
            Err(_) => {
                error!("[runtimed] Conda warmup timed out (180s)");
                WarmupOutcome::Timeout
            }
        }
    }

    /// Replenish a single Conda environment.
    async fn replenish_conda_env(self: &Arc<Self>) {
        self.conda_pool.lock().await.mark_warming(1);
        self.create_conda_env().await;
    }

    /// Create a pixi environment using rattler (no subprocess).
    ///
    /// Creates a pixi-compatible project directory with ipykernel and default
    /// packages, solved and installed via rattler. Replaces the old
    /// `pixi init` + `pixi add` subprocess approach.
    async fn create_pixi_env(self: &Arc<Self>) {
        let cache_dir = self.config.cache_dir.clone();
        let env_id = uuid::Uuid::new_v4().to_string();
        let project_dir = cache_dir.join(format!("{}{}", crate::POOL_PREFIX_PIXI, env_id));

        // Register warming path before creating the directory so GC won't
        // delete it while packages are being installed.
        self.pixi_pool
            .lock()
            .await
            .register_warming_path(project_dir.clone());

        // Guard rolls back pool accounting on panic or early exit.
        let mut guard = WarmingGuard::new(self.clone(), project_dir.clone(), PoolKind::Pixi);

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
                guard.commit();
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
                guard
                    .fail_with(Some(PackageInstallError {
                        error_message: format!("{}", e),
                        failed_package: None,
                        error_kind: "setup_failed".to_string(),
                    }))
                    .await;
            }
        }
    }

    /// Mark pixi pool as warming and create a pixi environment.
    async fn replenish_pixi_env(self: &Arc<Self>) {
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
                pool_size: pool.target() as u64,
                error: pool.failure_state.last_error.clone(),
                failed_package: pool.failure_state.failed_package.clone(),
                error_kind: pool.failure_state.error_kind.clone(),
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
                pool_size: pool.target() as u64,
                error: pool.failure_state.last_error.clone(),
                failed_package: pool.failure_state.failed_package.clone(),
                error_kind: pool.failure_state.error_kind.clone(),
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
                pool_size: pool.target() as u64,
                error: pool.failure_state.last_error.clone(),
                failed_package: pool.failure_state.failed_package.clone(),
                error_kind: pool.failure_state.error_kind.clone(),
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
    async fn create_uv_env(self: &Arc<Self>) {
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
                        error_kind: "setup_failed".to_string(),
                    }));
                self.update_pool_doc().await;
                return;
            }
        };

        let temp_id = format!("{}{}", crate::POOL_PREFIX_UV, uuid::Uuid::new_v4());
        let venv_path = self.config.cache_dir.join(&temp_id);

        // Register warming path before creating the directory so GC won't
        // delete it while packages are being installed.
        self.uv_pool
            .lock()
            .await
            .register_warming_path(venv_path.clone());

        // Guard rolls back pool accounting on panic or early exit.
        let mut guard = WarmingGuard::new(self.clone(), venv_path.clone(), PoolKind::Uv);

        #[cfg(target_os = "windows")]
        let python_path = venv_path.join("Scripts").join("python.exe");
        #[cfg(not(target_os = "windows"))]
        let python_path = venv_path.join("bin").join("python");

        info!("[runtimed] Creating UV environment at {:?}", venv_path);

        // Ensure cache directory exists
        if let Err(e) = tokio::fs::create_dir_all(&self.config.cache_dir).await {
            error!("[runtimed] Failed to create cache dir: {}", e);
            guard
                .fail_with(Some(PackageInstallError {
                    failed_package: None,
                    error_message: format!("Failed to create cache dir: {}", e),
                    error_kind: "setup_failed".to_string(),
                }))
                .await;
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
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to create venv: {}", stderr),
                        error_kind: "setup_failed".to_string(),
                    }))
                    .await;
                return;
            }
            Ok(Err(e)) => {
                error!("[runtimed] Failed to create venv: {}", e);
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!("Failed to create venv: {}", e),
                        error_kind: "setup_failed".to_string(),
                    }))
                    .await;
                return;
            }
            Err(_) => {
                error!("[runtimed] Timeout creating venv");
                tokio::fs::remove_dir_all(&venv_path).await.ok();
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: "Timeout creating venv after 60 seconds".to_string(),
                        error_kind: "timeout".to_string(),
                    }))
                    .await;
                return;
            }
        }

        // Read default uv packages from synced settings
        let (user_default_packages, feature_flags) = {
            let settings = self.settings.read().await;
            let synced = settings.get_all();
            let flags = synced.feature_flags();
            let configured = synced.uv.default_packages;
            if !configured.is_empty() {
                info!("[runtimed] Including default uv packages: {:?}", configured);
            }
            (configured, flags)
        };
        let install_packages = uv_prewarmed_packages(&user_default_packages, feature_flags);

        // Install packages (180 second timeout)
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
            std::time::Duration::from_secs(180),
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
                guard.fail_with(parsed_error).await;
                return;
            }
            Ok(Err(e)) => {
                error!("[runtimed] Failed to run uv pip install: {}", e);
                tokio::fs::remove_dir_all(&venv_path).await.ok();
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: e.to_string(),
                        error_kind: "setup_failed".to_string(),
                    }))
                    .await;
                return;
            }
            Err(_) => {
                error!("[runtimed] Timeout installing packages (180s)");
                tokio::fs::remove_dir_all(&venv_path).await.ok();
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: "Timeout after 180 seconds".to_string(),
                        error_kind: "timeout".to_string(),
                    }))
                    .await;
                return;
            }
        }

        // Warm up the environment (30 second timeout)
        let site_packages = {
            let lib_dir = venv_path.join("lib");
            std::fs::read_dir(&lib_dir).ok().and_then(|entries| {
                entries.flatten().find_map(|entry| {
                    let path = entry.path();
                    let name = path.file_name()?.to_str()?;
                    if name.starts_with("python") {
                        let sp = path.join("site-packages");
                        sp.is_dir().then(|| sp.to_string_lossy().into_owned())
                    } else {
                        None
                    }
                })
            })
        };
        let warmup_script = kernel_env::warmup::build_warmup_command(
            &user_default_packages,
            false,
            site_packages.as_deref(),
        );

        let warmup_result = tokio::time::timeout(
            std::time::Duration::from_secs(180),
            tokio::process::Command::new(&python_path)
                .args(["-c", &warmup_script])
                .output(),
        )
        .await;

        let warmup_outcome = match warmup_result {
            Ok(Ok(output)) if output.status.success() => {
                // Create marker file
                tokio::fs::write(venv_path.join(".warmed"), "").await.ok();
                WarmupOutcome::Ok
            }
            Ok(Ok(output)) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                error!(
                    "[runtimed] UV warmup failed, NOT adding to pool: {}",
                    stderr.lines().take(3).collect::<Vec<_>>().join(" | ")
                );
                WarmupOutcome::ImportError(stderr.to_string())
            }
            Ok(Err(e)) => {
                error!("[runtimed] Failed to run UV warmup: {}", e);
                WarmupOutcome::ImportError(e.to_string())
            }
            Err(_) => {
                error!("[runtimed] UV warmup timed out, NOT adding to pool (180s)");
                WarmupOutcome::Timeout
            }
        };

        match warmup_outcome {
            WarmupOutcome::Ok => {
                info!("[runtimed] UV environment ready at {:?}", venv_path);
                guard.commit();
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
            }
            WarmupOutcome::Timeout => {
                let _ = tokio::fs::remove_dir_all(&venv_path).await;
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: "UV warmup timed out".into(),
                        error_kind: "timeout".to_string(),
                    }))
                    .await;
            }
            WarmupOutcome::ImportError(msg) => {
                let _ = tokio::fs::remove_dir_all(&venv_path).await;
                guard
                    .fail_with(Some(PackageInstallError {
                        failed_package: None,
                        error_message: format!(
                            "UV warmup failed: {}",
                            msg.chars().take(200).collect::<String>()
                        ),
                        error_kind: "import_error".to_string(),
                    }))
                    .await;
            }
        }
    }
}

fn looks_like_untitled_notebook_path(path: &str) -> bool {
    let candidate = Path::new(path);
    candidate.components().count() == 1
        && candidate.extension().is_none()
        && uuid::Uuid::parse_str(path).is_ok()
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
            error_kind: "invalid_package".to_string(),
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
            error_kind: "invalid_package".to_string(),
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

    #[test]
    fn test_collect_blob_hashes_display_data() {
        let manifest = serde_json::json!({
            "output_type": "display_data",
            "data": {
                "text/plain": {"blob": "aaa", "size": 10},
                "image/png": {"blob": "bbb", "size": 5000},
                "text/html": {"inline": "<b>hello</b>"}
            }
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes(&manifest, &mut hashes);
        assert!(hashes.contains("aaa"));
        assert!(hashes.contains("bbb"));
        assert_eq!(hashes.len(), 2); // inline entry not collected
    }

    #[test]
    fn test_collect_blob_hashes_stream() {
        let manifest = serde_json::json!({
            "output_type": "stream",
            "name": "stdout",
            "text": {"blob": "ccc", "size": 2000}
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes(&manifest, &mut hashes);
        assert!(hashes.contains("ccc"));
        assert_eq!(hashes.len(), 1);
    }

    #[test]
    fn test_collect_blob_hashes_error() {
        let manifest = serde_json::json!({
            "output_type": "error",
            "ename": "ValueError",
            "evalue": "bad",
            "traceback": {"blob": "ddd", "size": 500}
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes(&manifest, &mut hashes);
        assert!(hashes.contains("ddd"));
    }

    #[test]
    fn test_collect_blob_hashes_recursive_comm_state() {
        let state = serde_json::json!({
            "_model_name": "VegaWidget",
            "_esm": {"blob": "esm_hash", "size": 50000, "media_type": "text/javascript"},
            "spec": {"blob": "spec_hash", "size": 10000, "media_type": "application/json"},
            "small_value": 42,
            "nested": {
                "buffer": {"blob": "buf_hash", "size": 8000, "media_type": "application/octet-stream"}
            }
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes_recursive(&state, &mut hashes);
        assert!(hashes.contains("esm_hash"));
        assert!(hashes.contains("spec_hash"));
        assert!(hashes.contains("buf_hash"));
        assert_eq!(hashes.len(), 3);
    }

    #[test]
    fn test_collect_blob_hashes_recursive_no_false_positives() {
        // An object with a "blob" key but no "size" should NOT be collected
        let value = serde_json::json!({
            "blob": "not_a_ref",
            "other_key": true
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes_recursive(&value, &mut hashes);
        assert!(hashes.is_empty());
    }

    // ── Blob GC edge case tests ────────────────────────────────────

    #[test]
    fn test_collect_blob_hashes_empty_manifest() {
        let manifest = serde_json::json!({});
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes(&manifest, &mut hashes);
        assert!(hashes.is_empty());
    }

    #[test]
    fn test_collect_blob_hashes_inline_only_no_blobs() {
        // Manifests with only inline content should yield no blob hashes
        let manifest = serde_json::json!({
            "output_type": "display_data",
            "data": {
                "text/plain": {"inline": "hello world"},
                "text/html": {"inline": "<b>hello</b>"}
            }
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes(&manifest, &mut hashes);
        assert!(hashes.is_empty());
    }

    #[test]
    fn test_collect_blob_hashes_mixed_inline_and_blob() {
        // Same output with both inline and blob MIME types
        let manifest = serde_json::json!({
            "output_type": "execute_result",
            "data": {
                "text/plain": {"inline": "Figure(...)"},
                "image/png": {"blob": "png_hash", "size": 50000}
            },
            "execution_count": 5
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes(&manifest, &mut hashes);
        assert_eq!(hashes.len(), 1);
        assert!(hashes.contains("png_hash"));
    }

    #[test]
    fn test_collect_blob_hashes_null_and_missing_fields() {
        // Manifest with null values and missing expected fields
        let manifest = serde_json::json!({
            "output_type": "display_data",
            "data": null,
            "text": null,
            "traceback": null
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes(&manifest, &mut hashes);
        assert!(hashes.is_empty());
    }

    #[test]
    fn test_collect_blob_hashes_stream_inline_text() {
        // Stream with inline text (small output, no blob)
        let manifest = serde_json::json!({
            "output_type": "stream",
            "name": "stdout",
            "text": "just a string, not an object"
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes(&manifest, &mut hashes);
        assert!(hashes.is_empty()); // text is a string, not {blob: ...}
    }

    #[test]
    fn test_collect_blob_hashes_multiple_outputs_dedup() {
        // Same blob hash referenced by multiple MIME types
        let manifest = serde_json::json!({
            "output_type": "display_data",
            "data": {
                "image/png": {"blob": "same_hash", "size": 1000},
                "image/jpeg": {"blob": "same_hash", "size": 1000}
            }
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes(&manifest, &mut hashes);
        assert_eq!(hashes.len(), 1); // deduplicated by HashSet
        assert!(hashes.contains("same_hash"));
    }

    #[test]
    fn test_collect_blob_hashes_recursive_empty_state() {
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes_recursive(&serde_json::json!({}), &mut hashes);
        assert!(hashes.is_empty());

        collect_blob_hashes_recursive(&serde_json::json!(null), &mut hashes);
        assert!(hashes.is_empty());

        collect_blob_hashes_recursive(&serde_json::json!([]), &mut hashes);
        assert!(hashes.is_empty());

        collect_blob_hashes_recursive(&serde_json::json!("just a string"), &mut hashes);
        assert!(hashes.is_empty());
    }

    #[test]
    fn test_collect_blob_hashes_recursive_array_of_blob_refs() {
        // Widget buffer_paths can produce arrays containing blob refs
        let value = serde_json::json!([
            {"blob": "buf1", "size": 100, "media_type": "application/octet-stream"},
            {"blob": "buf2", "size": 200, "media_type": "application/octet-stream"},
            "not a blob ref"
        ]);
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes_recursive(&value, &mut hashes);
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains("buf1"));
        assert!(hashes.contains("buf2"));
    }

    #[test]
    fn test_collect_blob_hashes_recursive_deeply_nested() {
        // 4 levels deep — store_widget_buffers can place refs at arbitrary depth
        let value = serde_json::json!({
            "level1": {
                "level2": {
                    "level3": {
                        "data": {"blob": "deep_hash", "size": 999}
                    }
                }
            }
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes_recursive(&value, &mut hashes);
        assert_eq!(hashes.len(), 1);
        assert!(hashes.contains("deep_hash"));
    }

    #[test]
    fn test_collect_blob_hashes_recursive_blob_key_with_size_zero() {
        // size: 0 is technically valid (empty blob)
        let value = serde_json::json!({
            "empty_blob": {"blob": "empty_hash", "size": 0}
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes_recursive(&value, &mut hashes);
        assert!(hashes.contains("empty_hash"));
    }

    #[test]
    fn test_collect_blob_hashes_recursive_non_string_blob_value() {
        // blob value is a number (malformed) — should not be collected
        let value = serde_json::json!({
            "weird": {"blob": 12345, "size": 100}
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes_recursive(&value, &mut hashes);
        assert!(hashes.is_empty());
    }

    #[test]
    fn test_collect_blob_hashes_recursive_mixed_blob_and_regular_objects() {
        // State with a mix of blob refs and regular data that should not match
        let value = serde_json::json!({
            "_model_name": "PlotWidget",
            "_esm": {"blob": "esm_hash", "size": 80000, "media_type": "text/javascript"},
            "layout": {
                "width": 800,
                "height": 600,
                "title": "My Plot"
            },
            "data": [
                {"x": [1, 2, 3], "y": [4, 5, 6]},
                {"blob": "data_blob", "size": 5000, "media_type": "application/json"}
            ],
            "config": {"responsive": true}
        });
        let mut hashes = std::collections::HashSet::new();
        collect_blob_hashes_recursive(&value, &mut hashes);
        assert_eq!(hashes.len(), 2);
        assert!(hashes.contains("esm_hash"));
        assert!(hashes.contains("data_blob"));
    }

    // =========================================================================
    // Network failure detection tests
    // =========================================================================

    #[test]
    fn test_is_network_error_classification() {
        // Network errors
        assert!(is_network_error("connection refused"));
        assert!(is_network_error("connection reset by peer"));
        assert!(is_network_error("connection timed out"));
        assert!(is_network_error("request timed out"));
        assert!(is_network_error("connect timed out"));
        assert!(is_network_error("DNS resolution failed"));
        assert!(is_network_error("Failed to fetch repodata"));
        assert!(is_network_error("No cached repodata available"));
        assert!(is_network_error("network is unreachable"));
        assert!(is_network_error("could not resolve host"));

        // NOT network errors — these should use normal backoff
        assert!(!is_network_error("package pandas not found"));
        assert!(!is_network_error("invalid version specifier"));
        assert!(!is_network_error("Failed to solve dependencies"));
        assert!(!is_network_error("connection pool exhausted")); // not a network error
        assert!(!is_network_error("subprocess timed out")); // not a network error
    }

    #[test]
    fn test_pool_network_backoff_shorter() {
        let mut pool = Pool::new(3, 3600);
        pool.failure_state.consecutive_failures = 1;
        pool.failure_state.is_network_failure = true;
        let network_delay = pool.backoff_delay();

        pool.failure_state.is_network_failure = false;
        let normal_delay = pool.backoff_delay();

        assert!(
            network_delay < normal_delay,
            "network backoff {:?} should be shorter than normal {:?}",
            network_delay,
            normal_delay
        );
    }

    #[test]
    fn test_pool_non_network_backoff_unchanged() {
        let mut pool = Pool::new(3, 3600);
        pool.failure_state.consecutive_failures = 3;
        pool.failure_state.is_network_failure = false;
        let delay = pool.backoff_delay();
        // 30s * 2^2 = 120s
        assert_eq!(delay.as_secs(), 120);
    }

    #[test]
    fn test_pool_network_backoff_progression() {
        let mut pool = Pool::new(3, 3600);
        pool.failure_state.is_network_failure = true;

        // 10s base, doubling, capped at 60s
        let expected = [10, 20, 40, 60, 60];
        for (i, &expected_secs) in expected.iter().enumerate() {
            pool.failure_state.consecutive_failures = (i + 1) as u32;
            assert_eq!(
                pool.backoff_delay().as_secs(),
                expected_secs,
                "network backoff at {} failures should be {}s",
                i + 1,
                expected_secs
            );
        }
    }

    #[test]
    fn test_warming_paths_registered_and_cleared_on_success() {
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(3, 3600);

        // Use a pool-prefixed name so pool_env_root can find it
        let env = create_test_env(&temp_dir, "runtimed-uv-test");
        let root = pool_env_root(&env.venv_path);

        pool.register_warming_path(root.clone());
        assert!(pool.warming_paths.contains(&root));
        assert_eq!(pool.warming_paths.len(), 1);

        pool.mark_warming(1);
        pool.add(env);

        // add() should remove the path from warming_paths
        assert!(pool.warming_paths.is_empty());
        assert_eq!(pool.warming, 0);
    }

    #[test]
    fn test_warming_paths_cleared_on_failure() {
        let mut pool = Pool::new(3, 3600);
        let path = PathBuf::from("/cache/runtimed-uv-failed");

        pool.register_warming_path(path.clone());
        pool.mark_warming(1);

        assert!(pool.warming_paths.contains(&path));

        pool.warming_failed_for_path(&path, None);

        assert!(pool.warming_paths.is_empty());
        assert_eq!(pool.warming, 0);
        assert_eq!(pool.failure_state.consecutive_failures, 1);
    }

    #[test]
    fn test_warming_paths_multiple_concurrent() {
        let mut pool = Pool::new(3, 3600);
        let path1 = PathBuf::from("/cache/runtimed-uv-aaa");
        let path2 = PathBuf::from("/cache/runtimed-uv-bbb");
        let path3 = PathBuf::from("/cache/runtimed-conda-ccc");

        pool.register_warming_path(path1.clone());
        pool.register_warming_path(path2.clone());
        pool.register_warming_path(path3.clone());
        pool.mark_warming(3);

        assert_eq!(pool.warming_paths.len(), 3);

        // One fails, two remain
        pool.warming_failed_for_path(&path2, None);
        assert_eq!(pool.warming_paths.len(), 2);
        assert!(!pool.warming_paths.contains(&path2));

        // Another fails
        pool.warming_failed_for_path(&path1, None);
        assert_eq!(pool.warming_paths.len(), 1);
        assert!(pool.warming_paths.contains(&path3));
    }

    // ── Pool eviction on settings change (issue #1915) ───────────────

    #[test]
    fn test_evict_mismatched_packages_removes_stale_entries() {
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(3, 3600);

        let mut e1 = create_test_env(&temp_dir, "runtimed-uv-a");
        e1.prewarmed_packages = vec!["ipykernel".into(), "pandas".into()];
        let mut e2 = create_test_env(&temp_dir, "runtimed-uv-b");
        e2.prewarmed_packages = vec!["ipykernel".into(), "numpy".into()];
        let e2_path = e2.venv_path.clone();
        pool.add(e1);
        pool.add(e2);
        assert_eq!(pool.available.len(), 2);

        let expected = vec!["ipykernel".to_string(), "pandas".to_string()];
        let evicted = pool.evict_mismatched_packages(&expected);

        assert_eq!(evicted, vec![e2_path]);
        assert_eq!(pool.available.len(), 1);
        assert_eq!(
            pool.available.front().unwrap().env.prewarmed_packages,
            vec!["ipykernel".to_string(), "pandas".to_string()]
        );
    }

    #[test]
    fn test_evict_mismatched_packages_ignores_order() {
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(3, 3600);

        let mut env = create_test_env(&temp_dir, "runtimed-uv-reorder");
        env.prewarmed_packages = vec!["pandas".into(), "ipykernel".into(), "numpy".into()];
        pool.add(env);

        let expected = vec!["numpy".into(), "ipykernel".into(), "pandas".into()];
        let evicted = pool.evict_mismatched_packages(&expected);

        assert!(evicted.is_empty(), "sorted equality should ignore order");
        assert_eq!(pool.available.len(), 1);
    }

    #[test]
    fn test_evict_mismatched_packages_evicts_all_when_all_stale() {
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(3, 3600);

        for name in ["runtimed-uv-1", "runtimed-uv-2", "runtimed-uv-3"] {
            let mut env = create_test_env(&temp_dir, name);
            env.prewarmed_packages = vec!["ipykernel".into()];
            pool.add(env);
        }
        assert_eq!(pool.available.len(), 3);

        // Settings added a new default package — every env is now stale.
        let expected = vec!["ipykernel".to_string(), "pandas".to_string()];
        let evicted = pool.evict_mismatched_packages(&expected);

        assert_eq!(evicted.len(), 3);
        assert!(pool.available.is_empty());
    }

    #[test]
    fn test_evict_mismatched_packages_empty_pool_is_noop() {
        let mut pool = Pool::new(3, 3600);
        let evicted = pool.evict_mismatched_packages(&["ipykernel".to_string()]);
        assert!(evicted.is_empty());
        assert!(pool.available.is_empty());
    }

    #[test]
    fn test_evict_mismatched_packages_returns_pool_root_for_nested_venv() {
        // Pixi envs live at `runtimed-pixi-*/.pixi/envs/default`. Eviction
        // must return the pool root so `spawn_env_deletions` removes the
        // whole directory, not just the inner venv.
        let temp_dir = TempDir::new().unwrap();
        let mut pool = Pool::new(3, 3600);

        let pool_root = temp_dir.path().join("runtimed-pixi-abc");
        let nested_venv = pool_root.join(".pixi").join("envs").join("default");
        let python = nested_venv.join("bin").join("python");
        std::fs::create_dir_all(python.parent().unwrap()).unwrap();
        std::fs::write(&python, "").unwrap();
        std::fs::write(nested_venv.join(".warmed"), "").unwrap();

        pool.add(PooledEnv {
            env_type: EnvType::Conda,
            venv_path: nested_venv,
            python_path: python,
            prewarmed_packages: vec!["ipykernel".into()],
        });

        let expected = vec!["ipykernel".to_string(), "pandas".to_string()];
        let evicted = pool.evict_mismatched_packages(&expected);

        assert_eq!(
            evicted,
            vec![pool_root],
            "nested venv should evict by pool root, not inner venv path"
        );
    }

    // ── Blob GC correctness (spec 1) ─────────────────────────────────

    use crate::blob_store::BlobStore;

    #[test]
    fn should_skip_blob_sweep_only_when_rooms_empty_and_never_seen() {
        // Post-restart, no client has reconnected: skip — we don't know
        // what's needed yet.
        assert!(Daemon::should_skip_blob_sweep(true, false));

        // Idle daemon whose user closed every notebook: run the sweep.
        // Refs are legitimately empty (persisted-doc walk still runs to
        // pick up refs for anything they might reopen).
        assert!(!Daemon::should_skip_blob_sweep(true, true));

        // Rooms populated: always run. Acquisition sites call
        // `mark_rooms_ever_seen` before the next GC tick, so the
        // `(false, false)` state is transient — but `should_skip_blob_sweep`
        // must still return false for it since rooms are present.
        assert!(!Daemon::should_skip_blob_sweep(false, false));
        assert!(!Daemon::should_skip_blob_sweep(false, true));
    }

    /// Build a persisted notebook-doc `.automerge` file with one markdown
    /// cell that references `blob_hash` via `resolved_assets`. Mirrors the
    /// real shape of a persisted untitled-notebook doc for GC purposes.
    fn write_persisted_doc_with_blob(
        docs_dir: &Path,
        notebook_id: &str,
        blob_hash: &str,
    ) -> PathBuf {
        use notebook_doc::NotebookDoc;
        let mut doc = NotebookDoc::new_with_actor(notebook_id, "test");
        // Add a markdown cell and mark a resolved asset pointing at blob_hash.
        let cell_id = "cell-gc-test";
        doc.add_cell(0, cell_id, "markdown").unwrap();
        let mut assets = std::collections::HashMap::new();
        assets.insert("image.png".to_string(), blob_hash.to_string());
        doc.set_cell_resolved_assets(cell_id, &assets).unwrap();

        let filename = notebook_doc::notebook_doc_filename(notebook_id);
        let path = docs_dir.join(filename);
        std::fs::create_dir_all(docs_dir).unwrap();
        std::fs::write(&path, doc.save()).unwrap();
        path
    }

    #[tokio::test]
    async fn blob_gc_grace_respects_env_override() {
        // Scoped env var: set → read → unset to avoid polluting other tests.
        // Tests in the same process share env, so these asserts check
        // behavior at the time of the call, not global state.
        std::env::set_var(BLOB_GC_GRACE_ENV, "7");
        assert_eq!(blob_gc_grace(), std::time::Duration::from_secs(7));

        std::env::set_var(BLOB_GC_GRACE_ENV, "not-a-number");
        assert_eq!(
            blob_gc_grace(),
            std::time::Duration::from_secs(BLOB_GC_GRACE_SECS)
        );

        std::env::remove_var(BLOB_GC_GRACE_ENV);
        assert_eq!(
            blob_gc_grace(),
            std::time::Duration::from_secs(BLOB_GC_GRACE_SECS)
        );
    }

    #[tokio::test]
    async fn blob_gc_default_grace_is_thirty_days() {
        // Guard constant — changing it silently would undo spec 1.
        assert_eq!(BLOB_GC_GRACE_SECS, 30 * 24 * 3600);
    }

    #[tokio::test]
    async fn collect_hashes_walks_persisted_doc_resolved_assets() {
        let tmp = tempfile::TempDir::new().unwrap();
        let docs_dir = tmp.path().to_path_buf();
        let path = write_persisted_doc_with_blob(&docs_dir, "untitled-abc", "deadbeef");

        let mut hashes = std::collections::HashSet::new();
        let ok = collect_hashes_from_persisted_doc(&path, &mut hashes).await;
        assert!(ok, "expected to decode persisted doc");
        assert!(
            hashes.contains("deadbeef"),
            "resolved_assets blob hash should be collected, got {:?}",
            hashes
        );
    }

    #[tokio::test]
    async fn collect_hashes_skips_corrupt_persisted_doc() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("corrupt.automerge");
        std::fs::write(&path, b"not an automerge document").unwrap();

        let mut hashes = std::collections::HashSet::new();
        let ok = collect_hashes_from_persisted_doc(&path, &mut hashes).await;
        assert!(!ok, "corrupt doc should return false, not panic");
        assert!(hashes.is_empty());
    }

    #[tokio::test]
    async fn collect_blob_refs_for_gc_reads_persisted_docs() {
        // No active rooms, but a persisted doc on disk carries a blob ref.
        // The mark phase must still surface it.
        let tmp = tempfile::TempDir::new().unwrap();
        let docs_dir = tmp.path().join("notebook-docs");
        write_persisted_doc_with_blob(&docs_dir, "untitled-xyz", "cafebabe");

        let rooms: Vec<(String, Arc<crate::notebook_sync_server::NotebookRoom>)> = vec![];
        let refs = Daemon::collect_blob_refs_for_gc(&rooms, &docs_dir).await;
        assert!(
            refs.contains("cafebabe"),
            "persisted-doc blob ref should be collected, got {:?}",
            refs
        );
    }

    #[tokio::test]
    async fn sweep_preserves_referenced_blob() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = BlobStore::new(tmp.path().join("blobs"));
        let hash = blob_store
            .put(b"referenced-content", "application/octet-stream")
            .await
            .unwrap();

        let mut referenced = std::collections::HashSet::new();
        referenced.insert(hash.clone());

        // Short grace so "older than grace" is easy to trigger if the blob
        // were unreferenced — but since it IS referenced, it should survive.
        Daemon::sweep_orphaned_blobs(&blob_store, &referenced, std::time::Duration::from_secs(0))
            .await;

        assert!(
            blob_store.get(&hash).await.unwrap().is_some(),
            "referenced blob should survive sweep"
        );
    }

    #[tokio::test]
    async fn sweep_deletes_unreferenced_blob_past_grace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = BlobStore::new(tmp.path().join("blobs"));
        let hash = blob_store
            .put(b"orphan-content", "application/octet-stream")
            .await
            .unwrap();

        // Let wall-clock advance past the zero-second grace window before
        // sweeping. `num_seconds()` truncates, so we need >1 full second of
        // elapsed time to satisfy `age_secs > 0` when grace is 0.
        tokio::time::sleep(std::time::Duration::from_millis(1100)).await;

        let referenced = std::collections::HashSet::new();
        Daemon::sweep_orphaned_blobs(&blob_store, &referenced, std::time::Duration::from_secs(0))
            .await;

        assert!(
            blob_store.get(&hash).await.unwrap().is_none(),
            "unreferenced blob past grace should be deleted"
        );
    }

    #[tokio::test]
    async fn sweep_preserves_unreferenced_blob_within_grace() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = BlobStore::new(tmp.path().join("blobs"));
        let hash = blob_store
            .put(b"recent-orphan", "application/octet-stream")
            .await
            .unwrap();

        let referenced = std::collections::HashSet::new();
        // 30-day grace — blob just written is well within it.
        Daemon::sweep_orphaned_blobs(
            &blob_store,
            &referenced,
            std::time::Duration::from_secs(BLOB_GC_GRACE_SECS),
        )
        .await;

        assert!(
            blob_store.get(&hash).await.unwrap().is_some(),
            "unreferenced blob within grace should survive"
        );
    }

    #[test]
    fn bare_uuid_path_is_rejected() {
        assert!(looks_like_untitled_notebook_path(
            "550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(!looks_like_untitled_notebook_path(
            "/tmp/550e8400-e29b-41d4-a716-446655440000"
        ));
        assert!(!looks_like_untitled_notebook_path(
            "550e8400-e29b-41d4-a716-446655440000.ipynb"
        ));
    }
}
