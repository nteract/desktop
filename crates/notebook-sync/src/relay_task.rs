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
//! 2. **Incoming daemon frames** — pipe to frontend via `frame_tx`
//!
//! No `changed_rx` (no local doc to sync). No snapshot publishing.
//! No `FrameForwarder` conditionals — the relay always pipes.

use std::time::Duration;

use log::{debug, info, warn};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::mpsc;

use notebook_protocol::connection::{self, NotebookFrameType};
use notebook_protocol::protocol::{NotebookBroadcast, NotebookResponse};

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

/// Run the relay task.
///
/// This is spawned as a background tokio task. It runs until the socket
/// closes or all handles are dropped (command channel closes).
///
/// The relay has no local document, no mutex, no snapshot publishing.
/// It is a transparent byte pipe with request/response support.
pub async fn run<R, W>(mut config: RelayTaskConfig, reader: R, writer: W)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    let notebook_id = &config.notebook_id;

    info!("[relay] Started for {}", notebook_id);

    loop {
        enum SelectResult {
            Command(Option<RelayCommand>),
            Frame(std::io::Result<Option<connection::TypedNotebookFrame>>),
        }

        let select_result = tokio::select! {
            biased;
            // Prioritize incoming daemon frames (sync, broadcast, presence)
            // over outgoing commands.  Keeping sync frames flowing prevents
            // head divergence; commands can wait a tick.
            frame = connection::recv_typed_frame(&mut reader) => SelectResult::Frame(frame),
            cmd = config.cmd_rx.recv() => SelectResult::Command(cmd),
        };

        match select_result {
            // ─── Command from handle ───────────────────────────────────
            SelectResult::Command(None) => {
                // All handles dropped — shut down
                info!(
                    "[relay] All handles dropped for {}, shutting down",
                    notebook_id
                );
                break;
            }

            SelectResult::Command(Some(cmd)) => match cmd {
                RelayCommand::SendRequest {
                    request,
                    reply,
                    broadcast_tx,
                } => {
                    let result = send_request_impl(
                        &mut reader,
                        &mut writer,
                        &config.frame_tx,
                        broadcast_tx.as_ref(),
                        &request,
                        notebook_id,
                    )
                    .await;
                    let _ = reply.send(result);
                }

                RelayCommand::ForwardFrame {
                    frame_type,
                    payload,
                    reply,
                } => {
                    let ft = NotebookFrameType::try_from(frame_type);
                    let result = match ft {
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

            // ─── Incoming frame from daemon → pipe to frontend ─────────
            SelectResult::Frame(Ok(Some(frame))) => {
                pipe_frame(&config.frame_tx, &frame);
            }

            SelectResult::Frame(Ok(None)) => {
                info!("[relay] Daemon closed connection for {}", notebook_id);
                break;
            }

            SelectResult::Frame(Err(e)) => {
                warn!("[relay] Read error for {}: {}", notebook_id, e);
                break;
            }
        }
    }

    info!("[relay] Stopped for {}", notebook_id);
}

/// Pipe a daemon frame to the frontend.
///
/// Only sync, broadcast, and presence frames are forwarded. Response and
/// request frames are internal to the protocol and must not reach the frontend.
fn pipe_frame(frame_tx: &mpsc::UnboundedSender<Vec<u8>>, frame: &connection::TypedNotebookFrame) {
    match frame.frame_type {
        NotebookFrameType::AutomergeSync
        | NotebookFrameType::Broadcast
        | NotebookFrameType::Presence
        | NotebookFrameType::RuntimeStateSync
        | NotebookFrameType::PoolStateSync => {
            let mut bytes = vec![frame.frame_type as u8];
            bytes.extend_from_slice(&frame.payload);
            let _ = frame_tx.send(bytes);
        }
        _ => {
            debug!(
                "[relay] Not piping {:?} frame ({} bytes)",
                frame.frame_type,
                frame.payload.len()
            );
        }
    }
}

/// Send a request to the daemon and wait for the response.
///
/// While waiting, non-response frames are piped to the frontend.
/// This ensures the WASM frontend doesn't miss sync/broadcast/presence
/// frames that arrive during a request/response cycle.
async fn send_request_impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut R,
    writer: &mut W,
    frame_tx: &mpsc::UnboundedSender<Vec<u8>>,
    req_broadcast_tx: Option<&tokio::sync::broadcast::Sender<NotebookBroadcast>>,
    request: &notebook_protocol::protocol::NotebookRequest,
    notebook_id: &str,
) -> Result<NotebookResponse, SyncError> {
    // Wrap the request in a correlation envelope. The daemon echoes the
    // id on the response envelope. The relay currently serializes requests
    // (one in flight per room), so the correlation id is redundant here —
    // but it's part of the wire protocol at `PROTOCOL_VERSION = 3`, and a
    // later PR (direct JS sendRequest) will use the id to dispatch
    // concurrent responses via a pending map.
    let expected_id = uuid::Uuid::new_v4().to_string();
    let envelope = notebook_protocol::protocol::NotebookRequestEnvelope {
        id: Some(expected_id.clone()),
        request: request.clone(),
    };
    let payload =
        serde_json::to_vec(&envelope).map_err(|e| SyncError::Serialization(e.to_string()))?;
    connection::send_typed_frame(writer, NotebookFrameType::Request, &payload)
        .await
        .map_err(SyncError::Io)?;

    // Determine timeout based on request type.
    // Completions use 7s — the daemon's kernel-level timeout is 5s, so the
    // daemon always responds within ~5s. The extra 2s margin ensures the
    // relay never independently times out during normal operation (only on
    // daemon crash/hang). A long wait blocks the entire relay.
    let timeout_secs = match request {
        notebook_protocol::protocol::NotebookRequest::LaunchKernel { .. } => 300,
        notebook_protocol::protocol::NotebookRequest::SyncEnvironment { .. } => 300,
        notebook_protocol::protocol::NotebookRequest::Complete { .. } => 7,
        _ => 30,
    };

    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        wait_for_response(
            reader,
            frame_tx,
            req_broadcast_tx,
            notebook_id,
            &expected_id,
        ),
    )
    .await;

    match result {
        Ok(inner) => inner,
        Err(_) => {
            warn!(
                "[relay] Request timed out after {}s: {:?}",
                timeout_secs, request
            );
            // NOTE: We intentionally do NOT drain stale response frames here.
            // `recv_typed_frame` uses `read_exact` internally, which is not
            // cancellation-safe — wrapping it in `tokio::time::timeout` could
            // cancel mid-frame and corrupt the stream. The relay timeout (7s)
            // exceeds the daemon's kernel-level timeout (5s), so the daemon
            // always responds before the relay gives up. Any stale Response
            // frames that reach the select loop are harmlessly discarded by
            // `pipe_frame`.
            Err(SyncError::Timeout)
        }
    }
}

