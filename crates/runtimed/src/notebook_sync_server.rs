//! Room-based notebook synchronization server.
//!
//! Each open notebook gets a "room" in the daemon. Multiple windows editing
//! the same notebook sync through the room's canonical Automerge document.
//!
//! Follows the same sync protocol pattern as `sync_server.rs` (settings sync)
//! but with per-notebook state managed through rooms.
//!
//! ## Room lifecycle
//!
//! 1. First window opens notebook → daemon creates room, loads persisted doc
//! 2. Client exchanges Automerge sync messages with the room
//! 3. Additional windows join the same room
//! 4. Changes from any peer broadcast to all others in the room
//! 5. When the last peer disconnects, the room is evicted from memory
//!    (any pending doc bytes are flushed to disk via debounced persistence)
//! 6. Documents persist to `~/.cache/runt/notebook-docs/{hash}.automerge`
//!
//! ## Phase 8: Daemon-owned kernel execution
//!
//! Each room can have an optional kernel. When a kernel is launched:
//! - Execute requests flow through the daemon
//! - Daemon tracks msg_id → cell_id mapping
//! - Outputs are broadcast to all connected windows
//! - Multiple windows share the same kernel

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use automerge::sync;
use log::{debug, error, info, warn};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, oneshot, watch, Mutex, RwLock};

use notify_debouncer_mini::DebounceEventResult;

use crate::blob_store::BlobStore;
use crate::comm_state::CommState;
use crate::connection::{self, NotebookFrameType};
use crate::kernel_manager::{DenoLaunchedConfig, LaunchedEnvConfig, RoomKernel};
use crate::notebook_doc::{notebook_doc_filename, CellSnapshot, NotebookDoc};
use crate::notebook_metadata::{NotebookMetadataSnapshot, NOTEBOOK_METADATA_KEY};
use crate::protocol::{EnvSyncDiff, NotebookBroadcast, NotebookRequest, NotebookResponse};

/// Trust state for a notebook room.
/// Tracks whether the notebook's dependencies are trusted for auto-launch.
#[derive(Debug, Clone)]
pub struct TrustState {
    pub status: runt_trust::TrustStatus,
    pub info: runt_trust::TrustInfo,
    /// If true, kernel launch is pending user trust approval
    pub pending_launch: bool,
}

/// Detect the kernel type from a notebook's metadata snapshot.
/// Returns "python" or "deno" based on the kernelspec and language_info.
/// This is the #1 priority - the notebook's kernelspec determines the runtime.
fn detect_notebook_kernel_type(snapshot: &NotebookMetadataSnapshot) -> Option<String> {
    // Check kernelspec.name first (most reliable)
    if let Some(ref kernelspec) = snapshot.kernelspec {
        let name_lower = kernelspec.name.to_lowercase();
        if name_lower.contains("deno") {
            return Some("deno".to_string());
        }
        if name_lower.contains("python") {
            return Some("python".to_string());
        }
        // Also check language field
        if let Some(ref lang) = kernelspec.language {
            let lang_lower = lang.to_lowercase();
            if lang_lower == "typescript" || lang_lower == "javascript" {
                return Some("deno".to_string());
            }
            if lang_lower == "python" {
                return Some("python".to_string());
            }
        }
    }

    // Fallback: check language_info.name
    if let Some(ref lang_info) = snapshot.language_info {
        let name_lower = lang_info.name.to_lowercase();
        if name_lower == "typescript" || name_lower == "javascript" || name_lower == "deno" {
            return Some("deno".to_string());
        }
        if name_lower == "python" {
            return Some("python".to_string());
        }
    }

    None // Unknown kernel type
}

/// Check if a notebook's metadata snapshot has inline dependencies or Deno config.
/// Returns the appropriate env_source if found ("uv:inline", "conda:inline", or "deno").
///
/// Priority: Deno is checked first, then UV deps, then conda deps.
fn check_inline_deps(snapshot: &NotebookMetadataSnapshot) -> Option<String> {
    // Check for Deno config first (runt.deno)
    if snapshot.runt.deno.is_some() {
        return Some("deno".to_string());
    }

    // Check UV dependencies
    if let Some(ref uv) = snapshot.runt.uv {
        if !uv.dependencies.is_empty() {
            return Some("uv:inline".to_string());
        }
    }

    // Check conda dependencies
    if let Some(ref conda) = snapshot.runt.conda {
        if !conda.dependencies.is_empty() {
            return Some("conda:inline".to_string());
        }
    }

    None
}

/// Extract inline conda dependencies from a metadata snapshot.
/// Returns the list of dependency strings if conda deps are present.
fn get_inline_conda_deps(snapshot: &NotebookMetadataSnapshot) -> Option<Vec<String>> {
    if let Some(ref conda) = snapshot.runt.conda {
        if !conda.dependencies.is_empty() {
            return Some(conda.dependencies.clone());
        }
    }
    None
}

/// Extract inline UV dependencies from a metadata snapshot.
/// Returns the list of dependency strings if UV deps are present.
fn get_inline_uv_deps(snapshot: &NotebookMetadataSnapshot) -> Option<Vec<String>> {
    if let Some(ref uv) = snapshot.runt.uv {
        if !uv.dependencies.is_empty() {
            return Some(uv.dependencies.clone());
        }
    }
    None
}

/// Extract conda channels from a metadata snapshot.
/// Returns the list of channel strings, or defaults to ["conda-forge"].
fn get_inline_conda_channels(snapshot: &NotebookMetadataSnapshot) -> Vec<String> {
    if let Some(ref conda) = snapshot.runt.conda {
        if !conda.channels.is_empty() {
            return conda.channels.clone();
        }
    }
    vec!["conda-forge".to_string()]
}

/// Build a LaunchedEnvConfig from the current metadata snapshot.
/// This captures what configuration was used at kernel launch time.
fn build_launched_config(
    kernel_type: &str,
    env_source: &str,
    inline_deps: Option<&[String]>,
    metadata_snapshot: Option<&NotebookMetadataSnapshot>,
    venv_path: Option<PathBuf>,
    python_path: Option<PathBuf>,
) -> LaunchedEnvConfig {
    let mut config = LaunchedEnvConfig::default();

    match env_source {
        "uv:inline" => {
            config.uv_deps = inline_deps.map(|d| d.to_vec());
            config.venv_path = venv_path;
            config.python_path = python_path;
        }
        "conda:inline" => {
            config.conda_deps = inline_deps.map(|d| d.to_vec());
            config.venv_path = venv_path;
            config.python_path = python_path;
            if let Some(snapshot) = metadata_snapshot {
                config.conda_channels = Some(get_inline_conda_channels(snapshot));
            }
        }
        _ => {}
    }

    // For Deno kernels, capture the deno config
    if kernel_type == "deno" {
        if let Some(snapshot) = metadata_snapshot {
            if let Some(ref deno) = snapshot.runt.deno {
                config.deno_config = Some(DenoLaunchedConfig {
                    permissions: deno.permissions.clone(),
                    import_map: deno.import_map.clone(),
                    config: deno.config.clone(),
                    flexible_npm_imports: deno.flexible_npm_imports.unwrap_or(true),
                });
            }
        }
    }

    // Generate unique launch ID for this kernel session (for race detection during hot-sync)
    config.launch_id = Some(uuid::Uuid::new_v4().to_string());

    config
}

/// Compute the difference between launched config and current metadata.
/// Returns Some(diff) if there are differences, None if in sync.
fn compute_env_sync_diff(
    launched: &LaunchedEnvConfig,
    current: &NotebookMetadataSnapshot,
) -> Option<EnvSyncDiff> {
    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut deno_changed = false;

    // Check UV deps
    if let Some(ref launched_uv) = launched.uv_deps {
        let current_uv = current
            .runt
            .uv
            .as_ref()
            .map(|u| &u.dependencies[..])
            .unwrap_or(&[]);

        for dep in current_uv {
            if !launched_uv.contains(dep) {
                added.push(dep.clone());
            }
        }
        for dep in launched_uv {
            if !current_uv.contains(dep) {
                removed.push(dep.clone());
            }
        }
    }

    // Check conda deps and channels
    let mut channels_changed = false;
    if let Some(ref launched_conda) = launched.conda_deps {
        let current_conda = current
            .runt
            .conda
            .as_ref()
            .map(|c| &c.dependencies[..])
            .unwrap_or(&[]);

        for dep in current_conda {
            if !launched_conda.contains(dep) {
                added.push(dep.clone());
            }
        }
        for dep in launched_conda {
            if !current_conda.contains(dep) {
                removed.push(dep.clone());
            }
        }

        // Check channels
        if let Some(ref launched_channels) = launched.conda_channels {
            let current_channels = current
                .runt
                .conda
                .as_ref()
                .map(|c| &c.channels[..])
                .unwrap_or(&[]);

            // Channels are ordered, so compare as slices
            if launched_channels.as_slice() != current_channels {
                channels_changed = true;
            }
        }
    }

    // Check deno config
    if let Some(ref launched_deno) = launched.deno_config {
        if let Some(ref current_deno) = current.runt.deno {
            let current_flexible = current_deno.flexible_npm_imports.unwrap_or(true);
            if launched_deno.permissions != current_deno.permissions
                || launched_deno.import_map != current_deno.import_map
                || launched_deno.config != current_deno.config
                || launched_deno.flexible_npm_imports != current_flexible
            {
                deno_changed = true;
            }
        } else {
            // Deno config was removed
            deno_changed = true;
        }
    }

    if added.is_empty() && removed.is_empty() && !channels_changed && !deno_changed {
        None
    } else {
        Some(EnvSyncDiff {
            added,
            removed,
            channels_changed,
            deno_changed,
        })
    }
}

/// Check if the current metadata differs from kernel's launched config and broadcast sync state.
/// Called after metadata sync to notify all clients about dependency drift.
///
/// Handles two cases:
/// 1. Kernel launched with inline deps - track drift (additions/removals)
/// 2. Kernel launched with prewarmed - detect when user adds inline deps (needs restart)
async fn check_and_broadcast_sync_state(room: &NotebookRoom) {
    // Get current metadata from doc
    let current_metadata = {
        let doc = room.doc.read().await;
        if let Some(meta_json) = doc.get_metadata(NOTEBOOK_METADATA_KEY) {
            serde_json::from_str::<NotebookMetadataSnapshot>(&meta_json).ok()
        } else {
            None
        }
    };

    let Some(current_metadata) = current_metadata else {
        return;
    };

    // Check if kernel is running
    let kernel_guard = room.kernel.lock().await;
    if let Some(ref kernel) = *kernel_guard {
        if !kernel.is_running() {
            return;
        }

        let launched = kernel.launched_config();

        // Check if we're tracking inline deps or deno config
        let is_tracking = launched.uv_deps.is_some()
            || launched.conda_deps.is_some()
            || launched.deno_config.is_some();

        if is_tracking {
            // Case 1: Kernel launched with inline deps - compute drift
            let diff = compute_env_sync_diff(launched, &current_metadata);
            let in_sync = diff.is_none();

            let _ = room
                .kernel_broadcast_tx
                .send(NotebookBroadcast::EnvSyncState { in_sync, diff });
        } else {
            // Case 2: Kernel launched with prewarmed - check if metadata now has inline deps
            // This happens when user adds deps to a notebook with a prewarmed kernel running
            let current_inline = check_inline_deps(&current_metadata);

            if let Some(ref inline_source) = current_inline {
                // Metadata now has inline deps but kernel is prewarmed - needs restart
                let added = match inline_source.as_str() {
                    "uv:inline" => get_inline_uv_deps(&current_metadata).unwrap_or_default(),
                    "conda:inline" => get_inline_conda_deps(&current_metadata).unwrap_or_default(),
                    _ => vec![],
                };

                if !added.is_empty() {
                    let _ = room
                        .kernel_broadcast_tx
                        .send(NotebookBroadcast::EnvSyncState {
                            in_sync: false,
                            diff: Some(EnvSyncDiff {
                                added,
                                removed: vec![],
                                channels_changed: false,
                                deno_changed: false,
                            }),
                        });
                }
            }
        }
    }
}

/// Resolve the metadata snapshot for a notebook, trying the Automerge doc first
/// and falling back to disk if the doc doesn't have metadata yet (e.g., before
/// the first client has synced).
async fn resolve_metadata_snapshot(
    room: &NotebookRoom,
    notebook_path: Option<&Path>,
) -> Option<NotebookMetadataSnapshot> {
    // Try reading from the Automerge doc first
    {
        let doc = room.doc.read().await;
        if let Some(meta_json) = doc.get_metadata(NOTEBOOK_METADATA_KEY) {
            if let Ok(snapshot) = serde_json::from_str::<NotebookMetadataSnapshot>(&meta_json) {
                debug!("[notebook-sync] Resolved metadata snapshot from Automerge doc");
                return Some(snapshot);
            }
        }
    }

    // Fall back to reading from disk
    if let Some(path) = notebook_path {
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(nb) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(metadata) = nb.get("metadata") {
                    let snapshot = NotebookMetadataSnapshot::from_metadata_value(metadata);
                    debug!("[notebook-sync] Resolved metadata snapshot from disk");
                    return Some(snapshot);
                }
            }
        }
    }

    None
}

/// Verify trust status of a notebook by reading its file.
/// Returns TrustState with the verification result.
///
/// Note: Trust verification requires the raw metadata HashMap (including
/// trust_signature) which is not part of NotebookMetadataSnapshot. This
/// must read from disk until trust_signature is added to the snapshot.
fn verify_trust_from_file(notebook_path: &Path) -> TrustState {
    // Read and parse the notebook file
    let metadata = match std::fs::read_to_string(notebook_path) {
        Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
            Ok(nb) => nb
                .get("metadata")
                .and_then(|m| m.as_object())
                .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
            Err(_) => std::collections::HashMap::new(),
        },
        Err(_) => std::collections::HashMap::new(),
    };

    // Verify trust using the shared runt-trust crate
    match runt_trust::verify_notebook_trust(&metadata) {
        Ok(info) => TrustState {
            status: info.status.clone(),
            info,
            pending_launch: false,
        },
        Err(_) => TrustState {
            status: runt_trust::TrustStatus::Untrusted,
            info: runt_trust::TrustInfo {
                status: runt_trust::TrustStatus::Untrusted,
                uv_dependencies: vec![],
                conda_dependencies: vec![],
                conda_channels: vec![],
            },
            pending_launch: false,
        },
    }
}

/// A notebook sync room — holds the canonical document and a broadcast
/// channel for notifying peers of changes.
pub struct NotebookRoom {
    /// The canonical Automerge notebook document.
    pub doc: Arc<RwLock<NotebookDoc>>,
    /// Broadcast channel to notify all peers in this room of changes.
    pub changed_tx: broadcast::Sender<()>,
    /// Broadcast channel for kernel events (outputs, status changes).
    pub kernel_broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    /// Channel to send doc bytes to the debounced persistence task.
    /// Uses watch for "latest value" semantics - always keeps most recent state.
    pub persist_tx: watch::Sender<Option<Vec<u8>>>,
    /// Persistence path for this room's document.
    pub persist_path: PathBuf,
    /// Number of active peer connections in this room.
    pub active_peers: AtomicUsize,
    /// Optional kernel for this room (Phase 8: daemon-owned execution).
    /// Arc-wrapped so spawned command processor task can access it.
    pub kernel: Arc<Mutex<Option<RoomKernel>>>,
    /// Blob store for output manifests.
    pub blob_store: Arc<BlobStore>,
    /// Trust state for this notebook (for auto-launch decisions).
    pub trust_state: Arc<RwLock<TrustState>>,
    /// The notebook file path (notebook_id is the path).
    pub notebook_path: PathBuf,
    /// Working directory for untitled notebooks (used for project file detection).
    /// When the notebook_id is a UUID (untitled), this provides the directory context
    /// for finding pyproject.toml, pixi.toml, or environment.yaml.
    pub working_dir: Arc<RwLock<Option<PathBuf>>>,
    /// Timestamp when auto-launch was triggered (for grace period on eviction).
    /// If set, the room won't be evicted for 30 seconds to allow client reconnect.
    pub auto_launch_at: Arc<RwLock<Option<std::time::Instant>>>,
    /// Comm channel state for widgets.
    /// Stores active comms so new windows can sync widget models.
    /// Arc-wrapped so it can be shared with the kernel's iopub task.
    pub comm_state: Arc<CommState>,
    /// Timestamp (ms since epoch) of last self-write to the .ipynb file.
    /// Used to skip file watcher events triggered by our own saves.
    pub last_self_write: Arc<AtomicU64>,
    /// Shutdown signal for the file watcher task.
    /// Wrapped in Mutex to allow setting after Arc creation.
    /// Sent when the room is evicted to stop the watcher.
    watcher_shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
}

