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
use log::{debug, info, warn};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, oneshot};

use crate::connection::{
    self, Handshake, NotebookConnectionInfo, NotebookFrameType, ProtocolCapabilities, PROTOCOL_V2,
};
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
    AppendSource {
        cell_id: String,
        text: String,
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
    SendRequest {
        request: NotebookRequest,
        reply: oneshot::Sender<Result<NotebookResponse, NotebookSyncError>>,
        /// Optional broadcast sender for delivering broadcasts during long-running requests.
        /// If provided, broadcasts received while waiting for the response are sent immediately
        /// instead of being queued for later delivery.
        broadcast_tx: Option<broadcast::Sender<NotebookBroadcast>>,
    },
    /// Export the local Automerge document as bytes for frontend initialization.
    GetDocBytes { reply: oneshot::Sender<Vec<u8>> },
    /// Apply a raw Automerge sync message from the frontend and forward to daemon.
    ReceiveFrontendSyncMessage {
        message: Vec<u8>,
        reply: oneshot::Sender<Result<(), NotebookSyncError>>,
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

    /// Append text to a cell's source (no diff, direct CRDT insert at end).
    ///
    /// Unlike `update_source` which replaces the entire text, this appends
    /// characters at the end of the source. Ideal for streaming/agentic use
    /// cases where tokens are appended incrementally.
    pub async fn append_source(&self, cell_id: &str, text: &str) -> Result<(), NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::AppendSource {
                cell_id: cell_id.to_string(),
                text: text.to_string(),
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

    /// Export the local Automerge document as bytes.
    ///
    /// The frontend can load these bytes with `Automerge.load()` to initialize
    /// its own local document replica.
    pub async fn get_doc_bytes(&self) -> Result<Vec<u8>, NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::GetDocBytes { reply: reply_tx })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx.await.map_err(|_| NotebookSyncError::ChannelClosed)
    }

    /// Receive a raw Automerge sync message from the frontend.
    ///
    /// The message is applied to the local doc and forwarded to the daemon.
    /// This enables the frontend to act as a full Automerge peer.
    pub async fn receive_frontend_sync_message(
        &self,
        message: Vec<u8>,
    ) -> Result<(), NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::ReceiveFrontendSyncMessage {
                message,
                reply: reply_tx,
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
        broadcast_tx: broadcast::Sender<NotebookBroadcast>,
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
///
/// This receiver supports multiple subscribers - use `resubscribe()` to create
/// additional receivers that will receive all future broadcasts.
pub struct NotebookBroadcastReceiver {
    rx: broadcast::Receiver<NotebookBroadcast>,
}

impl NotebookBroadcastReceiver {
    /// Wait for the next broadcast event.
    ///
    /// Returns `None` if the sync task has stopped (channel closed).
    /// If events were missed due to a slow consumer, they are skipped
    /// and the next available event is returned.
    pub async fn recv(&mut self) -> Option<NotebookBroadcast> {
        loop {
            match self.rx.recv().await {
                Ok(msg) => return Some(msg),
                Err(broadcast::error::RecvError::Lagged(count)) => {
                    // Consumer was too slow, some messages were dropped
                    log::warn!(
                        "[NotebookBroadcastReceiver] Lagged by {} messages, continuing",
                        count
                    );
                    // Continue to get the next available message
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    /// Create a new receiver that will receive all future broadcasts.
    ///
    /// The new receiver starts from the next message sent after this call.
    /// This allows multiple consumers to independently receive the same
    /// broadcast events (e.g., for streaming execution + independent subscription).
    pub fn resubscribe(&self) -> Self {
        Self {
            rx: self.rx.resubscribe(),
        }
    }
}

/// Client for the notebook sync service.
///
/// Holds a local Automerge document replica that stays in sync with the
/// daemon's canonical copy for a specific notebook.
///
/// Uses typed frames protocol for daemon communication.
pub struct NotebookSyncClient<S> {
    doc: AutoCommit,
    peer_state: sync::State,
    stream: S,
    notebook_id: String,
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
        Self::connect_with_options(socket_path, notebook_id, Duration::from_secs(2), None, None)
            .await
    }

    /// Connect with a custom timeout.
    pub async fn connect_with_timeout(
        socket_path: PathBuf,
        notebook_id: String,
        timeout: Duration,
    ) -> Result<Self, NotebookSyncError> {
        Self::connect_with_options(socket_path, notebook_id, timeout, None, None).await
    }

    /// Connect with custom timeout and working directory for untitled notebooks.
    pub async fn connect_with_options(
        socket_path: PathBuf,
        notebook_id: String,
        timeout: Duration,
        working_dir: Option<PathBuf>,
        initial_metadata: Option<String>,
    ) -> Result<Self, NotebookSyncError> {
        let stream = tokio::time::timeout(timeout, tokio::net::UnixStream::connect(&socket_path))
            .await
            .map_err(|_| NotebookSyncError::Timeout)?
            .map_err(NotebookSyncError::ConnectionFailed)?;

        info!(
            "[notebook-sync-client] Connected to {:?} for {} (working_dir: {:?})",
            socket_path, notebook_id, working_dir
        );

        Self::init(stream, notebook_id, working_dir, initial_metadata).await
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
        Self::connect_split_with_options(socket_path, notebook_id, None, None).await
    }

    /// Connect and return split handle/receiver with working directory for untitled notebooks.
    pub async fn connect_split_with_options(
        socket_path: PathBuf,
        notebook_id: String,
        working_dir: Option<PathBuf>,
        initial_metadata: Option<String>,
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
            initial_metadata,
        )
        .await?;
        Ok(client.into_split())
    }

    /// Connect and return split handle/receiver with raw sync relay support.
    ///
    /// When `raw_sync_tx` is provided, incoming Automerge sync messages from
    /// the daemon are also forwarded as raw bytes to this channel. This enables
    /// the Tauri process to relay sync messages to the frontend for Phase 2.
    pub async fn connect_split_with_raw_sync(
        socket_path: PathBuf,
        notebook_id: String,
        working_dir: Option<PathBuf>,
        initial_metadata: Option<String>,
        raw_sync_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
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
            initial_metadata,
        )
        .await?;
        Ok(client.into_split_with_raw_sync(raw_sync_tx))
    }

    /// Connect by opening an existing notebook file (daemon-owned loading).
    ///
    /// The daemon loads the file, derives the notebook_id, and returns it.
    /// Returns NotebookConnectionInfo so caller can get notebook_id and trust status.
    pub async fn connect_open_split(
        socket_path: PathBuf,
        path: PathBuf,
        raw_sync_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    ) -> Result<
        (
            NotebookSyncHandle,
            NotebookSyncReceiver,
            NotebookBroadcastReceiver,
            Vec<CellSnapshot>,
            Option<String>,
            NotebookConnectionInfo,
        ),
        NotebookSyncError,
    > {
        let stream = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::UnixStream::connect(&socket_path),
        )
        .await
        .map_err(|_| NotebookSyncError::Timeout)?
        .map_err(NotebookSyncError::ConnectionFailed)?;

        let (client, info) = Self::init_open_notebook(stream, path).await?;
        let (handle, receiver, broadcast_rx, cells, metadata) =
            client.into_split_with_raw_sync(raw_sync_tx);
        Ok((handle, receiver, broadcast_rx, cells, metadata, info))
    }

    /// Connect by creating a new notebook (daemon-owned creation).
    ///
    /// The daemon creates an empty notebook with one cell and returns the notebook_id.
    /// Returns NotebookConnectionInfo so caller can get notebook_id.
    pub async fn connect_create_split(
        socket_path: PathBuf,
        runtime: String,
        working_dir: Option<PathBuf>,
        raw_sync_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    ) -> Result<
        (
            NotebookSyncHandle,
            NotebookSyncReceiver,
            NotebookBroadcastReceiver,
            Vec<CellSnapshot>,
            Option<String>,
            NotebookConnectionInfo,
        ),
        NotebookSyncError,
    > {
        let stream = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::net::UnixStream::connect(&socket_path),
        )
        .await
        .map_err(|_| NotebookSyncError::Timeout)?
        .map_err(NotebookSyncError::ConnectionFailed)?;

        let (client, info) = Self::init_create_notebook(stream, runtime, working_dir).await?;
        let (handle, receiver, broadcast_rx, cells, metadata) =
            client.into_split_with_raw_sync(raw_sync_tx);
        Ok((handle, receiver, broadcast_rx, cells, metadata, info))
    }
}

#[cfg(windows)]
impl NotebookSyncClient<tokio::net::windows::named_pipe::NamedPipeClient> {
    /// Connect to the daemon and join the notebook room.
    pub async fn connect(
        socket_path: PathBuf,
        notebook_id: String,
    ) -> Result<Self, NotebookSyncError> {
        Self::connect_with_options(socket_path, notebook_id, None, None).await
    }

    /// Connect with working directory for untitled notebooks.
    pub async fn connect_with_options(
        socket_path: PathBuf,
        notebook_id: String,
        working_dir: Option<PathBuf>,
        initial_metadata: Option<String>,
    ) -> Result<Self, NotebookSyncError> {
        let pipe_name = socket_path.to_string_lossy().to_string();
        let client = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&pipe_name)
            .map_err(NotebookSyncError::ConnectionFailed)?;
        Self::init(client, notebook_id, working_dir, initial_metadata).await
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
        Self::connect_split_with_options(socket_path, notebook_id, None, None).await
    }

    /// Connect and return split handle/receiver with working directory for untitled notebooks.
    pub async fn connect_split_with_options(
        socket_path: PathBuf,
        notebook_id: String,
        working_dir: Option<PathBuf>,
        initial_metadata: Option<String>,
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
        let client =
            Self::connect_with_options(socket_path, notebook_id, working_dir, initial_metadata)
                .await?;
        Ok(client.into_split())
    }

    /// Connect and return split handle/receiver with raw sync relay support.
    pub async fn connect_split_with_raw_sync(
        socket_path: PathBuf,
        notebook_id: String,
        working_dir: Option<PathBuf>,
        initial_metadata: Option<String>,
        raw_sync_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
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
        let client =
            Self::connect_with_options(socket_path, notebook_id, working_dir, initial_metadata)
                .await?;
        Ok(client.into_split_with_raw_sync(raw_sync_tx))
    }

    /// Connect by opening an existing notebook file (daemon-owned loading).
    pub async fn connect_open_split(
        socket_path: PathBuf,
        path: PathBuf,
        raw_sync_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    ) -> Result<
        (
            NotebookSyncHandle,
            NotebookSyncReceiver,
            NotebookBroadcastReceiver,
            Vec<CellSnapshot>,
            Option<String>,
            NotebookConnectionInfo,
        ),
        NotebookSyncError,
    > {
        let pipe_name = socket_path.to_string_lossy().to_string();
        let stream = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&pipe_name)
            .map_err(NotebookSyncError::ConnectionFailed)?;

        let (client, info) = Self::init_open_notebook(stream, path).await?;
        let (handle, receiver, broadcast_rx, cells, metadata) =
            client.into_split_with_raw_sync(raw_sync_tx);
        Ok((handle, receiver, broadcast_rx, cells, metadata, info))
    }

    /// Connect by creating a new notebook (daemon-owned creation).
    pub async fn connect_create_split(
        socket_path: PathBuf,
        runtime: String,
        working_dir: Option<PathBuf>,
        raw_sync_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
    ) -> Result<
        (
            NotebookSyncHandle,
            NotebookSyncReceiver,
            NotebookBroadcastReceiver,
            Vec<CellSnapshot>,
            Option<String>,
            NotebookConnectionInfo,
        ),
        NotebookSyncError,
    > {
        let pipe_name = socket_path.to_string_lossy().to_string();
        let stream = tokio::net::windows::named_pipe::ClientOptions::new()
            .open(&pipe_name)
            .map_err(NotebookSyncError::ConnectionFailed)?;

        let (client, info) = Self::init_create_notebook(stream, runtime, working_dir).await?;
        let (handle, receiver, broadcast_rx, cells, metadata) =
            client.into_split_with_raw_sync(raw_sync_tx);
        Ok((handle, receiver, broadcast_rx, cells, metadata, info))
    }
}

