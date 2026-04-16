//! `RelayHandle` — transparent byte pipe between a frontend (WASM) and the daemon.
//!
//! Unlike [`DocHandle`](crate::DocHandle), the relay does not maintain a local
//! Automerge document replica. It does not participate in the sync protocol —
//! the frontend owns the sync state and the relay just forwards bytes.
//!
//! This eliminates the "dual sync" problem where both the relay and the WASM
//! generate sync messages on the same daemon connection, and removes the 100ms
//! convergence floor from `do_initial_sync` (which the relay never calls).
//!
//! ## API surface
//!
//! The relay handle exposes only what Tauri needs:
//!
//! - `send_request` — daemon protocol (launch kernel, save, etc.)
//! - `notebook_id` — read the notebook identifier
//! - `forward_frame` — pipe a typed frame from the frontend to the daemon
//!
//! No `with_doc`. No `get_cells`. No `snapshot`. No `subscribe`.

use tokio::sync::{broadcast, mpsc, oneshot};

use notebook_protocol::protocol::{NotebookBroadcast, NotebookRequest, NotebookResponse};

use crate::error::SyncError;

/// Commands for the relay task — only socket I/O operations.
///
/// This is intentionally minimal. The relay has no local document,
/// so there are no mutation or sync confirmation commands.
pub enum RelayCommand {
    /// Send a request to the daemon and wait for a response.
    SendRequest {
        request: NotebookRequest,
        reply: oneshot::Sender<Result<NotebookResponse, SyncError>>,
        /// Optional broadcast sender for delivering broadcasts during long-running
        /// requests (e.g., LaunchKernel with environment progress updates).
        broadcast_tx: Option<broadcast::Sender<NotebookBroadcast>>,
    },

    /// Forward a typed frame from the frontend to the daemon.
    ///
    /// The relay validates the frame type byte but does not decode
    /// the payload — it writes the type and payload directly to the
    /// daemon socket.
    ForwardFrame {
        frame_type: u8,
        payload: Vec<u8>,
        reply: oneshot::Sender<Result<(), SyncError>>,
    },
}

/// A handle to a relay connection — forwards frames between a frontend
/// (WASM) and the daemon without maintaining a local document replica.
///
/// Unlike `DocHandle`, this does not participate in the Automerge sync
/// protocol. The frontend owns the sync state; the relay just pipes bytes.
///
/// `RelayHandle` is `Clone` — multiple callers can hold handles to the
/// same relay connection.
///
/// # Example
///
/// ```ignore
/// // Forward a WASM sync frame to the daemon
/// handle.forward_frame(frame_types::AUTOMERGE_SYNC, payload).await?;
///
/// // Send a daemon protocol request
/// let response = handle.send_request(NotebookRequest::SaveNotebook { ... }).await?;
///
/// // Read the notebook ID (synchronous)
/// let id = handle.notebook_id();
/// ```
#[derive(Clone)]
pub struct RelayHandle {
    cmd_tx: mpsc::Sender<RelayCommand>,
    notebook_id: std::sync::Arc<std::sync::RwLock<String>>,
}

impl std::fmt::Debug for RelayHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayHandle")
            .field(
                "notebook_id",
                &*self.notebook_id.read().unwrap_or_else(|e| e.into_inner()),
            )
            .finish()
    }
}

impl RelayHandle {
    /// Create a new `RelayHandle` from a command channel and notebook ID.
    ///
    /// Called by the relay connect functions, not by end users.
    pub(crate) fn new(cmd_tx: mpsc::Sender<RelayCommand>, notebook_id: String) -> Self {
        Self {
            cmd_tx,
            notebook_id: std::sync::Arc::new(std::sync::RwLock::new(notebook_id)),
        }
    }

    /// Get the notebook ID this handle is connected to.
    pub fn notebook_id(&self) -> String {
        self.notebook_id
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Update the notebook ID stored in this relay handle.
    pub fn set_notebook_id(&self, new_id: String) {
        *self.notebook_id.write().unwrap_or_else(|e| e.into_inner()) = new_id;
    }

    /// Send a request to the daemon and wait for a response.
    ///
    /// This is async because it involves socket I/O. The request is sent
    /// to the daemon via the relay task, which handles the wire protocol.
    pub async fn send_request(
        &self,
        request: NotebookRequest,
    ) -> Result<NotebookResponse, SyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(RelayCommand::SendRequest {
                request,
                reply: reply_tx,
                broadcast_tx: None,
            })
            .await
            .map_err(|_| SyncError::Disconnected)?;
        reply_rx.await.map_err(|_| SyncError::Disconnected)?
    }

    /// Send a request with a broadcast channel for real-time progress updates.
    ///
    /// Used for long-running requests like `LaunchKernel` where the daemon
    /// sends progress broadcasts (env creation, package installs) while
    /// the request is in flight.
    pub async fn send_request_with_broadcast(
        &self,
        request: NotebookRequest,
        broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    ) -> Result<NotebookResponse, SyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(RelayCommand::SendRequest {
                request,
                reply: reply_tx,
                broadcast_tx: Some(broadcast_tx),
            })
            .await
            .map_err(|_| SyncError::Disconnected)?;
        reply_rx.await.map_err(|_| SyncError::Disconnected)?
    }

    /// Forward a typed frame from the frontend to the daemon.
    ///
    /// The relay writes the frame directly to the daemon socket without
    /// decoding or processing it. Used for Automerge sync messages and
    /// presence frames originating from the WASM frontend.
    pub async fn forward_frame(&self, frame_type: u8, payload: Vec<u8>) -> Result<(), SyncError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.cmd_tx
            .send(RelayCommand::ForwardFrame {
                frame_type,
                payload,
                reply: reply_tx,
            })
            .await
            .map_err(|_| SyncError::Disconnected)?;
        reply_rx.await.map_err(|_| SyncError::Disconnected)?
    }
}
