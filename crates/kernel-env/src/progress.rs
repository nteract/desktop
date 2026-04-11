// RwLock::read/write only fail if another thread panicked while holding the lock.
// In that case the program is already crashing, so unwrap is acceptable here.
#![allow(clippy::unwrap_used)]

//! Progress reporting for environment operations.
//!
//! Provides [`EnvProgressPhase`] events covering the full lifecycle of
//! environment creation (fetching repodata, solving, downloading, linking)
//! and a [`ProgressHandler`] trait that consumers implement to route events
//! to their UI layer.

#[cfg(feature = "runtime")]
use rattler::install::{Reporter, Transaction};
#[cfg(feature = "runtime")]
use rattler_conda_types::{PrefixRecord, RepoDataRecord};
use serde::{Deserialize, Serialize};
#[cfg(feature = "runtime")]
use std::collections::HashMap;
#[cfg(feature = "runtime")]
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
#[cfg(feature = "runtime")]
use std::sync::{Arc, RwLock};
#[cfg(feature = "runtime")]
use std::time::Instant;

/// Progress phases during environment preparation.
///
/// These events cover the full lifecycle from cache check through
/// ready-to-use. Serializable for transport over IPC / Tauri events.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum EnvProgressPhase {
    /// Starting environment preparation.
    Starting { env_hash: String },
    /// Using a cached environment (fast path).
    CacheHit { env_path: String },
    /// Environment resolved from local package cache without network access.
    OfflineHit,
    /// Fetching package metadata from channels.
    FetchingRepodata { channels: Vec<String> },
    /// Repodata fetch complete.
    RepodataComplete {
        record_count: usize,
        elapsed_ms: u64,
    },
    /// Solving dependency graph.
    Solving { spec_count: usize },
    /// Solve complete.
    SolveComplete {
        package_count: usize,
        elapsed_ms: u64,
    },
    /// Installing packages (aggregate phase).
    Installing { total: usize },
    /// Download progress for individual packages.
    DownloadProgress {
        /// Number of packages fully downloaded.
        completed: usize,
        /// Total number of packages to download.
        total: usize,
        /// Name of the package currently being downloaded.
        current_package: String,
        /// Total bytes downloaded so far.
        bytes_downloaded: u64,
        /// Total bytes to download (if known).
        bytes_total: Option<u64>,
        /// Current download speed in bytes per second.
        bytes_per_second: f64,
    },
    /// Linking/installing packages into the environment.
    LinkProgress {
        /// Number of packages fully linked.
        completed: usize,
        /// Total number of packages to link.
        total: usize,
        /// Name of the package currently being linked.
        current_package: String,
    },
    /// Installation complete.
    InstallComplete { elapsed_ms: u64 },
    /// Creating virtual environment (UV-specific).
    CreatingVenv,
    /// Installing pip packages (UV-specific).
    InstallingPackages { packages: Vec<String> },
    /// Environment is ready.
    Ready {
        env_path: String,
        python_path: String,
    },
    /// An error occurred.
    Error { message: String },
}

/// Trait for receiving environment progress events.
///
/// Implement this to route progress to your UI layer (Tauri events,
/// daemon broadcast channel, logs, etc.).
pub trait ProgressHandler: Send + Sync {
    /// Called for each progress phase during environment creation.
    ///
    /// `env_type` is `"conda"` or `"uv"`.
    fn on_progress(&self, env_type: &str, phase: EnvProgressPhase);
}

/// Log-only progress handler.
///
/// Writes progress phases to the `log` crate at info level.
pub struct LogHandler;

