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
use crate::notebook_metadata::{NotebookMetadataSnapshot, NOTEBOOK_METADATA_KEY};
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
                info!("[notebook-sync] Resolved metadata snapshot from Automerge doc");
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
                    info!("[notebook-sync] Resolved metadata snapshot from disk (doc not yet populated)");
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
    default_runtime: crate::runtime::Runtime,
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

    // Resolve metadata snapshot: try Automerge doc first, fall back to disk
    let metadata_snapshot = resolve_metadata_snapshot(room, notebook_path_opt.as_deref()).await;

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
    let project_source = notebook_path_opt
        .as_ref()
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

    match kernel
        .launch(
            kernel_type,
            &env_source,
            notebook_path_opt.as_deref(),
            pooled_env,
            inline_deps,
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

            // Auto-detect environment if env_source is "auto" or empty
            let resolved_env_source =
                if env_source == "auto" || env_source.is_empty() || env_source == "prewarmed" {
                    // Priority 1: Check inline deps in notebook metadata
                    if let Some(inline_source) =
                        metadata_snapshot.as_ref().and_then(check_inline_deps)
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

            match kernel
                .launch(
                    &resolved_kernel_type,
                    &resolved_env_source,
                    notebook_path.as_deref(),
                    pooled_env,
                    inline_deps,
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

        NotebookRequest::SaveNotebook { format_cells: _ } => {
            // TODO: format_cells support (requires ruff/deno formatter access)
            match save_notebook_to_disk(room).await {
                Ok(()) => NotebookResponse::NotebookSaved {},
                Err(e) => NotebookResponse::Error {
                    error: format!("Failed to save notebook: {e}"),
                },
            }
        }
    }
}

/// Save the notebook from the Automerge doc to disk as .ipynb.
///
/// 1. Read existing .ipynb from disk (if it exists) to preserve unknown metadata
/// 2. Read cells and metadata from the Automerge doc
/// 3. Merge metadata: replace kernelspec, language_info, runt; preserve everything else
/// 4. Reconstruct cells: source and outputs from Automerge, cell metadata from existing file
/// 5. Write the merged notebook to disk
async fn save_notebook_to_disk(room: &NotebookRoom) -> Result<(), String> {
    let notebook_path = &room.notebook_path;

    // Read existing .ipynb to preserve unknown metadata and cell metadata
    // Distinguish between file-not-found (ok, create new) and parse errors (warn, continue)
    let existing: Option<serde_json::Value> = match tokio::fs::read_to_string(notebook_path).await {
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
    tokio::fs::write(notebook_path, content_with_newline)
        .await
        .map_err(|e| format!("Failed to write notebook: {e}"))?;

    info!(
        "[notebook-sync] Saved notebook to disk: {:?} ({} cells)",
        notebook_path, cell_count
    );

    Ok(())
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
            },
        };
        assert_eq!(check_inline_deps(&snapshot), Some("deno".to_string()));
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

        let room = NotebookRoom {
            doc: Arc::new(RwLock::new(doc)),
            changed_tx,
            kernel_broadcast_tx,
            persist_path: tmp.path().join("doc.automerge"),
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
            auto_launch_at: Arc::new(RwLock::new(None)),
            comm_state: Arc::new(crate::comm_state::CommState::new()),
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
        save_notebook_to_disk(&room).await.unwrap();

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

        save_notebook_to_disk(&room).await.unwrap();

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

        save_notebook_to_disk(&room).await.unwrap();

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

        save_notebook_to_disk(&room).await.unwrap();

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
}
