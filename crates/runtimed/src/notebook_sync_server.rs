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
use log::{info, warn};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::connection::{self, NotebookFrameType};
use crate::kernel_manager::RoomKernel;
use crate::notebook_doc::{notebook_doc_filename, NotebookDoc};
use crate::protocol::{NotebookBroadcast, NotebookRequest, NotebookResponse};

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
    pub fn new_fresh(notebook_id: &str, docs_dir: &Path) -> Self {
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
        Self {
            doc: Arc::new(RwLock::new(doc)),
            changed_tx,
            kernel_broadcast_tx,
            persist_path,
            active_peers: AtomicUsize::new(0),
            kernel: Arc::new(Mutex::new(None)),
        }
    }

    /// Create a new room by loading a persisted document or creating a fresh one.
    ///
    /// Note: This method is kept for tests that verify persistence behavior.
    /// For normal operation, `new_fresh` is used to ensure the .ipynb file
    /// is the source of truth.
    #[cfg(test)]
    pub fn load_or_create(notebook_id: &str, docs_dir: &Path) -> Self {
        let filename = notebook_doc_filename(notebook_id);
        let persist_path = docs_dir.join(filename);
        let doc = NotebookDoc::load_or_create(&persist_path, notebook_id);
        let (changed_tx, _) = broadcast::channel(16);
        let (kernel_broadcast_tx, _) = broadcast::channel(64);
        Self {
            doc: Arc::new(RwLock::new(doc)),
            changed_tx,
            kernel_broadcast_tx,
            persist_path,
            active_peers: AtomicUsize::new(0),
            kernel: Arc::new(Mutex::new(None)),
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
) -> Arc<NotebookRoom> {
    rooms
        .entry(notebook_id.to_string())
        .or_insert_with(|| {
            info!("[notebook-sync] Creating room for {}", notebook_id);
            Arc::new(NotebookRoom::new_fresh(notebook_id, docs_dir))
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
pub async fn handle_notebook_sync_connection<R, W>(
    mut reader: R,
    mut writer: W,
    room: Arc<NotebookRoom>,
    rooms: NotebookRooms,
    notebook_id: String,
    use_typed_frames: bool,
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

    // For v2 protocol, send capabilities response first
    if use_typed_frames {
        let caps = connection::ProtocolCapabilities {
            protocol: connection::PROTOCOL_V2.to_string(),
        };
        connection::send_json_frame(&mut writer, &caps).await?;
    }

    let result = if use_typed_frames {
        run_sync_loop_v2(&mut reader, &mut writer, &room).await
    } else {
        run_sync_loop_v1(&mut reader, &mut writer, &room).await
    };

    // Peer disconnected — decrement and possibly evict the room
    let remaining = room.active_peers.fetch_sub(1, Ordering::Relaxed) - 1;
    if remaining == 0 {
        let mut rooms_guard = rooms.lock().await;
        // Re-check under the lock — another peer may have joined between
        // our decrement and acquiring the lock.
        if room.active_peers.load(Ordering::Relaxed) == 0 {
            rooms_guard.remove(&notebook_id);
            info!(
                "[notebook-sync] Evicted room {} (no remaining peers)",
                notebook_id
            );
        }
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
                                let response = handle_notebook_request(room, request).await;
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

/// Handle a NotebookRequest and return a NotebookResponse.
async fn handle_notebook_request(
    room: &NotebookRoom,
    request: NotebookRequest,
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

            // Create new kernel
            let mut kernel = RoomKernel::new(
                room.kernel_broadcast_tx.clone(),
                room.doc.clone(),
                room.persist_path.clone(),
                room.changed_tx.clone(),
            );
            let notebook_path = notebook_path.map(std::path::PathBuf::from);

            // Auto-detect environment if env_source is "auto" or empty
            let resolved_env_source =
                if env_source == "auto" || env_source.is_empty() || env_source == "prewarmed" {
                    // Detect project files near notebook path
                    notebook_path
                        .as_ref()
                        .and_then(|path| crate::project_file::detect_project_file(path))
                        .map(|detected| {
                            info!(
                                "[notebook-sync] Auto-detected project file: {:?} -> {}",
                                detected.path,
                                detected.to_env_source()
                            );
                            detected.to_env_source().to_string()
                        })
                        .unwrap_or_else(|| {
                            info!("[notebook-sync] No project file detected, using prewarmed");
                            "uv:prewarmed".to_string()
                        })
                } else {
                    // Use explicit env_source (e.g., "uv:inline", "conda:inline")
                    env_source.clone()
                };

            match kernel
                .launch(&kernel_type, &resolved_env_source, notebook_path.as_deref())
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

    #[test]
    fn test_room_load_or_create_new() {
        let tmp = tempfile::TempDir::new().unwrap();
        let room = NotebookRoom::load_or_create("test-nb", tmp.path());

        let doc = room.doc.try_read().unwrap();
        assert_eq!(doc.notebook_id(), Some("test-nb".to_string()));
        assert_eq!(doc.cell_count(), 0);
        assert_eq!(room.active_peers.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn test_room_persists_and_reloads() {
        let tmp = tempfile::TempDir::new().unwrap();

        // Create room and add a cell
        {
            let room = NotebookRoom::load_or_create("persist-test", tmp.path());
            let mut doc = room.doc.try_write().unwrap();
            doc.add_cell(0, "c1", "code").unwrap();
            doc.update_source("c1", "hello").unwrap();
            let bytes = doc.save();
            persist_notebook_bytes(&bytes, &room.persist_path);
        }

        // Load again — should have the cell
        {
            let room = NotebookRoom::load_or_create("persist-test", tmp.path());
            let doc = room.doc.try_read().unwrap();
            assert_eq!(doc.cell_count(), 1);
            let cell = doc.get_cell("c1").unwrap();
            assert_eq!(cell.source, "hello");
        }
    }

    #[test]
    fn test_get_or_create_room_reuses_existing() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut rooms = HashMap::new();

        let room1 = get_or_create_room(&mut rooms, "nb1", tmp.path());
        let room2 = get_or_create_room(&mut rooms, "nb1", tmp.path());

        // Should be the same Arc (same room)
        assert!(Arc::ptr_eq(&room1, &room2));
    }

    #[test]
    fn test_get_or_create_room_different_notebooks() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut rooms = HashMap::new();

        let room1 = get_or_create_room(&mut rooms, "nb1", tmp.path());
        let room2 = get_or_create_room(&mut rooms, "nb2", tmp.path());

        // Should be different rooms
        assert!(!Arc::ptr_eq(&room1, &room2));
        assert_eq!(rooms.len(), 2);
    }

    #[test]
    fn test_room_peer_counting() {
        let tmp = tempfile::TempDir::new().unwrap();
        let room = NotebookRoom::load_or_create("peer-test", tmp.path());

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
        let room = NotebookRoom::new_fresh("fresh-test", tmp.path());

        let doc = room.doc.try_read().unwrap();
        assert_eq!(doc.notebook_id(), Some("fresh-test".to_string()));
        assert_eq!(doc.cell_count(), 0);
    }

    #[test]
    fn test_new_fresh_deletes_stale_persisted_doc() {
        let tmp = tempfile::TempDir::new().unwrap();

        // Create and persist a room with content using load_or_create
        {
            let room = NotebookRoom::load_or_create("stale-test", tmp.path());
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
        let room = NotebookRoom::new_fresh("stale-test", tmp.path());

        // Persisted file should be deleted
        assert!(
            !persist_path.exists(),
            "Persisted file should be deleted by new_fresh"
        );

        // Room should be empty (no cells from persisted doc)
        let doc = room.doc.try_read().unwrap();
        assert_eq!(doc.cell_count(), 0, "new_fresh should start with empty doc");
    }
}