impl ProgressHandler for LogHandler {
    fn on_progress(&self, env_type: &str, phase: EnvProgressPhase) {
        match &phase {
            EnvProgressPhase::Starting { env_hash } => {
                log::info!("[{env_type}] Starting environment preparation (hash: {env_hash})");
            }
            EnvProgressPhase::CacheHit { env_path } => {
                log::info!("[{env_type}] Cache hit: {env_path}");
            }
            EnvProgressPhase::OfflineHit => {
                log::info!("[{env_type}] Resolved from local cache (offline mode)");
            }
            EnvProgressPhase::FetchingRepodata { channels } => {
                log::info!("[{env_type}] Fetching repodata from: {channels:?}");
            }
            EnvProgressPhase::RepodataComplete {
                record_count,
                elapsed_ms,
            } => {
                log::info!("[{env_type}] Loaded {record_count} package records in {elapsed_ms}ms");
            }
            EnvProgressPhase::Solving { spec_count } => {
                log::info!("[{env_type}] Solving {spec_count} specs...");
            }
            EnvProgressPhase::SolveComplete {
                package_count,
                elapsed_ms,
            } => {
                log::info!("[{env_type}] Resolved {package_count} packages in {elapsed_ms}ms");
            }
            EnvProgressPhase::Installing { total } => {
                log::info!("[{env_type}] Installing {total} packages...");
            }
            EnvProgressPhase::DownloadProgress {
                completed, total, ..
            } => {
                log::debug!("[{env_type}] Download {completed}/{total}");
            }
            EnvProgressPhase::LinkProgress {
                completed, total, ..
            } => {
                log::debug!("[{env_type}] Link {completed}/{total}");
            }
            EnvProgressPhase::InstallComplete { elapsed_ms } => {
                log::info!("[{env_type}] Installation complete in {elapsed_ms}ms");
            }
            EnvProgressPhase::CreatingVenv => {
                log::info!("[{env_type}] Creating virtual environment...");
            }
            EnvProgressPhase::InstallingPackages { packages } => {
                log::info!("[{env_type}] Installing packages: {packages:?}");
            }
            EnvProgressPhase::Ready {
                env_path,
                python_path,
            } => {
                log::info!("[{env_type}] Ready: env={env_path} python={python_path}");
            }
            EnvProgressPhase::Error { message } => {
                log::error!("[{env_type}] Error: {message}");
            }
        }
    }
}

/// Rattler [`Reporter`] implementation that delegates to [`ProgressHandler`].
///
/// Tracks download/link progress atomically and emits throttled
/// [`EnvProgressPhase::DownloadProgress`] / [`LinkProgress`] events.
///
/// Uses `Arc<dyn ProgressHandler>` for ownership since rattler's `Installer`
/// requires `'static` reporters.
#[cfg(feature = "runtime")]
pub struct RattlerReporter {
    handler: Arc<dyn ProgressHandler>,
    /// Label for progress events (e.g. "conda", "pixi").
    env_type: String,
    /// Total packages to download.
    total_downloads: AtomicUsize,
    /// Number of packages fully downloaded.
    downloaded_packages: AtomicUsize,
    /// Total bytes downloaded across all packages.
    bytes_downloaded: AtomicU64,
    /// Total bytes to download (if known).
    bytes_total: AtomicU64,
    /// When downloading started.
    download_start: RwLock<Option<Instant>>,
    /// Total packages to link.
    total_to_link: AtomicUsize,
    /// Number of packages fully linked.
    linked_packages: AtomicUsize,
    /// Package names indexed by operation/cache index.
    package_names: RwLock<HashMap<usize, String>>,
    /// Current package being downloaded.
    current_download: RwLock<Option<String>>,
    /// Last time we emitted a download progress event (throttling).
    last_download_emit: RwLock<Option<Instant>>,
    /// Per-download cumulative progress (for computing deltas).
    /// Rattler reports cumulative bytes per download, not deltas.
    download_progress_by_idx: RwLock<HashMap<usize, u64>>,
}

#[cfg(feature = "runtime")]
impl RattlerReporter {
    /// Create a new reporter that delegates to the given handler.
    ///
    /// The `env_type` label defaults to `"conda"`. Use [`new_with_env_type`]
    /// to override it (e.g. `"pixi"`).
    pub fn new(handler: Arc<dyn ProgressHandler>) -> Self {
        Self::new_with_env_type(handler, "conda")
    }

    /// Create a reporter with a custom environment type label.
    pub fn new_with_env_type(handler: Arc<dyn ProgressHandler>, env_type: &str) -> Self {
        Self {
            handler,
            env_type: env_type.to_string(),
            total_downloads: AtomicUsize::new(0),
            downloaded_packages: AtomicUsize::new(0),
            bytes_downloaded: AtomicU64::new(0),
            bytes_total: AtomicU64::new(0),
            download_start: RwLock::new(None),
            total_to_link: AtomicUsize::new(0),
            linked_packages: AtomicUsize::new(0),
            package_names: RwLock::new(HashMap::new()),
            current_download: RwLock::new(None),
            last_download_emit: RwLock::new(None),
            download_progress_by_idx: RwLock::new(HashMap::new()),
        }
    }