impl NotebookRoom {
    /// Create a fresh room, ignoring any persisted state.
    ///
    /// The .ipynb file is the source of truth. When a room is created, we start
    /// with an empty Automerge doc and let the first client populate it from
    /// their local .ipynb file. This prevents stale outputs from previous
    /// sessions from accumulating.
    ///
    /// Any existing persisted doc is deleted to avoid clutter.
    ///
    /// Note: Trust state is initialized from disk because the Automerge doc
    /// starts empty (first client hasn't synced yet). Trust verification
    /// also requires trust_signature which is not in NotebookMetadataSnapshot.
    pub fn new_fresh(notebook_id: &str, docs_dir: &Path, blob_store: Arc<BlobStore>) -> Self {
        let filename = notebook_doc_filename(notebook_id);
        let persist_path = docs_dir.join(&filename);

        // Delete any stale persisted doc - .ipynb is the source of truth
        if persist_path.exists() {
            info!(
                "[notebook-sync] Deleting stale persisted doc: {:?}",
                persist_path
            );
            let _ = std::fs::remove_file(&persist_path);
        }

        let doc = NotebookDoc::new(notebook_id);
        let (changed_tx, _) = broadcast::channel(16);
        let (kernel_broadcast_tx, _) = broadcast::channel(64);

        // Spawn debounced persistence task (watch channel keeps latest value only)
        let (persist_tx, persist_rx) = watch::channel::<Option<Vec<u8>>>(None);
        spawn_persist_debouncer(persist_rx, persist_path.clone());

        // Verify trust from the notebook file
        let notebook_path = PathBuf::from(notebook_id);
        let trust_state = verify_trust_from_file(&notebook_path);
        info!(
            "[notebook-sync] Trust status for {}: {:?}",
            notebook_id, trust_state.status
        );

        Self {
            doc: Arc::new(RwLock::new(doc)),
            changed_tx,
            kernel_broadcast_tx,
            persist_tx,
            persist_path,
            active_peers: AtomicUsize::new(0),
            kernel: Arc::new(Mutex::new(None)),
            blob_store,
            trust_state: Arc::new(RwLock::new(trust_state)),
            notebook_path,
            working_dir: Arc::new(RwLock::new(None)),
            auto_launch_at: Arc::new(RwLock::new(None)),
            comm_state: Arc::new(CommState::new()),
            last_self_write: Arc::new(AtomicU64::new(0)),
            watcher_shutdown_tx: Mutex::new(None),
        }
    }

    /// Create a new room by loading a persisted document or creating a fresh one.
    ///
    /// Note: This method is kept for tests that verify persistence behavior.
    /// For normal operation, `new_fresh` is used to ensure the .ipynb file
    /// is the source of truth.
    #[cfg(test)]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    pub fn load_or_create(notebook_id: &str, docs_dir: &Path, blob_store: Arc<BlobStore>) -> Self {
        let filename = notebook_doc_filename(notebook_id);
        let persist_path = docs_dir.join(filename);
        let doc = NotebookDoc::load_or_create(&persist_path, notebook_id);
        let (changed_tx, _) = broadcast::channel(16);
        let (kernel_broadcast_tx, _) = broadcast::channel(64);
        let (persist_tx, persist_rx) = watch::channel::<Option<Vec<u8>>>(None);
        spawn_persist_debouncer(persist_rx, persist_path.clone());
        let notebook_path = PathBuf::from(notebook_id);
        let trust_state = verify_trust_from_file(&notebook_path);
        Self {
            doc: Arc::new(RwLock::new(doc)),
            changed_tx,
            kernel_broadcast_tx,
            persist_tx,
            persist_path,
            active_peers: AtomicUsize::new(0),
            kernel: Arc::new(Mutex::new(None)),
            blob_store,
            trust_state: Arc::new(RwLock::new(trust_state)),
            notebook_path,
            working_dir: Arc::new(RwLock::new(None)),
            auto_launch_at: Arc::new(RwLock::new(None)),
            comm_state: Arc::new(CommState::new()),
            last_self_write: Arc::new(AtomicU64::new(0)),
            watcher_shutdown_tx: Mutex::new(None),
        }
    }

    /// Check if this room has an active kernel.
    pub async fn has_kernel(&self) -> bool {
        let kernel = self.kernel.lock().await;
        kernel.as_ref().is_some_and(|k| k.is_running())
    }

    /// Get kernel info if a kernel is running.
    pub async fn kernel_info(&self) -> Option<(String, String, String)> {
        let kernel = self.kernel.lock().await;
        kernel.as_ref().and_then(|k| {
            if k.is_running() {
                Some((
                    k.kernel_type().to_string(),
                    k.env_source().to_string(),
                    k.status().to_string(),
                ))
            } else {
                None
            }
        })
    }
}

/// Thread-safe map of notebook rooms, keyed by notebook_id.
pub type NotebookRooms = Arc<Mutex<HashMap<String, Arc<NotebookRoom>>>>;

/// Get or create a room for a notebook.
///
/// The caller must hold the rooms mutex. This function will create a new
/// fresh room if one doesn't exist. The .ipynb file is the source of truth -
/// the first client to connect will populate the Automerge doc from their
/// local file.
///
/// For .ipynb files, a file watcher is spawned to detect external changes.
pub fn get_or_create_room(
    rooms: &mut HashMap<String, Arc<NotebookRoom>>,
    notebook_id: &str,
    docs_dir: &Path,
    blob_store: Arc<BlobStore>,
) -> Arc<NotebookRoom> {
    rooms
        .entry(notebook_id.to_string())
        .or_insert_with(|| {
            info!("[notebook-sync] Creating room for {}", notebook_id);
            let room = Arc::new(NotebookRoom::new_fresh(notebook_id, docs_dir, blob_store));

            // Spawn file watcher for .ipynb files (not for untitled notebooks with UUID IDs)
            if !is_untitled_notebook(notebook_id) {
                let notebook_path = PathBuf::from(notebook_id);
                if notebook_path.extension().is_some_and(|ext| ext == "ipynb") {
                    let shutdown_tx = spawn_notebook_file_watcher(notebook_path, room.clone());
                    // Store the shutdown sender (blocking lock is OK here, room is new)
                    if let Ok(mut guard) = room.watcher_shutdown_tx.try_lock() {
                        *guard = Some(shutdown_tx);
                    }
                }
            }

            room
        })
        .clone()
}

/// Handle a single notebook sync client connection.
///
/// The caller has already consumed the handshake frame and resolved the room.
/// This function runs the Automerge sync protocol:
/// 1. Initial sync: server sends first message
/// 2. Watch loop: wait for changes (from other peers or from this client),
///    exchange sync messages to propagate
///
/// When the connection closes (client disconnect or error), the peer count
/// is decremented. If it reaches zero, the room is evicted and any pending
/// doc bytes are flushed via debounced persistence.
///
/// Uses v2 typed frames protocol (with first-byte type indicator).
#[allow(clippy::too_many_arguments)]
pub async fn handle_notebook_sync_connection<R, W>(
    mut reader: R,
    mut writer: W,
    room: Arc<NotebookRoom>,
    rooms: NotebookRooms,
    notebook_id: String,
    default_runtime: crate::runtime::Runtime,
    default_python_env: crate::settings_doc::PythonEnvType,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
    working_dir: Option<PathBuf>,
    initial_metadata: Option<String>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Set working_dir on the room if provided (for untitled notebook project detection)
    if let Some(wd) = working_dir {
        let mut room_wd = room.working_dir.write().await;
        *room_wd = Some(wd);
    }

    // Seed initial metadata into the Automerge doc if provided and doc has no metadata yet.
    // This ensures the kernelspec is available before auto-launch decides which kernel to use.
    if let Some(ref metadata_json) = initial_metadata {
        let mut doc = room.doc.write().await;
        if doc.get_metadata(NOTEBOOK_METADATA_KEY).is_none() {
            match doc.set_metadata(NOTEBOOK_METADATA_KEY, metadata_json) {
                Ok(()) => {
                    info!(
                        "[notebook-sync] Seeded initial metadata from handshake for {}",
                        notebook_id
                    );
                }
                Err(e) => {
                    warn!("[notebook-sync] Failed to seed initial metadata: {}", e);
                }
            }
        }
    }

    room.active_peers.fetch_add(1, Ordering::Relaxed);
    let peers = room.active_peers.load(Ordering::Relaxed);
    info!(
        "[notebook-sync] Client connected to room {} ({} peer{})",
        notebook_id,
        peers,
        if peers == 1 { "" } else { "s" }
    );

    // Auto-launch kernel if this is the first peer and notebook is trusted
    if peers == 1 {
        // Check if notebook_id is a UUID (new unsaved notebook) vs a file path
        let is_new_notebook =
            !room.notebook_path.exists() && uuid::Uuid::parse_str(&notebook_id).is_ok();

        let (should_auto_launch, trust_status) = {
            let trust_state = room.trust_state.read().await;
            let has_kernel = room.has_kernel().await;
            let status = trust_state.status.clone();
            let should_launch = !has_kernel
                && matches!(
                    status,
                    runt_trust::TrustStatus::Trusted | runt_trust::TrustStatus::NoDependencies
                )
                // For existing files: trust must be verified (Trusted or NoDependencies)
                // For new notebooks (UUID, no file): NoDependencies is safe to auto-launch
                && (room.notebook_path.exists() || is_new_notebook);
            (should_launch, status)
        };

        if should_auto_launch {
            info!(
                "[notebook-sync] Auto-launching kernel for notebook {} (trust: {:?}, new: {})",
                notebook_id, trust_status, is_new_notebook
            );
            // Record auto-launch time for grace period on eviction
            {
                let mut auto_launch_at = room.auto_launch_at.write().await;
                *auto_launch_at = Some(std::time::Instant::now());
            }
            // Spawn auto-launch in background so we don't block sync
            let room_clone = room.clone();
            let notebook_id_clone = notebook_id.clone();
            let daemon_clone = daemon.clone();
            tokio::spawn(async move {
                auto_launch_kernel(
                    &room_clone,
                    &notebook_id_clone,
                    default_runtime,
                    default_python_env,
                    daemon_clone,
                )
                .await;
            });
        } else if !matches!(
            trust_status,
            runt_trust::TrustStatus::Trusted | runt_trust::TrustStatus::NoDependencies
        ) {
            info!(
                "[notebook-sync] Notebook {} not trusted, skipping auto-launch (status: {:?})",
                notebook_id, trust_status
            );
        }
    }

    // Send capabilities response (v2 protocol)
    let caps = connection::ProtocolCapabilities {
        protocol: connection::PROTOCOL_V2.to_string(),
    };
    connection::send_json_frame(&mut writer, &caps).await?;

    let result = run_sync_loop_v2(&mut reader, &mut writer, &room, daemon.clone()).await;

    // Peer disconnected — decrement and possibly evict the room
    let remaining = room.active_peers.fetch_sub(1, Ordering::Relaxed) - 1;
    if remaining == 0 {
        // Schedule delayed eviction check. This handles:
        // 1. Grace period during auto-launch (client may reconnect)
        // 2. Kernel running with no peers (idle timeout)
        // Without this, rooms with kernels would leak indefinitely.
        let eviction_delay = daemon.room_eviction_delay().await;
        let rooms_for_eviction = rooms.clone();
        let room_for_eviction = room.clone();
        let notebook_id_for_eviction = notebook_id.clone();

        info!(
            "[notebook-sync] All peers disconnected from room {}, scheduling eviction check in {:?}",
            notebook_id,
            eviction_delay
        );

        tokio::spawn(async move {
            tokio::time::sleep(eviction_delay).await;

            // Check if peers reconnected during the delay
            if room_for_eviction.active_peers.load(Ordering::Relaxed) > 0 {
                info!(
                    "[notebook-sync] Eviction cancelled for {} (peers reconnected)",
                    notebook_id_for_eviction
                );
                return;
            }

            // Evict the room and shut down kernel if running
            let mut rooms_guard = rooms_for_eviction.lock().await;
            // Re-check under lock
            if room_for_eviction.active_peers.load(Ordering::Relaxed) == 0 {
                // Shutdown kernel if running
                if let Some(mut kernel) = room_for_eviction.kernel.lock().await.take() {
                    info!(
                        "[notebook-sync] Shutting down idle kernel for {}",
                        notebook_id_for_eviction
                    );
                    if let Err(e) = kernel.shutdown().await {
                        warn!(
                            "[notebook-sync] Error shutting down kernel for {}: {}",
                            notebook_id_for_eviction, e
                        );
                    }
                }

                // Stop file watcher if running
                if let Some(shutdown_tx) = room_for_eviction.watcher_shutdown_tx.lock().await.take()
                {
                    let _ = shutdown_tx.send(());
                    debug!(
                        "[notebook-sync] Stopped file watcher for {}",
                        notebook_id_for_eviction
                    );
                }

                rooms_guard.remove(&notebook_id_for_eviction);
                info!(
                    "[notebook-sync] Evicted room {} (idle timeout)",
                    notebook_id_for_eviction
                );
            }
        });
    } else {
        info!(
            "[notebook-sync] Client disconnected from room {} ({} peer{} remaining)",
            notebook_id,
            remaining,
            if remaining == 1 { "" } else { "s" }
        );
    }

    result
}

/// Typed frames sync loop with first-byte type indicator.
///
/// Handles both Automerge sync messages and NotebookRequest messages.
/// This protocol supports daemon-owned kernel execution (Phase 8).
async fn run_sync_loop_v2<R, W>(
    reader: &mut R,
    writer: &mut W,
    room: &NotebookRoom,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut peer_state = sync::State::new();
    let mut changed_rx = room.changed_tx.subscribe();
    let mut kernel_broadcast_rx = room.kernel_broadcast_tx.subscribe();

    // Phase 1: Initial sync — server sends first (typed frame)
    {
        let mut doc = room.doc.write().await;
        if let Some(msg) = doc.generate_sync_message(&mut peer_state) {
            connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &msg.encode())
                .await?;
        }
    }

    // Phase 1.5: Send comm state sync for widget reconstruction
    // New clients need active comm channels to render widgets created before they connected
    {
        let comms = room.comm_state.get_all().await;
        if !comms.is_empty() {
            info!(
                "[notebook-sync] Sending comm_sync with {} active comms",
                comms.len()
            );
            connection::send_typed_json_frame(
                writer,
                NotebookFrameType::Broadcast,
                &NotebookBroadcast::CommSync { comms },
            )
            .await?;
        }
    }

    // Phase 2: Exchange messages until sync is complete, then watch for changes
    loop {
        tokio::select! {
            // Incoming message from this client
            result = connection::recv_typed_frame(reader) => {
                match result? {
                    Some(frame) => {
                        match frame.frame_type {
                            NotebookFrameType::AutomergeSync => {
                                // Handle Automerge sync message
                                let message = sync::Message::decode(&frame.payload)
                                    .map_err(|e| anyhow::anyhow!("decode error: {}", e))?;

                                // Serialize bytes inside the lock, then persist outside it
                                let persist_bytes = {
                                    let mut doc = room.doc.write().await;
                                    doc.receive_sync_message(&mut peer_state, message)?;

                                    let bytes = doc.save();

                                    // Notify other peers in this room
                                    let _ = room.changed_tx.send(());

                                    // Send our response while still holding the lock
                                    if let Some(reply) = doc.generate_sync_message(&mut peer_state) {
                                        connection::send_typed_frame(
                                            writer,
                                            NotebookFrameType::AutomergeSync,
                                            &reply.encode(),
                                        )
                                        .await?;
                                    }

                                    bytes
                                };

                                // Send to debounced persistence task
                                let _ = room.persist_tx.send(Some(persist_bytes));

                                // Check if metadata changed and kernel is running - broadcast sync state
                                check_and_broadcast_sync_state(room).await;
                            }

                            NotebookFrameType::Request => {
                                // Handle NotebookRequest
                                let request: NotebookRequest = serde_json::from_slice(&frame.payload)?;
                                let response =
                                    handle_notebook_request(room, request, daemon.clone()).await;
                                connection::send_typed_json_frame(
                                    writer,
                                    NotebookFrameType::Response,
                                    &response,
                                )
                                .await?;
                            }

                            NotebookFrameType::Response | NotebookFrameType::Broadcast => {
                                // Clients shouldn't send these
                                warn!(
                                    "[notebook-sync] Unexpected frame type from client: {:?}",
                                    frame.frame_type
                                );
                            }
                        }
                    }
                    None => {
                        // Client disconnected
                        return Ok(());
                    }
                }
            }

            // Another peer changed the document — push update to this client
            _ = changed_rx.recv() => {
                let mut doc = room.doc.write().await;
                if let Some(msg) = doc.generate_sync_message(&mut peer_state) {
                    connection::send_typed_frame(
                        writer,
                        NotebookFrameType::AutomergeSync,
                        &msg.encode(),
                    )
                    .await?;
                }
            }

            // Kernel broadcast event — forward to this client
            Ok(broadcast) = kernel_broadcast_rx.recv() => {
                connection::send_typed_json_frame(
                    writer,
                    NotebookFrameType::Broadcast,
                    &broadcast,
                )
                .await?;
            }
        }
    }
}

