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
//!    (the doc is already persisted on every change)
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
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use automerge::sync;
use log::{error, info, warn};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::blob_store::BlobStore;
use crate::comm_state::CommState;
use crate::connection::{self, NotebookFrameType};
use crate::kernel_manager::RoomKernel;
use crate::notebook_doc::{notebook_doc_filename, NotebookDoc};
use crate::protocol::{NotebookBroadcast, NotebookRequest, NotebookResponse};

/// Trust state for a notebook room.
/// Tracks whether the notebook's dependencies are trusted for auto-launch.
#[derive(Debug, Clone)]
pub struct TrustState {
    pub status: runt_trust::TrustStatus,
    pub info: runt_trust::TrustInfo,
    /// If true, kernel launch is pending user trust approval
    pub pending_launch: bool,
}

/// Check if a notebook file has inline dependencies in its metadata.
/// Returns the appropriate env_source if found ("uv:inline" or "conda:inline").
///
/// Priority: UV deps are checked first, then conda deps.
/// Uses runt_trust helpers to check both new (runt.*) and legacy paths.
fn check_inline_deps(notebook_path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(notebook_path).ok()?;
    let nb: serde_json::Value = serde_json::from_str(&content).ok()?;
    let metadata_value = nb.get("metadata")?;

    // Convert to HashMap for runt_trust functions
    let metadata: std::collections::HashMap<String, serde_json::Value> =
        serde_json::from_value(metadata_value.clone()).ok()?;

    // Check UV dependencies first (runt.uv then legacy uv)
    if let Some(uv) = runt_trust::get_uv_metadata(&metadata) {
        if let Some(deps) = uv.get("dependencies").and_then(|d| d.as_array()) {
            if !deps.is_empty() {
                return Some("uv:inline".to_string());
            }
        }
    }

    // Check conda dependencies (runt.conda then legacy conda)
    if let Some(conda) = runt_trust::get_conda_metadata(&metadata) {
        if let Some(deps) = conda.get("dependencies").and_then(|d| d.as_array()) {
            if !deps.is_empty() {
                return Some("conda:inline".to_string());
            }
        }
    }

    None
}