    /// Emit download progress (throttled to at most once per 100ms).
    fn emit_download_progress(&self) {
        {
            let mut last_emit = self.last_download_emit.write().unwrap();
            if let Some(last) = *last_emit {
                if last.elapsed().as_millis() < 100 {
                    return;
                }
            }
            *last_emit = Some(Instant::now());
        }

        let completed = self.downloaded_packages.load(Ordering::SeqCst);
        let total = self.total_downloads.load(Ordering::SeqCst);
        let bytes_downloaded = self.bytes_downloaded.load(Ordering::SeqCst);
        let bytes_total = self.bytes_total.load(Ordering::SeqCst);

        let current_package = self
            .current_download
            .read()
            .unwrap()
            .clone()
            .unwrap_or_default();

        let bytes_per_second = {
            let start = self.download_start.read().unwrap();
            match *start {
                Some(s) => {
                    let elapsed = s.elapsed().as_secs_f64();
                    if elapsed > 0.0 {
                        bytes_downloaded as f64 / elapsed
                    } else {
                        0.0
                    }
                }
                None => 0.0,
            }
        };

        self.handler.on_progress(
            &self.env_type,
            EnvProgressPhase::DownloadProgress {
                completed,
                total,
                current_package,
                bytes_downloaded,
                bytes_total: if bytes_total > 0 {
                    Some(bytes_total)
                } else {
                    None
                },
                bytes_per_second,
            },
        );
    }

    /// Emit link progress.
    fn emit_link_progress(&self, current_package: String) {
        let completed = self.linked_packages.load(Ordering::SeqCst);
        let total = self.total_to_link.load(Ordering::SeqCst);

        self.handler.on_progress(
            &self.env_type,
            EnvProgressPhase::LinkProgress {
                completed,
                total,
                current_package,
            },
        );
    }
}

#[cfg(feature = "runtime")]
impl Reporter for RattlerReporter {
    fn on_transaction_start(&self, transaction: &Transaction<PrefixRecord, RepoDataRecord>) {
        let total = transaction.operations.len();
        self.total_to_link.store(total, Ordering::SeqCst);
        self.total_downloads.store(total, Ordering::SeqCst);
        *self.download_start.write().unwrap() = Some(Instant::now());
    }

    fn on_transaction_operation_start(&self, _operation: usize) {}

    fn on_populate_cache_start(&self, cache_entry: usize, record: &RepoDataRecord) -> usize {
        let name = record.package_record.name.as_source().to_string();
        self.package_names
            .write()
            .unwrap()
            .insert(cache_entry, name);
        cache_entry
    }

    fn on_validate_start(&self, cache_entry: usize) -> usize {
        cache_entry
    }

    fn on_validate_complete(&self, _validate_idx: usize) {}

    fn on_download_start(&self, cache_entry: usize) -> usize {
        let name = self
            .package_names
            .read()
            .unwrap()
            .get(&cache_entry)
            .cloned()
            .unwrap_or_default();
        *self.current_download.write().unwrap() = Some(name);
        cache_entry
    }

    fn on_download_progress(&self, download_idx: usize, progress: u64, total: Option<u64>) {
        // Rattler reports cumulative bytes per download, not deltas.
        // Track per-download progress and compute the delta to avoid overcounting.
        let delta = {
            let mut progress_map = self.download_progress_by_idx.write().unwrap();
            let prev = progress_map.insert(download_idx, progress).unwrap_or(0);
            progress.saturating_sub(prev)
        };
        self.bytes_downloaded.fetch_add(delta, Ordering::SeqCst);
        if let Some(t) = total {
            let current_total = self.bytes_total.load(Ordering::SeqCst);
            if current_total == 0 {
                self.bytes_total.store(t, Ordering::SeqCst);
            }
        }
        self.emit_download_progress();
    }

    fn on_download_completed(&self, _download_idx: usize) {
        self.downloaded_packages.fetch_add(1, Ordering::SeqCst);
        self.emit_download_progress();
    }

    fn on_populate_cache_complete(&self, _cache_entry: usize) {}

    fn on_unlink_start(&self, operation: usize, _record: &PrefixRecord) -> usize {
        operation
    }

    fn on_unlink_complete(&self, _index: usize) {}

    fn on_link_start(&self, operation: usize, record: &RepoDataRecord) -> usize {
        let name = record.package_record.name.as_source().to_string();
        self.package_names
            .write()
            .unwrap()
            .insert(operation, name.clone());
        self.emit_link_progress(name);
        operation
    }

    fn on_link_complete(&self, index: usize) {
        self.linked_packages.fetch_add(1, Ordering::SeqCst);
        let name = self
            .package_names
            .read()
            .unwrap()
            .get(&index)
            .cloned()
            .unwrap_or_default();
        self.emit_link_progress(name);
    }

    fn on_transaction_operation_complete(&self, _operation: usize) {}

    fn on_transaction_complete(&self) {}

    fn on_post_link_start(&self, _package_name: &str, _script_path: &str) -> usize {
        0
    }

    fn on_post_link_complete(&self, _index: usize, _success: bool) {}

    fn on_pre_unlink_start(&self, _package_name: &str, _script_path: &str) -> usize {
        0
    }

    fn on_pre_unlink_complete(&self, _index: usize, _success: bool) {}
}