/// Acquire a pooled environment from the appropriate pool based on env_source.
/// Returns None and broadcasts error if pool is empty.
async fn acquire_pool_env_for_source(
    env_source: &str,
    daemon: &std::sync::Arc<crate::daemon::Daemon>,
    room: &NotebookRoom,
) -> Option<Option<crate::PooledEnv>> {
    // Route to appropriate pool based on source prefix
    if env_source.starts_with("conda:") {
        match daemon.take_conda_env().await {
            Some(env) => {
                info!(
                    "[notebook-sync] Acquired Conda env from pool: {:?}",
                    env.python_path
                );
                Some(Some(env))
            }
            None => {
                error!("[notebook-sync] Conda pool empty, cannot launch");
                let _ = room
                    .kernel_broadcast_tx
                    .send(NotebookBroadcast::KernelStatus {
                        status: "error: Conda pool empty".to_string(),
                        cell_id: None,
                    });
                None // Signal caller to return early
            }
        }
    } else {
        // UV pool for uv:* sources and as default
        match daemon.take_uv_env().await {
            Some(env) => {
                info!(
                    "[notebook-sync] Acquired UV env from pool: {:?}",
                    env.python_path
                );
                Some(Some(env))
            }
            None => {
                error!("[notebook-sync] UV pool empty, cannot launch");
                let _ = room
                    .kernel_broadcast_tx
                    .send(NotebookBroadcast::KernelStatus {
                        status: "error: UV pool empty".to_string(),
                        cell_id: None,
                    });
                None // Signal caller to return early
            }
        }
    }
}

/// Check if a notebook_id is a UUID (untitled/unsaved notebook).
fn is_untitled_notebook(notebook_id: &str) -> bool {
    uuid::Uuid::parse_str(notebook_id).is_ok()
}

/// Auto-launch kernel for a trusted notebook when first peer connects.
/// This is similar to handle_notebook_request(LaunchKernel) but without a request/response.
///
/// Resolves the metadata snapshot from the Automerge doc (if the first client has
/// already synced) or falls back to reading the .ipynb from disk.
async fn auto_launch_kernel(
    room: &NotebookRoom,
    notebook_id: &str,
    default_runtime: crate::runtime::Runtime,
    default_python_env: crate::settings_doc::PythonEnvType,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
) {
    // Check if room still has peers (protect against race condition where client disconnects
    // before we finish launching)
    if room.active_peers.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        debug!("[notebook-sync] Auto-launch aborted: no peers remaining");
        return;
    }

    // notebook_path_opt is ONLY for saved notebooks (actual file paths).
    // For untitled notebooks, kernel_manager will use default_notebooks_dir() for CWD.
    let notebook_path = PathBuf::from(notebook_id);
    let notebook_path_opt = if notebook_path.exists() {
        Some(notebook_path.clone())
    } else {
        None
    };

    // For untitled notebooks, get working_dir separately for project file detection.
    // This is NOT passed to kernel.launch() - that would cause .parent() to give wrong CWD.
    let working_dir_for_detection = if is_untitled_notebook(notebook_id) {
        let working_dir = room.working_dir.read().await;
        working_dir.clone().inspect(|p| {
            info!(
                "[notebook-sync] Using working_dir for untitled notebook project detection: {}",
                p.display()
            );
        })
    } else {
        None
    };

    // Resolve metadata snapshot: try Automerge doc first, fall back to disk
    let metadata_snapshot = resolve_metadata_snapshot(room, notebook_path_opt.as_deref()).await;

    let mut kernel_guard = room.kernel.lock().await;

    // Double-check no kernel is already running
    if let Some(ref kernel) = *kernel_guard {
        if kernel.is_running() {
            debug!("[notebook-sync] Auto-launch skipped: kernel already running");
            return;
        }
    }

    // Re-check peers after acquiring lock (another race check)
    if room.active_peers.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        debug!("[notebook-sync] Auto-launch aborted: no peers (after lock)");
        return;
    }

    // Clear any stale comm state from a previous kernel (in case it crashed)
    room.comm_state.clear().await;

    // Create new kernel
    let mut kernel = RoomKernel::new(
        room.kernel_broadcast_tx.clone(),
        room.doc.clone(),
        room.persist_tx.clone(),
        room.changed_tx.clone(),
        room.blob_store.clone(),
        room.comm_state.clone(),
    );

    // Detection priority:
    // 1. Notebook's kernelspec (for existing notebooks) - determines python vs deno
    // 2. For Python: resolve environment (inline deps → project files → prewarmed)
    // 3. For Deno: just launch Deno (no env resolution needed)
    // 4. For new notebooks (no kernelspec): use default_runtime setting

    // Step 1: Detect kernel type from metadata snapshot
    let notebook_kernel_type = metadata_snapshot
        .as_ref()
        .and_then(detect_notebook_kernel_type);

    // Step 2: Check inline deps (for environment source, and runt.deno override)
    let inline_source = metadata_snapshot.as_ref().and_then(check_inline_deps);

    // Step 3: Check project files (for Python environment resolution)
    // Use notebook path for saved notebooks, or working_dir for untitled notebooks
    let detection_path = notebook_path_opt
        .as_ref()
        .or(working_dir_for_detection.as_ref());
    let project_source = detection_path
        .and_then(|path| crate::project_file::detect_project_file(path))
        .map(|detected| {
            info!(
                "[notebook-sync] Auto-launch: detected project file {:?} -> {}",
                detected.path,
                detected.to_env_source()
            );
            detected.to_env_source().to_string()
        });

    // Determine kernel type and environment
    let (kernel_type, env_source, pooled_env) = match notebook_kernel_type.as_deref() {
        Some("deno") => {
            // Notebook is a Deno notebook (per its kernelspec)
            info!("[notebook-sync] Auto-launch: Deno kernel (notebook kernelspec)");
            ("deno", "deno".to_string(), None)
        }
        Some("python") => {
            // Notebook is a Python notebook - resolve environment
            let env_source = if let Some(ref source) = inline_source {
                // Skip "deno" inline source for Python notebooks (kernelspec takes priority)
                if source != "deno" {
                    info!(
                        "[notebook-sync] Auto-launch: found inline deps -> {}",
                        source
                    );
                    source.clone()
                } else if let Some(ref proj) = project_source {
                    info!(
                        "[notebook-sync] Auto-launch: using project file -> {}",
                        proj
                    );
                    proj.clone()
                } else {
                    let prewarmed = match default_python_env {
                        crate::settings_doc::PythonEnvType::Conda => "conda:prewarmed",
                        _ => "uv:prewarmed",
                    };
                    prewarmed.to_string()
                }
            } else if let Some(ref source) = project_source {
                info!(
                    "[notebook-sync] Auto-launch: using project file -> {}",
                    source
                );
                source.clone()
            } else {
                let prewarmed = match default_python_env {
                    crate::settings_doc::PythonEnvType::Conda => "conda:prewarmed",
                    _ => "uv:prewarmed",
                };
                info!(
                    "[notebook-sync] Auto-launch: using prewarmed ({})",
                    prewarmed
                );
                prewarmed.to_string()
            };
            // For uv:inline, uv:pyproject, and conda:inline we don't need a pooled env -
            // these sources prepare their own environments
            let pooled_env = if env_source == "uv:pyproject"
                || env_source == "uv:inline"
                || env_source == "conda:inline"
            {
                info!(
                    "[notebook-sync] Auto-launch: {} prepares its own env, no pool env needed",
                    env_source
                );
                None
            } else {
                match acquire_pool_env_for_source(&env_source, &daemon, room).await {
                    Some(env) => env,
                    None => return, // Error already broadcast
                }
            };
            ("python", env_source, pooled_env)
        }
        None => {
            // New notebook or unknown kernelspec - use default_runtime
            if inline_source.as_deref() == Some("deno") {
                // runt.deno config present
                info!("[notebook-sync] Auto-launch: Deno kernel (runt.deno config)");
                ("deno", "deno".to_string(), None)
            } else if matches!(default_runtime, crate::runtime::Runtime::Deno) {
                // User's default is Deno
                info!("[notebook-sync] Auto-launch: Deno kernel (default runtime)");
                ("deno", "deno".to_string(), None)
            } else {
                // Default to Python
                let env_source = if let Some(ref source) = inline_source {
                    info!(
                        "[notebook-sync] Auto-launch: found inline deps -> {}",
                        source
                    );
                    source.clone()
                } else if let Some(ref source) = project_source {
                    info!(
                        "[notebook-sync] Auto-launch: using project file -> {}",
                        source
                    );
                    source.clone()
                } else {
                    let prewarmed = match default_python_env {
                        crate::settings_doc::PythonEnvType::Conda => "conda:prewarmed",
                        _ => "uv:prewarmed",
                    };
                    info!(
                        "[notebook-sync] Auto-launch: using prewarmed ({})",
                        prewarmed
                    );
                    prewarmed.to_string()
                };
                // For uv:inline, uv:pyproject, and conda:inline we don't need a pooled env -
                // these sources prepare their own environments
                let pooled_env = if env_source == "uv:pyproject"
                    || env_source == "uv:inline"
                    || env_source == "conda:inline"
                {
                    info!(
                        "[notebook-sync] Auto-launch: {} prepares its own env, no pool env needed",
                        env_source
                    );
                    None
                } else {
                    match acquire_pool_env_for_source(&env_source, &daemon, room).await {
                        Some(env) => env,
                        None => return, // Error already broadcast
                    }
                };
                ("python", env_source, pooled_env)
            }
        }
        Some(other) => {
            // Unknown kernel type - default to Python
            warn!(
                "[notebook-sync] Unknown kernel type '{}', defaulting to Python",
                other
            );
            let prewarmed = match default_python_env {
                crate::settings_doc::PythonEnvType::Conda => "conda:prewarmed",
                _ => "uv:prewarmed",
            };
            let pooled_env = match acquire_pool_env_for_source(prewarmed, &daemon, room).await {
                Some(env) => env,
                None => return,
            };
            ("python", prewarmed.to_string(), pooled_env)
        }
    };

    // For inline deps, prepare a cached environment with rich progress
    let progress_handler: std::sync::Arc<dyn kernel_env::ProgressHandler> = std::sync::Arc::new(
        crate::inline_env::BroadcastProgressHandler::new(room.kernel_broadcast_tx.clone()),
    );

    let (pooled_env, inline_deps) = if env_source == "uv:inline" {
        if let Some(deps) = metadata_snapshot.as_ref().and_then(get_inline_uv_deps) {
            info!(
                "[notebook-sync] Preparing cached UV env for inline deps: {:?}",
                deps
            );
            match crate::inline_env::prepare_uv_inline_env(&deps, progress_handler.clone()).await {
                Ok(prepared) => {
                    info!(
                        "[notebook-sync] Using cached inline env at {:?}",
                        prepared.python_path
                    );
                    let env = Some(crate::PooledEnv {
                        env_type: crate::EnvType::Uv,
                        venv_path: prepared.env_path,
                        python_path: prepared.python_path,
                    });
                    (env, Some(deps))
                }
                Err(e) => {
                    error!("[notebook-sync] Failed to prepare inline env: {}", e);
                    let _ = room
                        .kernel_broadcast_tx
                        .send(NotebookBroadcast::KernelStatus {
                            status: format!("error: Failed to prepare environment: {}", e),
                            cell_id: None,
                        });
                    return;
                }
            }
        } else {
            (pooled_env, None)
        }
    } else if env_source == "conda:inline" {
        if let Some(deps) = metadata_snapshot.as_ref().and_then(get_inline_conda_deps) {
            let channels = metadata_snapshot
                .as_ref()
                .map(get_inline_conda_channels)
                .unwrap_or_else(|| vec!["conda-forge".to_string()]);
            info!(
                "[notebook-sync] Preparing cached Conda env for inline deps: {:?} (channels: {:?})",
                deps, channels
            );
            match crate::inline_env::prepare_conda_inline_env(
                &deps,
                &channels,
                progress_handler.clone(),
            )
            .await
            {
                Ok(prepared) => {
                    info!(
                        "[notebook-sync] Using cached conda inline env at {:?}",
                        prepared.python_path
                    );
                    let env = Some(crate::PooledEnv {
                        env_type: crate::EnvType::Conda,
                        venv_path: prepared.env_path,
                        python_path: prepared.python_path,
                    });
                    (env, Some(deps))
                }
                Err(e) => {
                    error!("[notebook-sync] Failed to prepare conda inline env: {}", e);
                    let _ = room
                        .kernel_broadcast_tx
                        .send(NotebookBroadcast::KernelStatus {
                            status: format!("error: Failed to prepare conda environment: {}", e),
                            cell_id: None,
                        });
                    return;
                }
            }
        } else {
            (pooled_env, None)
        }
    } else {
        (pooled_env, None)
    };

    // Build LaunchedEnvConfig to track what config the kernel was launched with
    let venv_path = pooled_env.as_ref().map(|e| e.venv_path.clone());
    let python_path = pooled_env.as_ref().map(|e| e.python_path.clone());
    let launched_config = build_launched_config(
        kernel_type,
        &env_source,
        inline_deps.as_deref(),
        metadata_snapshot.as_ref(),
        venv_path,
        python_path,
    );

    match kernel
        .launch(
            kernel_type,
            &env_source,
            notebook_path_opt.as_deref(),
            pooled_env,
            launched_config,
        )
        .await
    {
        Ok(()) => {
            let kt = kernel.kernel_type().to_string();
            let es = kernel.env_source().to_string();

            // Take the command receiver and spawn a task to process execution events
            if let Some(mut cmd_rx) = kernel.take_command_rx() {
                let room_kernel = room.kernel.clone();
                tokio::spawn(async move {
                    use crate::kernel_manager::QueueCommand;
                    while let Some(cmd) = cmd_rx.recv().await {
                        match cmd {
                            QueueCommand::ExecutionDone { cell_id } => {
                                debug!("[notebook-sync] ExecutionDone for {}", cell_id);
                                let mut guard = room_kernel.lock().await;
                                if let Some(ref mut k) = *guard {
                                    if let Err(e) = k.execution_done(&cell_id).await {
                                        warn!("[notebook-sync] execution_done error: {}", e);
                                    }
                                }
                            }
                            QueueCommand::CellError { cell_id } => {
                                warn!("[notebook-sync] Cell error (stop-on-error): {}", cell_id);
                            }
                        }
                    }
                });
            }

            *kernel_guard = Some(kernel);

            // Broadcast kernel status to all connected peers
            let _ = room
                .kernel_broadcast_tx
                .send(NotebookBroadcast::KernelStatus {
                    status: "idle".to_string(),
                    cell_id: None,
                });

            info!(
                "[notebook-sync] Auto-launch succeeded: {} kernel with {} environment",
                kt, es
            );
        }
        Err(e) => {
            warn!("[notebook-sync] Auto-launch failed: {}", e);
            // Broadcast error to connected peers
            let _ = room
                .kernel_broadcast_tx
                .send(NotebookBroadcast::KernelStatus {
                    status: format!("error: {}", e),
                    cell_id: None,
                });
        }
    }
}

