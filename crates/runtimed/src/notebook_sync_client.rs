//! Client for the notebook sync service.
//!
//! Each notebook window creates a `NotebookSyncClient` that maintains a local
//! Automerge document replica of the notebook. Changes made locally are sent
//! to the daemon, and changes from other peers arrive as sync messages.
//!
//! The client uses a split pattern with channels:
//! - `NotebookSyncHandle` is a clonable handle for sending commands
//! - `NotebookSyncReceiver` receives incoming changes from other peers
//! - A background task owns the actual connection and Automerge state
//!
//! This design avoids holding locks during network I/O.

use std::path::PathBuf;
use std::time::Duration;

use automerge::sync::{self, SyncDoc};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ReadDoc};
use futures::FutureExt;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};

use crate::connection::{self, Handshake, NotebookFrameType, ProtocolCapabilities, PROTOCOL_V2};
use crate::notebook_doc::{
    get_cells_from_doc, get_metadata_from_doc, set_metadata_in_doc, CellSnapshot,
};
use crate::notebook_metadata::NOTEBOOK_METADATA_KEY;
use crate::protocol::{NotebookBroadcast, NotebookRequest, NotebookResponse};

/// Error type for notebook sync client operations.
#[derive(Debug, thiserror::Error)]
pub enum NotebookSyncError {
    #[error("Failed to connect: {0}")]
    ConnectionFailed(#[from] std::io::Error),

    #[error("Sync protocol error: {0}")]
    SyncError(String),

    #[error("Connection timeout")]
    Timeout,

    #[error("Disconnected")]
    Disconnected,

    #[error("Cell not found: {0}")]
    CellNotFound(String),

    #[error("Channel closed")]
    ChannelClosed,
}

/// Commands sent from handles to the sync task.
#[derive(Debug)]
enum SyncCommand {
    AddCell {
        index: usize,
        cell_id: String,
        cell_type: String,
        reply: oneshot::Sender<Result<(), NotebookSyncError>>,
    },
    DeleteCell {
        cell_id: String,
        reply: oneshot::Sender<Result<(), NotebookSyncError>>,
    },
    UpdateSource {
        cell_id: String,
        source: String,
        reply: oneshot::Sender<Result<(), NotebookSyncError>>,
    },
    ClearOutputs {
        cell_id: String,
        reply: oneshot::Sender<Result<(), NotebookSyncError>>,
    },
    AppendOutput {
        cell_id: String,
        output: String,
        reply: oneshot::Sender<Result<(), NotebookSyncError>>,
    },
    SetExecutionCount {
        cell_id: String,
        count: String,
        reply: oneshot::Sender<Result<(), NotebookSyncError>>,
    },
    GetCells {
        reply: oneshot::Sender<Vec<CellSnapshot>>,
    },
    /// Set a metadata value in the Automerge doc and sync to daemon.
    SetMetadata {
        key: String,
        value: String,
        reply: oneshot::Sender<Result<(), NotebookSyncError>>,
    },
    /// Read a metadata value from the local Automerge doc replica.
    GetMetadata {
        key: String,
        reply: oneshot::Sender<Option<String>>,
    },
    /// Send a request to the daemon and wait for a response.
    /// Only works with v2 protocol; returns error on v1.
    SendRequest {
        request: NotebookRequest,
        reply: oneshot::Sender<Result<NotebookResponse, NotebookSyncError>>,
        /// Optional broadcast sender for delivering broadcasts during long-running requests.
        /// If provided, broadcasts received while waiting for the response are sent immediately
        /// instead of being queued for later delivery.
        broadcast_tx: Option<mpsc::Sender<NotebookBroadcast>>,
    },
}

/// Handle for sending commands to the notebook sync task.
///
/// This is clonable and can be shared across threads. Commands are sent
/// through a channel and processed by the background sync task.
#[derive(Clone)]
pub struct NotebookSyncHandle {
    tx: mpsc::Sender<SyncCommand>,
    notebook_id: String,
}

impl NotebookSyncHandle {
    /// Get the notebook ID this handle is connected to.
    pub fn notebook_id(&self) -> &str {
        &self.notebook_id
    }

    /// Get all cells from the local replica.
    pub async fn get_cells(&self) -> Result<Vec<CellSnapshot>, NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::GetCells { reply: reply_tx })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx.await.map_err(|_| NotebookSyncError::ChannelClosed)
    }

    /// Add a new cell at the given index.
    pub async fn add_cell(
        &self,
        index: usize,
        cell_id: &str,
        cell_type: &str,
    ) -> Result<(), NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::AddCell {
                index,
                cell_id: cell_id.to_string(),
                cell_type: cell_type.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
    }

    /// Delete a cell by ID.
    pub async fn delete_cell(&self, cell_id: &str) -> Result<(), NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::DeleteCell {
                cell_id: cell_id.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
    }

    /// Update a cell's source text.
    pub async fn update_source(
        &self,
        cell_id: &str,
        source: &str,
    ) -> Result<(), NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::UpdateSource {
                cell_id: cell_id.to_string(),
                source: source.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
    }

    /// Clear all outputs for a cell.
    pub async fn clear_outputs(&self, cell_id: &str) -> Result<(), NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::ClearOutputs {
                cell_id: cell_id.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
    }

    /// Append an output to a cell.
    pub async fn append_output(
        &self,
        cell_id: &str,
        output: &str,
    ) -> Result<(), NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::AppendOutput {
                cell_id: cell_id.to_string(),
                output: output.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
    }

    /// Set execution count for a cell.
    pub async fn set_execution_count(
        &self,
        cell_id: &str,
        count: &str,
    ) -> Result<(), NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::SetExecutionCount {
                cell_id: cell_id.to_string(),
                count: count.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
    }

    /// Set a metadata value in the Automerge doc and sync to daemon.
    pub async fn set_metadata(&self, key: &str, value: &str) -> Result<(), NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::SetMetadata {
                key: key.to_string(),
                value: value.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
    }

    /// Read a metadata value from the local Automerge doc replica.
    pub async fn get_metadata(&self, key: &str) -> Result<Option<String>, NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::GetMetadata {
                key: key.to_string(),
                reply: reply_tx,
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx.await.map_err(|_| NotebookSyncError::ChannelClosed)
    }

    /// Send a request to the daemon and wait for a response.
    ///
    /// This only works with v2 protocol. If the daemon is running v1,
    /// this will return an error.
    pub async fn send_request(
        &self,
        request: NotebookRequest,
    ) -> Result<NotebookResponse, NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::SendRequest {
                request,
                reply: reply_tx,
                broadcast_tx: None,
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
    }

    /// Send a request to the daemon, delivering broadcasts immediately as they arrive.
    ///
    /// This is useful for long-running requests (like `LaunchKernel`) where you want
    /// progress updates delivered in real-time instead of being queued until the
    /// response is received.
    pub async fn send_request_with_broadcast(
        &self,
        request: NotebookRequest,
        broadcast_tx: mpsc::Sender<NotebookBroadcast>,
    ) -> Result<NotebookResponse, NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::SendRequest {
                request,
                reply: reply_tx,
                broadcast_tx: Some(broadcast_tx),
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
    }
}

