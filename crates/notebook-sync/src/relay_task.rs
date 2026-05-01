//! Relay task — transparent byte pipe between frontend (WASM) and daemon.
//!
//! Unlike the sync task, the relay does not maintain a local Automerge document.
//! It does not participate in the sync protocol — the frontend owns the sync
//! state and the relay just forwards bytes in both directions.
//!
//! ## Select loop
//!
//! Two arms:
//! 1. **Commands** from `RelayHandle` — send requests, forward frames
//! 2. **Incoming daemon frames** — route via pending map or pipe to frontend
//!
//! ## Request/response correlation
//!
//! Requests carry a correlation id (see `NotebookRequestEnvelope`). When a
//! Rust-side caller issues `SendRequest`, the relay:
//! 1. Generates a uuid id.
//! 2. Registers a `PendingEntry { reply, broadcast_tx }` in the pending map.
//! 3. Writes the `0x01` request envelope (with id) to the daemon socket.
//!
//! When a `0x02` response arrives on the socket, the relay parses the
//! envelope and looks up the id in the pending map. If a Rust caller
//! registered, the response is delivered via their oneshot. If the id is
//! unknown, the raw frame is piped to the frontend — a frontend-originated
//! request was waiting for it. This lets Rust and JS share the socket for
//! request/response without per-type Tauri commands.
//!
//! JS-originated requests arrive via `RelayCommand::ForwardFrame` with
//! frame type `0x01`; the relay forwards them unchanged and never looks at
//! the id. The frontend maintains its own pending map and matches responses
//! via the `notebook:frame` event stream.

use std::collections::HashMap;
use std::time::Duration;

use log::{debug, info, warn};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};

use notebook_protocol::connection::{self, NotebookFrameType};
use notebook_protocol::protocol::{
    NotebookBroadcast, NotebookRequest, NotebookRequestEnvelope, NotebookResponse,
    NotebookResponseEnvelope,
};

use crate::error::SyncError;
use crate::relay::RelayCommand;

/// Configuration for the relay task.
pub struct RelayTaskConfig {
    /// Receives commands from `RelayHandle` (send_request, forward_frame).
    pub cmd_rx: mpsc::Receiver<RelayCommand>,

    /// Sends piped daemon frames to the frontend (e.g., Tauri webview).
    /// NOT optional — the relay always pipes.
    pub frame_tx: mpsc::UnboundedSender<Vec<u8>>,

    /// The notebook identifier (for logging).
    pub notebook_id: String,
}

/// A Rust-side caller awaiting a response for a specific correlation id.
struct PendingEntry {
    reply: oneshot::Sender<Result<NotebookResponse, SyncError>>,
    /// Progress broadcasts to deliver while the request is in flight
    /// (e.g., LaunchKernel env-creation progress). Optional.
    broadcast_tx: Option<tokio::sync::broadcast::Sender<NotebookBroadcast>>,
}