/// Handle a NotebookRequest and return a NotebookResponse.
async fn handle_notebook_request(
    room: &NotebookRoom,
    request: NotebookRequest,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
) -> NotebookResponse {
    debug!("[notebook-sync] Handling request: {:?}", request);

    match request {
        NotebookRequest::LaunchKernel {
            kernel_type,
            env_source,
            notebook_path,
        } => {
            let mut kernel_guard = room.kernel.lock().await;

            // Check if kernel already running
            if let Some(ref kernel) = *kernel_guard {
                if kernel.is_running() {
                    return NotebookResponse::KernelAlreadyRunning {
                        kernel_type: kernel.kernel_type().to_string(),
                        env_source: kernel.env_source().to_string(),
                        launched_config: kernel.launched_config().clone(),
                    };
                }
            }

            // Clear any stale comm state from a previous kernel (in case it crashed)
            room.comm_state.clear().await;

            // Create new kernel
            let mut kernel = RoomKernel::new(
                room.kernel_broadcast_tx.clone(),
                room.doc.clone(),
                room.persist_tx.clone(),
                room.changed_tx.clone(),
                room.blob_store.clone(),
                room.comm_state.clone(),
            );
            let notebook_path = notebook_path.map(std::path::PathBuf::from);

            // Resolve metadata snapshot from Automerge doc (preferred) or disk
            let metadata_snapshot = resolve_metadata_snapshot(room, notebook_path.as_deref()).await;

            // Auto-detect kernel type if "auto" or empty
            let resolved_kernel_type = if kernel_type == "auto" || kernel_type.is_empty() {
                metadata_snapshot
                    .as_ref()
                    .and_then(detect_notebook_kernel_type)
                    .unwrap_or_else(|| {
                        info!("[notebook-sync] LaunchKernel: kernel type unknown, defaulting to python");
                        "python".to_string()
                    })
            } else {
                kernel_type.clone()
            };
            info!(
                "[notebook-sync] LaunchKernel: resolved kernel_type='{}' (from '{}')",
                resolved_kernel_type, kernel_type
            );

            // Deno kernels don't use Python environments - always use "deno" regardless
            // of what env_source was requested. Log a warning if caller passed a Python env.
            let resolved_env_source = if resolved_kernel_type == "deno" {
                if !env_source.is_empty()
                    && env_source != "auto"
                    && env_source != "deno"
                    && env_source != "prewarmed"
                {
                    warn!(
                        "[notebook-sync] Deno kernel requested with Python env_source '{}' - \
                         ignoring and using 'deno' instead",
                        env_source
                    );
                } else {
                    info!("[notebook-sync] Deno kernel detected, using 'deno' env_source");
                }
                "deno".to_string()
            } else if env_source == "auto" || env_source.is_empty() || env_source == "prewarmed" {
                // Auto-detect Python environment
                // Priority 1: Check inline deps in notebook metadata
                if let Some(inline_source) = metadata_snapshot.as_ref().and_then(check_inline_deps)
                {
                    info!(
                        "[notebook-sync] Found inline deps in notebook metadata -> {}",
                        inline_source
                    );
                    inline_source
                }
                // Priority 2: Detect project files near notebook path
                else if let Some(detected) = notebook_path
                    .as_ref()
                    .and_then(|path| crate::project_file::detect_project_file(path))
                {
                    info!(
                        "[notebook-sync] Auto-detected project file: {:?} -> {}",
                        detected.path,
                        detected.to_env_source()
                    );
                    detected.to_env_source().to_string()
                }
                // Priority 3: Fall back to prewarmed
                else {
                    info!("[notebook-sync] No project file detected, using prewarmed");
                    "uv:prewarmed".to_string()
                }
            } else {
                // Use explicit env_source (e.g., "uv:inline", "conda:inline")
                env_source.clone()
            };

            // Deno kernels don't need pooled environments
            let pooled_env = if resolved_kernel_type == "deno" {
                info!("[notebook-sync] LaunchKernel: Deno kernel (no pooled env)");
                None
            } else {
                // Python kernels require pooled environment
                match resolved_env_source.as_str() {
                    "uv:prewarmed" => match daemon.take_uv_env().await {
                        Some(env) => {
                            info!(
                                "[notebook-sync] LaunchKernel: acquired UV env from pool: {:?}",
                                env.python_path
                            );
                            Some(env)
                        }
                        None => {
                            return NotebookResponse::Error {
                                error: "UV pool empty - no environment available".to_string(),
                            };
                        }
                    },
                    "conda:prewarmed" => match daemon.take_conda_env().await {
                        Some(env) => {
                            info!(
                                "[notebook-sync] LaunchKernel: acquired Conda env from pool: {:?}",
                                env.python_path
                            );
                            Some(env)
                        }
                        None => {
                            return NotebookResponse::Error {
                                error: "Conda pool empty - no environment available".to_string(),
                            };
                        }
                    },
                    "uv:pyproject" | "uv:inline" | "conda:inline" => {
                        // These sources prepare their own environments, no pooled env needed
                        info!(
                            "[notebook-sync] LaunchKernel: {} prepares its own env, no pool env",
                            resolved_env_source
                        );
                        None
                    }
                    other => {
                        // For remaining conda sources, route to conda pool
                        if other.starts_with("conda:") {
                            match daemon.take_conda_env().await {
                                Some(env) => Some(env),
                                None => {
                                    return NotebookResponse::Error {
                                        error: "Conda pool empty".to_string(),
                                    };
                                }
                            }
                        } else {
                            // Prewarmed UV
                            match daemon.take_uv_env().await {
                                Some(env) => Some(env),
                                None => {
                                    return NotebookResponse::Error {
                                        error: "UV pool empty".to_string(),
                                    };
                                }
                            }
                        }
                    }
                }
            };

            // For inline deps, prepare a cached environment with rich progress
            let launch_progress_handler: std::sync::Arc<dyn kernel_env::ProgressHandler> =
                std::sync::Arc::new(crate::inline_env::BroadcastProgressHandler::new(
                    room.kernel_broadcast_tx.clone(),
                ));

            let (pooled_env, inline_deps) = if resolved_env_source == "uv:inline" {
                if let Some(deps) = metadata_snapshot.as_ref().and_then(get_inline_uv_deps) {
                    info!(
                        "[notebook-sync] LaunchKernel: Preparing cached UV env for inline deps: {:?}",
                        deps
                    );
                    match crate::inline_env::prepare_uv_inline_env(
                        &deps,
                        launch_progress_handler.clone(),
                    )
                    .await
                    {
                        Ok(prepared) => {
                            info!(
                                "[notebook-sync] LaunchKernel: Using cached inline env at {:?}",
                                prepared.python_path
                            );
                            let env = Some(crate::PooledEnv {
                                env_type: crate::EnvType::Uv,
                                venv_path: prepared.env_path,
                                python_path: prepared.python_path,
                            });
                            (env, Some(deps))
                        }
                        Err(e) => {
                            return NotebookResponse::Error {
                                error: format!("Failed to prepare inline environment: {}", e),
                            };
                        }
                    }
                } else {
                    (pooled_env, None)
                }
            } else if resolved_env_source == "conda:inline" {
                if let Some(deps) = metadata_snapshot.as_ref().and_then(get_inline_conda_deps) {
                    let channels = metadata_snapshot
                        .as_ref()
                        .map(get_inline_conda_channels)
                        .unwrap_or_else(|| vec!["conda-forge".to_string()]);
                    info!(
                        "[notebook-sync] LaunchKernel: Preparing cached Conda env for inline deps: {:?} (channels: {:?})",
                        deps, channels
                    );
                    match crate::inline_env::prepare_conda_inline_env(
                        &deps,
                        &channels,
                        launch_progress_handler.clone(),
                    )
                    .await
                    {
                        Ok(prepared) => {
                            info!(
                                "[notebook-sync] LaunchKernel: Using cached conda inline env at {:?}",
                                prepared.python_path
                            );
                            let env = Some(crate::PooledEnv {
                                env_type: crate::EnvType::Conda,
                                venv_path: prepared.env_path,
                                python_path: prepared.python_path,
                            });
                            (env, Some(deps))
                        }
                        Err(e) => {
                            return NotebookResponse::Error {
                                error: format!("Failed to prepare conda inline environment: {}", e),
                            };
                        }
                    }
                } else {
                    (pooled_env, None)
                }
            } else {
                (pooled_env, None)
            };

            // Build LaunchedEnvConfig to track what config the kernel was launched with
            let venv_path = pooled_env.as_ref().map(|e| e.venv_path.clone());
            let python_path = pooled_env.as_ref().map(|e| e.python_path.clone());
            let launched_config = build_launched_config(
                &resolved_kernel_type,
                &resolved_env_source,
                inline_deps.as_deref(),
                metadata_snapshot.as_ref(),
                venv_path,
                python_path,
            );

            match kernel
                .launch(
                    &resolved_kernel_type,
                    &resolved_env_source,
                    notebook_path.as_deref(),
                    pooled_env,
                    launched_config.clone(),
                )
                .await
            {
                Ok(()) => {
                    let kt = kernel.kernel_type().to_string();
                    let es = kernel.env_source().to_string();

                    // Take the command receiver and spawn a task to process execution events
                    if let Some(mut cmd_rx) = kernel.take_command_rx() {
                        let room_kernel = room.kernel.clone();
                        tokio::spawn(async move {
                            use crate::kernel_manager::QueueCommand;
                            while let Some(cmd) = cmd_rx.recv().await {
                                match cmd {
                                    QueueCommand::ExecutionDone { cell_id } => {
                                        info!(
                                            "[notebook-sync] Processing ExecutionDone for {}",
                                            cell_id
                                        );
                                        let mut guard = room_kernel.lock().await;
                                        if let Some(ref mut k) = *guard {
                                            if let Err(e) = k.execution_done(&cell_id).await {
                                                warn!(
                                                    "[notebook-sync] execution_done error: {}",
                                                    e
                                                );
                                            }
                                        }
                                    }
                                    QueueCommand::CellError { cell_id } => {
                                        warn!(
                                            "[notebook-sync] Cell error (stop-on-error): {}",
                                            cell_id
                                        );
                                        // Clear the queue to stop execution on error
                                        let mut guard = room_kernel.lock().await;
                                        if let Some(ref mut k) = *guard {
                                            let cleared = k.clear_queue();
                                            if !cleared.is_empty() {
                                                info!(
                                                    "[notebook-sync] Cleared {} queued cells due to error",
                                                    cleared.len()
                                                );
                                            }
                                        }
                                    }
                                }
                            }
                            info!(
                                "[notebook-sync] Command receiver closed, kernel likely shutdown"
                            );
                        });
                    }

                    *kernel_guard = Some(kernel);
                    NotebookResponse::KernelLaunched {
                        kernel_type: kt,
                        env_source: es,
                        launched_config,
                    }
                }
                Err(e) => NotebookResponse::Error {
                    error: format!("Failed to launch kernel: {}", e),
                },
            }
        }

        #[allow(deprecated)]
        NotebookRequest::QueueCell { cell_id, code } => {
            let mut kernel_guard = room.kernel.lock().await;
            if let Some(ref mut kernel) = *kernel_guard {
                match kernel.queue_cell(cell_id.clone(), code).await {
                    Ok(()) => NotebookResponse::CellQueued { cell_id },
                    Err(e) => NotebookResponse::Error {
                        error: format!("Failed to queue cell: {}", e),
                    },
                }
            } else {
                NotebookResponse::NoKernel {}
            }
        }

        NotebookRequest::ExecuteCell { cell_id } => {
            // Read cell source FIRST (before kernel lock) to avoid holding
            // kernel mutex while waiting on doc lock
            let (source, cell_type) = {
                let doc = room.doc.read().await;
                match doc.get_cell(&cell_id) {
                    Some(c) => (c.source, c.cell_type),
                    None => {
                        return NotebookResponse::Error {
                            error: format!("Cell not found in document: {}", cell_id),
                        };
                    }
                }
            }; // doc lock released here

            // Only execute code cells
            if cell_type != "code" {
                return NotebookResponse::Error {
                    error: format!(
                        "Cannot execute non-code cell: {} (type: {})",
                        cell_id, cell_type
                    ),
                };
            }

            // Format before execution (best-effort, non-blocking on failure)
            let source = if let Some(runtime) = detect_room_runtime(room).await {
                if let Some(formatted) = format_source(&source, &runtime).await {
                    // Write formatted source back to the Automerge doc
                    let mut doc = room.doc.write().await;
                    if doc.update_source(&cell_id, &formatted).is_ok() {
                        let _ = room.changed_tx.send(());
                        debug!("[format] Formatted cell {} before execution", cell_id);
                    }
                    formatted
                } else {
                    source
                }
            } else {
                source
            };

            // NOW lock kernel for the queue operation
            let mut kernel_guard = room.kernel.lock().await;
            if let Some(ref mut kernel) = *kernel_guard {
                match kernel.queue_cell(cell_id.clone(), source).await {
                    Ok(()) => NotebookResponse::CellQueued { cell_id },
                    Err(e) => NotebookResponse::Error {
                        error: format!("Failed to queue cell: {}", e),
                    },
                }
            } else {
                NotebookResponse::NoKernel {}
            }
        }

        NotebookRequest::ClearOutputs { cell_id } => {
            // 1. Mutate the Automerge document to remove outputs
            let persist_bytes = {
                let mut doc = room.doc.write().await;
                if let Err(e) = doc.clear_outputs(&cell_id) {
                    return NotebookResponse::Error {
                        error: format!("Failed to clear outputs: {}", e),
                    };
                }
                // Also reset execution count
                let _ = doc.set_execution_count(&cell_id, "null");
                let bytes = doc.save();
                // Notify other peers of doc change
                let _ = room.changed_tx.send(());
                bytes
            };

            // 2. Send to debounced persistence task
            let _ = room.persist_tx.send(Some(persist_bytes));

            // 3. Broadcast for cross-window UI sync (fast path)
            let _ = room
                .kernel_broadcast_tx
                .send(NotebookBroadcast::OutputsCleared {
                    cell_id: cell_id.clone(),
                });

            // 4. Update kernel's internal tracking if kernel exists
            let kernel_guard = room.kernel.lock().await;
            if let Some(ref kernel) = *kernel_guard {
                kernel.clear_outputs(&cell_id).await;
            }

            NotebookResponse::OutputsCleared { cell_id }
        }

        NotebookRequest::InterruptExecution {} => {
            let mut kernel_guard = room.kernel.lock().await;
            if let Some(ref mut kernel) = *kernel_guard {
                match kernel.interrupt().await {
                    Ok(()) => NotebookResponse::InterruptSent {},
                    Err(e) => NotebookResponse::Error {
                        error: format!("Failed to interrupt: {}", e),
                    },
                }
            } else {
                NotebookResponse::NoKernel {}
            }
        }

        NotebookRequest::ShutdownKernel {} => {
            let mut kernel_guard = room.kernel.lock().await;
            if let Some(ref mut kernel) = *kernel_guard {
                match kernel.shutdown().await {
                    Ok(()) => {
                        *kernel_guard = None;
                        // Clear comm state - all widgets become invalid when kernel shuts down
                        room.comm_state.clear().await;
                        NotebookResponse::KernelShuttingDown {}
                    }
                    Err(e) => NotebookResponse::Error {
                        error: format!("Failed to shutdown kernel: {}", e),
                    },
                }
            } else {
                NotebookResponse::NoKernel {}
            }
        }

        NotebookRequest::GetKernelInfo {} => {
            let kernel_guard = room.kernel.lock().await;
            if let Some(ref kernel) = *kernel_guard {
                if kernel.is_running() {
                    NotebookResponse::KernelInfo {
                        kernel_type: Some(kernel.kernel_type().to_string()),
                        env_source: Some(kernel.env_source().to_string()),
                        status: kernel.status().to_string(),
                    }
                } else {
                    NotebookResponse::KernelInfo {
                        kernel_type: None,
                        env_source: None,
                        status: "not_started".to_string(),
                    }
                }
            } else {
                NotebookResponse::KernelInfo {
                    kernel_type: None,
                    env_source: None,
                    status: "not_started".to_string(),
                }
            }
        }

        NotebookRequest::GetQueueState {} => {
            let kernel_guard = room.kernel.lock().await;
            if let Some(ref kernel) = *kernel_guard {
                NotebookResponse::QueueState {
                    executing: kernel.executing_cell().cloned(),
                    queued: kernel.queued_cells(),
                }
            } else {
                NotebookResponse::QueueState {
                    executing: None,
                    queued: vec![],
                }
            }
        }

        NotebookRequest::RunAllCells {} => {
            let mut kernel_guard = room.kernel.lock().await;
            if let Some(ref mut kernel) = *kernel_guard {
                // Read all cells from the synced Automerge document
                let doc = room.doc.read().await;
                let cells = doc.get_cells();

                // Queue all code cells in document order
                let mut count = 0;
                for cell in cells {
                    if cell.cell_type == "code" {
                        if let Err(e) = kernel
                            .queue_cell(cell.id.clone(), cell.source.clone())
                            .await
                        {
                            return NotebookResponse::Error {
                                error: format!("Failed to queue cell {}: {}", cell.id, e),
                            };
                        }
                        count += 1;
                    }
                }

                NotebookResponse::AllCellsQueued { count }
            } else {
                NotebookResponse::NoKernel {}
            }
        }

        NotebookRequest::SendComm { message } => {
            let mut kernel_guard = room.kernel.lock().await;
            if let Some(ref mut kernel) = *kernel_guard {
                match kernel.send_comm_message(message).await {
                    Ok(()) => NotebookResponse::Ok {},
                    Err(e) => NotebookResponse::Error {
                        error: format!("Failed to send comm message: {}", e),
                    },
                }
            } else {
                NotebookResponse::NoKernel {}
            }
        }

        NotebookRequest::GetHistory { pattern, n, unique } => {
            let mut kernel_guard = room.kernel.lock().await;
            if let Some(ref mut kernel) = *kernel_guard {
                match kernel.get_history(pattern, n, unique).await {
                    Ok(entries) => NotebookResponse::HistoryResult { entries },
                    Err(e) => NotebookResponse::Error {
                        error: format!("Failed to get history: {}", e),
                    },
                }
            } else {
                NotebookResponse::NoKernel {}
            }
        }

        NotebookRequest::Complete { code, cursor_pos } => {
            let mut kernel_guard = room.kernel.lock().await;
            if let Some(ref mut kernel) = *kernel_guard {
                match kernel.complete(code, cursor_pos).await {
                    Ok((items, cursor_start, cursor_end)) => NotebookResponse::CompletionResult {
                        items,
                        cursor_start,
                        cursor_end,
                    },
                    Err(e) => NotebookResponse::Error {
                        error: format!("Failed to get completions: {}", e),
                    },
                }
            } else {
                NotebookResponse::NoKernel {}
            }
        }

        NotebookRequest::SaveNotebook { format_cells, path } => {
            // Format cells if requested (before saving)
            if format_cells {
                if let Err(e) = format_notebook_cells(room).await {
                    warn!("[save] Format cells failed (continuing with save): {}", e);
                }
            }

            match save_notebook_to_disk(room, path.as_deref()).await {
                Ok(saved_path) => NotebookResponse::NotebookSaved { path: saved_path },
                Err(e) => NotebookResponse::Error {
                    error: format!("Failed to save notebook: {e}"),
                },
            }
        }

        NotebookRequest::CloneNotebook { path } => {
            match clone_notebook_to_disk(room, &path).await {
                Ok(cloned_path) => NotebookResponse::NotebookCloned { path: cloned_path },
                Err(e) => NotebookResponse::Error {
                    error: format!("Failed to clone notebook: {e}"),
                },
            }
        }

        NotebookRequest::SyncEnvironment {} => handle_sync_environment(room).await,
    }
}