/// Receiver for incoming changes from other peers.
///
/// An update received from the Automerge sync protocol.
///
/// Contains cells (always present after any sync) and optionally the
/// notebook metadata snapshot if it changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncUpdate {
    /// Current cell state after applying the sync message.
    pub cells: Vec<CellSnapshot>,
    /// JSON-serialized `NotebookMetadataSnapshot`, present when metadata changed.
    pub notebook_metadata: Option<String>,
}

/// This is separate from the handle to allow receiving changes independently
/// of sending commands. Call `recv()` to wait for the next batch of changes.
pub struct NotebookSyncReceiver {
    rx: mpsc::Receiver<SyncUpdate>,
}

impl NotebookSyncReceiver {
    /// Wait for the next sync update from other peers.
    ///
    /// Returns `None` if the sync task has stopped.
    pub async fn recv(&mut self) -> Option<SyncUpdate> {
        self.rx.recv().await
    }
}

/// Receiver for kernel broadcast events from the daemon.
///
/// These are events like kernel status changes, execution outputs, etc.
/// that are broadcast to all clients connected to the same notebook room.
pub struct NotebookBroadcastReceiver {
    rx: mpsc::Receiver<NotebookBroadcast>,
}

impl NotebookBroadcastReceiver {
    /// Wait for the next broadcast event.
    ///
    /// Returns `None` if the sync task has stopped.
    pub async fn recv(&mut self) -> Option<NotebookBroadcast> {
        self.rx.recv().await
    }
}

/// Client for the notebook sync service.
///
/// Holds a local Automerge document replica that stays in sync with the
/// daemon's canonical copy for a specific notebook.
pub struct NotebookSyncClient<S> {
    doc: AutoCommit,
    peer_state: sync::State,
    stream: S,
    notebook_id: String,
    /// Whether to use typed frames (v2 protocol) or raw frames (v1).
    /// Determined during connection based on server capabilities.
    use_typed_frames: bool,
    /// Broadcasts received during initial sync (before split).
    /// These are delivered immediately after into_split creates the channels.
    pending_broadcasts: Vec<NotebookBroadcast>,
}

#[cfg(unix)]
impl NotebookSyncClient<tokio::net::UnixStream> {
    /// Connect to the daemon and join the notebook room.
    pub async fn connect(
        socket_path: PathBuf,
        notebook_id: String,
    ) -> Result<Self, NotebookSyncError> {
        Self::connect_with_options(socket_path, notebook_id, Duration::from_secs(2), None).await
    }

    /// Connect with a custom timeout.
    pub async fn connect_with_timeout(
        socket_path: PathBuf,
        notebook_id: String,
        timeout: Duration,
    ) -> Result<Self, NotebookSyncError> {
        Self::connect_with_options(socket_path, notebook_id, timeout, None).await
    }

    /// Connect with custom timeout and working directory for untitled notebooks.
    pub async fn connect_with_options(
        socket_path: PathBuf,
        notebook_id: String,
        timeout: Duration,
        working_dir: Option<PathBuf>,
    ) -> Result<Self, NotebookSyncError> {
        let stream = tokio::time::timeout(timeout, tokio::net::UnixStream::connect(&socket_path))
            .await
            .map_err(|_| NotebookSyncError::Timeout)?
            .map_err(NotebookSyncError::ConnectionFailed)?;

        info!(
            "[notebook-sync-client] Connected to {:?} for {} (working_dir: {:?})",
            socket_path, notebook_id, working_dir
        );

        Self::init(stream, notebook_id, working_dir).await
    }

    /// Connect and return split handle/receiver for concurrent send/receive.
    ///
    /// This is the preferred API for use in applications. The returned handle
    /// can be cloned and used from multiple tasks to send commands. The receiver
    /// should be polled in a dedicated task to receive changes from other peers.
    /// The broadcast receiver receives kernel events from the daemon.
    pub async fn connect_split(
        socket_path: PathBuf,
        notebook_id: String,
    ) -> Result<
        (
            NotebookSyncHandle,
            NotebookSyncReceiver,
            NotebookBroadcastReceiver,
            Vec<CellSnapshot>,
            Option<String>,
        ),
        NotebookSyncError,
    > {
        Self::connect_split_with_options(socket_path, notebook_id, None).await
    }

    /// Connect and return split handle/receiver with working directory for untitled notebooks.
    pub async fn connect_split_with_options(
        socket_path: PathBuf,
        notebook_id: String,
        working_dir: Option<PathBuf>,
    ) -> Result<
        (
            NotebookSyncHandle,
            NotebookSyncReceiver,
            NotebookBroadcastReceiver,
            Vec<CellSnapshot>,
            Option<String>,
        ),
        NotebookSyncError,
    > {
        let client = Self::connect_with_options(
            socket_path,
            notebook_id,
            Duration::from_secs(2),
            working_dir,
        )
        .await?;
        Ok(client.into_split())
    }
}

#[cfg(windows)]
impl NotebookSyncClient<tokio::net::windows::named_pipe::NamedPipeClient> {
    /// Connect to the daemon and join the notebook room.
    pub async fn connect(
        socket_path: PathBuf,
        notebook_id: String,
    ) -> Result<Self, NotebookSyncError> {
        Self::connect_with_options(socket_path, notebook_id, None).await
    }

    /// Connect with working directory for untitled notebooks.
    pub async fn connect_with_options(
        socket_path: PathBuf,
        notebook_id: String,
        working_dir: Option<PathBuf>,
    ) -> Result<Self, NotebookSyncError> {
        let pipe_name = socket_path.to_string_lossy().to_string();
        let client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&pipe_name)
            .map_err(NotebookSyncError::ConnectionFailed)?;
        Self::init(client, notebook_id, working_dir).await
    }

    /// Connect and return split handle/receiver for concurrent send/receive.
    pub async fn connect_split(
        socket_path: PathBuf,
        notebook_id: String,
    ) -> Result<
        (
            NotebookSyncHandle,
            NotebookSyncReceiver,
            NotebookBroadcastReceiver,
            Vec<CellSnapshot>,
            Option<String>,
        ),
        NotebookSyncError,
    > {
        Self::connect_split_with_options(socket_path, notebook_id, None).await
    }

    /// Connect and return split handle/receiver with working directory for untitled notebooks.
    pub async fn connect_split_with_options(
        socket_path: PathBuf,
        notebook_id: String,
        working_dir: Option<PathBuf>,
    ) -> Result<
        (
            NotebookSyncHandle,
            NotebookSyncReceiver,
            NotebookBroadcastReceiver,
            Vec<CellSnapshot>,
            Option<String>,
        ),
        NotebookSyncError,
    > {
        let client = Self::connect_with_options(socket_path, notebook_id, working_dir).await?;
        Ok(client.into_split())
    }
}