impl<S> NotebookSyncClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Initialize the client by sending the handshake and performing initial sync.
    ///
    /// The client requests the v2 protocol (typed frames) in the handshake.
    /// The server must respond with a ProtocolCapabilities frame confirming v2.
    ///
    /// The `working_dir` parameter is used for untitled notebooks to provide
    /// the directory context for project file detection (pyproject.toml, etc).
    async fn init(
        mut stream: S,
        notebook_id: String,
        working_dir: Option<PathBuf>,
        initial_metadata: Option<String>,
    ) -> Result<Self, NotebookSyncError> {
        // Send the channel handshake, requesting v2 protocol
        connection::send_json_frame(
            &mut stream,
            &Handshake::NotebookSync {
                notebook_id: notebook_id.clone(),
                protocol: Some(PROTOCOL_V2.to_string()),
                working_dir: working_dir.map(|p| p.to_string_lossy().to_string()),
                initial_metadata,
            },
        )
        .await
        .map_err(|e| NotebookSyncError::SyncError(format!("handshake: {}", e)))?;

        let mut doc = AutoCommit::new();
        let mut peer_state = sync::State::new();

        // Read first frame - must be ProtocolCapabilities confirming v2
        let first_frame = connection::recv_frame(&mut stream)
            .await?
            .ok_or(NotebookSyncError::Disconnected)?;

        // Parse as ProtocolCapabilities - server must support v2
        match serde_json::from_slice::<ProtocolCapabilities>(&first_frame) {
            Ok(caps) if caps.protocol == PROTOCOL_V2 => {
                info!(
                    "[notebook-sync-client] Server supports v2 protocol for {}",
                    notebook_id
                );
            }
            Ok(caps) => {
                return Err(NotebookSyncError::SyncError(format!(
                    "unsupported protocol version: {}",
                    caps.protocol
                )));
            }
            Err(_) => {
                return Err(NotebookSyncError::SyncError(
                    "server does not support v2 protocol".to_string(),
                ));
            }
        };

        // Read the first typed frame (Automerge sync)
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

        // Send our sync message back
        if let Some(msg) = doc.sync().generate_sync_message(&mut peer_state) {
            connection::send_typed_frame(
                &mut stream,
                NotebookFrameType::AutomergeSync,
                &msg.encode(),
            )
            .await?;
        }

        // Continue sync rounds until no more messages (short timeout)
        // We may receive Broadcast frames during initial sync (e.g., from auto-launch).
        let mut pending_broadcasts = Vec::new();
        loop {
            match tokio::time::timeout(
                Duration::from_millis(100),
                connection::recv_typed_frame(&mut stream),
            )
            .await
            {
                Ok(Ok(Some(frame))) => match frame.frame_type {
                    NotebookFrameType::AutomergeSync => {
                        let message = sync::Message::decode(&frame.payload)
                            .map_err(|e| NotebookSyncError::SyncError(format!("decode: {}", e)))?;
                        doc.sync()
                            .receive_sync_message(&mut peer_state, message)
                            .map_err(|e| NotebookSyncError::SyncError(format!("receive: {}", e)))?;

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
                        warn!("[notebook-sync-client] Unexpected Response frame during init");
                    }
                    NotebookFrameType::Request => {
                        warn!("[notebook-sync-client] Unexpected Request frame during init");
                    }
                },
                Ok(Ok(None)) => return Err(NotebookSyncError::Disconnected),
                Ok(Err(e)) => return Err(NotebookSyncError::ConnectionFailed(e)),
                Err(_) => break, // Timeout — initial sync is done
            }
        }

        let cells = get_cells_from_doc(&doc);
        info!(
            "[notebook-sync-client] Initial sync complete for {}: {} cells, {} pending broadcasts",
            notebook_id,
            cells.len(),
            pending_broadcasts.len(),
        );

        Ok(Self {
            doc,
            peer_state,
            stream,
            notebook_id,
            pending_broadcasts,
        })
    }

    /// Initialize by opening an existing notebook file.
    ///
    /// The daemon loads the file, derives notebook_id, and returns NotebookConnectionInfo.
    async fn init_open_notebook(
        mut stream: S,
        path: PathBuf,
    ) -> Result<(Self, NotebookConnectionInfo), NotebookSyncError> {
        let path_str = path.to_string_lossy().to_string();
        info!("[notebook-sync-client] Opening notebook: {}", path_str);

        // Send OpenNotebook handshake
        connection::send_json_frame(&mut stream, &Handshake::OpenNotebook { path: path_str })
            .await
            .map_err(|e| NotebookSyncError::SyncError(format!("handshake: {}", e)))?;

        // Receive NotebookConnectionInfo
        let first_frame = connection::recv_frame(&mut stream)
            .await?
            .ok_or(NotebookSyncError::Disconnected)?;

        let info: NotebookConnectionInfo = serde_json::from_slice(&first_frame)
            .map_err(|e| NotebookSyncError::SyncError(format!("invalid response: {}", e)))?;

        // Check for error in response
        if let Some(ref error) = info.error {
            return Err(NotebookSyncError::SyncError(error.clone()));
        }

        // Validate protocol version
        if info.protocol != PROTOCOL_V2 {
            return Err(NotebookSyncError::SyncError(format!(
                "unsupported protocol version: {}",
                info.protocol
            )));
        }

        let notebook_id = info.notebook_id.clone();
        info!(
            "[notebook-sync-client] Daemon returned notebook_id: {} ({} cells, trust_approval: {})",
            notebook_id, info.cell_count, info.needs_trust_approval
        );

        // Continue with Automerge sync (same as init)
        let client = Self::do_initial_sync(stream, notebook_id).await?;
        Ok((client, info))
    }

    /// Initialize by creating a new notebook.
    ///
    /// The daemon creates an empty notebook and returns NotebookConnectionInfo.
    async fn init_create_notebook(
        mut stream: S,
        runtime: String,
        working_dir: Option<PathBuf>,
    ) -> Result<(Self, NotebookConnectionInfo), NotebookSyncError> {
        info!(
            "[notebook-sync-client] Creating new notebook (runtime: {}, working_dir: {:?})",
            runtime, working_dir
        );

        // Send CreateNotebook handshake
        connection::send_json_frame(
            &mut stream,
            &Handshake::CreateNotebook {
                runtime,
                working_dir: working_dir.map(|p| p.to_string_lossy().to_string()),
            },
        )
        .await
        .map_err(|e| NotebookSyncError::SyncError(format!("handshake: {}", e)))?;

        // Receive NotebookConnectionInfo
        let first_frame = connection::recv_frame(&mut stream)
            .await?
            .ok_or(NotebookSyncError::Disconnected)?;

        let info: NotebookConnectionInfo = serde_json::from_slice(&first_frame)
            .map_err(|e| NotebookSyncError::SyncError(format!("invalid response: {}", e)))?;

        // Check for error in response
        if let Some(ref error) = info.error {
            return Err(NotebookSyncError::SyncError(error.clone()));
        }

        // Validate protocol version
        if info.protocol != PROTOCOL_V2 {
            return Err(NotebookSyncError::SyncError(format!(
                "unsupported protocol version: {}",
                info.protocol
            )));
        }

        let notebook_id = info.notebook_id.clone();
        info!(
            "[notebook-sync-client] Daemon created notebook_id: {} ({} cells)",
            notebook_id, info.cell_count
        );

        // Continue with Automerge sync (same as init)
        let client = Self::do_initial_sync(stream, notebook_id).await?;
        Ok((client, info))
    }

    /// Perform initial Automerge sync after handshake is complete.
    ///
    /// This is the common sync logic used by all init methods after the handshake
    /// response has been received.
    async fn do_initial_sync(
        mut stream: S,
        notebook_id: String,
    ) -> Result<Self, NotebookSyncError> {
        let mut doc = AutoCommit::new();
        let mut peer_state = sync::State::new();
        let mut pending_broadcasts = Vec::new();

        // Read the first typed frame (Automerge sync)
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

        // Send our sync message back
        if let Some(msg) = doc.sync().generate_sync_message(&mut peer_state) {
            connection::send_typed_frame(
                &mut stream,
                NotebookFrameType::AutomergeSync,
                &msg.encode(),
            )
            .await?;
        }

        // Continue sync rounds until no more messages (short timeout)
        loop {
            match tokio::time::timeout(
                Duration::from_millis(100),
                connection::recv_typed_frame(&mut stream),
            )
            .await
            {
                Ok(Ok(Some(frame))) => match frame.frame_type {
                    NotebookFrameType::AutomergeSync => {
                        let message = sync::Message::decode(&frame.payload)
                            .map_err(|e| NotebookSyncError::SyncError(format!("decode: {}", e)))?;
                        doc.sync()
                            .receive_sync_message(&mut peer_state, message)
                            .map_err(|e| NotebookSyncError::SyncError(format!("receive: {}", e)))?;

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
                                    "[notebook-sync-client] Failed to deserialize broadcast: {}",
                                    e
                                );
                            }
                        }
                    }
                    NotebookFrameType::Response => {
                        warn!("[notebook-sync-client] Unexpected Response frame during init");
                    }
                    NotebookFrameType::Request => {
                        warn!("[notebook-sync-client] Unexpected Request frame during init");
                    }
                },
                Ok(Ok(None)) => return Err(NotebookSyncError::Disconnected),
                Ok(Err(e)) => return Err(NotebookSyncError::ConnectionFailed(e)),
                Err(_) => break, // Timeout — initial sync is done
            }
        }

        let cells = get_cells_from_doc(&doc);
        info!(
            "[notebook-sync-client] Initial sync complete for {}: {} cells, {} pending broadcasts",
            notebook_id,
            cells.len(),
            pending_broadcasts.len(),
        );

        Ok(Self {
            doc,
            peer_state,
            stream,
            notebook_id,
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

    /// Append text to a cell's source and sync to daemon.
    ///
    /// Directly inserts at the end of the Text CRDT without diffing.
    pub async fn append_source(
        &mut self,
        cell_id: &str,
        text: &str,
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

        let len = self
            .doc
            .text(&source_id)
            .map_err(|e| NotebookSyncError::SyncError(format!("text read: {}", e)))?
            .len();
        self.doc
            .splice_text(&source_id, len, 0, text)
            .map_err(|e| NotebookSyncError::SyncError(format!("splice_text: {}", e)))?;

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
    /// the updated cells. Also handles Broadcast frames.
    pub async fn recv_changes(&mut self) -> Result<Vec<CellSnapshot>, NotebookSyncError> {
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
                    // Ignore broadcast frames - caller can handle separately
                    Ok(self.get_cells())
                }
                _ => {
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

    /// Check for any pending broadcasts that were collected during wait_for_response().
    fn drain_pending_broadcast(&mut self) -> Option<NotebookBroadcast> {
        if !self.pending_broadcasts.is_empty() {
            Some(self.pending_broadcasts.remove(0))
        } else {
            None
        }
    }

    /// Process a raw frame received from the daemon.
    ///
    /// This handles all frame types for the v2 protocol, including applying
    /// AutomergeSync messages and sending acknowledgments.
    async fn process_incoming_frame(
        &mut self,
        frame: connection::TypedNotebookFrame,
    ) -> Result<Option<ReceivedFrame>, NotebookSyncError> {
        match frame.frame_type {
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

                Ok(Some(ReceivedFrame::Changes(self.get_cells())))
            }
            NotebookFrameType::Broadcast => {
                let broadcast: NotebookBroadcast =
                    serde_json::from_slice(&frame.payload).map_err(|e| {
                        NotebookSyncError::SyncError(format!("deserialize broadcast: {}", e))
                    })?;
                Ok(Some(ReceivedFrame::Broadcast(broadcast)))
            }
            NotebookFrameType::Response => {
                let response: NotebookResponse =
                    serde_json::from_slice(&frame.payload).map_err(|e| {
                        NotebookSyncError::SyncError(format!("deserialize response: {}", e))
                    })?;
                Ok(Some(ReceivedFrame::Response(response)))
            }
            NotebookFrameType::Request => {
                // Unexpected - server shouldn't send requests
                warn!("[notebook-sync-client] Unexpected Request frame from server");
                Ok(None)
            }
        }
    }

    // ── Frontend relay operations ──────────────────────────────────

    /// Receive a raw Automerge sync message from the frontend, apply it to
    /// the local doc, and relay changes to the daemon.
    ///
    /// This is the core of the binary sync relay: the frontend generates an
    /// Automerge sync message from its local doc, sends it here, and we
    /// apply it and forward the resulting changes to the daemon.
    pub async fn receive_and_relay_sync_message(
        &mut self,
        raw_message: &[u8],
    ) -> Result<(), NotebookSyncError> {
        let message = sync::Message::decode(raw_message)
            .map_err(|e| NotebookSyncError::SyncError(format!("decode frontend sync: {}", e)))?;
        self.doc
            .sync()
            .receive_sync_message(&mut self.peer_state, message)
            .map_err(|e| NotebookSyncError::SyncError(format!("receive frontend sync: {}", e)))?;

        // Relay the changes to the daemon
        self.sync_to_daemon().await
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
    /// The request is sent as a typed Request frame, and we wait for a
    /// Response frame back.
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
        broadcast_tx: Option<&broadcast::Sender<NotebookBroadcast>>,
    ) -> Result<NotebookResponse, NotebookSyncError> {
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
        broadcast_tx: Option<&broadcast::Sender<NotebookBroadcast>>,
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
                                // broadcast::Sender::send is synchronous
                                // Only fails if there are no receivers, in which case queue it
                                if tx.send(broadcast.clone()).is_err() {
                                    self.pending_broadcasts.push(broadcast);
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
        self.into_split_with_raw_sync(None)
    }

    /// Split into handle/receiver/broadcast, optionally forwarding raw Automerge
    /// sync messages from the daemon to a channel.
    ///
    /// When `raw_sync_tx` is provided, incoming `AutomergeSync` frames from the
    /// daemon are also forwarded as raw bytes to this channel (in addition to
    /// being applied to the local doc as usual). This enables the Tauri process
    /// to relay sync messages to the frontend for Phase 2 local-first support.
    pub fn into_split_with_raw_sync(
        self,
        raw_sync_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
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

        // Channel for kernel broadcasts (broadcast channel supports multiple subscribers)
        let (broadcast_tx, broadcast_rx) = broadcast::channel::<NotebookBroadcast>(64);

        // Send pending broadcasts (received during init) before spawning the task
        // This ensures the broadcast receiver can get them immediately
        if !pending_broadcasts.is_empty() {
            info!(
                "[notebook-sync-client] Sending {} pending broadcasts for {}",
                pending_broadcasts.len(),
                notebook_id
            );
            for broadcast in pending_broadcasts {
                // broadcast::Sender::send is synchronous, no await needed
                // Errors only if there are no receivers, which can't happen here
                let _ = broadcast_tx.send(broadcast);
            }
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
            let result = std::panic::AssertUnwindSafe(run_sync_task(
                self,
                cmd_rx,
                changes_tx,
                broadcast_tx,
                raw_sync_tx,
            ))
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
    broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    raw_sync_tx: Option<mpsc::UnboundedSender<Vec<u8>>>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let notebook_id = client.notebook_id().to_string();
    info!(
        "[notebook-sync-task] Starting for {} (changes_tx strong_count before loop: N/A)",
        notebook_id
    );

    // Sync state for the frontend peer (used when raw_sync_tx is active).
    // This tracks what the frontend doc has seen, enabling incremental sync messages.
    //
    // IMPORTANT: Starts as None even when raw_sync_tx is provided. It gets
    // initialized in GetDocBytes after the virtual sync handshake. Before that,
    // the frontend hasn't loaded the doc yet, so any sync messages we generate
    // would be stale — when the frontend later loads the doc bytes and receives
    // those stale messages, the CRDT merge produces phantom cells (cells that
    // exist in the list but have no readable ID).
    let mut frontend_peer_state: Option<sync::State> = None;

    let mut loop_count = 0u64;
    // Track last metadata to only send updates when it actually changes
    let mut last_metadata: Option<String> = client.get_metadata(NOTEBOOK_METADATA_KEY);

    loop {
        loop_count += 1;

        // First, check for any pending broadcasts (collected during request/response)
        // These need to be drained before we do anything else
        if let Some(broadcast) = client.drain_pending_broadcast() {
            let send_result = broadcast_tx.send(broadcast);
            if send_result.is_err() {
                info!(
                    "[notebook-sync-task] No broadcast receivers for {}",
                    notebook_id
                );
            }
            continue;
        }

        // Direct socket reads in the select! for instant responsiveness.
        // If a command arrives while waiting on the socket, select! drops the
        // socket future and processes the command immediately.
        enum SelectResult {
            Command(Option<SyncCommand>),
            Frame(std::io::Result<Option<connection::TypedNotebookFrame>>),
        }

        let select_result = tokio::select! {
            biased;
            cmd_opt = cmd_rx.recv() => SelectResult::Command(cmd_opt),
            frame_result = connection::recv_typed_frame(&mut client.stream) => {
                SelectResult::Frame(frame_result)
            }
        };

        match select_result {
            SelectResult::Command(cmd_opt) => match cmd_opt {
                Some(cmd) => match cmd {
                    SyncCommand::AddCell {
                        index,
                        cell_id,
                        cell_type,
                        reply,
                    } => {
                        let result = client.add_cell(index, &cell_id, &cell_type).await;
                        let _ = reply.send(result);
                    }
                    SyncCommand::DeleteCell { cell_id, reply } => {
                        let result = client.delete_cell(&cell_id).await;
                        let _ = reply.send(result);
                    }
                    SyncCommand::UpdateSource {
                        cell_id,
                        source,
                        reply,
                    } => {
                        let result = client.update_source(&cell_id, &source).await;
                        let _ = reply.send(result);
                    }
                    SyncCommand::AppendSource {
                        cell_id,
                        text,
                        reply,
                    } => {
                        let result = client.append_source(&cell_id, &text).await;
                        let _ = reply.send(result);
                    }
                    SyncCommand::ClearOutputs { cell_id, reply } => {
                        let result = client.clear_outputs(&cell_id).await;
                        let _ = reply.send(result);
                    }
                    SyncCommand::AppendOutput {
                        cell_id,
                        output,
                        reply,
                    } => {
                        let result = client.append_output(&cell_id, &output).await;
                        let _ = reply.send(result);
                    }
                    SyncCommand::SetExecutionCount {
                        cell_id,
                        count,
                        reply,
                    } => {
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
                    SyncCommand::SendRequest {
                        request,
                        reply,
                        broadcast_tx: override_tx,
                    } => {
                        // Use the override broadcast_tx if provided, otherwise use the task's broadcast_tx
                        // This allows broadcasts to be delivered immediately during long-running requests
                        let tx_to_use = override_tx.as_ref().unwrap_or(&broadcast_tx);
                        let result = client
                            .send_request_with_broadcast(&request, Some(tx_to_use))
                            .await;
                        let _ = reply.send(result);
                    }
                    SyncCommand::GetDocBytes { reply } => {
                        let bytes = client.doc.save();

                        // In pipe mode (Tauri), the frontend gets doc bytes from the
                        // daemon via send_request(GetDocBytes), not from this command.
                        // But runtimed-py tests may still call this. Skip the virtual
                        // sync handshake in pipe mode — the frontend and daemon sync
                        // directly through the byte pipe, so no relay peer state needed.
                        if raw_sync_tx.is_none() {
                            // Full peer mode (runtimed-py): initialize frontend_peer_state
                            // via virtual sync handshake so we know what the peer has seen.
                            let mut fe_state = sync::State::new();
                            if let Ok(mut mirror) = automerge::AutoCommit::load(&bytes) {
                                let mut mirror_state = sync::State::new();
                                for _ in 0..10 {
                                    let our_msg =
                                        client.doc.sync().generate_sync_message(&mut fe_state);
                                    let their_msg =
                                        mirror.sync().generate_sync_message(&mut mirror_state);
                                    if our_msg.is_none() && their_msg.is_none() {
                                        break;
                                    }
                                    if let Some(m) = our_msg {
                                        let _ = mirror
                                            .sync()
                                            .receive_sync_message(&mut mirror_state, m);
                                    }
                                    if let Some(m) = their_msg {
                                        let _ = client
                                            .doc
                                            .sync()
                                            .receive_sync_message(&mut fe_state, m);
                                    }
                                }
                                debug!(
                                    "[notebook-sync-task] Initialized frontend_peer_state via virtual sync for {}",
                                    notebook_id
                                );
                            }
                            frontend_peer_state = Some(fe_state);
                        }

                        let _ = reply.send(bytes);
                    }
                    SyncCommand::ReceiveFrontendSyncMessage { message, reply } => {
                        let result = if raw_sync_tx.is_some() {
                            // Pipe mode (Tauri): forward raw sync bytes to daemon
                            // without merging into the relay's doc. The daemon processes
                            // the sync message and sends back a response frame, which
                            // arrives in the socket read branch and is forwarded raw
                            // to the frontend via raw_sync_tx.
                            connection::send_typed_frame(
                                &mut client.stream,
                                NotebookFrameType::AutomergeSync,
                                &message,
                            )
                            .await
                            .map_err(|e| {
                                NotebookSyncError::SyncError(format!(
                                    "forward frontend sync to daemon: {}",
                                    e
                                ))
                            })
                        } else if let Some(ref mut fe_state) = frontend_peer_state {
                            // Full peer mode (runtimed-py): merge into local doc
                            match sync::Message::decode(&message) {
                                Ok(msg) => {
                                    let recv_result =
                                        client.doc.sync().receive_sync_message(fe_state, msg);
                                    match recv_result {
                                        Ok(()) => {
                                            // Relay the changes to the daemon
                                            client.sync_to_daemon().await
                                        }
                                        Err(e) => Err(NotebookSyncError::SyncError(format!(
                                            "receive frontend sync: {}",
                                            e
                                        ))),
                                    }
                                }
                                Err(e) => Err(NotebookSyncError::SyncError(format!(
                                    "decode frontend sync: {}",
                                    e
                                ))),
                            }
                        } else {
                            Err(NotebookSyncError::SyncError(
                                "frontend sync relay not active".to_string(),
                            ))
                        };
                        // In full peer mode, send response sync message back to frontend
                        if raw_sync_tx.is_none() {
                            if let (Some(ref tx), Some(ref mut fe_state)) =
                                (&raw_sync_tx, &mut frontend_peer_state)
                            {
                                if let Some(msg) = client.doc.sync().generate_sync_message(fe_state)
                                {
                                    let _ = tx.send(msg.encode());
                                }
                            }
                        }
                        let _ = reply.send(result);
                    }
                },
                None => {
                    // Command channel closed - handle was dropped
                    info!(
                        "[notebook-sync-task] Command channel closed for {} (handle dropped), loop_count={}",
                        notebook_id, loop_count
                    );
                    break;
                }
            },

            SelectResult::Frame(frame_result) => {
                // v2 protocol: direct socket read completed
                match frame_result {
                    Ok(Some(frame)) => {
                        // Pipe mode (Tauri): forward AutomergeSync frames raw to the
                        // frontend without merging into the relay's doc. This makes
                        // the relay a transparent byte pipe between frontend and daemon
                        // — two Automerge peers instead of three.
                        // Broadcast and Response frames still need normal processing.
                        if raw_sync_tx.is_some()
                            && frame.frame_type == NotebookFrameType::AutomergeSync
                        {
                            if let Some(ref tx) = raw_sync_tx {
                                let _ = tx.send(frame.payload);
                            }
                        } else {
                            match client.process_incoming_frame(frame).await {
                                Ok(Some(ReceivedFrame::Changes(cells))) => {
                                    // Full peer mode: metadata diffing and SyncUpdate
                                    let current_metadata =
                                        client.get_metadata(NOTEBOOK_METADATA_KEY);
                                    let metadata_changed = current_metadata != last_metadata;
                                    if metadata_changed {
                                        last_metadata = current_metadata.clone();
                                    }
                                    let update = SyncUpdate {
                                        cells,
                                        notebook_metadata: if metadata_changed {
                                            current_metadata
                                        } else {
                                            None
                                        },
                                    };
                                    match changes_tx.try_send(update) {
                                        Ok(()) => {}
                                        Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => {}
                                        Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                                            info!(
                                            "[notebook-sync-task] Changes receiver dropped for {}, loop_count={}",
                                            notebook_id, loop_count
                                        );
                                            break;
                                        }
                                    }
                                    // Forward sync message to frontend (full peer mode only)
                                    if let (Some(ref tx), Some(ref mut fe_state)) =
                                        (&raw_sync_tx, &mut frontend_peer_state)
                                    {
                                        if let Some(msg) =
                                            client.doc.sync().generate_sync_message(fe_state)
                                        {
                                            let _ = tx.send(msg.encode());
                                        }
                                    }
                                }
                                Ok(Some(ReceivedFrame::Broadcast(broadcast))) => {
                                    let send_result = broadcast_tx.send(broadcast);
                                    if send_result.is_err() {
                                        info!(
                                            "[notebook-sync-task] No broadcast receivers for {}",
                                            notebook_id
                                        );
                                    }
                                }
                                Ok(Some(ReceivedFrame::Response(_))) => {
                                    warn!(
                                        "[notebook-sync-task] Unexpected response frame for {}",
                                        notebook_id
                                    );
                                }
                                Ok(None) => {
                                    // Frame was handled internally (e.g., unexpected Request)
                                }
                                Err(e) => {
                                    warn!(
                                    "[notebook-sync-task] Error processing frame for {}: {}, loop_count={}",
                                    notebook_id, e, loop_count
                                );
                                    break;
                                }
                            }
                        } // end else (non-pipe mode)
                    }
                    Ok(None) => {
                        // Connection closed
                        warn!(
                            "[notebook-sync-task] Disconnected from daemon for {}, loop_count={}",
                            notebook_id, loop_count
                        );
                        break;
                    }
                    Err(e) => {
                        warn!(
                            "[notebook-sync-task] Socket error for {}: {}, loop_count={}",
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