/// Handle sync environment request - hot-install new packages without kernel restart.
///
/// Only supported for UV inline dependencies when there are only additions (no removals).
/// Conda and other env types fall back to restart.
async fn handle_sync_environment(room: &NotebookRoom) -> NotebookResponse {
    use crate::inline_env::UvEnvironment;

    // Get kernel info, venv_path, python_path, and launch_id while holding lock, then release
    let (launched, venv_path, python_path, launch_id) = {
        let kernel_guard = room.kernel.lock().await;

        // Check if kernel is running
        let kernel = match kernel_guard.as_ref() {
            Some(k) if k.is_running() => k,
            Some(_) | None => {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: "No kernel running".to_string(),
                    needs_restart: false,
                };
            }
        };

        let launched = kernel.launched_config().clone();

        // Only UV inline deps support hot-sync
        if launched.uv_deps.is_none() {
            return NotebookResponse::SyncEnvironmentFailed {
                error: "Hot-sync only supported for UV inline dependencies".to_string(),
                needs_restart: true,
            };
        }

        let venv_path = match &launched.venv_path {
            Some(path) => path.clone(),
            None => {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: "No venv path stored - restart required".to_string(),
                    needs_restart: true,
                };
            }
        };

        let python_path = match &launched.python_path {
            Some(path) => path.clone(),
            None => {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: "No python path stored - restart required".to_string(),
                    needs_restart: true,
                };
            }
        };

        // Capture launch_id to detect if kernel is swapped during async install
        let launch_id = launched.launch_id.clone();

        (launched, venv_path, python_path, launch_id)
    };

    // Get current metadata from the document
    let current_metadata = match resolve_metadata_snapshot(room, Some(&room.notebook_path)).await {
        Some(m) => m,
        None => {
            return NotebookResponse::SyncEnvironmentFailed {
                error: "Could not read notebook metadata".to_string(),
                needs_restart: false,
            };
        }
    };

    // Compute diff
    let diff = compute_env_sync_diff(&launched, &current_metadata);

    match diff {
        None => {
            // Already in sync
            NotebookResponse::SyncEnvironmentComplete {
                synced_packages: vec![],
            }
        }
        Some(d) => {
            // Check if there are removals - require restart for those
            if !d.removed.is_empty() {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: format!(
                        "Cannot remove packages from running env: {:?}. Restart required.",
                        d.removed
                    ),
                    needs_restart: true,
                };
            }

            // Check for channel changes (conda only, but check anyway)
            if d.channels_changed {
                return NotebookResponse::SyncEnvironmentFailed {
                    error: "Channel changes require restart".to_string(),
                    needs_restart: true,
                };
            }

            // Nothing to add?
            if d.added.is_empty() {
                return NotebookResponse::SyncEnvironmentComplete {
                    synced_packages: vec![],
                };
            }

            // Send started notification
            let packages_to_install = d.added.clone();
            let _ = room
                .kernel_broadcast_tx
                .send(NotebookBroadcast::EnvProgress {
                    env_type: "uv".to_string(),
                    phase: kernel_env::progress::EnvProgressPhase::InstallingPackages {
                        packages: packages_to_install.clone(),
                    },
                });

            // Build UvEnvironment and call sync_dependencies
            let env = UvEnvironment {
                venv_path: venv_path.clone(),
                python_path: python_path.clone(),
            };

            info!(
                "[notebook-sync] Hot-syncing {} packages to {:?}",
                packages_to_install.len(),
                venv_path
            );

            match kernel_env::uv::sync_dependencies(&env, &packages_to_install).await {
                Ok(()) => {
                    info!(
                        "[notebook-sync] Hot-sync complete: {:?}",
                        packages_to_install
                    );

                    // Verify kernel wasn't swapped during async install (race protection)
                    // Update the kernel's launched config so future sync checks are accurate
                    let mut kernel_guard = room.kernel.lock().await;
                    if let Some(ref mut kernel) = *kernel_guard {
                        // Check launch_id still matches - if kernel was restarted, skip update
                        let current_launch_id = kernel.launched_config().launch_id.clone();
                        if current_launch_id != launch_id {
                            warn!(
                                "[notebook-sync] Kernel was swapped during hot-sync, skipping update"
                            );
                            // Still report success - packages were installed to the old env
                            // User will see sync banner again for the new kernel
                        } else if let Some(ref current_uv) = current_metadata.runt.uv {
                            kernel.update_launched_uv_deps(current_uv.dependencies.clone());
                        }
                    }

                    // Broadcast ready
                    let _ = room
                        .kernel_broadcast_tx
                        .send(NotebookBroadcast::EnvProgress {
                            env_type: "uv".to_string(),
                            phase: kernel_env::progress::EnvProgressPhase::Ready {
                                env_path: venv_path.to_string_lossy().to_string(),
                                python_path: env.python_path.to_string_lossy().to_string(),
                            },
                        });

                    // Broadcast that we're back in sync
                    let _ = room
                        .kernel_broadcast_tx
                        .send(NotebookBroadcast::EnvSyncState {
                            in_sync: true,
                            diff: None,
                        });

                    NotebookResponse::SyncEnvironmentComplete {
                        synced_packages: packages_to_install,
                    }
                }
                Err(e) => {
                    error!("[notebook-sync] Hot-sync failed: {}", e);

                    // Broadcast error
                    let _ = room
                        .kernel_broadcast_tx
                        .send(NotebookBroadcast::EnvProgress {
                            env_type: "uv".to_string(),
                            phase: kernel_env::progress::EnvProgressPhase::Error {
                                message: e.to_string(),
                            },
                        });

                    NotebookResponse::SyncEnvironmentFailed {
                        error: format!("Failed to install packages: {}", e),
                        needs_restart: true,
                    }
                }
            }
        }
    }
}

/// Format a single source string using ruff (Python) or deno fmt (Deno).
///
/// Returns the formatted source with trailing newline stripped (cells shouldn't
/// end with \n), or None if formatting failed or wasn't applicable.
async fn format_source(source: &str, runtime: &str) -> Option<String> {
    use kernel_launch::tools;
    use std::process::Stdio;
    use tokio::io::AsyncWriteExt;

    if source.trim().is_empty() {
        return None;
    }

    let raw = match runtime {
        "python" => {
            let ruff_path = tools::get_ruff_path().await.ok()?;
            let mut child = tokio::process::Command::new(&ruff_path)
                .args(["format", "--stdin-filename", "cell.py", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .ok()?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(source.as_bytes()).await.ok()?;
            }

            let output = child.wait_with_output().await.ok()?;
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        }
        "deno" => {
            let deno_path = tools::get_deno_path().await.ok()?;
            let mut child = tokio::process::Command::new(&deno_path)
                .args(["fmt", "--ext=ts", "-"])
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .ok()?;

            if let Some(mut stdin) = child.stdin.take() {
                stdin.write_all(source.as_bytes()).await.ok()?;
            }

            let output = child.wait_with_output().await.ok()?;
            if output.status.success() {
                String::from_utf8(output.stdout).ok()
            } else {
                None
            }
        }
        _ => None,
    }?;

    // Strip trailing newline (formatters always add one, but cells shouldn't have it)
    let formatted = raw.strip_suffix('\n').unwrap_or(&raw).to_string();

    if formatted != source {
        Some(formatted)
    } else {
        None
    }
}

/// Detect the runtime from room metadata, returning "python", "deno", or None.
async fn detect_room_runtime(room: &NotebookRoom) -> Option<String> {
    let metadata_json = {
        let doc = room.doc.read().await;
        doc.get_metadata(NOTEBOOK_METADATA_KEY)
    };
    metadata_json
        .as_ref()
        .and_then(|json| serde_json::from_str::<NotebookMetadataSnapshot>(json).ok())
        .and_then(|snapshot| detect_notebook_kernel_type(&snapshot))
}

/// Format all code cells in a notebook using ruff (Python) or deno fmt (Deno).
///
/// Reads the runtime type from the notebook metadata and formats accordingly.
/// Updates the Automerge doc with formatted sources and broadcasts changes.
/// Formatting errors are logged but don't fail the operation (best-effort).
async fn format_notebook_cells(room: &NotebookRoom) -> Result<usize, String> {
    let runtime = match detect_room_runtime(room).await {
        Some(rt) => rt,
        None => {
            info!("[format] Skipping format: unknown kernelspec (no formatter available)");
            return Ok(0);
        }
    };

    // Get all code cells
    let cells: Vec<(String, String)> = {
        let doc = room.doc.read().await;
        doc.get_cells()
            .into_iter()
            .filter(|cell| cell.cell_type == "code" && !cell.source.trim().is_empty())
            .map(|cell| (cell.id, cell.source))
            .collect()
    };

    if cells.is_empty() {
        return Ok(0);
    }

    let mut formatted_count = 0;

    for (cell_id, source) in cells {
        if let Some(formatted) = format_source(&source, &runtime).await {
            let mut doc = room.doc.write().await;
            if doc.update_source(&cell_id, &formatted).is_ok() {
                formatted_count += 1;
            }
        }
    }

    // Broadcast changes to connected peers if any cells were formatted
    if formatted_count > 0 {
        let _ = room.changed_tx.send(());
        info!(
            "[format] Formatted {} code cells (runtime: {})",
            formatted_count, runtime
        );
    }

    Ok(formatted_count)
}

/// Save the notebook from the Automerge doc to disk as .ipynb.
///
/// If `target_path` is Some, saves to that path (with .ipynb appended if needed).
/// If `target_path` is None, saves to `room.notebook_path` (original file location).
///
/// 1. Read existing .ipynb from disk (if it exists) to preserve unknown metadata
/// 2. Read cells and metadata from the Automerge doc
/// 3. Merge metadata: replace kernelspec, language_info, runt; preserve everything else
/// 4. Reconstruct cells: source and outputs from Automerge, cell metadata from existing file
/// 5. Write the merged notebook to disk
///
/// Returns the absolute path where the notebook was written.
async fn save_notebook_to_disk(
    room: &NotebookRoom,
    target_path: Option<&str>,
) -> Result<String, String> {
    // Determine the actual save path
    let notebook_path = match target_path {
        Some(p) => {
            let path = PathBuf::from(p);

            // Reject relative paths - daemon CWD is unpredictable (could be / when running as launchd)
            // Clients (Tauri file dialog, Python SDK) should always provide absolute paths.
            if path.is_relative() {
                return Err(format!(
                    "Relative paths are not supported for save: '{}'. Please provide an absolute path.",
                    p
                ));
            }

            // Ensure .ipynb extension
            if p.ends_with(".ipynb") {
                path
            } else {
                PathBuf::from(format!("{}.ipynb", p))
            }
        }
        None => room.notebook_path.clone(),
    };

    // Read existing .ipynb to preserve unknown metadata and cell metadata
    // Distinguish between file-not-found (ok, create new) and parse errors (warn, continue)
    let existing: Option<serde_json::Value> = match tokio::fs::read_to_string(&notebook_path).await
    {
        Ok(content) => match serde_json::from_str(&content) {
            Ok(value) => Some(value),
            Err(e) => {
                warn!(
                    "[notebook-sync] Existing notebook at {:?} has invalid JSON ({}), \
                     will overwrite without preserving metadata",
                    notebook_path, e
                );
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            warn!(
                "[notebook-sync] Failed to read existing notebook {:?}: {}, \
                 will create new without preserving metadata",
                notebook_path, e
            );
            None
        }
    };

    // Read cells and metadata from the Automerge doc
    let (cells, metadata_json) = {
        let doc = room.doc.read().await;
        let cells = doc.get_cells();
        let metadata_json = doc.get_metadata(NOTEBOOK_METADATA_KEY);
        (cells, metadata_json)
    };

    // Build existing cell metadata index (cell_id -> cell metadata from .ipynb)
    let existing_cell_metadata: HashMap<String, serde_json::Value> = existing
        .as_ref()
        .and_then(|nb| nb.get("cells"))
        .and_then(|c| c.as_array())
        .map(|cells_arr| {
            cells_arr
                .iter()
                .filter_map(|cell| {
                    let id = cell.get("id").and_then(|v| v.as_str())?;
                    let meta = cell
                        .get("metadata")
                        .cloned()
                        .unwrap_or(serde_json::json!({}));
                    Some((id.to_string(), meta))
                })
                .collect()
        })
        .unwrap_or_default();

    // Reconstruct cells as JSON
    let mut nb_cells = Vec::new();
    for cell in &cells {
        let cell_meta = existing_cell_metadata
            .get(&cell.id)
            .cloned()
            .unwrap_or(serde_json::json!({}));

        // Parse source into multiline array format (split_inclusive('\n'))
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
            "metadata": cell_meta,
        });

        if cell.cell_type == "code" {
            // Resolve outputs (may be manifest hashes or raw JSON)
            let mut resolved_outputs = Vec::new();
            for output_str in &cell.outputs {
                let output_value = resolve_cell_output(output_str, &room.blob_store).await;
                resolved_outputs.push(output_value);
            }
            cell_json["outputs"] = serde_json::Value::Array(resolved_outputs);

            // Parse execution_count
            let exec_count: serde_json::Value =
                serde_json::from_str(&cell.execution_count).unwrap_or(serde_json::Value::Null);
            cell_json["execution_count"] = exec_count;
        }

        nb_cells.push(cell_json);
    }

    // Build metadata by merging synced snapshot onto existing
    let mut metadata = existing
        .as_ref()
        .and_then(|nb| nb.get("metadata"))
        .cloned()
        .unwrap_or(serde_json::json!({}));

    if let Some(ref meta_json) = metadata_json {
        if let Ok(snapshot) =
            serde_json::from_str::<crate::notebook_metadata::NotebookMetadataSnapshot>(meta_json)
        {
            snapshot.merge_into_metadata_value(&mut metadata);
        }
    }

    // Build the final notebook JSON
    // Cell IDs were introduced in nbformat 4.5, so ensure minor >= 5
    let existing_minor = existing
        .as_ref()
        .and_then(|nb| nb.get("nbformat_minor"))
        .and_then(|v| v.as_u64())
        .unwrap_or(5);
    let nbformat_minor = std::cmp::max(existing_minor, 5);

    let cell_count = nb_cells.len();
    let notebook_json = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": nbformat_minor,
        "metadata": metadata,
        "cells": nb_cells,
    });

    // Serialize with trailing newline (nbformat convention)
    let content = serde_json::to_string_pretty(&notebook_json)
        .map_err(|e| format!("Failed to serialize notebook: {e}"))?;
    let content_with_newline = format!("{content}\n");

    // Write to disk (async to avoid blocking the runtime)
    tokio::fs::write(&notebook_path, content_with_newline)
        .await
        .map_err(|e| format!("Failed to write notebook: {e}"))?;

    // Update last_self_write timestamp so file watcher skips this change
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    room.last_self_write.store(now, Ordering::Relaxed);

    info!(
        "[notebook-sync] Saved notebook to disk: {:?} ({} cells)",
        notebook_path, cell_count
    );

    Ok(notebook_path.to_string_lossy().to_string())
}