impl<S> NotebookSyncClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Initialize the client by sending the handshake and performing initial sync.
    ///
    /// The client requests the v2 protocol (typed frames) in the handshake.
    /// If the server supports v2, it responds with a ProtocolCapabilities frame.
    /// Old servers (v1) ignore the protocol field and send raw Automerge frames.
    /// The client detects which protocol to use based on the first response.
    ///
    /// The `working_dir` parameter is used for untitled notebooks to provide
    /// the directory context for project file detection (pyproject.toml, etc).
    async fn init(
        mut stream: S,
        notebook_id: String,
        working_dir: Option<PathBuf>,
    ) -> Result<Self, NotebookSyncError> {
        // Send the channel handshake, requesting v2 protocol
        connection::send_json_frame(
            &mut stream,
            &Handshake::NotebookSync {
                notebook_id: notebook_id.clone(),
                protocol: Some(PROTOCOL_V2.to_string()),
                working_dir: working_dir.map(|p| p.to_string_lossy().to_string()),
            },
        )
        .await
        .map_err(|e| NotebookSyncError::SyncError(format!("handshake: {}", e)))?;

        let mut doc = AutoCommit::new();
        let mut peer_state = sync::State::new();

        // Read first frame to detect protocol version
        // v2 servers send ProtocolCapabilities JSON first
        // v1 servers send raw Automerge sync message first
        let first_frame = connection::recv_frame(&mut stream)
            .await?
            .ok_or(NotebookSyncError::Disconnected)?;

        // Try to parse as ProtocolCapabilities (v2 server)
        let use_typed_frames = match serde_json::from_slice::<ProtocolCapabilities>(&first_frame) {
            Ok(caps) if caps.protocol == PROTOCOL_V2 => {
                info!(
                    "[notebook-sync-client] Server supports v2 protocol for {}",
                    notebook_id
                );
                true
            }
            _ => {
                // Not valid capabilities JSON — this is a raw Automerge frame from v1 server
                // Process it as the initial sync message
                info!(
                    "[notebook-sync-client] Server uses v1 protocol for {}",
                    notebook_id
                );
                let message = sync::Message::decode(&first_frame)
                    .map_err(|e| NotebookSyncError::SyncError(format!("decode: {}", e)))?;
                doc.sync()
                    .receive_sync_message(&mut peer_state, message)
                    .map_err(|e| NotebookSyncError::SyncError(format!("receive: {}", e)))?;
                false
            }
        };

        // For v2 protocol, read the first typed frame (Automerge sync)
        if use_typed_frames {
            match connection::recv_typed_frame(&mut stream).await? {
                Some(frame) => {
                    if frame.frame_type != NotebookFrameType::AutomergeSync {
                        return Err(NotebookSyncError::SyncError(format!(
                            "expected AutomergeSync frame, got {:?}",
                            frame.frame_type
                        )));
                    }
                    let message = sync::Message::decode(&frame.payload)
                        .map_err(|e| NotebookSyncError::SyncError(format!("decode: {}", e)))?;
                    doc.sync()
                        .receive_sync_message(&mut peer_state, message)
                        .map_err(|e| NotebookSyncError::SyncError(format!("receive: {}", e)))?;
                }
                None => return Err(NotebookSyncError::Disconnected),
            }
        }

        // Send our sync message back using the negotiated protocol
        if let Some(msg) = doc.sync().generate_sync_message(&mut peer_state) {
            if use_typed_frames {
                connection::send_typed_frame(
                    &mut stream,
                    NotebookFrameType::AutomergeSync,
                    &msg.encode(),
                )
                .await?;
            } else {
                connection::send_frame(&mut stream, &msg.encode()).await?;
            }
        }

        // Continue sync rounds until no more messages (short timeout)
        // For v2 protocol, we may receive Broadcast frames during initial sync (e.g., from auto-launch).
        // We need to handle these properly instead of treating them as Automerge sync messages.
        let mut pending_broadcasts = Vec::new();
        loop {
            if use_typed_frames {
                // v2 protocol: receive typed frame and handle by type
                match tokio::time::timeout(
                    Duration::from_millis(100),
                    connection::recv_typed_frame(&mut stream),
                )
                .await
                {
                    Ok(Ok(Some(frame))) => match frame.frame_type {
                        NotebookFrameType::AutomergeSync => {
                            let message = sync::Message::decode(&frame.payload).map_err(|e| {
                                NotebookSyncError::SyncError(format!("decode: {}", e))
                            })?;
                            doc.sync()
                                .receive_sync_message(&mut peer_state, message)
                                .map_err(|e| {
                                    NotebookSyncError::SyncError(format!("receive: {}", e))
                                })?;

                            if let Some(msg) = doc.sync().generate_sync_message(&mut peer_state) {
                                connection::send_typed_frame(
                                    &mut stream,
                                    NotebookFrameType::AutomergeSync,
                                    &msg.encode(),
                                )
                                .await?;
                            }
                        }
                        NotebookFrameType::Broadcast => {
                            // Queue broadcasts to deliver after sync completes
                            match serde_json::from_slice::<NotebookBroadcast>(&frame.payload) {
                                Ok(broadcast) => {
                                    info!(
                                        "[notebook-sync-client] Received broadcast during init: {:?}",
                                        broadcast
                                    );
                                    pending_broadcasts.push(broadcast);
                                }
                                Err(e) => {
                                    warn!(
                                        "[notebook-sync-client] Failed to deserialize broadcast: {} (payload: {} bytes)",
                                        e,
                                        frame.payload.len()
                                    );
                                }
                            }
                        }
                        NotebookFrameType::Response => {
                            // Unexpected during init, ignore
                            warn!("[notebook-sync-client] Unexpected Response frame during init");
                        }
                        NotebookFrameType::Request => {
                            // Server shouldn't send requests, ignore
                            warn!("[notebook-sync-client] Unexpected Request frame during init");
                        }
                    },
                    Ok(Ok(None)) => return Err(NotebookSyncError::Disconnected),
                    Ok(Err(e)) => return Err(NotebookSyncError::ConnectionFailed(e)),
                    Err(_) => break, // Timeout — initial sync is done
                }
            } else {
                // v1 protocol: raw Automerge frames
                match tokio::time::timeout(
                    Duration::from_millis(100),
                    connection::recv_frame(&mut stream),
                )
                .await
                {
                    Ok(Ok(Some(data))) => {
                        let message = sync::Message::decode(&data)
                            .map_err(|e| NotebookSyncError::SyncError(format!("decode: {}", e)))?;
                        doc.sync()
                            .receive_sync_message(&mut peer_state, message)
                            .map_err(|e| NotebookSyncError::SyncError(format!("receive: {}", e)))?;

                        if let Some(msg) = doc.sync().generate_sync_message(&mut peer_state) {
                            connection::send_frame(&mut stream, &msg.encode()).await?;
                        }
                    }
                    Ok(Ok(None)) => return Err(NotebookSyncError::Disconnected),
                    Ok(Err(e)) => return Err(NotebookSyncError::ConnectionFailed(e)),
                    Err(_) => break, // Timeout — initial sync is done
                }
            }
        }

        let cells = get_cells_from_doc(&doc);
        info!(
            "[notebook-sync-client] Initial sync complete for {}: {} cells, {} pending broadcasts (protocol {})",
            notebook_id,
            cells.len(),
            pending_broadcasts.len(),
            if use_typed_frames { "v2" } else { "v1" }
        );

        Ok(Self {
            doc,
            peer_state,
            stream,
            notebook_id,
            use_typed_frames,
            pending_broadcasts,
        })
    }

    /// Get the notebook ID this client is syncing.
    pub fn notebook_id(&self) -> &str {
        &self.notebook_id
    }

    // ── Read operations ─────────────────────────────────────────────

    /// Get all cells from the local replica.
    pub fn get_cells(&self) -> Vec<CellSnapshot> {
        get_cells_from_doc(&self.doc)
    }

    /// Get a single cell by ID from the local replica.
    pub fn get_cell(&self, cell_id: &str) -> Option<CellSnapshot> {
        self.get_cells().into_iter().find(|c| c.id == cell_id)
    }

    /// Read a metadata value from the local Automerge doc replica.
    pub fn get_metadata(&self, key: &str) -> Option<String> {
        get_metadata_from_doc(&self.doc, key)
    }

    // ── Write operations (mutate local + sync) ──────────────────────

    /// Set a metadata value and sync to daemon.
    pub async fn set_metadata(&mut self, key: &str, value: &str) -> Result<(), NotebookSyncError> {
        set_metadata_in_doc(&mut self.doc, key, value)
            .map_err(|e| NotebookSyncError::SyncError(format!("set_metadata: {}", e)))?;
        self.sync_to_daemon().await
    }

    /// Add a new cell at the given index and sync to daemon.
    pub async fn add_cell(
        &mut self,
        index: usize,
        cell_id: &str,
        cell_type: &str,
    ) -> Result<(), NotebookSyncError> {
        let cells_id = self
            .ensure_cells_list()
            .map_err(|e| NotebookSyncError::SyncError(format!("ensure cells: {}", e)))?;

        let len = self.doc.length(&cells_id);
        let index = index.min(len);

        let cell_map = self
            .doc
            .insert_object(&cells_id, index, ObjType::Map)
            .map_err(|e| NotebookSyncError::SyncError(format!("insert: {}", e)))?;
        self.doc
            .put(&cell_map, "id", cell_id)
            .map_err(|e| NotebookSyncError::SyncError(format!("put id: {}", e)))?;
        self.doc
            .put(&cell_map, "cell_type", cell_type)
            .map_err(|e| NotebookSyncError::SyncError(format!("put type: {}", e)))?;
        self.doc
            .put_object(&cell_map, "source", ObjType::Text)
            .map_err(|e| NotebookSyncError::SyncError(format!("put source: {}", e)))?;
        self.doc
            .put(&cell_map, "execution_count", "null")
            .map_err(|e| NotebookSyncError::SyncError(format!("put exec_count: {}", e)))?;
        self.doc
            .put_object(&cell_map, "outputs", ObjType::List)
            .map_err(|e| NotebookSyncError::SyncError(format!("put outputs: {}", e)))?;

        self.sync_to_daemon().await
    }

    /// Delete a cell by ID and sync to daemon.
    pub async fn delete_cell(&mut self, cell_id: &str) -> Result<(), NotebookSyncError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };

        let idx = match self.find_cell_index(&cells_id, cell_id) {
            Some(i) => i,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };

        self.doc
            .delete(&cells_id, idx)
            .map_err(|e| NotebookSyncError::SyncError(format!("delete: {}", e)))?;

        self.sync_to_daemon().await
    }

    /// Update a cell's source text and sync to daemon.
    pub async fn update_source(
        &mut self,
        cell_id: &str,
        source: &str,
    ) -> Result<(), NotebookSyncError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let idx = match self.find_cell_index(&cells_id, cell_id) {
            Some(i) => i,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let cell_obj = match self.cell_at_index(&cells_id, idx) {
            Some(o) => o,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let source_id = match self.text_id(&cell_obj, "source") {
            Some(id) => id,
            None => {
                return Err(NotebookSyncError::SyncError(
                    "source Text not found".to_string(),
                ))
            }
        };

        self.doc
            .update_text(&source_id, source)
            .map_err(|e| NotebookSyncError::SyncError(format!("update_text: {}", e)))?;

        self.sync_to_daemon().await
    }

    /// Set outputs for a cell and sync to daemon.
    pub async fn set_outputs(
        &mut self,
        cell_id: &str,
        outputs: &[String],
    ) -> Result<(), NotebookSyncError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let idx = match self.find_cell_index(&cells_id, cell_id) {
            Some(i) => i,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let cell_obj = match self.cell_at_index(&cells_id, idx) {
            Some(o) => o,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };

        let _ = self.doc.delete(&cell_obj, "outputs");
        let list_id = self
            .doc
            .put_object(&cell_obj, "outputs", ObjType::List)
            .map_err(|e| NotebookSyncError::SyncError(format!("put outputs: {}", e)))?;
        for (i, output) in outputs.iter().enumerate() {
            self.doc
                .insert(&list_id, i, output.as_str())
                .map_err(|e| NotebookSyncError::SyncError(format!("insert output: {}", e)))?;
        }

        self.sync_to_daemon().await
    }

    /// Append a single output to a cell's output list and sync to daemon.
    pub async fn append_output(
        &mut self,
        cell_id: &str,
        output: &str,
    ) -> Result<(), NotebookSyncError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let idx = match self.find_cell_index(&cells_id, cell_id) {
            Some(i) => i,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let cell_obj = match self.cell_at_index(&cells_id, idx) {
            Some(o) => o,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };

        let list_id = self
            .outputs_list_id(&cell_obj)
            .ok_or_else(|| NotebookSyncError::SyncError("outputs list not found".to_string()))?;

        let len = self.doc.length(&list_id);
        self.doc
            .insert(&list_id, len, output)
            .map_err(|e| NotebookSyncError::SyncError(format!("insert output: {}", e)))?;

        self.sync_to_daemon().await
    }

    /// Clear all outputs and reset execution_count for a cell, then sync to daemon.
    pub async fn clear_outputs(&mut self, cell_id: &str) -> Result<(), NotebookSyncError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let idx = match self.find_cell_index(&cells_id, cell_id) {
            Some(i) => i,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let cell_obj = match self.cell_at_index(&cells_id, idx) {
            Some(o) => o,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };

        // Replace outputs with a fresh empty list
        let _ = self.doc.delete(&cell_obj, "outputs");
        self.doc
            .put_object(&cell_obj, "outputs", ObjType::List)
            .map_err(|e| NotebookSyncError::SyncError(format!("put outputs: {}", e)))?;

        // Reset execution count
        self.doc
            .put(&cell_obj, "execution_count", "null")
            .map_err(|e| NotebookSyncError::SyncError(format!("put exec_count: {}", e)))?;

        self.sync_to_daemon().await
    }

    /// Set execution count for a cell and sync to daemon.
    pub async fn set_execution_count(
        &mut self,
        cell_id: &str,
        count: &str,
    ) -> Result<(), NotebookSyncError> {
        let cells_id = match self.cells_list_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let idx = match self.find_cell_index(&cells_id, cell_id) {
            Some(i) => i,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let cell_obj = match self.cell_at_index(&cells_id, idx) {
            Some(o) => o,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };

        self.doc
            .put(&cell_obj, "execution_count", count)
            .map_err(|e| NotebookSyncError::SyncError(format!("put: {}", e)))?;

        self.sync_to_daemon().await
    }

    // ── Receiving changes ───────────────────────────────────────────

    /// Wait for the next change from the daemon.
    ///
    /// Blocks until a sync message arrives, applies it, and returns
    /// the updated cells. For v2 protocol, this also handles Broadcast frames.
    pub async fn recv_changes(&mut self) -> Result<Vec<CellSnapshot>, NotebookSyncError> {
        if self.use_typed_frames {
            self.recv_changes_v2().await
        } else {
            self.recv_changes_v1().await
        }
    }

    /// v1 protocol: receive raw Automerge frame
    async fn recv_changes_v1(&mut self) -> Result<Vec<CellSnapshot>, NotebookSyncError> {
        match connection::recv_frame(&mut self.stream).await? {
            Some(data) => {
                let message = sync::Message::decode(&data)
                    .map_err(|e| NotebookSyncError::SyncError(format!("decode: {}", e)))?;
                self.doc
                    .sync()
                    .receive_sync_message(&mut self.peer_state, message)
                    .map_err(|e| NotebookSyncError::SyncError(format!("receive: {}", e)))?;

                // Send ack if needed
                if let Some(msg) = self.doc.sync().generate_sync_message(&mut self.peer_state) {
                    connection::send_frame(&mut self.stream, &msg.encode()).await?;
                }

                Ok(self.get_cells())
            }
            None => Err(NotebookSyncError::Disconnected),
        }
    }

    /// v2 protocol: receive typed frame
    async fn recv_changes_v2(&mut self) -> Result<Vec<CellSnapshot>, NotebookSyncError> {
        match connection::recv_typed_frame(&mut self.stream).await? {
            Some(frame) => match frame.frame_type {
                NotebookFrameType::AutomergeSync => {
                    let message = sync::Message::decode(&frame.payload)
                        .map_err(|e| NotebookSyncError::SyncError(format!("decode: {}", e)))?;
                    self.doc
                        .sync()
                        .receive_sync_message(&mut self.peer_state, message)
                        .map_err(|e| NotebookSyncError::SyncError(format!("receive: {}", e)))?;

                    // Send ack if needed
                    if let Some(msg) = self.doc.sync().generate_sync_message(&mut self.peer_state) {
                        connection::send_typed_frame(
                            &mut self.stream,
                            NotebookFrameType::AutomergeSync,
                            &msg.encode(),
                        )
                        .await?;
                    }

                    Ok(self.get_cells())
                }
                NotebookFrameType::Broadcast => {
                    // For now, ignore broadcast frames - caller can handle separately
                    // In the future, we could return them or emit events
                    Ok(self.get_cells())
                }
                _ => {
                    // Unexpected frame type
                    warn!(
                        "[notebook-sync-client] Unexpected frame type in recv_changes: {:?}",
                        frame.frame_type
                    );
                    Ok(self.get_cells())
                }
            },
            None => Err(NotebookSyncError::Disconnected),
        }
    }

    /// Receive any frame type from the daemon.
    ///
    /// Returns `Ok(None)` if no frame is available (v1 protocol always returns None).
    /// This is used by the background task to handle all frame types.
    ///
    /// Note: This also drains any broadcasts collected during wait_for_response(),
    /// ensuring they aren't lost when a request/response exchange occurs.
    async fn recv_frame_any(&mut self) -> Result<Option<ReceivedFrame>, NotebookSyncError> {
        // First, drain any pending broadcasts collected during wait_for_response()
        if !self.pending_broadcasts.is_empty() {
            let broadcast = self.pending_broadcasts.remove(0);
            return Ok(Some(ReceivedFrame::Broadcast(broadcast)));
        }

        if !self.use_typed_frames {
            // v1 protocol: fall back to recv_changes behavior
            match self.recv_changes_v1().await {
                Ok(cells) => Ok(Some(ReceivedFrame::Changes(cells))),
                Err(NotebookSyncError::Disconnected) => Err(NotebookSyncError::Disconnected),
                Err(e) => Err(e),
            }
        } else {
            // v2 protocol: handle all frame types with timeout to avoid blocking
            let frame_result = tokio::time::timeout(
                Duration::from_millis(100),
                connection::recv_typed_frame(&mut self.stream),
            )
            .await;

            match frame_result {
                Ok(Ok(Some(frame))) => match frame.frame_type {
                    NotebookFrameType::AutomergeSync => {
                        let message = sync::Message::decode(&frame.payload)
                            .map_err(|e| NotebookSyncError::SyncError(format!("decode: {}", e)))?;
                        self.doc
                            .sync()
                            .receive_sync_message(&mut self.peer_state, message)
                            .map_err(|e| NotebookSyncError::SyncError(format!("receive: {}", e)))?;

                        // Send ack if needed
                        if let Some(msg) =
                            self.doc.sync().generate_sync_message(&mut self.peer_state)
                        {
                            connection::send_typed_frame(
                                &mut self.stream,
                                NotebookFrameType::AutomergeSync,
                                &msg.encode(),
                            )
                            .await?;
                        }

                        Ok(Some(ReceivedFrame::Changes(self.get_cells())))
                    }
                    NotebookFrameType::Broadcast => {
                        let broadcast: NotebookBroadcast = serde_json::from_slice(&frame.payload)
                            .map_err(|e| {
                            NotebookSyncError::SyncError(format!("deserialize broadcast: {}", e))
                        })?;
                        Ok(Some(ReceivedFrame::Broadcast(broadcast)))
                    }
                    NotebookFrameType::Response => {
                        let response: NotebookResponse = serde_json::from_slice(&frame.payload)
                            .map_err(|e| {
                                NotebookSyncError::SyncError(format!("deserialize response: {}", e))
                            })?;
                        Ok(Some(ReceivedFrame::Response(response)))
                    }
                    NotebookFrameType::Request => {
                        // Unexpected - server shouldn't send requests
                        warn!("[notebook-sync-client] Unexpected Request frame from server");
                        Ok(None)
                    }
                },
                // EOF/disconnect
                Ok(Ok(None)) => Err(NotebookSyncError::Disconnected),
                // I/O error
                Ok(Err(e)) => Err(NotebookSyncError::ConnectionFailed(e)),
                // Timeout - no data available, return Ok(None) so caller can continue
                Err(_) => Ok(None),
            }
        }
    }

    // ── Internal helpers ────────────────────────────────────────────

    /// Generate and send sync message to daemon, then wait for the
    /// server's acknowledgment.
    ///
    /// The Automerge sync protocol is bidirectional: after the server
    /// applies our changes, it sends back a sync message confirming
    /// what it now has. By waiting for this reply, callers know the
    /// daemon has processed and persisted the change when the write
    /// method returns.
    async fn sync_to_daemon(&mut self) -> Result<(), NotebookSyncError> {
        if self.use_typed_frames {
            self.sync_to_daemon_v2().await
        } else {
            self.sync_to_daemon_v1().await
        }
    }

    /// v1 protocol: raw Automerge frames
    async fn sync_to_daemon_v1(&mut self) -> Result<(), NotebookSyncError> {
        let encoded = {
            let msg = self.doc.sync().generate_sync_message(&mut self.peer_state);
            msg.map(|m| m.encode())
        };

        if let Some(data) = encoded {
            connection::send_frame(&mut self.stream, &data).await?;

            match tokio::time::timeout(
                Duration::from_millis(500),
                connection::recv_frame(&mut self.stream),
            )
            .await
            {
                Ok(Ok(Some(data))) => {
                    let message = sync::Message::decode(&data)
                        .map_err(|e| NotebookSyncError::SyncError(format!("decode: {}", e)))?;
                    self.doc
                        .sync()
                        .receive_sync_message(&mut self.peer_state, message)
                        .map_err(|e| NotebookSyncError::SyncError(format!("receive: {}", e)))?;
                }
                Ok(Ok(None)) => return Err(NotebookSyncError::Disconnected),
                Ok(Err(e)) => return Err(NotebookSyncError::ConnectionFailed(e)),
                Err(_) => {} // Timeout — server had nothing to send back
            }
        }
        Ok(())
    }

    /// v2 protocol: typed frames
    async fn sync_to_daemon_v2(&mut self) -> Result<(), NotebookSyncError> {
        let encoded = {
            let msg = self.doc.sync().generate_sync_message(&mut self.peer_state);
            msg.map(|m| m.encode())
        };

        if let Some(data) = encoded {
            connection::send_typed_frame(&mut self.stream, NotebookFrameType::AutomergeSync, &data)
                .await?;

            match tokio::time::timeout(
                Duration::from_millis(500),
                connection::recv_typed_frame(&mut self.stream),
            )
            .await
            {
                Ok(Ok(Some(frame))) => {
                    // Only handle AutomergeSync frames; ignore broadcasts
                    if frame.frame_type == NotebookFrameType::AutomergeSync {
                        let message = sync::Message::decode(&frame.payload)
                            .map_err(|e| NotebookSyncError::SyncError(format!("decode: {}", e)))?;
                        self.doc
                            .sync()
                            .receive_sync_message(&mut self.peer_state, message)
                            .map_err(|e| NotebookSyncError::SyncError(format!("receive: {}", e)))?;
                    }
                }
                Ok(Ok(None)) => return Err(NotebookSyncError::Disconnected),
                Ok(Err(e)) => return Err(NotebookSyncError::ConnectionFailed(e)),
                Err(_) => {} // Timeout — server had nothing to send back
            }
        }
        Ok(())
    }

    // ── Request/Response ───────────────────────────────────────────────

    /// Send a request to the daemon and wait for the response.
    ///
    /// This only works with v2 protocol. The request is sent as a typed
    /// Request frame, and we wait for a Response frame back.
    pub async fn send_request(
        &mut self,
        request: &NotebookRequest,
    ) -> Result<NotebookResponse, NotebookSyncError> {
        self.send_request_with_broadcast(request, None).await
    }

    /// Send a request to the daemon, optionally delivering broadcasts immediately.
    ///
    /// If `broadcast_tx` is provided, broadcasts received while waiting for the response
    /// are sent immediately instead of being queued. This is useful for long-running
    /// requests like `LaunchKernel` where progress updates should be delivered in real-time.
    pub async fn send_request_with_broadcast(
        &mut self,
        request: &NotebookRequest,
        broadcast_tx: Option<&mpsc::Sender<NotebookBroadcast>>,
    ) -> Result<NotebookResponse, NotebookSyncError> {
        if !self.use_typed_frames {
            return Err(NotebookSyncError::SyncError(
                "send_request requires v2 protocol".to_string(),
            ));
        }

        // Serialize and send the request
        let payload = serde_json::to_vec(request)
            .map_err(|e| NotebookSyncError::SyncError(format!("serialize request: {}", e)))?;

        connection::send_typed_frame(&mut self.stream, NotebookFrameType::Request, &payload)
            .await?;

        // Wait for a Response frame (with timeout)
        // Use longer timeout for requests that may create environments
        let timeout_secs = match request {
            NotebookRequest::LaunchKernel { .. } => 300, // 5 minutes for env creation
            _ => 30,
        };
        match tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            self.wait_for_response_with_broadcast(broadcast_tx),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(NotebookSyncError::Timeout),
        }
    }

    /// Wait for a Response frame, optionally delivering broadcasts immediately.
    ///
    /// If `broadcast_tx` is provided, broadcasts are sent immediately instead of
    /// being queued. This enables real-time progress updates during long-running requests.
    async fn wait_for_response_with_broadcast(
        &mut self,
        broadcast_tx: Option<&mpsc::Sender<NotebookBroadcast>>,
    ) -> Result<NotebookResponse, NotebookSyncError> {
        loop {
            match connection::recv_typed_frame(&mut self.stream).await? {
                Some(frame) => match frame.frame_type {
                    NotebookFrameType::Response => {
                        let response: NotebookResponse = serde_json::from_slice(&frame.payload)
                            .map_err(|e| {
                                NotebookSyncError::SyncError(format!("deserialize response: {}", e))
                            })?;
                        return Ok(response);
                    }
                    NotebookFrameType::AutomergeSync => {
                        // Handle sync message while waiting
                        let message = sync::Message::decode(&frame.payload)
                            .map_err(|e| NotebookSyncError::SyncError(format!("decode: {}", e)))?;
                        self.doc
                            .sync()
                            .receive_sync_message(&mut self.peer_state, message)
                            .map_err(|e| NotebookSyncError::SyncError(format!("receive: {}", e)))?;
                        // Continue waiting for Response
                    }
                    NotebookFrameType::Broadcast => {
                        // Parse the broadcast
                        if let Ok(broadcast) =
                            serde_json::from_slice::<NotebookBroadcast>(&frame.payload)
                        {
                            // If we have a broadcast sender, deliver immediately
                            // Otherwise queue for later delivery
                            if let Some(tx) = broadcast_tx {
                                // Try non-blocking send first for real-time delivery
                                if let Err(tokio::sync::mpsc::error::TrySendError::Full(b)) =
                                    tx.try_send(broadcast)
                                {
                                    // Channel full - queue for later to avoid dropping
                                    // Terminal events (ready, error) are especially important
                                    self.pending_broadcasts.push(b);
                                }
                            } else {
                                self.pending_broadcasts.push(broadcast);
                            }
                        }
                        continue;
                    }
                    NotebookFrameType::Request => {
                        // Unexpected - server shouldn't send requests
                        warn!(
                            "[notebook-sync-client] Unexpected Request frame while waiting for response"
                        );
                        continue;
                    }
                },
                None => return Err(NotebookSyncError::Disconnected),
            }
        }
    }

    /// Check if this client is using the v2 typed frames protocol.
    pub fn uses_typed_frames(&self) -> bool {
        self.use_typed_frames
    }

    fn cells_list_id(&self) -> Option<automerge::ObjId> {
        self.doc
            .get(automerge::ROOT, "cells")
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::List) => Some(id),
                _ => None,
            })
    }

    fn ensure_cells_list(&mut self) -> Result<automerge::ObjId, automerge::AutomergeError> {
        if let Some(id) = self.cells_list_id() {
            return Ok(id);
        }
        self.doc.put_object(automerge::ROOT, "cells", ObjType::List)
    }

    fn cell_at_index(&self, cells_id: &automerge::ObjId, index: usize) -> Option<automerge::ObjId> {
        self.doc
            .get(cells_id, index)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Map) => Some(id),
                _ => None,
            })
    }

    fn find_cell_index(&self, cells_id: &automerge::ObjId, cell_id: &str) -> Option<usize> {
        let len = self.doc.length(cells_id);
        for i in 0..len {
            if let Some(cell_obj) = self.cell_at_index(cells_id, i) {
                if self
                    .doc
                    .get(&cell_obj, "id")
                    .ok()
                    .flatten()
                    .and_then(|(v, _)| match v {
                        automerge::Value::Scalar(s) => match s.as_ref() {
                            automerge::ScalarValue::Str(s) => Some(s.to_string()),
                            _ => None,
                        },
                        _ => None,
                    })
                    .as_deref()
                    == Some(cell_id)
                {
                    return Some(i);
                }
            }
        }
        None
    }

    fn outputs_list_id(&self, cell_obj: &automerge::ObjId) -> Option<automerge::ObjId> {
        self.doc
            .get(cell_obj, "outputs")
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::List) => Some(id),
                _ => None,
            })
    }

    fn text_id(&self, parent: &automerge::ObjId, key: &str) -> Option<automerge::ObjId> {
        self.doc
            .get(parent, key)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Text) => Some(id),
                _ => None,
            })
    }
}

