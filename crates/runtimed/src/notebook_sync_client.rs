//! Client for the notebook sync service.
//!
//! Each notebook window creates a `NotebookSyncClient` that maintains a local
//! Automerge document replica of the notebook. Changes made locally are sent
//! to the daemon, and changes from other peers arrive as sync messages.
//!
//! ## Remote Heads Tracking (Phase A)
//!
//! Full-peer clients can call [`NotebookSyncHandle::confirm_sync`] before
//! execution requests to verify the daemon has merged their latest changes.
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
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use crate::connection::{
    self, Handshake, NotebookConnectionInfo, NotebookFrameType, ProtocolCapabilities, PROTOCOL_V2,
    PROTOCOL_VERSION,
};
use crate::notebook_doc::{
    self, get_cells_from_doc, get_metadata_from_doc, get_metadata_snapshot_from_doc,
    set_metadata_in_doc, CellSnapshot,
};
use crate::notebook_metadata::{NotebookMetadataSnapshot, NOTEBOOK_METADATA_KEY};
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

/// Single channel for pipe mode (Tauri relay).
///
/// In pipe mode, the sync task forwards raw typed frame bytes to the Tauri
/// process instead of processing them locally. All frame types (AutomergeSync,
/// Broadcast, Presence) flow through one channel, preserving daemon-sent order.
/// Response frames are consumed by the request/response cycle and never piped.
pub struct PipeChannel {
    /// Raw typed frame bytes (frame type byte + payload) forwarded to the frontend.
    pub frame_tx: mpsc::UnboundedSender<Vec<u8>>,
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
    /// Atomically add a cell with source content in a single Automerge
    /// transaction, producing one sync round-trip to the daemon. This avoids
    /// the race where remote peers see the cell structure before its source.
    AddCellWithSource {
        index: usize,
        cell_id: String,
        cell_type: String,
        source: String,
        reply: oneshot::Sender<Result<(), NotebookSyncError>>,
    },
    DeleteCell {
        cell_id: String,
        reply: oneshot::Sender<Result<(), NotebookSyncError>>,
    },
    MoveCell {
        cell_id: String,
        after_cell_id: Option<String>,
        reply: oneshot::Sender<Result<String, NotebookSyncError>>,
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
    /// Set cell metadata in the Automerge doc and sync to daemon.
    SetCellMetadata {
        cell_id: String,
        metadata_json: String,
        reply: oneshot::Sender<Result<bool, NotebookSyncError>>,
    },
    /// Update cell metadata at a specific path and sync to daemon.
    UpdateCellMetadataAt {
        cell_id: String,
        path: Vec<String>,
        value_json: String,
        reply: oneshot::Sender<Result<bool, NotebookSyncError>>,
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
    /// Apply a raw Automerge sync message from the frontend and forward to daemon.
    ReceiveFrontendSyncMessage {
        message: Vec<u8>,
        reply: oneshot::Sender<Result<(), NotebookSyncError>>,
    },
    /// Confirm that the daemon has merged all our local changes by checking
    /// that `peer_state.shared_heads` includes our local heads.
    ConfirmSync {
        reply: oneshot::Sender<Result<(), NotebookSyncError>>,
    },
    /// Send a raw presence frame (type 0x04) to the daemon.
    SendPresence {
        data: Vec<u8>,
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
    snapshot_rx: watch::Receiver<NotebookSnapshot>,
}

impl NotebookSyncHandle {
    /// Get the notebook ID this handle is connected to.
    pub fn notebook_id(&self) -> &str {
        &self.notebook_id
    }

    /// Get all cells from the local replica.
    ///
    /// Instant synchronous read from the latest snapshot published by the sync
    /// task — no channel round-trip, no async. Modeled after automerge-repo's
    /// `DocHandle.doc()` pattern.
    pub fn get_cells(&self) -> Vec<CellSnapshot> {
        self.snapshot_rx.borrow().cells.as_ref().clone()
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

    /// Atomically add a cell with source content.
    ///
    /// Both the cell structure and its source text are written in a single
    /// Automerge transaction and synced to the daemon in one round-trip.
    /// This prevents remote peers from seeing an empty cell before the source
    /// arrives (which caused flaky multi-peer sync tests).
    pub async fn add_cell_with_source(
        &self,
        index: usize,
        cell_id: &str,
        cell_type: &str,
        source: &str,
    ) -> Result<(), NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::AddCellWithSource {
                index,
                cell_id: cell_id.to_string(),
                cell_type: cell_type.to_string(),
                source: source.to_string(),
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

    /// Move a cell to a new position. Returns the new fractional position string.
    pub async fn move_cell(
        &self,
        cell_id: &str,
        after_cell_id: Option<&str>,
    ) -> Result<String, NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::MoveCell {
                cell_id: cell_id.to_string(),
                after_cell_id: after_cell_id.map(|s| s.to_string()),
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
    ///
    /// `notebook_metadata` uses the watch snapshot fast path. Other metadata
    /// keys fall back to the sync task so callers keep the pre-watch behavior.
    pub async fn get_metadata(&self, key: &str) -> Result<Option<String>, NotebookSyncError> {
        if key == NOTEBOOK_METADATA_KEY {
            let snap = self.snapshot_rx.borrow();
            return Ok(snap
                .notebook_metadata
                .as_ref()
                .and_then(|m| serde_json::to_string(m).ok()));
        }

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

    /// Get the typed notebook metadata snapshot.
    ///
    /// Prefer this over `get_metadata("notebook_metadata")` followed by
    /// `serde_json::from_str` — the snapshot is already parsed.
    pub fn get_notebook_metadata(&self) -> Option<NotebookMetadataSnapshot> {
        self.snapshot_rx.borrow().notebook_metadata.clone()
    }

    /// Get cell metadata from the local snapshot.
    ///
    /// Returns None if the cell is not found.
    pub fn get_cell_metadata(&self, cell_id: &str) -> Option<serde_json::Value> {
        let snap = self.snapshot_rx.borrow();
        snap.cells
            .iter()
            .find(|c| c.id == cell_id)
            .map(|c| c.metadata.clone())
    }

    /// Set cell metadata and sync to daemon.
    ///
    /// Returns true if the cell was found and updated, false if not found.
    pub async fn set_cell_metadata(
        &self,
        cell_id: &str,
        metadata: &serde_json::Value,
    ) -> Result<bool, NotebookSyncError> {
        let metadata_json = serde_json::to_string(metadata)
            .map_err(|e| NotebookSyncError::SyncError(format!("serialize metadata: {}", e)))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::SetCellMetadata {
                cell_id: cell_id.to_string(),
                metadata_json,
                reply: reply_tx,
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
    }

    /// Update cell metadata at a specific path and sync to daemon.
    ///
    /// Path is a sequence of keys, e.g., `["jupyter", "source_hidden"]`.
    /// Returns true if the cell was found and updated, false if not found.
    pub async fn update_cell_metadata_at(
        &self,
        cell_id: &str,
        path: &[&str],
        value: serde_json::Value,
    ) -> Result<bool, NotebookSyncError> {
        let value_json = serde_json::to_string(&value)
            .map_err(|e| NotebookSyncError::SyncError(format!("serialize value: {}", e)))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::UpdateCellMetadataAt {
                cell_id: cell_id.to_string(),
                path: path.iter().map(|s| s.to_string()).collect(),
                value_json,
                reply: reply_tx,
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
    }

    /// Best-effort confirmation that the daemon has merged our local changes.
    ///
    /// Intended for full-peer programmatic clients (`runtimed-py`) where
    /// `create_cell` → `execute_cell` can fire in microseconds. Not needed
    /// for the Tauri pipe path — the WASM frontend owns its doc locally,
    /// and human interaction provides natural sync latency.
    ///
    /// Attempts up to 5 sync rounds, checking `peer_state.shared_heads`
    /// after each. If confirmation does not arrive, degrades gracefully
    /// and returns `Ok(())` because failing execution is worse than the
    /// residual race — after 5 rounds the changes are almost certainly
    /// applied, the heads just haven't fully converged (e.g. concurrent
    /// edits from another peer).
    pub async fn confirm_sync(&self) -> Result<(), NotebookSyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::ConfirmSync { reply: reply_tx })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
    }

    /// Send a raw presence frame (type 0x04) to the daemon.
    ///
    /// The data should be encoded via `notebook_doc::presence::encode_*` functions.
    /// The daemon will decode, update room state, and relay to other peers.
    ///
    /// Returns an error if the frame exceeds `MAX_PRESENCE_FRAME_SIZE` (4 KiB).
    pub async fn send_presence(&self, data: Vec<u8>) -> Result<(), NotebookSyncError> {
        notebook_doc::presence::validate_frame_size(&data)
            .map_err(|e| NotebookSyncError::SyncError(e.to_string()))?;
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(SyncCommand::SendPresence {
                data,
                reply: reply_tx,
            })
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?;
        reply_rx
            .await
            .map_err(|_| NotebookSyncError::ChannelClosed)?
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

/// Materialized read-only snapshot of the notebook state.
///
/// Published via `tokio::watch` channel after every doc mutation in the sync
/// task. Readers (via `NotebookSyncHandle`) get instant access without any
/// channel round-trip or locking.
///
/// Modeled after automerge-repo's `DocHandle.doc()` pattern: the sync task is
/// the single owner/writer of the Automerge doc, and publishes snapshots for
/// consumers.
#[derive(Clone, Debug, Default)]
pub struct NotebookSnapshot {
    pub cells: std::sync::Arc<Vec<CellSnapshot>>,
    /// Typed notebook metadata (kernelspec, language_info, runt deps/trust).
    ///
    /// `None` when the doc has no `metadata.notebook_metadata` key yet (e.g.
    /// freshly created notebooks before the daemon seeds metadata).
    pub notebook_metadata: Option<NotebookMetadataSnapshot>,
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
    /// Raw AutomergeSync frame payloads buffered during request/response cycles.
    /// In pipe mode, `wait_for_response_with_broadcast` consumes sync frames from
    /// the socket that should be forwarded to the frontend. These are buffered
    /// here and drained by `run_sync_task` after each `SendRequest` completes.
    /// Each entry is a fully-typed frame (type byte + payload) that can include
    /// AutomergeSync, Broadcast, or Presence frames.
    pending_pipe_frames: Vec<Vec<u8>>,
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

    /// Connect and return split handle/receiver with unified frame pipe support.
    ///
    /// When a `PipeChannel` is provided, incoming typed frames (AutomergeSync,
    /// Broadcast, Presence) from the daemon are forwarded as raw bytes through
    /// one channel, preserving daemon-sent order. The Tauri relay emits these
    /// as `notebook:frame` events for the frontend WASM to demux.
    pub async fn connect_split_with_pipe(
        socket_path: PathBuf,
        notebook_id: String,
        working_dir: Option<PathBuf>,
        initial_metadata: Option<String>,
        pipe_channel: Option<PipeChannel>,
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
        Ok(client.into_split_with_pipe(pipe_channel))
    }

    /// Connect by opening an existing notebook file (daemon-owned loading).
    ///
    /// The daemon loads the file, derives the notebook_id, and returns it.
    /// Returns NotebookConnectionInfo so caller can get notebook_id and trust status.
    pub async fn connect_open_split(
        socket_path: PathBuf,
        path: PathBuf,
        pipe_channel: Option<PipeChannel>,
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

        let pipe_mode = pipe_channel.is_some();
        let (client, info) = Self::init_open_notebook(stream, path, pipe_mode).await?;
        let (handle, receiver, broadcast_rx, cells, metadata) =
            client.into_split_with_pipe(pipe_channel);
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
        notebook_id: Option<String>,
        pipe_channel: Option<PipeChannel>,
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

        let pipe_mode = pipe_channel.is_some();
        let (client, info) =
            Self::init_create_notebook(stream, runtime, working_dir, notebook_id, pipe_mode)
                .await?;
        let (handle, receiver, broadcast_rx, cells, metadata) =
            client.into_split_with_pipe(pipe_channel);
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

    /// Connect and return split handle/receiver with unified frame pipe support.
    ///
    /// When a `PipeChannel` is provided, incoming typed frames (AutomergeSync,
    /// Broadcast, Presence) from the daemon are forwarded as raw bytes through
    /// one channel, preserving daemon-sent order.
    pub async fn connect_split_with_pipe(
        socket_path: PathBuf,
        notebook_id: String,
        working_dir: Option<PathBuf>,
        initial_metadata: Option<String>,
        pipe_channel: Option<PipeChannel>,
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
        Ok(client.into_split_with_pipe(pipe_channel))
    }

    /// Connect by opening an existing notebook file (daemon-owned loading).
    pub async fn connect_open_split(
        socket_path: PathBuf,
        path: PathBuf,
        pipe_channel: Option<PipeChannel>,
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

        let pipe_mode = pipe_channel.is_some();
        let (client, info) = Self::init_open_notebook(stream, path, pipe_mode).await?;
        let (handle, receiver, broadcast_rx, cells, metadata) =
            client.into_split_with_pipe(pipe_channel);
        Ok((handle, receiver, broadcast_rx, cells, metadata, info))
    }

    /// Connect by creating a new notebook (daemon-owned creation).
    pub async fn connect_create_split(
        socket_path: PathBuf,
        runtime: String,
        working_dir: Option<PathBuf>,
        notebook_id: Option<String>,
        pipe_channel: Option<PipeChannel>,
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

        let pipe_mode = pipe_channel.is_some();
        let (client, info) =
            Self::init_create_notebook(stream, runtime, working_dir, notebook_id, pipe_mode)
                .await?;
        let (handle, receiver, broadcast_rx, cells, metadata) =
            client.into_split_with_pipe(pipe_channel);
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
        // Send preamble (magic bytes + protocol version)
        connection::send_preamble(&mut stream)
            .await
            .map_err(|e| NotebookSyncError::SyncError(format!("preamble: {}", e)))?;

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
                // Validate numeric protocol_version if present (#635)
                if let Some(version) = caps.protocol_version {
                    if version != PROTOCOL_VERSION {
                        return Err(NotebookSyncError::SyncError(format!(
                            "protocol version mismatch: server has {}, client expects {}",
                            version, PROTOCOL_VERSION
                        )));
                    }
                }
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
                    NotebookFrameType::Presence => {
                        // Presence frames are currently unsupported in this client; ignore during init.
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
            pending_pipe_frames: Vec::new(),
        })
    }

    /// Initialize by opening an existing notebook file.
    ///
    /// The daemon loads the file, derives notebook_id, and returns NotebookConnectionInfo.
    async fn init_open_notebook(
        mut stream: S,
        path: PathBuf,
        pipe_mode: bool,
    ) -> Result<(Self, NotebookConnectionInfo), NotebookSyncError> {
        let path_str = path.to_string_lossy().to_string();
        info!("[notebook-sync-client] Opening notebook: {}", path_str);

        // Send preamble (magic bytes + protocol version)
        connection::send_preamble(&mut stream)
            .await
            .map_err(|e| NotebookSyncError::SyncError(format!("preamble: {}", e)))?;

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
        // Validate numeric protocol_version if present (#635)
        if let Some(version) = info.protocol_version {
            if version != PROTOCOL_VERSION {
                return Err(NotebookSyncError::SyncError(format!(
                    "protocol version mismatch: server has {}, client expects {}",
                    version, PROTOCOL_VERSION
                )));
            }
        }

        let notebook_id = info.notebook_id.clone();
        info!(
            "[notebook-sync-client] Daemon returned notebook_id: {} ({} cells, trust_approval: {})",
            notebook_id, info.cell_count, info.needs_trust_approval
        );

        // In pipe mode the relay is a transparent byte pipe — the frontend WASM
        // handles the full Automerge sync protocol with the daemon directly.
        // Skipping do_initial_sync means the daemon's peer_state tracks the WASM
        // peer (through the pipe) instead of the relay, fixing the sync state
        // mismatch that caused changed=false on every frame (#617).
        let client = if pipe_mode {
            info!(
                "[notebook-sync-client] Pipe mode: skipping initial sync for {}",
                notebook_id
            );
            Self {
                doc: AutoCommit::new(),
                peer_state: sync::State::new(),
                stream,
                notebook_id,
                pending_broadcasts: Vec::new(),
                pending_pipe_frames: Vec::new(),
            }
        } else {
            Self::do_initial_sync(stream, notebook_id).await?
        };
        Ok((client, info))
    }

    /// Initialize by creating a new notebook.
    ///
    /// The daemon creates an empty notebook and returns NotebookConnectionInfo.
    async fn init_create_notebook(
        mut stream: S,
        runtime: String,
        working_dir: Option<PathBuf>,
        notebook_id: Option<String>,
        pipe_mode: bool,
    ) -> Result<(Self, NotebookConnectionInfo), NotebookSyncError> {
        info!(
            "[notebook-sync-client] Creating new notebook (runtime: {}, working_dir: {:?}, notebook_id: {:?})",
            runtime, working_dir, notebook_id
        );

        // Send preamble (magic bytes + protocol version)
        connection::send_preamble(&mut stream)
            .await
            .map_err(|e| NotebookSyncError::SyncError(format!("preamble: {}", e)))?;

        // Send CreateNotebook handshake
        connection::send_json_frame(
            &mut stream,
            &Handshake::CreateNotebook {
                runtime,
                working_dir: working_dir.map(|p| p.to_string_lossy().to_string()),
                notebook_id,
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
        // Validate numeric protocol_version if present (#635)
        if let Some(version) = info.protocol_version {
            if version != PROTOCOL_VERSION {
                return Err(NotebookSyncError::SyncError(format!(
                    "protocol version mismatch: server has {}, client expects {}",
                    version, PROTOCOL_VERSION
                )));
            }
        }

        let notebook_id = info.notebook_id.clone();
        info!(
            "[notebook-sync-client] Daemon created notebook_id: {} ({} cells)",
            notebook_id, info.cell_count
        );

        // See init_open_notebook for explanation of the pipe_mode skip.
        let client = if pipe_mode {
            info!(
                "[notebook-sync-client] Pipe mode: skipping initial sync for {}",
                notebook_id
            );
            Self {
                doc: AutoCommit::new(),
                peer_state: sync::State::new(),
                stream,
                notebook_id,
                pending_broadcasts: Vec::new(),
                pending_pipe_frames: Vec::new(),
            }
        } else {
            Self::do_initial_sync(stream, notebook_id).await?
        };
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
                    NotebookFrameType::Presence => {
                        // Presence frames are currently unsupported in this client; ignore during initial sync.
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
            pending_pipe_frames: Vec::new(),
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

    /// Set cell metadata and sync to daemon.
    ///
    /// Returns true if the cell was found and updated, false if not found.
    pub async fn set_cell_metadata(
        &mut self,
        cell_id: &str,
        metadata: &serde_json::Value,
    ) -> Result<bool, NotebookSyncError> {
        let mut doc = notebook_doc::NotebookDoc::wrap(std::mem::take(&mut self.doc));
        let result = doc
            .set_cell_metadata(cell_id, metadata)
            .map_err(|e| NotebookSyncError::SyncError(format!("set_cell_metadata: {}", e)))?;
        self.doc = doc.into_inner();
        if result {
            self.sync_to_daemon().await?;
        }
        Ok(result)
    }

    /// Update cell metadata at a specific path and sync to daemon.
    ///
    /// Returns true if the cell was found and updated, false if not found.
    pub async fn update_cell_metadata_at(
        &mut self,
        cell_id: &str,
        path: &[&str],
        value: serde_json::Value,
    ) -> Result<bool, NotebookSyncError> {
        let mut doc = notebook_doc::NotebookDoc::wrap(std::mem::take(&mut self.doc));
        let result = doc
            .update_cell_metadata_at(cell_id, path, value)
            .map_err(|e| NotebookSyncError::SyncError(format!("update_cell_metadata_at: {}", e)))?;
        self.doc = doc.into_inner();
        if result {
            self.sync_to_daemon().await?;
        }
        Ok(result)
    }

    /// Add a new cell at the given index and sync to daemon.
    ///
    /// Internally converts the index to a fractional position using the
    /// Map-based cell schema (schema_version 2).
    pub async fn add_cell(
        &mut self,
        index: usize,
        cell_id: &str,
        cell_type: &str,
    ) -> Result<(), NotebookSyncError> {
        use loro_fractional_index::FractionalIndex;

        let cells_id = self
            .ensure_cells_map()
            .map_err(|e| NotebookSyncError::SyncError(format!("ensure cells: {}", e)))?;

        // Compute position from index: get sorted cells, find neighbors
        let sorted_cells = get_cells_from_doc(&self.doc);
        let position = if sorted_cells.is_empty() {
            FractionalIndex::default()
        } else if index == 0 {
            let first_pos = FractionalIndex::from_hex_string(&sorted_cells[0].position);
            FractionalIndex::new_before(&first_pos)
        } else {
            let clamped = index.min(sorted_cells.len());
            let prev_pos = FractionalIndex::from_hex_string(&sorted_cells[clamped - 1].position);
            if clamped < sorted_cells.len() {
                let next_pos = FractionalIndex::from_hex_string(&sorted_cells[clamped].position);
                FractionalIndex::new(Some(&prev_pos), Some(&next_pos))
                    .unwrap_or_else(|| FractionalIndex::new_after(&prev_pos))
            } else {
                FractionalIndex::new_after(&prev_pos)
            }
        };

        let cell_map = self
            .doc
            .put_object(&cells_id, cell_id, ObjType::Map)
            .map_err(|e| NotebookSyncError::SyncError(format!("put cell: {}", e)))?;
        self.doc
            .put(&cell_map, "id", cell_id)
            .map_err(|e| NotebookSyncError::SyncError(format!("put id: {}", e)))?;
        self.doc
            .put(&cell_map, "cell_type", cell_type)
            .map_err(|e| NotebookSyncError::SyncError(format!("put type: {}", e)))?;
        self.doc
            .put(&cell_map, "position", position.to_string().as_str())
            .map_err(|e| NotebookSyncError::SyncError(format!("put position: {}", e)))?;
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

    /// Atomically add a cell with source content and sync to daemon.
    ///
    /// Combines add_cell + update_source into a single Automerge transaction
    /// so the cell structure and source text arrive as one sync message.
    pub async fn add_cell_with_source(
        &mut self,
        index: usize,
        cell_id: &str,
        cell_type: &str,
        source: &str,
    ) -> Result<(), NotebookSyncError> {
        use loro_fractional_index::FractionalIndex;

        let cells_id = self
            .ensure_cells_map()
            .map_err(|e| NotebookSyncError::SyncError(format!("ensure cells: {}", e)))?;

        // Compute position from index
        let sorted_cells = get_cells_from_doc(&self.doc);
        let position = if sorted_cells.is_empty() {
            FractionalIndex::default()
        } else if index == 0 {
            let first_pos = FractionalIndex::from_hex_string(&sorted_cells[0].position);
            FractionalIndex::new_before(&first_pos)
        } else {
            let clamped = index.min(sorted_cells.len());
            let prev_pos = FractionalIndex::from_hex_string(&sorted_cells[clamped - 1].position);
            if clamped < sorted_cells.len() {
                let next_pos = FractionalIndex::from_hex_string(&sorted_cells[clamped].position);
                FractionalIndex::new(Some(&prev_pos), Some(&next_pos))
                    .unwrap_or_else(|| FractionalIndex::new_after(&prev_pos))
            } else {
                FractionalIndex::new_after(&prev_pos)
            }
        };

        let cell_map = self
            .doc
            .put_object(&cells_id, cell_id, ObjType::Map)
            .map_err(|e| NotebookSyncError::SyncError(format!("put cell: {}", e)))?;
        self.doc
            .put(&cell_map, "id", cell_id)
            .map_err(|e| NotebookSyncError::SyncError(format!("put id: {}", e)))?;
        self.doc
            .put(&cell_map, "cell_type", cell_type)
            .map_err(|e| NotebookSyncError::SyncError(format!("put type: {}", e)))?;
        self.doc
            .put(&cell_map, "position", position.to_string().as_str())
            .map_err(|e| NotebookSyncError::SyncError(format!("put position: {}", e)))?;
        let source_id = self
            .doc
            .put_object(&cell_map, "source", ObjType::Text)
            .map_err(|e| NotebookSyncError::SyncError(format!("put source: {}", e)))?;
        // Write source text in the same transaction (before syncing to daemon)
        if !source.is_empty() {
            self.doc
                .splice_text(&source_id, 0, 0, source)
                .map_err(|e| NotebookSyncError::SyncError(format!("splice source: {}", e)))?;
        }
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };

        // Direct key delete — O(1), no find_cell_index needed
        self.doc
            .delete(&cells_id, cell_id)
            .map_err(|e| NotebookSyncError::SyncError(format!("delete: {}", e)))?;

        self.sync_to_daemon().await
    }

    /// Move a cell to a new position and sync to daemon.
    /// Returns the new fractional position string.
    pub async fn move_cell(
        &mut self,
        cell_id: &str,
        after_cell_id: Option<&str>,
    ) -> Result<String, NotebookSyncError> {
        use loro_fractional_index::FractionalIndex;
        use notebook_doc::get_cells_from_doc;

        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
            Some(o) => o,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };

        // Compute new position from sorted cells
        let sorted_cells = get_cells_from_doc(&self.doc);
        let position = match after_cell_id {
            None => {
                // Move to start
                if sorted_cells.is_empty() {
                    FractionalIndex::default()
                } else {
                    let first_pos = FractionalIndex::from_hex_string(&sorted_cells[0].position);
                    FractionalIndex::new_before(&first_pos)
                }
            }
            Some(after_id) => {
                let idx = sorted_cells.iter().position(|c| c.id == after_id);
                match idx {
                    Some(i) if i + 1 < sorted_cells.len() => {
                        let prev = FractionalIndex::from_hex_string(&sorted_cells[i].position);
                        let next = FractionalIndex::from_hex_string(&sorted_cells[i + 1].position);
                        FractionalIndex::new(Some(&prev), Some(&next))
                            .unwrap_or_else(|| FractionalIndex::new_after(&prev))
                    }
                    Some(i) => FractionalIndex::new_after(&FractionalIndex::from_hex_string(
                        &sorted_cells[i].position,
                    )),
                    None => {
                        // after_cell_id not found: fall back to end
                        sorted_cells
                            .last()
                            .map(|c| {
                                FractionalIndex::new_after(&FractionalIndex::from_hex_string(
                                    &c.position,
                                ))
                            })
                            .unwrap_or_default()
                    }
                }
            }
        };

        let position_str = position.to_string();
        self.doc
            .put(&cell_obj, "position", position_str.as_str())
            .map_err(|e| NotebookSyncError::SyncError(format!("put position: {}", e)))?;

        self.sync_to_daemon().await?;
        Ok(position_str)
    }

    /// Update a cell's source text and sync to daemon.
    pub async fn update_source(
        &mut self,
        cell_id: &str,
        source: &str,
    ) -> Result<(), NotebookSyncError> {
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
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
        let cells_id = match self.cells_map_id() {
            Some(id) => id,
            None => return Err(NotebookSyncError::CellNotFound(cell_id.to_string())),
        };
        let cell_obj = match self.cell_obj_id(&cells_id, cell_id) {
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
                NotebookFrameType::Presence => {
                    // Ignore presence frames in recv_changes
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
            NotebookFrameType::Presence => {
                // Skip presence frames in process_incoming_frame
                Ok(None)
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

            // Loop until we receive the AutomergeSync ack or the deadline
            // expires. Previous code read exactly one frame, so a Broadcast
            // arriving before the ack would be silently dropped and the ack
            // left unprocessed in the buffer — leaving peer_state stale.
            let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
            loop {
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    break; // Timeout — server had nothing to send back
                }

                match tokio::time::timeout(
                    remaining,
                    connection::recv_typed_frame(&mut self.stream),
                )
                .await
                {
                    Ok(Ok(Some(frame))) => {
                        if frame.frame_type == NotebookFrameType::AutomergeSync {
                            let message = sync::Message::decode(&frame.payload).map_err(|e| {
                                NotebookSyncError::SyncError(format!("decode: {}", e))
                            })?;
                            self.doc
                                .sync()
                                .receive_sync_message(&mut self.peer_state, message)
                                .map_err(|e| {
                                    NotebookSyncError::SyncError(format!("receive: {}", e))
                                })?;
                            break; // Got the ack
                        }
                        // Queue non-ack frames (broadcasts) for later delivery
                        // instead of silently dropping them.
                        if frame.frame_type == NotebookFrameType::Broadcast {
                            if let Ok(broadcast) =
                                serde_json::from_slice::<NotebookBroadcast>(&frame.payload)
                            {
                                self.pending_broadcasts.push(broadcast);
                            }
                        }
                        // Continue looping to find the ack
                    }
                    Ok(Ok(None)) => return Err(NotebookSyncError::Disconnected),
                    Ok(Err(e)) => return Err(NotebookSyncError::ConnectionFailed(e)),
                    Err(_) => break, // Timeout — server had nothing to send back
                }
            }
        }
        Ok(())
    }

    /// Best-effort confirmation that the daemon has merged our local
    /// changes, by checking `peer_state.shared_heads` after sync.
    ///
    /// **Scope:** Full-peer clients (`runtimed-py`) only. The Tauri pipe
    /// relay keeps an empty local doc and forwards raw bytes — it has no
    /// meaningful heads to confirm. The WASM frontend doesn't need this
    /// because human interaction provides natural sync latency.
    ///
    /// **Contract:** Attempts confirmation for a bounded number of rounds
    /// (currently 5). If confirmation does not arrive, degrades gracefully
    /// and returns `Ok(())`. Failing execution is worse than the residual
    /// race — after multiple sync round-trips the changes are almost
    /// certainly applied, the heads just haven't converged yet (e.g.
    /// concurrent edits from another peer, or slow daemon under load).
    ///
    /// **Why it exists:** In the document-first architecture, the daemon
    /// reads cell source from its own Automerge doc when executing. If
    /// `create_cell` → `execute_cell` fires faster than the sync
    /// round-trip, the daemon won't find the cell. This method closes
    /// that gap for programmatic callers.
    async fn sync_to_daemon_confirmed(&mut self) -> Result<(), NotebookSyncError> {
        // Do the initial sync round
        self.sync_to_daemon().await?;

        let our_heads = self.doc.get_heads();
        if our_heads.is_empty() {
            return Ok(()); // Empty doc, nothing to confirm
        }

        // The sync protocol may need multiple rounds. Bound the retries
        // so we don't loop forever if something is wrong.
        for _ in 0..5 {
            let shared = &self.peer_state.shared_heads;
            if our_heads.iter().all(|h| shared.contains(h)) {
                return Ok(()); // Daemon has confirmed all our changes
            }
            // Not yet confirmed — do another sync round
            self.sync_to_daemon().await?;
        }

        // Best-effort: even if not fully confirmed after retries, the
        // changes are very likely applied. Log and continue rather than
        // failing the mutation — a hard error here would be worse than
        // the original race.
        log::debug!(
            "[notebook-sync-client] sync_to_daemon_confirmed: heads not fully confirmed after retries (our_heads={}, shared_heads={})",
            our_heads.len(),
            self.peer_state.shared_heads.len()
        );
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
                        // Buffer as typed frame bytes (type byte + payload) so
                        // run_sync_task can forward them through the unified pipe.
                        // Without this, frames arriving during request/response
                        // (e.g. run-all sending 5x ClearOutputs) are consumed
                        // by the relay and never reach the frontend.
                        let mut typed = vec![NotebookFrameType::AutomergeSync as u8];
                        typed.extend_from_slice(&frame.payload);
                        self.pending_pipe_frames.push(typed);

                        // Also merge into the relay's local doc (needed for
                        // non-pipe mode / runtimed-py, harmless in pipe mode).
                        let message = sync::Message::decode(&frame.payload)
                            .map_err(|e| NotebookSyncError::SyncError(format!("decode: {}", e)))?;
                        self.doc
                            .sync()
                            .receive_sync_message(&mut self.peer_state, message)
                            .map_err(|e| NotebookSyncError::SyncError(format!("receive: {}", e)))?;
                        // Continue waiting for Response
                    }
                    NotebookFrameType::Broadcast => {
                        // Buffer as typed frame bytes for the unified pipe.
                        let mut typed = vec![NotebookFrameType::Broadcast as u8];
                        typed.extend_from_slice(&frame.payload);
                        self.pending_pipe_frames.push(typed);

                        // Also deliver via broadcast_tx for non-pipe consumers.
                        if let Ok(broadcast) =
                            serde_json::from_slice::<NotebookBroadcast>(&frame.payload)
                        {
                            if let Some(tx) = broadcast_tx {
                                if tx.send(broadcast.clone()).is_err() {
                                    self.pending_broadcasts.push(broadcast);
                                }
                            } else {
                                self.pending_broadcasts.push(broadcast);
                            }
                        }
                        continue;
                    }
                    NotebookFrameType::Presence => {
                        // Buffer as typed frame bytes for the unified pipe.
                        let mut typed = vec![NotebookFrameType::Presence as u8];
                        typed.extend_from_slice(&frame.payload);
                        self.pending_pipe_frames.push(typed);
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

    fn cells_map_id(&self) -> Option<automerge::ObjId> {
        self.doc
            .get(automerge::ROOT, "cells")
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Map) => Some(id),
                _ => None,
            })
    }

    fn ensure_cells_map(&mut self) -> Result<automerge::ObjId, automerge::AutomergeError> {
        if let Some(id) = self.cells_map_id() {
            return Ok(id);
        }
        self.doc.put_object(automerge::ROOT, "cells", ObjType::Map)
    }

    fn cell_obj_id(&self, cells_id: &automerge::ObjId, cell_id: &str) -> Option<automerge::ObjId> {
        self.doc
            .get(cells_id, cell_id)
            .ok()
            .flatten()
            .and_then(|(value, id)| match value {
                automerge::Value::Object(ObjType::Map) => Some(id),
                _ => None,
            })
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
        self.into_split_with_pipe(None)
    }

    /// Split into handle/receiver/broadcast, optionally forwarding raw Automerge
    /// sync messages from the daemon to a channel.
    ///
    /// When `raw_sync_tx` is provided, incoming `AutomergeSync` frames from the
    /// daemon are forwarded as raw typed frame bytes to this channel. This
    /// enables the Tauri process to relay all frame types to the frontend
    /// through a single event.
    pub fn into_split_with_pipe(
        self,
        pipe_channel: Option<PipeChannel>,
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

        // Watch channel: sync task publishes snapshots, handle reads instantly.
        // This is the Rust equivalent of automerge-repo's DocHandle.doc() pattern.
        let initial_snapshot = NotebookSnapshot {
            cells: std::sync::Arc::new(initial_cells.clone()),
            notebook_metadata: get_metadata_snapshot_from_doc(&self.doc),
        };
        let (snapshot_tx, snapshot_rx) = watch::channel(initial_snapshot);

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
                pipe_channel,
                snapshot_tx,
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
            snapshot_rx,
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
/// Publish a materialized snapshot of the current doc state via the watch channel.
///
/// Called after every successful doc mutation (local write or incoming sync frame)
/// so that `NotebookSyncHandle` readers always see the latest state without any
/// channel round-trip.
fn publish_snapshot<S: AsyncRead + AsyncWrite + Unpin>(
    client: &NotebookSyncClient<S>,
    snapshot_tx: &watch::Sender<NotebookSnapshot>,
) {
    let snapshot = NotebookSnapshot {
        cells: std::sync::Arc::new(get_cells_from_doc(&client.doc)),
        notebook_metadata: get_metadata_snapshot_from_doc(&client.doc),
    };
    let _ = snapshot_tx.send(snapshot);
}

async fn run_sync_task<S>(
    mut client: NotebookSyncClient<S>,
    mut cmd_rx: mpsc::Receiver<SyncCommand>,
    changes_tx: mpsc::Sender<SyncUpdate>,
    broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    pipe_channel: Option<PipeChannel>,
    snapshot_tx: watch::Sender<NotebookSnapshot>,
) where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let notebook_id = client.notebook_id().to_string();
    info!(
        "[notebook-sync-task] Starting for {} (changes_tx strong_count before loop: N/A)",
        notebook_id
    );

    // Buffer for outgoing sync frames in pipe mode. Instead of writing directly
    // to client.stream inside the command handler (which can corrupt framing if
    // a socket read is pending in the select!), we queue bytes here and flush
    // them at the top of the loop before entering select!.
    let mut pending_pipe_frames: std::collections::VecDeque<Vec<u8>> =
        std::collections::VecDeque::new();

    let mut loop_count = 0u64;
    // Track last metadata to only send updates when it actually changes
    let mut last_metadata: Option<String> = client.get_metadata(NOTEBOOK_METADATA_KEY);

    loop {
        loop_count += 1;

        // Flush any queued pipe frames to the daemon BEFORE entering select!.
        // This ensures writes happen when no read is pending on the socket.
        while let Some(frame_data) = pending_pipe_frames.pop_front() {
            if let Err(e) = connection::send_typed_frame(
                &mut client.stream,
                NotebookFrameType::AutomergeSync,
                &frame_data,
            )
            .await
            {
                warn!(
                    "[notebook-sync-task] Failed to flush pipe frame for {}: {}",
                    notebook_id, e
                );
                break;
            }
        }

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

        // Select between commands and incoming frames with fair scheduling.
        // Both branches are equally likely to be chosen when ready, ensuring
        // sync frames from daemon aren't starved by command polling.
        enum SelectResult {
            Command(Option<SyncCommand>),
            Frame(std::io::Result<Option<connection::TypedNotebookFrame>>),
        }

        let select_result = tokio::select! {
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
                        if result.is_ok() {
                            publish_snapshot(&client, &snapshot_tx);
                        }
                        let _ = reply.send(result);
                    }
                    SyncCommand::AddCellWithSource {
                        index,
                        cell_id,
                        cell_type,
                        source,
                        reply,
                    } => {
                        let result = client
                            .add_cell_with_source(index, &cell_id, &cell_type, &source)
                            .await;
                        if result.is_ok() {
                            publish_snapshot(&client, &snapshot_tx);
                        }
                        let _ = reply.send(result);
                    }
                    SyncCommand::DeleteCell { cell_id, reply } => {
                        let result = client.delete_cell(&cell_id).await;
                        if result.is_ok() {
                            publish_snapshot(&client, &snapshot_tx);
                        }
                        let _ = reply.send(result);
                    }
                    SyncCommand::MoveCell {
                        cell_id,
                        after_cell_id,
                        reply,
                    } => {
                        let result = client.move_cell(&cell_id, after_cell_id.as_deref()).await;
                        if result.is_ok() {
                            publish_snapshot(&client, &snapshot_tx);
                        }
                        let _ = reply.send(result);
                    }
                    SyncCommand::UpdateSource {
                        cell_id,
                        source,
                        reply,
                    } => {
                        let result = client.update_source(&cell_id, &source).await;
                        if result.is_ok() {
                            publish_snapshot(&client, &snapshot_tx);
                        }
                        let _ = reply.send(result);
                    }
                    SyncCommand::AppendSource {
                        cell_id,
                        text,
                        reply,
                    } => {
                        let result = client.append_source(&cell_id, &text).await;
                        if result.is_ok() {
                            publish_snapshot(&client, &snapshot_tx);
                        }
                        let _ = reply.send(result);
                    }
                    SyncCommand::ClearOutputs { cell_id, reply } => {
                        let result = client.clear_outputs(&cell_id).await;
                        if result.is_ok() {
                            publish_snapshot(&client, &snapshot_tx);
                        }
                        let _ = reply.send(result);
                    }
                    SyncCommand::AppendOutput {
                        cell_id,
                        output,
                        reply,
                    } => {
                        let result = client.append_output(&cell_id, &output).await;
                        if result.is_ok() {
                            publish_snapshot(&client, &snapshot_tx);
                        }
                        let _ = reply.send(result);
                    }
                    SyncCommand::SetExecutionCount {
                        cell_id,
                        count,
                        reply,
                    } => {
                        let result = client.set_execution_count(&cell_id, &count).await;
                        if result.is_ok() {
                            publish_snapshot(&client, &snapshot_tx);
                        }
                        let _ = reply.send(result);
                    }
                    SyncCommand::SetMetadata { key, value, reply } => {
                        let result = client.set_metadata(&key, &value).await;
                        if result.is_ok() {
                            publish_snapshot(&client, &snapshot_tx);
                        }
                        let _ = reply.send(result);
                    }
                    SyncCommand::GetMetadata { key, reply } => {
                        let _ = reply.send(client.get_metadata(&key));
                    }
                    SyncCommand::SetCellMetadata {
                        cell_id,
                        metadata_json,
                        reply,
                    } => {
                        let result = match serde_json::from_str(&metadata_json) {
                            Ok(metadata) => {
                                let res = client.set_cell_metadata(&cell_id, &metadata).await;
                                if res.is_ok() {
                                    publish_snapshot(&client, &snapshot_tx);
                                }
                                res
                            }
                            Err(e) => Err(NotebookSyncError::SyncError(format!(
                                "parse metadata: {}",
                                e
                            ))),
                        };
                        let _ = reply.send(result);
                    }
                    SyncCommand::UpdateCellMetadataAt {
                        cell_id,
                        path,
                        value_json,
                        reply,
                    } => {
                        let result = match serde_json::from_str(&value_json) {
                            Ok(value) => {
                                let path_refs: Vec<&str> =
                                    path.iter().map(|s| s.as_str()).collect();
                                let res = client
                                    .update_cell_metadata_at(&cell_id, &path_refs, value)
                                    .await;
                                if res.is_ok() {
                                    publish_snapshot(&client, &snapshot_tx);
                                }
                                res
                            }
                            Err(e) => {
                                Err(NotebookSyncError::SyncError(format!("parse value: {}", e)))
                            }
                        };
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

                        // Forward any frames that were buffered during the
                        // request/response wait. In pipe mode these must reach
                        // the frontend; without this, run-all breaks because
                        // sync frames for cleared outputs get consumed.
                        if let Some(ref pipe) = pipe_channel {
                            // pending_pipe_frames already stores fully-typed frames
                            // (type byte + payload). Forward as-is — no re-prefixing.
                            for frame_bytes in client.pending_pipe_frames.drain(..) {
                                let _ = pipe.frame_tx.send(frame_bytes);
                            }
                        } else {
                            client.pending_pipe_frames.clear();
                        }

                        // AutomergeSync frames may have been applied to client.doc
                        // during wait_for_response_with_broadcast — refresh snapshot
                        // so readers see daemon-driven mutations (outputs, execution
                        // counts, etc.) immediately.
                        publish_snapshot(&client, &snapshot_tx);
                        let _ = reply.send(result);
                    }
                    SyncCommand::ConfirmSync { reply } => {
                        let result = client.sync_to_daemon_confirmed().await;
                        let _ = reply.send(result);
                    }
                    SyncCommand::SendPresence { data, reply } => {
                        let result = connection::send_typed_frame(
                            &mut client.stream,
                            connection::NotebookFrameType::Presence,
                            &data,
                        )
                        .await
                        .map_err(|e| NotebookSyncError::SyncError(format!("send presence: {}", e)));
                        let _ = reply.send(result);
                    }
                    SyncCommand::ReceiveFrontendSyncMessage { message, reply } => {
                        let result = if pipe_channel.is_some() {
                            // Pipe mode (Tauri): queue the sync bytes to be flushed
                            // at the top of the next loop iteration, BEFORE the
                            // select! starts a new socket read. Writing directly here
                            // would corrupt framing if a daemon read was pending.
                            pending_pipe_frames.push_back(message);
                            Ok(())
                        } else {
                            // Full peer mode (runtimed-py): no separate frontend peer
                            // exists, so this command should not be called.
                            Err(NotebookSyncError::SyncError(
                                "frontend sync relay not active (full peer mode)".to_string(),
                            ))
                        };
                        if result.is_ok() {
                            publish_snapshot(&client, &snapshot_tx);
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
                        // Pipe mode (Tauri): forward all frame types except Response
                        // as raw typed frame bytes (type byte + payload) through
                        // one channel, preserving daemon-sent order. Response frames
                        // are consumed by the request/response cycle.
                        if let Some(ref pipe) = pipe_channel {
                            match frame.frame_type {
                                NotebookFrameType::Response => {
                                    // Fall through to process_incoming_frame —
                                    // needed for send_request/wait_for_response
                                }
                                NotebookFrameType::Request => {
                                    warn!(
                                        "[notebook-sync-task] Unexpected Request frame from daemon"
                                    );
                                    continue;
                                }
                                _ => {
                                    // AutomergeSync, Broadcast, Presence — pipe raw
                                    let mut frame_bytes = vec![frame.frame_type as u8];
                                    frame_bytes.extend_from_slice(&frame.payload);
                                    let _ = pipe.frame_tx.send(frame_bytes);
                                    continue;
                                }
                            }
                        }
                        match client.process_incoming_frame(frame).await {
                            Ok(Some(ReceivedFrame::Changes(cells))) => {
                                publish_snapshot(&client, &snapshot_tx);
                                // Full peer mode: metadata diffing and SyncUpdate
                                let current_metadata = client.get_metadata(NOTEBOOK_METADATA_KEY);
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
                        // end else (non-pipe mode)
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
        // Manually build a notebook structure in an AutoCommit (Map-based schema v2)
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "schema_version", 2u64).unwrap();
        doc.put(automerge::ROOT, "notebook_id", "test").unwrap();
        let cells_id = doc
            .put_object(automerge::ROOT, "cells", ObjType::Map)
            .unwrap();

        // Add a code cell (keyed by cell ID)
        let cell = doc.put_object(&cells_id, "c1", ObjType::Map).unwrap();
        doc.put(&cell, "id", "c1").unwrap();
        doc.put(&cell, "cell_type", "code").unwrap();
        doc.put(&cell, "position", "80").unwrap();
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
        assert_eq!(cells[0].position, "80");
        assert_eq!(cells[0].source, "x = 1");
        assert_eq!(cells[0].execution_count, "1");
        assert_eq!(cells[0].outputs.len(), 1);
    }
}