/// Clone the notebook to a new path with a fresh env_id and cleared outputs.
///
/// This is used for "Save As Copy" functionality - creates a new independent notebook
/// without affecting the current document. The cloned notebook has:
/// - A fresh env_id (so it gets its own environment)
/// - All outputs cleared
/// - All execution_counts reset to null
/// - Cell metadata and attachments preserved from the source notebook
async fn clone_notebook_to_disk(room: &NotebookRoom, target_path: &str) -> Result<String, String> {
    let path = PathBuf::from(target_path);

    // Reject relative paths
    if path.is_relative() {
        return Err(format!(
            "Relative paths are not supported for clone: '{}'. Please provide an absolute path.",
            target_path
        ));
    }

    // Ensure .ipynb extension
    let notebook_path = if target_path.ends_with(".ipynb") {
        path
    } else {
        PathBuf::from(format!("{}.ipynb", target_path))
    };

    // Read cells and metadata from the Automerge doc
    let (cells, metadata_json) = {
        let doc = room.doc.read().await;
        (doc.get_cells(), doc.get_metadata(NOTEBOOK_METADATA_KEY))
    };

    // Read existing source notebook to preserve cell metadata and attachments
    // (Automerge doc doesn't store these)
    let existing: Option<serde_json::Value> =
        match tokio::fs::read_to_string(&room.notebook_path).await {
            Ok(content) => serde_json::from_str(&content).ok(),
            Err(_) => None,
        };

    // Build cell metadata/attachments index from existing notebook
    let existing_cell_data: HashMap<String, (serde_json::Value, Option<serde_json::Value>)> =
        existing
            .as_ref()
            .and_then(|nb| nb.get("cells"))
            .and_then(|c| c.as_array())
            .map(|cells_arr| {
                cells_arr
                    .iter()
                    .filter_map(|cell| {
                        let id = cell.get("id").and_then(|v| v.as_str())?;
                        let meta = cell
                            .get("metadata")
                            .cloned()
                            .unwrap_or(serde_json::json!({}));
                        let attachments = cell.get("attachments").cloned();
                        Some((id.to_string(), (meta, attachments)))
                    })
                    .collect()
            })
            .unwrap_or_default();

    // Generate fresh env_id for the cloned notebook
    let new_env_id = uuid::Uuid::new_v4().to_string();

    // Build cells with cleared outputs and execution counts, but preserved metadata
    let mut nb_cells = Vec::new();
    for cell in &cells {
        // Parse source into multiline array format using split_inclusive
        let source_lines: Vec<String> = if cell.source.is_empty() {
            vec![]
        } else {
            cell.source
                .split_inclusive('\n')
                .map(|s| s.to_string())
                .collect()
        };

        // Preserve cell metadata and attachments from existing notebook
        let (cell_meta, attachments) = existing_cell_data
            .get(&cell.id)
            .cloned()
            .unwrap_or((serde_json::json!({}), None));

        let mut cell_json = serde_json::json!({
            "id": cell.id,
            "cell_type": cell.cell_type,
            "source": source_lines,
            "metadata": cell_meta,
        });

        if cell.cell_type == "code" {
            // Clear outputs and execution_count for cloned notebook
            cell_json["outputs"] = serde_json::json!([]);
            cell_json["execution_count"] = serde_json::Value::Null;
        } else if cell.cell_type == "markdown" {
            // Preserve attachments for markdown cells (embedded images)
            if let Some(att) = attachments {
                cell_json["attachments"] = att;
            }
        }

        nb_cells.push(cell_json);
    }

    // Build metadata: start with existing notebook metadata to preserve unknown fields,
    // then apply snapshot with fresh env_id
    let mut metadata = existing
        .as_ref()
        .and_then(|nb| nb.get("metadata"))
        .cloned()
        .unwrap_or(serde_json::json!({}));

    if let Some(ref meta_json) = metadata_json {
        if let Ok(mut snapshot) =
            serde_json::from_str::<crate::notebook_metadata::NotebookMetadataSnapshot>(meta_json)
        {
            // Update env_id in the snapshot
            snapshot.runt.env_id = Some(new_env_id.clone());
            // Clear trust signature since this is a new notebook
            snapshot.runt.trust_signature = None;
            snapshot.runt.trust_timestamp = None;

            snapshot.merge_into_metadata_value(&mut metadata);
        }
    }

    // Determine nbformat_minor from existing or default to 5 (for cell IDs)
    let existing_minor = existing
        .as_ref()
        .and_then(|nb| nb.get("nbformat_minor"))
        .and_then(|v| v.as_u64())
        .unwrap_or(5);
    let nbformat_minor = std::cmp::max(existing_minor, 5);

    // Build the final notebook JSON
    let cell_count = nb_cells.len();
    let notebook_json = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": nbformat_minor,
        "metadata": metadata,
        "cells": nb_cells,
    });

    // Serialize with trailing newline
    let content = serde_json::to_string_pretty(&notebook_json)
        .map_err(|e| format!("Failed to serialize notebook: {e}"))?;
    let content_with_newline = format!("{content}\n");

    // Write to disk
    tokio::fs::write(&notebook_path, content_with_newline)
        .await
        .map_err(|e| format!("Failed to write notebook: {e}"))?;

    info!(
        "[notebook-sync] Cloned notebook to disk: {:?} ({} cells, new env_id: {})",
        notebook_path, cell_count, new_env_id
    );

    Ok(notebook_path.to_string_lossy().to_string())
}

/// Resolve a single cell output — handles both manifest hashes and raw JSON.
async fn resolve_cell_output(output_str: &str, blob_store: &BlobStore) -> serde_json::Value {
    // Check if it's a manifest hash (64-char hex string)
    if output_str.len() == 64 && output_str.chars().all(|c| c.is_ascii_hexdigit()) {
        // Try to fetch manifest from blob store
        if let Ok(Some(manifest_bytes)) = blob_store.get(output_str).await {
            if let Ok(manifest_json) = String::from_utf8(manifest_bytes) {
                // Resolve the manifest to full Jupyter output
                if let Ok(resolved) =
                    crate::output_store::resolve_manifest(&manifest_json, blob_store).await
                {
                    return resolved;
                }
            }
        }
        // If resolution fails, return empty output
        warn!(
            "[notebook-sync] Failed to resolve output manifest: {}",
            &output_str[..8]
        );
        serde_json::json!({"output_type": "stream", "name": "stderr", "text": ["[output could not be resolved]"]})
    } else {
        // Raw JSON output
        // TODO: investigate when this can happen - raw output should always be valid JSON from kernel
        match serde_json::from_str(output_str) {
            Ok(value) => value,
            Err(e) => {
                let preview = if output_str.len() > 100 {
                    format!("{}...", &output_str[..100])
                } else {
                    output_str.to_string()
                };
                warn!(
                    "[notebook-sync] Invalid JSON in raw output ({}): {}",
                    e, preview
                );
                // Return valid nbformat stream output instead of invalid {}
                serde_json::json!({
                    "output_type": "stream",
                    "name": "stderr",
                    "text": ["[invalid output JSON]"]
                })
            }
        }
    }
}

/// Configuration for the persist debouncer timing.
#[derive(Clone, Copy)]
struct PersistDebouncerConfig {
    /// How long to wait after last update before flushing (debounce window)
    debounce_ms: u64,
    /// Maximum time between flushes during continuous updates
    max_interval_ms: u64,
    /// How often to check if we should flush
    check_interval_ms: u64,
}

impl Default for PersistDebouncerConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 500,
            max_interval_ms: 5000,
            check_interval_ms: 100,
        }
    }
}

/// Spawn a debounced persistence task that coalesces writes.
///
/// Uses a `watch` channel for "latest value" semantics - new values replace old ones,
/// so we always persist the most recent state. No backpressure issues.
///
/// Persistence strategy:
/// - **Debounce (500ms)**: Wait 500ms after last update before writing
/// - **Max interval (5s)**: During continuous output, flush at least every 5s
/// - **Shutdown flush**: Persist any pending data when channel closes
///
/// This reduces disk I/O during rapid output while ensuring durability.
fn spawn_persist_debouncer(persist_rx: watch::Receiver<Option<Vec<u8>>>, persist_path: PathBuf) {
    spawn_persist_debouncer_with_config(
        persist_rx,
        persist_path,
        PersistDebouncerConfig::default(),
    );
}

/// Spawn debouncer with custom timing configuration (for testing).
fn spawn_persist_debouncer_with_config(
    mut persist_rx: watch::Receiver<Option<Vec<u8>>>,
    persist_path: PathBuf,
    config: PersistDebouncerConfig,
) {
    tokio::spawn(async move {
        use std::time::Duration;
        use tokio::time::{interval, Instant, MissedTickBehavior};

        let debounce_duration = Duration::from_millis(config.debounce_ms);
        let max_flush_interval = Duration::from_millis(config.max_interval_ms);

        let mut last_receive: Option<Instant> = None;
        let mut last_flush: Option<Instant> = None;

        // Persistent interval - fires regularly regardless of how often changed() fires.
        // This ensures we always check debounce/max-interval even during rapid updates.
        let mut check_interval = interval(Duration::from_millis(config.check_interval_ms));
        check_interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                result = persist_rx.changed() => {
                    if result.is_err() {
                        // Channel closed - flush any pending data and exit
                        let bytes = persist_rx.borrow().clone();
                        if let Some(data) = bytes {
                            do_persist(&data, &persist_path);
                        }
                        break;
                    }
                    last_receive = Some(Instant::now());
                }
                _ = check_interval.tick() => {
                    // Check if we should flush based on debounce or max interval
                    let should_flush = if let Some(recv) = last_receive {
                        // Debounce: 500ms quiet period since last receive
                        let debounce_ready = recv.elapsed() >= debounce_duration;
                        // Max interval: 5s since last flush (or since first receive)
                        let max_interval_ready = last_flush
                            .map(|f| f.elapsed() >= max_flush_interval)
                            .unwrap_or(recv.elapsed() >= max_flush_interval);
                        debounce_ready || max_interval_ready
                    } else {
                        false
                    };

                    if should_flush {
                        let bytes = persist_rx.borrow().clone();
                        if let Some(data) = bytes {
                            do_persist(&data, &persist_path);
                            last_flush = Some(Instant::now());
                            last_receive = None;
                        }
                    }
                }
            }
        }
    });
}

/// Actually persist bytes to disk, logging if it takes too long.
fn do_persist(data: &[u8], path: &Path) {
    let start = std::time::Instant::now();
    persist_notebook_bytes(data, path);
    let elapsed = start.elapsed();
    if elapsed > std::time::Duration::from_millis(500) {
        warn!(
            "[persist-debouncer] Slow write: {:?} took {:?}",
            path, elapsed
        );
    }
}

/// Persist pre-serialized notebook bytes to disk.
pub(crate) fn persist_notebook_bytes(data: &[u8], path: &Path) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            warn!(
                "[notebook-sync] Failed to create parent dir for {:?}: {}",
                path, e
            );
            return;
        }
    }
    if let Err(e) = std::fs::write(path, data) {
        warn!("[notebook-sync] Failed to save notebook doc: {}", e);
    }
}

// =============================================================================
// Notebook File Watching
// =============================================================================
//
// Watch .ipynb files for external changes (git, VS Code, other editors).
// When changes are detected, merge them into the Automerge doc and broadcast.

/// Time window (ms) to skip file change events after our own writes.
/// Must be larger than the debounce window (500ms) to reliably skip self-writes.
const SELF_WRITE_SKIP_WINDOW_MS: u64 = 600;

/// Parse cells from a Jupyter notebook JSON object.
///
/// Returns `Some(cells)` if parsing succeeded (including empty `cells: []`),
/// or `None` if the `cells` key is missing or invalid (parse failure).
///
/// The source field can be either a string or an array of strings (lines).
/// We normalize it to a single string.
///
/// For older notebooks (pre-nbformat 4.5) that don't have cell IDs, we generate
/// stable fallback IDs based on the cell index. This prevents data loss when
/// merging changes from externally-generated notebooks.
fn parse_cells_from_ipynb(json: &serde_json::Value) -> Option<Vec<CellSnapshot>> {
    let cells_json = json.get("cells").and_then(|c| c.as_array())?;

    let parsed_cells = cells_json
        .iter()
        .enumerate()
        .map(|(index, cell)| {
            // Use existing ID or generate a stable fallback based on index
            // This handles older notebooks (pre-nbformat 4.5) without cell IDs
            let id = cell
                .get("id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("__external_cell_{}", index));

            let cell_type = cell
                .get("cell_type")
                .and_then(|v| v.as_str())
                .unwrap_or("code")
                .to_string();

            // Source can be a string or array of strings
            let source = match cell.get("source") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Array(arr)) => arr
                    .iter()
                    .filter_map(|v| v.as_str())
                    .collect::<Vec<_>>()
                    .join(""),
                _ => String::new(),
            };

            // Execution count: number or null
            let execution_count = match cell.get("execution_count") {
                Some(serde_json::Value::Number(n)) => n.to_string(),
                _ => "null".to_string(),
            };

            // Outputs: keep as JSON strings
            let outputs = cell
                .get("outputs")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .map(|o| serde_json::to_string(o).unwrap_or_default())
                        .collect()
                })
                .unwrap_or_default();

            CellSnapshot {
                id,
                cell_type,
                source,
                execution_count,
                outputs,
            }
        })
        .collect();

    Some(parsed_cells)
}

/// Apply external .ipynb changes to the Automerge doc.
///
/// Compares cells by ID and:
/// - Adds new cells
/// - Removes deleted cells
/// - Updates source, execution_count, and outputs for modified cells
/// - Handles cell reordering by rebuilding the cell list
///
/// When a kernel is running, outputs and execution counts are preserved
/// to avoid losing in-progress execution results.
///
/// Returns true if any changes were applied.
async fn apply_ipynb_changes(
    room: &NotebookRoom,
    external_cells: &[CellSnapshot],
    has_running_kernel: bool,
) -> bool {
    let current_cells = {
        let doc = room.doc.read().await;
        doc.get_cells()
    };

    let mut doc = room.doc.write().await;

    // Build maps for comparison
    let current_map: HashMap<&str, &CellSnapshot> =
        current_cells.iter().map(|c| (c.id.as_str(), c)).collect();
    let external_map: HashMap<&str, &CellSnapshot> =
        external_cells.iter().map(|c| (c.id.as_str(), c)).collect();

    // Check if cell order changed
    let current_ids: Vec<&str> = current_cells.iter().map(|c| c.id.as_str()).collect();
    let external_ids: Vec<&str> = external_cells.iter().map(|c| c.id.as_str()).collect();
    let order_changed = {
        // Filter to only IDs that exist in both, then compare order
        let common_current: Vec<&str> = current_ids
            .iter()
            .filter(|id| external_map.contains_key(*id))
            .copied()
            .collect();
        let common_external: Vec<&str> = external_ids
            .iter()
            .filter(|id| current_map.contains_key(*id))
            .copied()
            .collect();
        common_current != common_external
    };

    let mut changed = false;

    // If order changed, we need to rebuild the cell list
    // This is expensive but necessary to match external order
    if order_changed {
        debug!("[notebook-watch] Cell order changed, rebuilding cell list");

        // Delete all current cells and re-add in external order
        for cell in &current_cells {
            let _ = doc.delete_cell(&cell.id);
        }

        for (index, ext_cell) in external_cells.iter().enumerate() {
            if doc
                .add_cell(index, &ext_cell.id, &ext_cell.cell_type)
                .is_ok()
            {
                let _ = doc.update_source(&ext_cell.id, &ext_cell.source);

                // For existing cells with running kernel: preserve current outputs/execution_count
                // For new cells: always use external values (they don't have in-progress state)
                if has_running_kernel {
                    if let Some(current) = current_map.get(ext_cell.id.as_str()) {
                        // Existing cell - preserve in-progress state
                        let _ = doc.set_outputs(&ext_cell.id, &current.outputs);
                        let _ = doc.set_execution_count(&ext_cell.id, &current.execution_count);
                    } else {
                        // New cell - use external values
                        let _ = doc.set_outputs(&ext_cell.id, &ext_cell.outputs);
                        let _ = doc.set_execution_count(&ext_cell.id, &ext_cell.execution_count);
                    }
                } else {
                    let _ = doc.set_outputs(&ext_cell.id, &ext_cell.outputs);
                    let _ = doc.set_execution_count(&ext_cell.id, &ext_cell.execution_count);
                }
            }
        }
        return true;
    }

    // Find cells to delete (in current but not in external)
    let cells_to_delete: Vec<String> = current_cells
        .iter()
        .filter(|c| !external_map.contains_key(c.id.as_str()))
        .map(|c| c.id.clone())
        .collect();

    for cell_id in cells_to_delete {
        debug!("[notebook-watch] Deleting cell: {}", cell_id);
        if let Ok(true) = doc.delete_cell(&cell_id) {
            changed = true;
        }
    }

    // Process external cells in order (add new or update existing)
    for (index, ext_cell) in external_cells.iter().enumerate() {
        if let Some(current_cell) = current_map.get(ext_cell.id.as_str()) {
            // Cell exists - check if source changed
            if current_cell.source != ext_cell.source {
                debug!("[notebook-watch] Updating source for cell: {}", ext_cell.id);
                if let Ok(true) = doc.update_source(&ext_cell.id, &ext_cell.source) {
                    changed = true;
                }
            }

            // Update cell type if changed
            if current_cell.cell_type != ext_cell.cell_type {
                debug!(
                    "[notebook-watch] Cell type changed for {}: {} -> {}",
                    ext_cell.id, current_cell.cell_type, ext_cell.cell_type
                );
                // Cell type changes require recreating the cell (rare case)
                // For now, just log - full support would need more work
            }

            // Preserve outputs and execution_count if kernel is running
            if !has_running_kernel {
                if current_cell.outputs != ext_cell.outputs {
                    debug!(
                        "[notebook-watch] Updating outputs for cell: {}",
                        ext_cell.id
                    );
                    if let Ok(true) = doc.set_outputs(&ext_cell.id, &ext_cell.outputs) {
                        changed = true;
                    }
                }

                if current_cell.execution_count != ext_cell.execution_count {
                    debug!(
                        "[notebook-watch] Updating execution_count for cell: {} ({} -> {})",
                        ext_cell.id, current_cell.execution_count, ext_cell.execution_count
                    );
                    if doc
                        .set_execution_count(&ext_cell.id, &ext_cell.execution_count)
                        .is_ok()
                    {
                        changed = true;
                    }
                }
            }
        } else {
            // New cell - add it
            // New cells don't have any in-progress state, so always use external values
            debug!(
                "[notebook-watch] Adding new cell at index {}: {}",
                index, ext_cell.id
            );
            if doc
                .add_cell(index, &ext_cell.id, &ext_cell.cell_type)
                .is_ok()
            {
                changed = true;
                let _ = doc.update_source(&ext_cell.id, &ext_cell.source);
                let _ = doc.set_outputs(&ext_cell.id, &ext_cell.outputs);
                let _ = doc.set_execution_count(&ext_cell.id, &ext_cell.execution_count);
            }
        }
    }

    changed
}