/// Split impl requires Send + 'static for spawning background task.
impl<S> NotebookSyncClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    /// Split this client into a handle and receivers.
    ///
    /// Returns:
    /// - `NotebookSyncHandle`: Clonable handle for sending commands
    /// - `NotebookSyncReceiver`: Receiver for sync updates from other peers
    /// - `NotebookBroadcastReceiver`: Receiver for kernel broadcast events
    /// - `Vec<CellSnapshot>`: Initial cells after sync
    /// - `Option<String>`: Initial notebook metadata JSON (if present)
    ///
    /// The client is consumed and a background task is spawned to process
    /// both commands and incoming changes concurrently.
    pub fn into_split(
        self,
    ) -> (
        NotebookSyncHandle,
        NotebookSyncReceiver,
        NotebookBroadcastReceiver,
        Vec<CellSnapshot>,
        Option<String>,
    ) {
        let initial_cells = self.get_cells();
        let initial_metadata = self.get_metadata(NOTEBOOK_METADATA_KEY);
        let notebook_id = self.notebook_id.clone();
        let pending_broadcasts = self.pending_broadcasts.clone();

        // Channel for commands from handles
        let (cmd_tx, cmd_rx) = mpsc::channel::<SyncCommand>(32);

        // Channel for sync updates to receivers
        let (changes_tx, changes_rx) = mpsc::channel::<SyncUpdate>(32);

        // Channel for kernel broadcasts
        let (broadcast_tx, broadcast_rx) = mpsc::channel::<NotebookBroadcast>(64);

        // Send pending broadcasts (received during init) before spawning the task
        // This ensures the broadcast receiver can get them immediately
        let broadcast_tx_for_pending = broadcast_tx.clone();
        if !pending_broadcasts.is_empty() {
            info!(
                "[notebook-sync-client] Sending {} pending broadcasts for {}",
                pending_broadcasts.len(),
                notebook_id
            );
            tokio::spawn(async move {
                for broadcast in pending_broadcasts {
                    if broadcast_tx_for_pending.send(broadcast).await.is_err() {
                        warn!("[notebook-sync-client] Failed to send pending broadcast");
                        break;
                    }
                }
            });
        }

        // Spawn background task with panic catching
        let notebook_id_for_task = notebook_id.clone();
        info!(
            "[notebook-sync-client] Spawning run_sync_task for {}",
            notebook_id_for_task
        );
        tokio::spawn(async move {
            info!(
                "[notebook-sync-task] Task started for {} (inside spawn)",
                notebook_id_for_task
            );
            let result =
                std::panic::AssertUnwindSafe(run_sync_task(self, cmd_rx, changes_tx, broadcast_tx))
                    .catch_unwind()
                    .await;

            match result {
                Ok(()) => {
                    info!(
                        "[notebook-sync-task] Task completed normally for {}",
                        notebook_id_for_task
                    );
                }
                Err(panic_info) => {
                    log::error!(
                        "[notebook-sync-task] PANIC in run_sync_task for {}: {:?}",
                        notebook_id_for_task,
                        panic_info
                    );
                }
            }
        });

        let handle = NotebookSyncHandle {
            tx: cmd_tx,
            notebook_id,
        };
        let receiver = NotebookSyncReceiver { rx: changes_rx };
        let broadcast_receiver = NotebookBroadcastReceiver { rx: broadcast_rx };

        (
            handle,
            receiver,
            broadcast_receiver,
            initial_cells,
            initial_metadata,
        )
    }
}