/// Wait for a Response frame, piping all other frames to the frontend.
async fn wait_for_response<R: AsyncRead + Unpin>(
    reader: &mut R,
    frame_tx: &mpsc::UnboundedSender<Vec<u8>>,
    req_broadcast_tx: Option<&tokio::sync::broadcast::Sender<NotebookBroadcast>>,
    _notebook_id: &str,
    expected_id: &str,
) -> Result<NotebookResponse, SyncError> {
    loop {
        let frame = connection::recv_typed_frame(reader)
            .await
            .map_err(SyncError::Io)?
            .ok_or_else(|| SyncError::Protocol("Connection closed waiting for response".into()))?;

        match frame.frame_type {
            NotebookFrameType::Response => {
                let envelope: notebook_protocol::protocol::NotebookResponseEnvelope =
                    serde_json::from_slice(&frame.payload)
                        .map_err(|e| SyncError::Serialization(e.to_string()))?;
                // Relay serializes requests per room, so the id we're waiting
                // for should always match. A mismatch means a stale response
                // leaked past a timeout or a protocol bug — warn and keep
                // reading so we eventually land on ours.
                if let Some(id) = envelope.id.as_deref() {
                    if id != expected_id {
                        warn!(
                            "[relay] Ignoring response with id {:?}, waiting for {:?}",
                            id, expected_id
                        );
                        continue;
                    }
                }
                return Ok(envelope.response);
            }

            NotebookFrameType::Broadcast => {
                // Deliver to request-specific broadcast channel if provided
                // (for real-time progress during long requests like LaunchKernel)
                if let Some(tx) = req_broadcast_tx {
                    if let Ok(bc) = serde_json::from_slice::<NotebookBroadcast>(&frame.payload) {
                        let _ = tx.send(bc);
                    }
                }
                // Also pipe to frontend
                pipe_frame(frame_tx, &frame);
            }

            _ => {
                // Sync, presence, etc. — pipe to frontend
                pipe_frame(frame_tx, &frame);
            }
        }
    }
}