/// Spawn a file watcher for a notebook's .ipynb file.
///
/// Watches for external changes and merges them into the Automerge doc.
/// Returns a shutdown sender to stop the watcher when the room is evicted.
pub(crate) fn spawn_notebook_file_watcher(
    notebook_path: PathBuf,
    room: Arc<NotebookRoom>,
) -> oneshot::Sender<()> {
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();

    tokio::spawn(async move {
        // Determine what path to watch
        let watch_path = if notebook_path.exists() {
            notebook_path.clone()
        } else if let Some(parent) = notebook_path.parent() {
            // Watch parent directory if file doesn't exist yet
            if !parent.exists() {
                warn!(
                    "[notebook-watch] Parent dir doesn't exist for {:?}",
                    notebook_path
                );
                return;
            }
            parent.to_path_buf()
        } else {
            warn!(
                "[notebook-watch] Cannot determine watch path for {:?}",
                notebook_path
            );
            return;
        };

        // Create tokio mpsc channel to bridge from notify callback thread
        let (tx, mut rx) = tokio::sync::mpsc::channel::<DebounceEventResult>(16);

        // Create debouncer with 500ms window (same as settings.json)
        let debouncer_result = notify_debouncer_mini::new_debouncer(
            std::time::Duration::from_millis(500),
            move |res: DebounceEventResult| {
                let _ = tx.blocking_send(res);
            },
        );

        let mut debouncer = match debouncer_result {
            Ok(d) => d,
            Err(e) => {
                error!(
                    "[notebook-watch] Failed to create file watcher for {:?}: {}",
                    notebook_path, e
                );
                return;
            }
        };

        if let Err(e) = debouncer
            .watcher()
            .watch(&watch_path, notify::RecursiveMode::NonRecursive)
        {
            error!("[notebook-watch] Failed to watch {:?}: {}", watch_path, e);
            return;
        }

        info!(
            "[notebook-watch] Watching {:?} for external changes",
            notebook_path
        );

        loop {
            tokio::select! {
                Some(result) = rx.recv() => {
                    match result {
                        Ok(events) => {
                            // Check if any event is for our notebook file
                            let relevant = events.iter().any(|e| e.path == notebook_path);
                            if !relevant {
                                continue;
                            }

                            // Check if this is a self-write (within skip window of our last save)
                            let now = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_millis() as u64)
                                .unwrap_or(0);
                            let last_write = room.last_self_write.load(Ordering::Relaxed);
                            if now.saturating_sub(last_write) < SELF_WRITE_SKIP_WINDOW_MS {
                                debug!(
                                    "[notebook-watch] Skipping self-write event for {:?}",
                                    notebook_path
                                );
                                continue;
                            }

                            // Read and parse the file
                            let contents = match tokio::fs::read_to_string(&notebook_path).await {
                                Ok(c) => c,
                                Err(e) => {
                                    // File may be deleted or being written
                                    debug!(
                                        "[notebook-watch] Cannot read {:?}: {}",
                                        notebook_path, e
                                    );
                                    continue;
                                }
                            };

                            let json: serde_json::Value = match serde_json::from_str(&contents) {
                                Ok(j) => j,
                                Err(e) => {
                                    // Partial write or invalid JSON - try again next event
                                    debug!(
                                        "[notebook-watch] Cannot parse {:?}: {}",
                                        notebook_path, e
                                    );
                                    continue;
                                }
                            };

                            // Parse cells from the .ipynb
                            // None = parse failure (missing cells key), Some([]) = valid empty notebook
                            let external_cells = match parse_cells_from_ipynb(&json) {
                                Some(cells) => cells,
                                None => {
                                    warn!(
                                        "[notebook-watch] Cannot parse cells from {:?} - skipping",
                                        notebook_path
                                    );
                                    continue;
                                }
                            };

                            // Check if kernel is running (to preserve outputs)
                            let has_kernel = room.has_kernel().await;

                            // Apply changes to Automerge doc
                            let changed = apply_ipynb_changes(&room, &external_cells, has_kernel).await;

                            if changed {
                                info!(
                                    "[notebook-watch] Applied external changes from {:?}",
                                    notebook_path
                                );

                                // Notify peers of the change
                                let _ = room.changed_tx.send(());

                                // Broadcast FileChanged to all connected clients
                                let cells = {
                                    let doc = room.doc.read().await;
                                    doc.get_cells()
                                };
                                let _ = room.kernel_broadcast_tx.send(
                                    NotebookBroadcast::FileChanged {
                                        cells,
                                        metadata: None, // TODO: handle metadata changes
                                    }
                                );
                            }
                        }
                        Err(errs) => {
                            warn!("[notebook-watch] Watch error for {:?}: {:?}", notebook_path, errs);
                        }
                    }
                }
                _ = &mut shutdown_rx => {
                    info!("[notebook-watch] Shutting down watcher for {:?}", notebook_path);
                    break;
                }
            }
        }
    });

    shutdown_tx
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Create a test blob store in the given temp directory.
    fn test_blob_store(tmp: &tempfile::TempDir) -> Arc<BlobStore> {
        Arc::new(BlobStore::new(tmp.path().join("blobs")))
    }

    #[tokio::test]
    async fn test_room_load_or_create_new() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let room = NotebookRoom::load_or_create("test-nb", tmp.path(), blob_store);

        let doc = room.doc.try_read().unwrap();
        assert_eq!(doc.notebook_id(), Some("test-nb".to_string()));
        assert_eq!(doc.cell_count(), 0);
        assert_eq!(room.active_peers.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_room_persists_and_reloads() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);

        // Create room and add a cell
        {
            let room = NotebookRoom::load_or_create("persist-test", tmp.path(), blob_store.clone());
            let mut doc = room.doc.try_write().unwrap();
            doc.add_cell(0, "c1", "code").unwrap();
            doc.update_source("c1", "hello").unwrap();
            let bytes = doc.save();
            persist_notebook_bytes(&bytes, &room.persist_path);
        }

        // Load again — should have the cell
        {
            let room = NotebookRoom::load_or_create("persist-test", tmp.path(), blob_store);
            let doc = room.doc.try_read().unwrap();
            assert_eq!(doc.cell_count(), 1);
            let cell = doc.get_cell("c1").unwrap();
            assert_eq!(cell.source, "hello");
        }
    }

    #[tokio::test]
    async fn test_get_or_create_room_reuses_existing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let mut rooms = HashMap::new();

        let room1 = get_or_create_room(&mut rooms, "nb1", tmp.path(), blob_store.clone());
        let room2 = get_or_create_room(&mut rooms, "nb1", tmp.path(), blob_store);

        // Should be the same Arc (same room)
        assert!(Arc::ptr_eq(&room1, &room2));
    }

    #[tokio::test]
    async fn test_get_or_create_room_different_notebooks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let mut rooms = HashMap::new();

        let room1 = get_or_create_room(&mut rooms, "nb1", tmp.path(), blob_store.clone());
        let room2 = get_or_create_room(&mut rooms, "nb2", tmp.path(), blob_store);

        // Should be different rooms
        assert!(!Arc::ptr_eq(&room1, &room2));
        assert_eq!(rooms.len(), 2);
    }

    #[tokio::test]
    async fn test_room_peer_counting() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let room = NotebookRoom::load_or_create("peer-test", tmp.path(), blob_store);

        assert_eq!(room.active_peers.load(Ordering::Relaxed), 0);

        room.active_peers.fetch_add(1, Ordering::Relaxed);
        room.active_peers.fetch_add(1, Ordering::Relaxed);
        assert_eq!(room.active_peers.load(Ordering::Relaxed), 2);

        room.active_peers.fetch_sub(1, Ordering::Relaxed);
        assert_eq!(room.active_peers.load(Ordering::Relaxed), 1);

        room.active_peers.fetch_sub(1, Ordering::Relaxed);
        assert_eq!(room.active_peers.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn test_new_fresh_creates_empty_doc() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let room = NotebookRoom::new_fresh("fresh-test", tmp.path(), blob_store);

        let doc = room.doc.try_read().unwrap();
        assert_eq!(doc.notebook_id(), Some("fresh-test".to_string()));
        assert_eq!(doc.cell_count(), 0);
    }

    #[tokio::test]
    async fn test_new_fresh_deletes_stale_persisted_doc() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);

        // Create and persist a room with content using load_or_create
        {
            let room = NotebookRoom::load_or_create("stale-test", tmp.path(), blob_store.clone());
            let mut doc = room.doc.try_write().unwrap();
            doc.add_cell(0, "c1", "code").unwrap();
            doc.update_source("c1", "old content").unwrap();
            let bytes = doc.save();
            persist_notebook_bytes(&bytes, &room.persist_path);
        }

        // Verify persisted file exists
        let filename = notebook_doc_filename("stale-test");
        let persist_path = tmp.path().join(&filename);
        assert!(persist_path.exists(), "Persisted file should exist");

        // Create fresh room - should delete persisted doc and start empty
        let room = NotebookRoom::new_fresh("stale-test", tmp.path(), blob_store);

        // Persisted file should be deleted
        assert!(
            !persist_path.exists(),
            "Persisted file should be deleted by new_fresh"
        );

        // Room should be empty (no cells from persisted doc)
        let doc = room.doc.try_read().unwrap();
        assert_eq!(doc.cell_count(), 0, "new_fresh should start with empty doc");
    }

    /// Helper to build a snapshot with UV inline deps.
    fn snapshot_with_uv(deps: Vec<String>) -> NotebookMetadataSnapshot {
        NotebookMetadataSnapshot {
            kernelspec: None,
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: Some(crate::notebook_metadata::UvInlineMetadata {
                    dependencies: deps,
                    requires_python: None,
                }),
                conda: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
            },
        }
    }

    /// Helper to build a snapshot with conda inline deps.
    fn snapshot_with_conda(deps: Vec<String>) -> NotebookMetadataSnapshot {
        NotebookMetadataSnapshot {
            kernelspec: None,
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: None,
                conda: Some(crate::notebook_metadata::CondaInlineMetadata {
                    dependencies: deps,
                    channels: vec!["conda-forge".to_string()],
                    python: None,
                }),
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
            },
        }
    }

    /// Helper to build an empty snapshot (no deps).
    fn snapshot_empty() -> NotebookMetadataSnapshot {
        NotebookMetadataSnapshot {
            kernelspec: None,
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: None,
                conda: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
            },
        }
    }

    #[test]
    fn test_check_inline_deps_uv() {
        let snapshot = snapshot_with_uv(vec!["numpy".to_string()]);
        assert_eq!(check_inline_deps(&snapshot), Some("uv:inline".to_string()));
    }

    #[test]
    fn test_check_inline_deps_conda() {
        let snapshot = snapshot_with_conda(vec!["pandas".to_string()]);
        assert_eq!(
            check_inline_deps(&snapshot),
            Some("conda:inline".to_string())
        );
    }

    #[test]
    fn test_check_inline_deps_empty() {
        let snapshot = snapshot_empty();
        assert_eq!(check_inline_deps(&snapshot), None);
    }

    #[test]
    fn test_check_inline_deps_empty_array() {
        // Snapshot with empty deps array - should return None
        let snapshot = snapshot_with_uv(vec![]);
        assert_eq!(check_inline_deps(&snapshot), None);
    }

    #[test]
    fn test_check_inline_deps_uv_priority() {
        // Snapshot with both UV and conda deps - UV takes priority
        let snapshot = NotebookMetadataSnapshot {
            kernelspec: None,
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: Some(crate::notebook_metadata::UvInlineMetadata {
                    dependencies: vec!["numpy".to_string()],
                    requires_python: None,
                }),
                conda: Some(crate::notebook_metadata::CondaInlineMetadata {
                    dependencies: vec!["pandas".to_string()],
                    channels: vec!["conda-forge".to_string()],
                    python: None,
                }),
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
            },
        };
        assert_eq!(check_inline_deps(&snapshot), Some("uv:inline".to_string()));
    }

    #[test]
    fn test_check_inline_deps_deno() {
        // Snapshot with deno config - deno takes priority over everything
        let snapshot = NotebookMetadataSnapshot {
            kernelspec: None,
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: Some(crate::notebook_metadata::UvInlineMetadata {
                    dependencies: vec!["numpy".to_string()],
                    requires_python: None,
                }),
                conda: None,
                deno: Some(crate::notebook_metadata::DenoMetadata {
                    permissions: vec![],
                    import_map: None,
                    config: None,
                    flexible_npm_imports: None,
                }),
                trust_signature: None,
                trust_timestamp: None,
            },
        };
        assert_eq!(check_inline_deps(&snapshot), Some("deno".to_string()));
    }

    // ── Tests for detect_notebook_kernel_type ──────────────────────────────

    #[test]
    fn test_detect_notebook_kernel_type_deno_kernelspec() {
        // Deno kernelspec name should be detected
        let snapshot = NotebookMetadataSnapshot {
            kernelspec: Some(crate::notebook_metadata::KernelspecSnapshot {
                name: "deno".to_string(),
                display_name: "Deno".to_string(),
                language: Some("typescript".to_string()),
            }),
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: None,
                conda: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
            },
        };
        assert_eq!(
            detect_notebook_kernel_type(&snapshot),
            Some("deno".to_string())
        );
    }

    #[test]
    fn test_detect_notebook_kernel_type_typescript_language() {
        // Kernelspec with typescript language should return deno
        let snapshot = NotebookMetadataSnapshot {
            kernelspec: Some(crate::notebook_metadata::KernelspecSnapshot {
                name: "some-kernel".to_string(),
                display_name: "Some Kernel".to_string(),
                language: Some("typescript".to_string()),
            }),
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: None,
                conda: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
            },
        };
        assert_eq!(
            detect_notebook_kernel_type(&snapshot),
            Some("deno".to_string())
        );
    }

    #[test]
    fn test_detect_notebook_kernel_type_python() {
        // Python kernelspec should be detected
        let snapshot = NotebookMetadataSnapshot {
            kernelspec: Some(crate::notebook_metadata::KernelspecSnapshot {
                name: "python3".to_string(),
                display_name: "Python 3".to_string(),
                language: Some("python".to_string()),
            }),
            language_info: None,
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: None,
                conda: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
            },
        };
        assert_eq!(
            detect_notebook_kernel_type(&snapshot),
            Some("python".to_string())
        );
    }

    #[test]
    fn test_detect_notebook_kernel_type_language_info_fallback() {
        // Falls back to language_info when kernelspec doesn't match
        let snapshot = NotebookMetadataSnapshot {
            kernelspec: Some(crate::notebook_metadata::KernelspecSnapshot {
                name: "unknown-kernel".to_string(),
                display_name: "Unknown".to_string(),
                language: None,
            }),
            language_info: Some(crate::notebook_metadata::LanguageInfoSnapshot {
                name: "typescript".to_string(),
                version: None,
            }),
            runt: crate::notebook_metadata::RuntMetadata {
                schema_version: "1".to_string(),
                env_id: None,
                uv: None,
                conda: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
            },
        };
        assert_eq!(
            detect_notebook_kernel_type(&snapshot),
            Some("deno".to_string())
        );
    }

    // ── Integration tests for save_notebook_to_disk ────────────────────────

    /// Create a test room with a notebook_path pointing to a file in temp dir.
    fn test_room_with_path(
        tmp: &tempfile::TempDir,
        notebook_filename: &str,
    ) -> (NotebookRoom, PathBuf) {
        let notebook_path = tmp.path().join(notebook_filename);
        let blob_store = test_blob_store(tmp);
        let notebook_id = notebook_path.to_string_lossy().to_string();

        let doc = crate::notebook_doc::NotebookDoc::new(&notebook_id);
        let (changed_tx, _) = broadcast::channel(16);
        let (kernel_broadcast_tx, _) = broadcast::channel(64);
        let persist_path = tmp.path().join("doc.automerge");
        let (persist_tx, persist_rx) = watch::channel::<Option<Vec<u8>>>(None);
        spawn_persist_debouncer(persist_rx, persist_path.clone());

        let room = NotebookRoom {
            doc: Arc::new(RwLock::new(doc)),
            changed_tx,
            kernel_broadcast_tx,
            persist_tx,
            persist_path,
            active_peers: AtomicUsize::new(0),
            kernel: Arc::new(Mutex::new(None)),
            blob_store,
            trust_state: Arc::new(RwLock::new(TrustState {
                status: runt_trust::TrustStatus::Untrusted,
                info: runt_trust::TrustInfo {
                    status: runt_trust::TrustStatus::Untrusted,
                    uv_dependencies: vec![],
                    conda_dependencies: vec![],
                    conda_channels: vec![],
                },
                pending_launch: false,
            })),
            notebook_path: notebook_path.clone(),
            working_dir: Arc::new(RwLock::new(None)),
            auto_launch_at: Arc::new(RwLock::new(None)),
            comm_state: Arc::new(crate::comm_state::CommState::new()),
            last_self_write: Arc::new(AtomicU64::new(0)),
            watcher_shutdown_tx: Mutex::new(None),
        };

        (room, notebook_path)
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_creates_valid_nbformat() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, notebook_path) = test_room_with_path(&tmp, "test.ipynb");

        // Add cells to the doc
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell1", "code").unwrap();
            doc.update_source("cell1", "print('hello')").unwrap();
            doc.add_cell(1, "cell2", "markdown").unwrap();
            doc.update_source("cell2", "# Title").unwrap();
        }

        // Save to disk
        save_notebook_to_disk(&room, None).await.unwrap();

        // Read and validate with nbformat
        let content = std::fs::read_to_string(&notebook_path).unwrap();
        let notebook: nbformat::v4::Notebook =
            serde_json::from_str(&content).expect("Saved notebook should be valid nbformat");

        assert_eq!(notebook.cells.len(), 2);
        assert_eq!(notebook.nbformat, 4);
        assert!(
            notebook.nbformat_minor >= 5,
            "Cell IDs require nbformat_minor >= 5"
        );
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_preserves_unknown_metadata() {
        use std::io::Write;
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, notebook_path) = test_room_with_path(&tmp, "metadata.ipynb");

        // Create existing file with unknown metadata fields
        {
            let mut f = std::fs::File::create(&notebook_path).unwrap();
            writeln!(
                f,
                r#"{{
                    "nbformat": 4,
                    "nbformat_minor": 5,
                    "metadata": {{
                        "custom_extension": {{"key": "value"}},
                        "jupyter": {{"source_hidden": true}},
                        "runt": {{"trust_signature": "abc123", "schema_version": "1"}}
                    }},
                    "cells": []
                }}"#
            )
            .unwrap();
        }

        // Add a cell and save
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell1", "code").unwrap();
            doc.update_source("cell1", "x = 1").unwrap();
        }

        save_notebook_to_disk(&room, None).await.unwrap();

        // Verify unknown metadata is preserved
        let content = std::fs::read_to_string(&notebook_path).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&content).unwrap();
        let metadata = saved.get("metadata").unwrap();

        // custom_extension should be preserved
        assert!(
            metadata.get("custom_extension").is_some(),
            "custom_extension should be preserved"
        );
        assert_eq!(
            metadata.get("custom_extension").unwrap().get("key"),
            Some(&serde_json::json!("value"))
        );

        // jupyter should be preserved
        assert!(
            metadata.get("jupyter").is_some(),
            "jupyter metadata should be preserved"
        );

        // trust_signature in runt should be preserved (deep-merge)
        let runt = metadata.get("runt").unwrap();
        assert_eq!(
            runt.get("trust_signature"),
            Some(&serde_json::json!("abc123")),
            "trust_signature should be preserved via deep-merge"
        );
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_enforces_nbformat_minor_5() {
        use std::io::Write;
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, notebook_path) = test_room_with_path(&tmp, "old_minor.ipynb");

        // Create existing file with old nbformat_minor
        {
            let mut f = std::fs::File::create(&notebook_path).unwrap();
            writeln!(
                f,
                r#"{{
                    "nbformat": 4,
                    "nbformat_minor": 2,
                    "metadata": {{}},
                    "cells": []
                }}"#
            )
            .unwrap();
        }

        // Add a cell with an id and save
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell-with-id", "code").unwrap();
        }

        save_notebook_to_disk(&room, None).await.unwrap();

        // Verify nbformat_minor is upgraded to 5
        let content = std::fs::read_to_string(&notebook_path).unwrap();
        let saved: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(
            saved.get("nbformat_minor"),
            Some(&serde_json::json!(5)),
            "nbformat_minor should be upgraded to 5 when writing cell IDs"
        );
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_with_outputs() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, notebook_path) = test_room_with_path(&tmp, "outputs.ipynb");

        // Add a cell with a raw output
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell1", "code").unwrap();
            doc.update_source("cell1", "print('hello')").unwrap();
            // Add raw JSON output (stream type)
            let output = r#"{"output_type": "stream", "name": "stdout", "text": ["hello\n"]}"#;
            doc.set_outputs("cell1", &[output.to_string()]).unwrap();
            doc.set_execution_count("cell1", "1").unwrap();
        }

        save_notebook_to_disk(&room, None).await.unwrap();

        // Read and validate
        let content = std::fs::read_to_string(&notebook_path).unwrap();
        let notebook: nbformat::v4::Notebook =
            serde_json::from_str(&content).expect("Should be valid nbformat with outputs");

        assert_eq!(notebook.cells.len(), 1);
        if let nbformat::v4::Cell::Code { outputs, .. } = &notebook.cells[0] {
            assert_eq!(outputs.len(), 1, "Should have one output");
            // Verify it's a stream output (nbformat types may vary)
            match &outputs[0] {
                nbformat::v4::Output::Stream { name, .. } => {
                    assert_eq!(name, "stdout");
                }
                _ => panic!("Expected stream output"),
            }
        } else {
            panic!("Expected code cell");
        }
    }

    #[test]
    fn test_is_untitled_notebook_with_uuid() {
        assert!(is_untitled_notebook("550e8400-e29b-41d4-a716-446655440000"));
        assert!(is_untitled_notebook("a1b2c3d4-e5f6-7890-abcd-ef1234567890"));
    }

    #[test]
    fn test_is_untitled_notebook_with_path() {
        assert!(!is_untitled_notebook("/home/user/notebook.ipynb"));
        assert!(!is_untitled_notebook("./relative/path.ipynb"));
        assert!(!is_untitled_notebook("notebook.ipynb"));
    }

    /// Test that the debouncer flushes at max interval even during continuous updates.
    ///
    /// Uses short intervals (50ms debounce, 200ms max) for fast testing.
    #[tokio::test]
    async fn test_persist_debouncer_max_interval_flush() {
        use std::time::Duration;

        let tmp = tempfile::TempDir::new().unwrap();
        let persist_path = tmp.path().join("test.automerge");

        // Create watch channel and spawn debouncer with short intervals for testing
        let (tx, rx) = watch::channel::<Option<Vec<u8>>>(None);
        let config = PersistDebouncerConfig {
            debounce_ms: 50,       // 50ms debounce window
            max_interval_ms: 200,  // 200ms max between flushes
            check_interval_ms: 10, // Check every 10ms
        };
        spawn_persist_debouncer_with_config(rx, persist_path.clone(), config);

        // Send updates every 20ms (faster than 50ms debounce, so debounce never triggers)
        // The 200ms max interval should force a flush even without a quiet period.
        for i in 0..20 {
            let data = format!("update-{}", i).into_bytes();
            tx.send(Some(data)).unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Total time: 20 * 20ms = 400ms, which is > 200ms max interval

        // Give debouncer time to flush
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            persist_path.exists(),
            "File should exist after max interval even with continuous updates"
        );

        // Verify content is from an update
        let content = std::fs::read(&persist_path).unwrap();
        let content_str = String::from_utf8_lossy(&content);
        assert!(
            content_str.starts_with("update-"),
            "Content should be from an update"
        );
    }

    // ==========================================================================
    // File watcher tests
    // ==========================================================================

    #[test]
    fn test_parse_cells_from_ipynb_with_ids() {
        let json = serde_json::json!({
            "cells": [
                {
                    "id": "cell-1",
                    "cell_type": "code",
                    "source": "print('hello')",
                    "execution_count": 5,
                    "outputs": []
                },
                {
                    "id": "cell-2",
                    "cell_type": "markdown",
                    "source": ["# Title\n", "Body"],
                    "execution_count": null,
                    "outputs": []
                }
            ]
        });

        let cells = parse_cells_from_ipynb(&json).expect("Should parse valid notebook");
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].id, "cell-1");
        assert_eq!(cells[0].cell_type, "code");
        assert_eq!(cells[0].source, "print('hello')");
        assert_eq!(cells[0].execution_count, "5");
        assert_eq!(cells[1].id, "cell-2");
        assert_eq!(cells[1].cell_type, "markdown");
        assert_eq!(cells[1].source, "# Title\nBody");
        assert_eq!(cells[1].execution_count, "null");
    }

    #[test]
    fn test_parse_cells_from_ipynb_missing_ids() {
        // Older notebooks (pre-nbformat 4.5) don't have cell IDs
        let json = serde_json::json!({
            "cells": [
                {
                    "cell_type": "code",
                    "source": "x = 1",
                    "execution_count": null,
                    "outputs": []
                },
                {
                    "cell_type": "code",
                    "source": "y = 2",
                    "execution_count": null,
                    "outputs": []
                }
            ]
        });

        let cells = parse_cells_from_ipynb(&json).expect("Should parse valid notebook");
        assert_eq!(cells.len(), 2);
        // Should generate fallback IDs based on index
        assert_eq!(cells[0].id, "__external_cell_0");
        assert_eq!(cells[1].id, "__external_cell_1");
        assert_eq!(cells[0].source, "x = 1");
        assert_eq!(cells[1].source, "y = 2");
    }

    #[test]
    fn test_parse_cells_from_ipynb_empty() {
        // Valid notebook with empty cells array - should return Some([])
        let json = serde_json::json!({
            "cells": []
        });
        let cells = parse_cells_from_ipynb(&json).expect("Should parse valid empty notebook");
        assert!(cells.is_empty());
    }

    #[test]
    fn test_parse_cells_from_ipynb_no_cells_key() {
        // Invalid notebook (missing cells key) - should return None
        let json = serde_json::json!({
            "metadata": {}
        });
        assert!(
            parse_cells_from_ipynb(&json).is_none(),
            "Should return None for invalid notebook"
        );
    }

    #[tokio::test]
    async fn test_apply_ipynb_changes_clears_all_cells() {
        // Valid "delete all cells" case - empty cells array should clear the doc
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _) = test_room_with_path(&tmp, "test.ipynb");

        // Add cells to the doc
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell-1", "code").unwrap();
            doc.update_source("cell-1", "x = 1").unwrap();
        }

        // Apply empty external cells - should delete all cells
        let external_cells = vec![];
        let changed = apply_ipynb_changes(&room, &external_cells, false).await;
        assert!(changed, "Should apply changes to clear all cells");

        // Verify all cells were deleted
        let cells = {
            let doc = room.doc.read().await;
            doc.get_cells()
        };
        assert!(cells.is_empty(), "All cells should be deleted");
    }

    #[tokio::test]
    async fn test_apply_ipynb_changes_updates_execution_count() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _) = test_room_with_path(&tmp, "test.ipynb");

        // Add cells to the doc
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell-1", "code").unwrap();
            doc.set_execution_count("cell-1", "null").unwrap();
        }

        // Apply external changes with execution_count
        let external_cells = vec![CellSnapshot {
            id: "cell-1".to_string(),
            cell_type: "code".to_string(),
            source: String::new(),
            execution_count: "42".to_string(),
            outputs: vec![],
        }];

        let changed = apply_ipynb_changes(&room, &external_cells, false).await;
        assert!(changed, "Should detect execution_count change");

        let cells = {
            let doc = room.doc.read().await;
            doc.get_cells()
        };
        assert_eq!(cells[0].execution_count, "42");
    }

    #[tokio::test]
    async fn test_apply_ipynb_changes_preserves_outputs_when_kernel_running() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _) = test_room_with_path(&tmp, "test.ipynb");

        // Add cells with outputs
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell-1", "code").unwrap();
            doc.set_outputs("cell-1", &[r#"{"output_type":"stream"}"#.to_string()])
                .unwrap();
            doc.set_execution_count("cell-1", "10").unwrap();
        }

        // Apply external changes (with different outputs) while kernel is "running"
        let external_cells = vec![CellSnapshot {
            id: "cell-1".to_string(),
            cell_type: "code".to_string(),
            source: "new source".to_string(),
            execution_count: "5".to_string(),
            outputs: vec![r#"{"output_type":"error"}"#.to_string()],
        }];

        let changed = apply_ipynb_changes(&room, &external_cells, true).await;
        assert!(changed, "Should apply source change");

        let cells = {
            let doc = room.doc.read().await;
            doc.get_cells()
        };
        // Source should be updated
        assert_eq!(cells[0].source, "new source");
        // But outputs and execution_count should be preserved
        assert_eq!(cells[0].outputs, vec![r#"{"output_type":"stream"}"#]);
        assert_eq!(cells[0].execution_count, "10");
    }

    #[tokio::test]
    async fn test_apply_ipynb_changes_new_cell_with_outputs_while_kernel_running() {
        // New external cells should get their external outputs even when kernel is running
        // (they don't have any in-progress state to preserve)
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _) = test_room_with_path(&tmp, "test.ipynb");

        // Start with one cell
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "existing-cell", "code").unwrap();
        }

        // Add a new external cell with outputs while kernel is "running"
        let external_cells = vec![
            CellSnapshot {
                id: "existing-cell".to_string(),
                cell_type: "code".to_string(),
                source: String::new(),
                execution_count: "null".to_string(),
                outputs: vec![],
            },
            CellSnapshot {
                id: "new-cell".to_string(),
                cell_type: "code".to_string(),
                source: "print('new')".to_string(),
                execution_count: "42".to_string(),
                outputs: vec![r#"{"output_type":"execute_result"}"#.to_string()],
            },
        ];

        let changed = apply_ipynb_changes(&room, &external_cells, true).await;
        assert!(changed, "Should add new cell");

        let cells = {
            let doc = room.doc.read().await;
            doc.get_cells()
        };
        assert_eq!(cells.len(), 2);

        // New cell should have external outputs and execution_count
        let new_cell = cells.iter().find(|c| c.id == "new-cell").unwrap();
        assert_eq!(new_cell.source, "print('new')");
        assert_eq!(new_cell.execution_count, "42");
        assert_eq!(
            new_cell.outputs,
            vec![r#"{"output_type":"execute_result"}"#]
        );
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_with_target_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _original_path) = test_room_with_path(&tmp, "original.ipynb");

        // Add a cell
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell1", "code").unwrap();
            doc.update_source("cell1", "x = 1").unwrap();
        }

        // Save to a different absolute path
        let new_path = tmp.path().join("new_location.ipynb");
        let result = save_notebook_to_disk(&room, Some(new_path.to_str().unwrap())).await;

        assert!(result.is_ok());
        let saved_path = result.unwrap();
        assert_eq!(saved_path, new_path.to_string_lossy());
        assert!(new_path.exists(), "File should be created at new path");

        // Verify content
        let content = std::fs::read_to_string(&new_path).unwrap();
        let notebook: serde_json::Value = serde_json::from_str(&content).unwrap();
        assert_eq!(notebook["cells"][0]["source"], serde_json::json!(["x = 1"]));
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_appends_ipynb_extension() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _original_path) = test_room_with_path(&tmp, "original.ipynb");

        // Add a cell
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell1", "code").unwrap();
        }

        // Save to path without .ipynb extension
        let base_path = tmp.path().join("no_extension");
        let result = save_notebook_to_disk(&room, Some(base_path.to_str().unwrap())).await;

        assert!(result.is_ok());
        let saved_path = result.unwrap();
        assert!(
            saved_path.ends_with(".ipynb"),
            "Saved path should have .ipynb extension"
        );

        let expected_path = tmp.path().join("no_extension.ipynb");
        assert!(
            expected_path.exists(),
            "File should exist with .ipynb extension"
        );
    }

    #[tokio::test]
    async fn test_save_notebook_to_disk_rejects_relative_path() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _original_path) = test_room_with_path(&tmp, "original.ipynb");

        // Try to save with a relative path
        let result = save_notebook_to_disk(&room, Some("relative/path.ipynb")).await;

        assert!(result.is_err());
        let error = result.unwrap_err();
        assert!(
            error.contains("Relative paths are not supported"),
            "Error should mention relative paths: {}",
            error
        );
    }

    #[tokio::test]
    async fn test_format_notebook_cells_skips_unknown_runtime() {
        let tmp = tempfile::TempDir::new().unwrap();
        let (room, _notebook_path) = test_room_with_path(&tmp, "unknown_runtime.ipynb");

        // Add a code cell (no kernelspec metadata set = unknown runtime)
        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell1", "code").unwrap();
            doc.update_source("cell1", "x=1").unwrap(); // Would be formatted if Python
        }

        // Run format - should skip (return 0) since no kernelspec
        let result = format_notebook_cells(&room).await;
        assert!(result.is_ok());
        assert_eq!(
            result.unwrap(),
            0,
            "Should format 0 cells for unknown runtime"
        );

        // Source should be unchanged
        let cells = {
            let doc = room.doc.read().await;
            doc.get_cells()
        };
        assert_eq!(cells[0].source, "x=1", "Source should remain unchanged");
    }
}