/// Background task that owns the client and processes commands/changes.
async fn run_sync_task<S>(
    mut client: NotebookSyncClient<S>,
    mut cmd_rx: mpsc::Receiver<SyncCommand>,
    changes_tx: mpsc::Sender<SyncUpdate>,
    broadcast_tx: mpsc::Sender<NotebookBroadcast>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    use tokio::time::{interval, Duration};

    let notebook_id = client.notebook_id().to_string();
    info!(
        "[notebook-sync-task] Starting for {} (changes_tx strong_count before loop: N/A)",
        notebook_id
    );

    // Use a short poll interval to check for incoming data
    let mut poll_interval = interval(Duration::from_millis(50));
    let mut loop_count = 0u64;
    // Track last metadata to only send updates when it actually changes
    let mut last_metadata: Option<String> = client.get_metadata(NOTEBOOK_METADATA_KEY);

    loop {
        loop_count += 1;
        tokio::select! {
            biased;  // Always check commands first
            // Process commands from handles
            cmd_opt = cmd_rx.recv() => {
                match cmd_opt {
                    Some(cmd) => {
                        match cmd {
                            SyncCommand::AddCell { index, cell_id, cell_type, reply } => {
                                let result = client.add_cell(index, &cell_id, &cell_type).await;
                                let _ = reply.send(result);
                            }
                            SyncCommand::DeleteCell { cell_id, reply } => {
                                let result = client.delete_cell(&cell_id).await;
                                let _ = reply.send(result);
                            }
                            SyncCommand::UpdateSource { cell_id, source, reply } => {
                                let result = client.update_source(&cell_id, &source).await;
                                let _ = reply.send(result);
                            }
                            SyncCommand::ClearOutputs { cell_id, reply } => {
                                let result = client.clear_outputs(&cell_id).await;
                                let _ = reply.send(result);
                            }
                            SyncCommand::AppendOutput { cell_id, output, reply } => {
                                let result = client.append_output(&cell_id, &output).await;
                                let _ = reply.send(result);
                            }
                            SyncCommand::SetExecutionCount { cell_id, count, reply } => {
                                let result = client.set_execution_count(&cell_id, &count).await;
                                let _ = reply.send(result);
                            }
                            SyncCommand::GetCells { reply } => {
                                let cells = client.get_cells();
                                let _ = reply.send(cells);
                            }
                            SyncCommand::SetMetadata { key, value, reply } => {
                                let result = client.set_metadata(&key, &value).await;
                                let _ = reply.send(result);
                            }
                            SyncCommand::GetMetadata { key, reply } => {
                                let result = client.get_metadata(&key);
                                let _ = reply.send(result);
                            }
                            SyncCommand::SendRequest { request, reply, broadcast_tx: override_tx } => {
                                // Use the override broadcast_tx if provided, otherwise use the task's broadcast_tx
                                // This allows broadcasts to be delivered immediately during long-running requests
                                let tx_to_use = override_tx.as_ref().unwrap_or(&broadcast_tx);
                                let result = client.send_request_with_broadcast(&request, Some(tx_to_use)).await;
                                let _ = reply.send(result);
                            }
                        }
                    }
                    None => {
                        // Command channel closed - handle was dropped
                        info!(
                            "[notebook-sync-task] Command channel closed for {} (handle dropped), loop_count={}",
                            notebook_id, loop_count
                        );
                        break;
                    }
                }
            }

            // Check for incoming changes
            // Note: recv_frame_any() has an internal 100ms timeout, so no outer timeout needed
            _ = poll_interval.tick() => {
                match client.recv_frame_any().await {
                    Ok(Some(ReceivedFrame::Changes(cells))) => {
                        // Got changes from another peer — only include metadata if it changed
                        let current_metadata = client.get_metadata(NOTEBOOK_METADATA_KEY);
                        let metadata_changed = current_metadata != last_metadata;
                        if metadata_changed {
                            last_metadata = current_metadata.clone();
                        }
                        let update = SyncUpdate {
                            cells,
                            notebook_metadata: if metadata_changed { current_metadata } else { None },
                        };
                        // Use try_send to avoid blocking if receiver isn't draining
                        // (e.g., Python bindings keep sync_rx alive but don't consume it)
                        match changes_tx.try_send(update) {
                            Ok(()) => {}
                            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {
                                // Channel full - receiver not keeping up, skip this update
                                // Next update will contain latest state anyway
                            }
                            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                info!(
                                    "[notebook-sync-task] Changes receiver dropped for {}, loop_count={}",
                                    notebook_id, loop_count
                                );
                                break;
                            }
                        }
                    }
                    Ok(Some(ReceivedFrame::Broadcast(broadcast))) => {
                        // Got a broadcast from daemon
                        let send_result = broadcast_tx.send(broadcast).await;
                        if send_result.is_err() {
                            info!(
                                "[notebook-sync-task] Broadcast receiver dropped for {}",
                                notebook_id
                            );
                            // Continue - broadcasts are optional
                        }
                    }
                    Ok(Some(ReceivedFrame::Response(_))) => {
                        // Unexpected response - we weren't waiting for one
                        warn!("[notebook-sync-task] Unexpected response frame for {}", notebook_id);
                    }
                    Ok(None) => {
                        // No frame available (timeout), continue
                    }
                    Err(NotebookSyncError::Disconnected) => {
                        warn!(
                            "[notebook-sync-task] Disconnected from daemon for {}, loop_count={}",
                            notebook_id, loop_count
                        );
                        break;
                    }
                    Err(e) => {
                        warn!(
                            "[notebook-sync-task] Error receiving for {}: {}, loop_count={}",
                            notebook_id, e, loop_count
                        );
                        break;
                    }
                }
            }
        }
    }

    info!(
        "[notebook-sync-task] Stopped for {} after {} loop iterations",
        notebook_id, loop_count
    );
}