/// Verify trust status of a notebook by reading its file.
/// Returns TrustState with the verification result.
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
    /// Timestamp when auto-launch was triggered (for grace period on eviction).
    /// If set, the room won't be evicted for 30 seconds to allow client reconnect.
    pub auto_launch_at: Arc<RwLock<Option<std::time::Instant>>>,
    /// Comm channel state for widgets.
    /// Stores active comms so new windows can sync widget models.
    /// Arc-wrapped so it can be shared with the kernel's iopub task.
    pub comm_state: Arc<CommState>,
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
            persist_path,
            active_peers: AtomicUsize::new(0),
            kernel: Arc::new(Mutex::new(None)),
            blob_store,
            trust_state: Arc::new(RwLock::new(trust_state)),
            notebook_path,
            auto_launch_at: Arc::new(RwLock::new(None)),
            comm_state: Arc::new(CommState::new()),
        }
    }

    /// Create a new room by loading a persisted document or creating a fresh one.
    ///
    /// Note: This method is kept for tests that verify persistence behavior.
    /// For normal operation, `new_fresh` is used to ensure the .ipynb file
    /// is the source of truth.
    #[cfg(test)]
    pub fn load_or_create(notebook_id: &str, docs_dir: &Path, blob_store: Arc<BlobStore>) -> Self {
        let filename = notebook_doc_filename(notebook_id);
        let persist_path = docs_dir.join(filename);
        let doc = NotebookDoc::load_or_create(&persist_path, notebook_id);
        let (changed_tx, _) = broadcast::channel(16);
        let (kernel_broadcast_tx, _) = broadcast::channel(64);
        let notebook_path = PathBuf::from(notebook_id);
        let trust_state = verify_trust_from_file(&notebook_path);
        Self {
            doc: Arc::new(RwLock::new(doc)),
            changed_tx,
            kernel_broadcast_tx,
            persist_path,
            active_peers: AtomicUsize::new(0),
            kernel: Arc::new(Mutex::new(None)),
            blob_store,
            trust_state: Arc::new(RwLock::new(trust_state)),
            notebook_path,
            auto_launch_at: Arc::new(RwLock::new(None)),
            comm_state: Arc::new(CommState::new()),
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
            Arc::new(NotebookRoom::new_fresh(notebook_id, docs_dir, blob_store))
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
/// is decremented. If it reaches zero, the room is evicted from the rooms
/// map (the doc has already been persisted on every change).
///
/// The `use_typed_frames` parameter determines the protocol version:
/// - `false` (v1): Raw Automerge frames (legacy, for old clients)
/// - `true` (v2): Typed frames with first-byte type indicator
#[allow(clippy::too_many_arguments)]
pub async fn handle_notebook_sync_connection<R, W>(
    mut reader: R,
    mut writer: W,
    room: Arc<NotebookRoom>,
    rooms: NotebookRooms,
    notebook_id: String,
    use_typed_frames: bool,
    default_python_env: crate::settings_doc::PythonEnvType,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    room.active_peers.fetch_add(1, Ordering::Relaxed);
    let peers = room.active_peers.load(Ordering::Relaxed);
    info!(
        "[notebook-sync] Client connected to room {} ({} peer{}, protocol {})",
        notebook_id,
        peers,
        if peers == 1 { "" } else { "s" },
        if use_typed_frames { "v2" } else { "v1" }
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

    // For v2 protocol, send capabilities response first
    if use_typed_frames {
        let caps = connection::ProtocolCapabilities {
            protocol: connection::PROTOCOL_V2.to_string(),
        };
        connection::send_json_frame(&mut writer, &caps).await?;
    }

    let result = if use_typed_frames {
        run_sync_loop_v2(&mut reader, &mut writer, &room, daemon).await
    } else {
        run_sync_loop_v1(&mut reader, &mut writer, &room).await
    };

    // Peer disconnected — decrement and possibly evict the room
    let remaining = room.active_peers.fetch_sub(1, Ordering::Relaxed) - 1;
    if remaining == 0 {
        // Schedule delayed eviction check. This handles:
        // 1. Grace period during auto-launch (client may reconnect)
        // 2. Kernel running with no peers (idle timeout)
        // Without this, rooms with kernels would leak forever.
        let eviction_delay = std::time::Duration::from_secs(30);
        let rooms_for_eviction = rooms.clone();
        let room_for_eviction = room.clone();
        let notebook_id_for_eviction = notebook_id.clone();

        info!(
            "[notebook-sync] All peers disconnected from room {}, scheduling eviction check in {}s",
            notebook_id,
            eviction_delay.as_secs()
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

/// Protocol v1: Raw Automerge frames (legacy, for backwards compatibility).
///
/// This is the original sync protocol used by older clients. It only supports
/// Automerge document sync, not kernel execution through the daemon.
async fn run_sync_loop_v1<R, W>(
    reader: &mut R,
    writer: &mut W,
    room: &NotebookRoom,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut peer_state = sync::State::new();
    let mut changed_rx = room.changed_tx.subscribe();

    // Phase 1: Initial sync — server sends first (raw frame)
    {
        let mut doc = room.doc.write().await;
        if let Some(msg) = doc.generate_sync_message(&mut peer_state) {
            connection::send_frame(writer, &msg.encode()).await?;
        }
    }

    // Phase 2: Exchange messages until sync is complete, then watch for changes
    loop {
        tokio::select! {
            // Incoming message from this client (raw frame)
            result = connection::recv_frame(reader) => {
                match result? {
                    Some(data) => {
                        let message = sync::Message::decode(&data)
                            .map_err(|e| anyhow::anyhow!("decode error: {}", e))?;

                        // Serialize bytes inside the lock, then persist outside it
                        let persist_bytes = {
                            let mut doc = room.doc.write().await;
                            doc.receive_sync_message(&mut peer_state, message)?;

                            let bytes = doc.save();

                            // Notify other peers in this room
                            let _ = room.changed_tx.send(());

                            // Send our response while still holding the lock (raw frame)
                            if let Some(reply) = doc.generate_sync_message(&mut peer_state) {
                                connection::send_frame(writer, &reply.encode()).await?;
                            }

                            bytes
                        };

                        // Persist outside the write lock
                        persist_notebook_bytes(&persist_bytes, &room.persist_path);
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
                    connection::send_frame(writer, &msg.encode()).await?;
                }
            }
        }
    }
}

/// Protocol v2: Typed frames with first-byte type indicator.
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

                                // Persist outside the write lock
                                persist_notebook_bytes(&persist_bytes, &room.persist_path);
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

/// Auto-launch kernel for a trusted notebook when first peer connects.
/// This is similar to handle_notebook_request(LaunchKernel) but without a request/response.
async fn auto_launch_kernel(
    room: &NotebookRoom,
    notebook_id: &str,
    default_python_env: crate::settings_doc::PythonEnvType,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
) {
    // Check if room still has peers (protect against race condition where client disconnects
    // before we finish launching)
    if room.active_peers.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        info!("[notebook-sync] Auto-launch aborted: no peers remaining");
        return;
    }

    // notebook_path is only valid if it's a real file (not a UUID for new notebooks)
    let notebook_path = PathBuf::from(notebook_id);
    let notebook_path_opt = if notebook_path.exists() {
        Some(notebook_path.clone())
    } else {
        None
    };

    let mut kernel_guard = room.kernel.lock().await;

    // Double-check no kernel is already running
    if let Some(ref kernel) = *kernel_guard {
        if kernel.is_running() {
            info!("[notebook-sync] Auto-launch skipped: kernel already running");
            return;
        }
    }

    // Re-check peers after acquiring lock (another race check)
    if room.active_peers.load(std::sync::atomic::Ordering::Relaxed) == 0 {
        info!("[notebook-sync] Auto-launch aborted: no peers remaining (after lock)");
        return;
    }

    // Clear any stale comm state from a previous kernel (in case it crashed)
    room.comm_state.clear().await;

    // Create new kernel
    let mut kernel = RoomKernel::new(
        room.kernel_broadcast_tx.clone(),
        room.doc.clone(),
        room.persist_path.clone(),
        room.changed_tx.clone(),
        room.blob_store.clone(),
        room.comm_state.clone(),
    );

    // Auto-detect environment source
    // Priority 1: Check inline deps in notebook metadata (only for existing files)
    let env_source = if let Some(ref path) = notebook_path_opt {
        if let Some(inline_source) = check_inline_deps(path) {
            info!(
                "[notebook-sync] Auto-launch: found inline deps -> {}",
                inline_source
            );
            inline_source
        } else if let Some(detected) = crate::project_file::detect_project_file(path) {
            info!(
                "[notebook-sync] Auto-launch: detected project file {:?} -> {}",
                detected.path,
                detected.to_env_source()
            );
            detected.to_env_source().to_string()
        } else {
            // Use user's preferred environment type for prewarmed
            let prewarmed = match default_python_env {
                crate::settings_doc::PythonEnvType::Conda => "conda:prewarmed",
                _ => "uv:prewarmed", // Default to UV for Uv and Other
            };
            info!(
                "[notebook-sync] Auto-launch: using prewarmed environment ({})",
                prewarmed
            );
            prewarmed.to_string()
        }
    } else {
        // New notebook (UUID, no file) - use user's preferred prewarmed env
        let prewarmed = match default_python_env {
            crate::settings_doc::PythonEnvType::Conda => "conda:prewarmed",
            _ => "uv:prewarmed", // Default to UV for Uv and Other
        };
        info!(
            "[notebook-sync] Auto-launch: new notebook, using prewarmed environment ({})",
            prewarmed
        );
        prewarmed.to_string()
    };

    // Acquire prewarmed environment from pool, or create on-demand if pool is empty
    let pooled_env = match env_source.as_str() {
        "uv:prewarmed" => match daemon.take_uv_env().await {
            Some(env) => {
                info!(
                    "[notebook-sync] Auto-launch: acquired UV env from pool: {:?}",
                    env.python_path
                );
                env
            }
            None => {
                info!("[notebook-sync] Auto-launch: UV pool empty, creating env on-demand");
                match daemon.create_uv_env_on_demand().await {
                    Ok(env) => {
                        info!(
                            "[notebook-sync] Auto-launch: created UV env on-demand: {:?}",
                            env.python_path
                        );
                        env
                    }
                    Err(e) => {
                        error!(
                            "[notebook-sync] Auto-launch failed: could not create UV env: {}",
                            e
                        );
                        let _ = room
                            .kernel_broadcast_tx
                            .send(NotebookBroadcast::KernelStatus {
                                status: format!("error: {}", e),
                                cell_id: None,
                            });
                        return;
                    }
                }
            }
        },
        "conda:prewarmed" => {
            match daemon.take_conda_env().await {
                Some(env) => {
                    info!(
                        "[notebook-sync] Auto-launch: acquired Conda env from pool: {:?}",
                        env.python_path
                    );
                    env
                }
                None => {
                    info!("[notebook-sync] Auto-launch: Conda pool empty, creating env on-demand");
                    match daemon.create_conda_env_on_demand().await {
                        Ok(env) => {
                            info!(
                                "[notebook-sync] Auto-launch: created Conda env on-demand: {:?}",
                                env.python_path
                            );
                            env
                        }
                        Err(e) => {
                            error!("[notebook-sync] Auto-launch failed: could not create Conda env: {}", e);
                            let _ =
                                room.kernel_broadcast_tx
                                    .send(NotebookBroadcast::KernelStatus {
                                        status: format!("error: {}", e),
                                        cell_id: None,
                                    });
                            return;
                        }
                    }
                }
            }
        }
        other => {
            // For unexpected env sources, route based on prefix to ensure correct env manager
            if other.starts_with("conda:") {
                warn!("[notebook-sync] Auto-launch: unexpected conda env_source '{}', creating Conda env on-demand", other);
                match daemon.create_conda_env_on_demand().await {
                    Ok(env) => env,
                    Err(e) => {
                        error!(
                            "[notebook-sync] Auto-launch failed: could not create Conda env: {}",
                            e
                        );
                        let _ = room
                            .kernel_broadcast_tx
                            .send(NotebookBroadcast::KernelStatus {
                                status: format!("error: {}", e),
                                cell_id: None,
                            });
                        return;
                    }
                }
            } else {
                // Default to UV for uv:* or unknown prefixes
                warn!("[notebook-sync] Auto-launch: unexpected env_source '{}', creating UV env on-demand", other);
                match daemon.create_uv_env_on_demand().await {
                    Ok(env) => env,
                    Err(e) => {
                        error!(
                            "[notebook-sync] Auto-launch failed: could not create UV env: {}",
                            e
                        );
                        let _ = room
                            .kernel_broadcast_tx
                            .send(NotebookBroadcast::KernelStatus {
                                status: format!("error: {}", e),
                                cell_id: None,
                            });
                        return;
                    }
                }
            }
        }
    };

    // Launch kernel (default to python)
    let kernel_type = "python";
    match kernel
        .launch(
            kernel_type,
            &env_source,
            notebook_path_opt.as_deref(),
            Some(pooled_env),
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
                                info!("[notebook-sync] Processing ExecutionDone for {}", cell_id);
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
    info!("[notebook-sync] Handling request: {:?}", request);

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
                    };
                }
            }

            // Clear any stale comm state from a previous kernel (in case it crashed)
            room.comm_state.clear().await;

            // Create new kernel
            let mut kernel = RoomKernel::new(
                room.kernel_broadcast_tx.clone(),
                room.doc.clone(),
                room.persist_path.clone(),
                room.changed_tx.clone(),
                room.blob_store.clone(),
                room.comm_state.clone(),
            );
            let notebook_path = notebook_path.map(std::path::PathBuf::from);

            // Auto-detect environment if env_source is "auto" or empty
            let resolved_env_source =
                if env_source == "auto" || env_source.is_empty() || env_source == "prewarmed" {
                    // Priority 1: Check inline deps in notebook metadata
                    if let Some(inline_source) =
                        notebook_path.as_ref().and_then(|p| check_inline_deps(p))
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

            // For Python kernels, acquire pooled env or create on-demand
            // For non-Python kernels (e.g., Deno), use kernelspec (no pooled env)
            let pooled_env: Option<crate::PooledEnv> = if kernel_type == "python" {
                Some(match resolved_env_source.as_str() {
                    "uv:prewarmed" | "uv:inline" | "uv:pyproject" => {
                        match daemon.take_uv_env().await {
                            Some(env) => {
                                info!(
                                    "[notebook-sync] LaunchKernel: acquired UV env from pool: {:?}",
                                    env.python_path
                                );
                                env
                            }
                            None => {
                                info!("[notebook-sync] LaunchKernel: UV pool empty, creating env on-demand");
                                match daemon.create_uv_env_on_demand().await {
                                    Ok(env) => {
                                        info!(
                                        "[notebook-sync] LaunchKernel: created UV env on-demand: {:?}",
                                        env.python_path
                                    );
                                        env
                                    }
                                    Err(e) => {
                                        error!("[notebook-sync] LaunchKernel failed: could not create UV env: {}", e);
                                        let _ = room.kernel_broadcast_tx.send(
                                            NotebookBroadcast::KernelStatus {
                                                status: format!("error: {}", e),
                                                cell_id: None,
                                            },
                                        );
                                        return NotebookResponse::Error {
                                            error: format!(
                                                "Failed to create UV environment: {}",
                                                e
                                            ),
                                        };
                                    }
                                }
                            }
                        }
                    }
                    other if other.starts_with("conda:") => match daemon.take_conda_env().await {
                        Some(env) => {
                            info!(
                                "[notebook-sync] LaunchKernel: acquired Conda env from pool: {:?}",
                                env.python_path
                            );
                            env
                        }
                        None => {
                            info!("[notebook-sync] LaunchKernel: Conda pool empty, creating env on-demand");
                            match daemon.create_conda_env_on_demand().await {
                                Ok(env) => {
                                    info!(
                                            "[notebook-sync] LaunchKernel: created Conda env on-demand: {:?}",
                                            env.python_path
                                        );
                                    env
                                }
                                Err(e) => {
                                    error!("[notebook-sync] LaunchKernel failed: could not create Conda env: {}", e);
                                    let _ = room.kernel_broadcast_tx.send(
                                        NotebookBroadcast::KernelStatus {
                                            status: format!("error: {}", e),
                                            cell_id: None,
                                        },
                                    );
                                    return NotebookResponse::Error {
                                        error: format!("Failed to create Conda environment: {}", e),
                                    };
                                }
                            }
                        }
                    },
                    _ => {
                        // For unknown env_source, default to UV on-demand
                        warn!("[notebook-sync] LaunchKernel: unknown env_source '{}', creating UV env on-demand", resolved_env_source);
                        match daemon.create_uv_env_on_demand().await {
                            Ok(env) => env,
                            Err(e) => {
                                error!(
                                    "[notebook-sync] LaunchKernel failed: could not create env: {}",
                                    e
                                );
                                let _ = room.kernel_broadcast_tx.send(
                                    NotebookBroadcast::KernelStatus {
                                        status: format!("error: {}", e),
                                        cell_id: None,
                                    },
                                );
                                return NotebookResponse::Error {
                                    error: format!("Failed to create environment: {}", e),
                                };
                            }
                        }
                    }
                })
            } else {
                // Non-Python kernels (e.g., Deno) don't use pooled environments
                info!(
                    "[notebook-sync] LaunchKernel: {} kernel uses kernelspec (no pooled env)",
                    kernel_type
                );
                None
            };

            match kernel
                .launch(
                    &kernel_type,
                    &resolved_env_source,
                    notebook_path.as_deref(),
                    pooled_env,
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
                    }
                }
                Err(e) => NotebookResponse::Error {
                    error: format!("Failed to launch kernel: {}", e),
                },
            }
        }

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

            // 2. Persist outside the write lock
            persist_notebook_bytes(&persist_bytes, &room.persist_path);

            // 3. Broadcast for cross-window UI sync (fast path)
            let _ = room
                .kernel_broadcast_tx
                .send(NotebookBroadcast::OutputsCleared {
                    cell_id: cell_id.clone(),
                });

            // 4. Update kernel's internal tracking if kernel exists
            let kernel_guard = room.kernel.lock().await;
            if let Some(ref kernel) = *kernel_guard {
                kernel.clear_outputs(&cell_id);
            }

            NotebookResponse::OutputsCleared { cell_id }
        }

        NotebookRequest::InterruptExecution {} => {
            let kernel_guard = room.kernel.lock().await;
            if let Some(ref kernel) = *kernel_guard {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Create a test blob store in the given temp directory.
    fn test_blob_store(tmp: &tempfile::TempDir) -> Arc<BlobStore> {
        Arc::new(BlobStore::new(tmp.path().join("blobs")))
    }

    #[test]
    fn test_room_load_or_create_new() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let room = NotebookRoom::load_or_create("test-nb", tmp.path(), blob_store);

        let doc = room.doc.try_read().unwrap();
        assert_eq!(doc.notebook_id(), Some("test-nb".to_string()));
        assert_eq!(doc.cell_count(), 0);
        assert_eq!(room.active_peers.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_room_persists_and_reloads() {
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

    #[test]
    fn test_get_or_create_room_reuses_existing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let mut rooms = HashMap::new();

        let room1 = get_or_create_room(&mut rooms, "nb1", tmp.path(), blob_store.clone());
        let room2 = get_or_create_room(&mut rooms, "nb1", tmp.path(), blob_store);

        // Should be the same Arc (same room)
        assert!(Arc::ptr_eq(&room1, &room2));
    }

    #[test]
    fn test_get_or_create_room_different_notebooks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let mut rooms = HashMap::new();

        let room1 = get_or_create_room(&mut rooms, "nb1", tmp.path(), blob_store.clone());
        let room2 = get_or_create_room(&mut rooms, "nb2", tmp.path(), blob_store);

        // Should be different rooms
        assert!(!Arc::ptr_eq(&room1, &room2));
        assert_eq!(rooms.len(), 2);
    }

    #[test]
    fn test_room_peer_counting() {
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

    #[test]
    fn test_new_fresh_creates_empty_doc() {
        let tmp = tempfile::TempDir::new().unwrap();
        let blob_store = test_blob_store(&tmp);
        let room = NotebookRoom::new_fresh("fresh-test", tmp.path(), blob_store);

        let doc = room.doc.try_read().unwrap();
        assert_eq!(doc.notebook_id(), Some("fresh-test".to_string()));
        assert_eq!(doc.cell_count(), 0);
    }

    #[test]
    fn test_new_fresh_deletes_stale_persisted_doc() {
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

    #[test]
    fn test_check_inline_deps_uv() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        // Notebook with UV deps
        let uv_path = dir.path().join("uv.ipynb");
        let mut f = std::fs::File::create(&uv_path).unwrap();
        writeln!(
            f,
            r#"{{"metadata": {{"uv": {{"dependencies": ["numpy"]}}}}, "cells": []}}"#
        )
        .unwrap();
        assert_eq!(check_inline_deps(&uv_path), Some("uv:inline".to_string()));
    }

    #[test]
    fn test_check_inline_deps_conda() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        // Notebook with conda deps
        let conda_path = dir.path().join("conda.ipynb");
        let mut f = std::fs::File::create(&conda_path).unwrap();
        writeln!(
            f,
            r#"{{"metadata": {{"conda": {{"dependencies": ["pandas"]}}}}, "cells": []}}"#
        )
        .unwrap();
        assert_eq!(
            check_inline_deps(&conda_path),
            Some("conda:inline".to_string())
        );
    }

    #[test]
    fn test_check_inline_deps_empty() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        // Notebook with no deps
        let empty_path = dir.path().join("empty.ipynb");
        let mut f = std::fs::File::create(&empty_path).unwrap();
        writeln!(f, r#"{{"metadata": {{}}, "cells": []}}"#).unwrap();
        assert_eq!(check_inline_deps(&empty_path), None);
    }

    #[test]
    fn test_check_inline_deps_empty_array() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        // Notebook with empty deps array - should return None
        let path = dir.path().join("empty-array.ipynb");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"metadata": {{"uv": {{"dependencies": []}}}}, "cells": []}}"#
        )
        .unwrap();
        assert_eq!(check_inline_deps(&path), None);
    }

    #[test]
    fn test_check_inline_deps_uv_priority() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        // Notebook with both UV and conda deps - UV takes priority
        let path = dir.path().join("both.ipynb");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"metadata": {{"uv": {{"dependencies": ["numpy"]}}, "conda": {{"dependencies": ["pandas"]}}}}, "cells": []}}"#
        )
        .unwrap();
        assert_eq!(check_inline_deps(&path), Some("uv:inline".to_string()));
    }

    #[test]
    fn test_check_inline_deps_nonexistent_file() {
        let path = std::path::PathBuf::from("/nonexistent/path/to/notebook.ipynb");
        assert_eq!(check_inline_deps(&path), None);
    }

    #[test]
    fn test_check_inline_deps_runt_uv() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        // Notebook with UV deps under runt namespace
        let path = dir.path().join("runt-uv.ipynb");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"metadata": {{"runt": {{"uv": {{"dependencies": ["numpy"]}}}}}}, "cells": []}}"#
        )
        .unwrap();
        assert_eq!(check_inline_deps(&path), Some("uv:inline".to_string()));
    }

    #[test]
    fn test_check_inline_deps_runt_conda() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        // Notebook with conda deps under runt namespace
        let path = dir.path().join("runt-conda.ipynb");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"metadata": {{"runt": {{"conda": {{"dependencies": ["pandas"]}}}}}}, "cells": []}}"#
        )
        .unwrap();
        assert_eq!(check_inline_deps(&path), Some("conda:inline".to_string()));
    }

    #[test]
    fn test_check_inline_deps_runt_takes_precedence() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();

        // Notebook with both runt.uv and legacy uv - runt should win
        let path = dir.path().join("mixed.ipynb");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(
            f,
            r#"{{"metadata": {{"runt": {{"uv": {{"dependencies": ["torch"]}}}}, "uv": {{"dependencies": ["numpy"]}}}}, "cells": []}}"#
        )
        .unwrap();
        // runt.uv should be checked first, so we get "uv:inline"
        assert_eq!(check_inline_deps(&path), Some("uv:inline".to_string()));
    }
}