/// Run the relay task.
///
/// Spawned as a background tokio task. Runs until the socket closes or all
/// handles are dropped (command channel closes).
pub async fn run<R, W>(mut config: RelayTaskConfig, reader: R, writer: W)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let mut writer = tokio::io::BufWriter::new(writer);

    let notebook_id = &config.notebook_id;
    let mut pending: HashMap<String, PendingEntry> = HashMap::new();

    info!("[relay] Started for {}", notebook_id);

    // Hand the read half to a dedicated FramedReader actor so the busy
    // `select!` below stays cancel-safe. `recv_typed_frame`'s internal
    // `read_exact` calls drop bytes when the future is cancelled
    // mid-read — production logs captured this as
    // `frame too large: 1818192238 bytes` (mid-payload text from a
    // streaming kernel output reinterpreted as a length prefix).
    // BufReader stays for syscall coalescing inside the actor task.
    let buffered = tokio::io::BufReader::new(reader);
    let mut framed_reader = connection::FramedReader::spawn(buffered, 16);

    loop {
        enum SelectResult {
            Command(Option<RelayCommand>),
            Frame(Option<std::io::Result<connection::TypedNotebookFrame>>),
        }

        let select_result = tokio::select! {
            biased;
            // Prioritize incoming daemon frames (sync, broadcast, presence,
            // responses) over outgoing commands. Keeping frames flowing
            // prevents head divergence; commands can wait a tick.
            frame = framed_reader.recv() => SelectResult::Frame(frame),
            cmd = config.cmd_rx.recv() => SelectResult::Command(cmd),
        };

        match select_result {
            SelectResult::Command(None) => {
                info!(
                    "[relay] All handles dropped for {}, shutting down",
                    notebook_id
                );
                break;
            }

            SelectResult::Command(Some(cmd)) => match cmd {
                RelayCommand::SendRequest {
                    id,
                    request,
                    required_heads,
                    reply,
                    broadcast_tx,
                } => {
                    handle_send_request(
                        &mut writer,
                        &mut pending,
                        id,
                        request,
                        required_heads,
                        reply,
                        broadcast_tx,
                    )
                    .await;
                }

                RelayCommand::CancelRequest { id } => {
                    // Caller gave up (timeout). Drop the pending entry so
                    // the map doesn't accumulate abandoned senders.
                    pending.remove(&id);
                }

                RelayCommand::ForwardFrame {
                    frame_type,
                    payload,
                    reply,
                } => {
                    let ft = NotebookFrameType::try_from(frame_type);
                    let result = match ft {
                        Ok(NotebookFrameType::SessionControl) => Err(SyncError::Protocol(
                            "SessionControl is daemon-originated only".into(),
                        )),
                        Ok(ft) => connection::send_typed_frame(&mut writer, ft, &payload)
                            .await
                            .map_err(SyncError::Io),
                        Err(_) => Err(SyncError::Protocol(format!(
                            "Unknown frame type: 0x{:02x}",
                            frame_type,
                        ))),
                    };
                    let _ = reply.send(result);
                }
            },

            SelectResult::Frame(Some(Ok(frame))) => {
                route_incoming_frame(&frame, &mut pending, &config.frame_tx);
            }

            SelectResult::Frame(Some(Err(e))) => {
                warn!("[relay] Read error for {}: {}", notebook_id, e);
                break;
            }

            SelectResult::Frame(None) => {
                info!("[relay] Daemon closed connection for {}", notebook_id);
                break;
            }
        }
    }

    // Any Rust callers still waiting get a Disconnected error so they
    // don't hang on a channel that will never deliver.
    for (_, entry) in pending.drain() {
        let _ = entry.reply.send(Err(SyncError::Disconnected));
    }

    info!("[relay] Stopped for {}", notebook_id);
}

/// Register a Rust-side request in the pending map and write the envelope
/// to the daemon. If the write fails, the entry is evicted and the error
/// is delivered on the caller's oneshot.
async fn handle_send_request<W: AsyncWrite + Unpin>(
    writer: &mut W,
    pending: &mut HashMap<String, PendingEntry>,
    id: String,
    request: NotebookRequest,
    required_heads: Vec<String>,
    reply: oneshot::Sender<Result<NotebookResponse, SyncError>>,
    broadcast_tx: Option<tokio::sync::broadcast::Sender<NotebookBroadcast>>,
) {
    let envelope = NotebookRequestEnvelope {
        id: Some(id.clone()),
        required_heads,
        request,
    };

    let payload = match serde_json::to_vec(&envelope) {
        Ok(p) => p,
        Err(e) => {
            let _ = reply.send(Err(SyncError::Serialization(e.to_string())));
            return;
        }
    };

    // Register BEFORE sending so a fast daemon response can't arrive before
    // the pending entry is in place.
    pending.insert(
        id.clone(),
        PendingEntry {
            reply,
            broadcast_tx,
        },
    );

    if let Err(e) = connection::send_typed_frame(writer, NotebookFrameType::Request, &payload).await
    {
        if let Some(entry) = pending.remove(&id) {
            let _ = entry.reply.send(Err(SyncError::Io(e)));
        }
    }
}