/// Result of receiving a frame from the daemon.
#[allow(dead_code)] // Response variant inner value is logged but not read
enum ReceivedFrame {
    /// Document changes from another peer.
    Changes(Vec<CellSnapshot>),
    /// Kernel broadcast event.
    Broadcast(NotebookBroadcast),
    /// Response to a request (unexpected in background task).
    Response(NotebookResponse),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_cells_from_empty_doc() {
        let doc = AutoCommit::new();
        let cells = get_cells_from_doc(&doc);
        assert!(cells.is_empty());
    }

    #[test]
    fn test_get_cells_from_populated_doc() {
        // Manually build a notebook structure in an AutoCommit
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "notebook_id", "test").unwrap();
        let cells_id = doc
            .put_object(automerge::ROOT, "cells", ObjType::List)
            .unwrap();

        // Add a code cell
        let cell = doc.insert_object(&cells_id, 0, ObjType::Map).unwrap();
        doc.put(&cell, "id", "c1").unwrap();
        doc.put(&cell, "cell_type", "code").unwrap();
        let source = doc.put_object(&cell, "source", ObjType::Text).unwrap();
        doc.splice_text(&source, 0, 0, "x = 1").unwrap();
        doc.put(&cell, "execution_count", "1").unwrap();
        let outputs = doc.put_object(&cell, "outputs", ObjType::List).unwrap();
        doc.insert(&outputs, 0, r#"{"output_type":"stream"}"#)
            .unwrap();

        let cells = get_cells_from_doc(&doc);
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].id, "c1");
        assert_eq!(cells[0].cell_type, "code");
        assert_eq!(cells[0].source, "x = 1");
        assert_eq!(cells[0].execution_count, "1");
        assert_eq!(cells[0].outputs.len(), 1);
    }
}