/// Dispatch an incoming frame from the daemon.
///
/// Response frames with a known correlation id are delivered to the
/// matching Rust caller. Response frames with no id, or with an id we
/// didn't register, are piped to the frontend — a JS caller is the likely
/// owner. Broadcast frames are both delivered to any subscribed request
/// (for progress updates on long-running calls) and piped to the frontend.
/// Everything else is piped to the frontend.
fn route_incoming_frame(
    frame: &connection::TypedNotebookFrame,
    pending: &mut HashMap<String, PendingEntry>,
    frame_tx: &mpsc::UnboundedSender<Vec<u8>>,
) {
    match frame.frame_type {
        NotebookFrameType::Response => {
            match serde_json::from_slice::<NotebookResponseEnvelope>(&frame.payload) {
                Ok(envelope) => {
                    let entry = envelope.id.as_deref().and_then(|id| pending.remove(id));
                    if let Some(entry) = entry {
                        let _ = entry.reply.send(Ok(envelope.response));
                    } else {
                        // Unknown id (or missing) — must belong to a frontend
                        // request. Pipe the whole frame so the frontend's pending
                        // map can match on the envelope's id.
                        pipe_frame(frame_tx, frame);
                    }
                }
                Err(e) => {
                    warn!("[relay] Malformed response envelope: {}", e);
                    // Pipe anyway so the frontend can surface / log it.
                    pipe_frame(frame_tx, frame);
                }
            }
        }

        NotebookFrameType::Broadcast => {
            // If any in-flight request subscribed to progress broadcasts,
            // deliver there too (e.g., LaunchKernel env-creation phases).
            let broadcast: Option<NotebookBroadcast> = serde_json::from_slice(&frame.payload).ok();
            if let Some(bc) = broadcast.as_ref() {
                for entry in pending.values() {
                    if let Some(tx) = &entry.broadcast_tx {
                        let _ = tx.send(bc.clone());
                    }
                }
            }
            pipe_frame(frame_tx, frame);
        }

        _ => pipe_frame(frame_tx, frame),
    }
}

/// Pipe a daemon frame to the frontend.
///
/// Forwards sync, broadcast, presence, runtime-state, pool-state,
/// session-control, AND
/// response (`0x02`) frames. Frontend requests rely on `0x02` reaching the
/// JS side so the frontend's pending map can correlate the response.
///
/// Request frames (`0x01`) are never piped outbound to the frontend — the
/// frontend sends them; the daemon never emits them to a client.
fn pipe_frame(frame_tx: &mpsc::UnboundedSender<Vec<u8>>, frame: &connection::TypedNotebookFrame) {
    match frame.frame_type {
        NotebookFrameType::AutomergeSync
        | NotebookFrameType::Broadcast
        | NotebookFrameType::Presence
        | NotebookFrameType::RuntimeStateSync
        | NotebookFrameType::PoolStateSync
        | NotebookFrameType::SessionControl
        | NotebookFrameType::Response => {
            let mut bytes = vec![frame.frame_type as u8];
            bytes.extend_from_slice(&frame.payload);
            let _ = frame_tx.send(bytes);
        }
        NotebookFrameType::Request => {
            debug!(
                "[relay] Not piping Request frame (outbound only) — {} bytes",
                frame.payload.len()
            );
        }
    }
}

/// Per-request-type timeouts — exported so `RelayHandle::send_request` can
/// wrap its oneshot await in the same bound that the old inline read loop
/// used to enforce.
pub fn request_timeout(request: &NotebookRequest) -> Duration {
    let secs = match request {
        NotebookRequest::LaunchKernel { .. } => 300,
        NotebookRequest::SyncEnvironment { .. } => 300,
        // Completions use 7s — the daemon's kernel-level timeout is 5s,
        // so the daemon always responds within ~5s under normal operation.
        NotebookRequest::Complete { .. } => 7,
        _ => 30,
    };
    Duration::from_secs(secs)
}
