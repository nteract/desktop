//! Sync task — background network I/O loop.
//!
//! The sync task owns the socket connection to the daemon and handles:
//!
//! 1. **Local changes** — when `DocHandle::with_doc` mutates the document,
//!    it sends a notification via `changed_rx`. The sync task generates an
//!    Automerge sync message and sends it to the daemon.
//!
//! 2. **Remote changes** — when the daemon sends sync messages (from other
//!    peers), the sync task applies them to the shared document and publishes
//!    a new snapshot.
//!
//! 3. **Protocol operations** — daemon request/response (`SendRequest`),
//!    sync confirmation (`ConfirmSync`), and presence frames still go through
//!    a command channel since they need socket I/O.
//!
//! Document mutations do NOT go through this task. Callers mutate directly
//! via `DocHandle::with_doc`. This task is purely for network synchronization.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use automerge::sync;
use log::{debug, info, warn};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, oneshot};

use notebook_protocol::connection::{self, NotebookFrameType};
use notebook_protocol::protocol::{NotebookBroadcast, NotebookRequest, NotebookResponse};

use crate::error::SyncError;
use crate::shared::SharedDocState;
use crate::snapshot::NotebookSnapshot;

/// Commands that require socket I/O (not document mutations).
///
/// This is intentionally minimal — only operations that need the network
/// connection go through this channel. Document mutations happen directly
/// on the `DocHandle` via `with_doc`.
pub enum SyncCommand {
    /// Send a request to the daemon and wait for a response.
    SendRequest {
        request: NotebookRequest,
        reply: oneshot::Sender<Result<NotebookResponse, SyncError>>,
        /// Optional broadcast sender for delivering broadcasts during long-running
        /// requests (e.g., LaunchKernel with environment progress updates).
        broadcast_tx: Option<broadcast::Sender<NotebookBroadcast>>,
    },

    /// Confirm that the daemon has merged all our local changes.
    ///
    /// Performs up to 5 sync round-trips, checking that the daemon's
    /// shared_heads include our local heads.
    ConfirmSync {
        reply: oneshot::Sender<Result<(), SyncError>>,
    },

    /// Send a raw presence frame to the daemon.
    SendPresence {
        data: Vec<u8>,
        reply: oneshot::Sender<Result<(), SyncError>>,
    },
}

/// Configuration for the sync task.
pub struct SyncTaskConfig {
    /// Shared document state (same Arc as DocHandle).
    pub doc: Arc<Mutex<SharedDocState>>,

    /// Receives notifications when the document was mutated locally.
    pub changed_rx: mpsc::UnboundedReceiver<()>,

    /// Receives protocol commands (request/response, confirm_sync, presence).
    pub cmd_rx: mpsc::Receiver<SyncCommand>,

    /// Watch sender for publishing snapshots after applying remote changes.
    pub snapshot_tx: Arc<tokio::sync::watch::Sender<NotebookSnapshot>>,

    /// Broadcast sender for kernel/execution events from the daemon.
    pub broadcast_tx: broadcast::Sender<NotebookBroadcast>,
}

/// Run the sync task.
///
/// This is spawned as a background tokio task. It runs until the socket
/// closes or all handles are dropped (channels close).
///
/// The document mutex is held briefly for sync message generation/application.
/// It is NEVER held across `.await` points (socket I/O).
pub async fn run<R, W>(mut config: SyncTaskConfig, reader: R, writer: W)
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    let notebook_id = {
        let state = config.doc.lock().unwrap_or_else(|e| e.into_inner());
        state.notebook_id().to_string()
    };

    let mut loop_count: u64 = 0;

    // Track last metadata for change detection (used for SyncUpdate-like behavior)
    let mut _last_metadata: Option<notebook_doc::metadata::NotebookMetadataSnapshot> = {
        let state = config.doc.lock().unwrap_or_else(|e| e.into_inner());
        notebook_doc::get_metadata_snapshot_from_doc(&state.doc)
    };

    loop {
        loop_count += 1;

        // Use select! with biased mode so we prioritize local changes and commands
        // over incoming frames (prevents starvation when the daemon is chatty).
        enum SelectResult {
            Changed,
            Command(Option<SyncCommand>),
            Frame(std::io::Result<Option<connection::TypedNotebookFrame>>),
        }

        let select_result = tokio::select! {
            biased;

            // Local document was mutated by a handle
            result = config.changed_rx.recv() => {
                if result.is_none() {
                    // All handles dropped — shut down
                    info!("[notebook-sync] All handles dropped for {}, shutting down", notebook_id);
                    break;
                }
                SelectResult::Changed
            }

            // Protocol command (request/response, confirm_sync, etc.)
            cmd = config.cmd_rx.recv() => SelectResult::Command(cmd),

            // Incoming frame from daemon
            frame = connection::recv_typed_frame(&mut reader) => SelectResult::Frame(frame),
        };

        match select_result {
            // ─── Local changes: generate sync message and send to daemon ───
            SelectResult::Changed => {
                // Drain any additional notifications (coalesce multiple mutations)
                while config.changed_rx.try_recv().is_ok() {}

                // Generate and send sync message
                let msg_bytes = {
                    let mut state = config.doc.lock().unwrap_or_else(|e| e.into_inner());
                    state.generate_sync_message().map(|msg| msg.encode())
                };

                if let Some(bytes) = msg_bytes {
                    if let Err(e) = connection::send_typed_frame(
                        &mut writer,
                        NotebookFrameType::AutomergeSync,
                        &bytes,
                    )
                    .await
                    {
                        warn!(
                            "[notebook-sync] Failed to send sync message for {}: {}",
                            notebook_id, e
                        );
                        break;
                    }
                }

                // Wait briefly for an ack from the daemon (like sync_to_daemon)
                match tokio::time::timeout(
                    Duration::from_millis(500),
                    connection::recv_typed_frame(&mut reader),
                )
                .await
                {
                    Ok(Ok(Some(frame))) => {
                        handle_incoming_frame(
                            &frame,
                            &config.doc,
                            &mut writer,
                            &config.snapshot_tx,
                            &config.broadcast_tx,
                            &notebook_id,
                        )
                        .await;
                    }
                    Ok(Ok(None)) => {
                        info!("[notebook-sync] Connection closed for {}", notebook_id);
                        break;
                    }
                    Ok(Err(e)) => {
                        warn!("[notebook-sync] Socket error for {}: {}", notebook_id, e);
                        break;
                    }
                    Err(_) => {
                        // Timeout — daemon hasn't responded yet, that's fine
                    }
                }
            }

            // ─── Protocol commands ─────────────────────────────────────────
            SelectResult::Command(cmd) => {
                let Some(cmd) = cmd else {
                    // Command channel closed — shut down
                    info!(
                        "[notebook-sync] Command channel closed for {}, shutting down",
                        notebook_id
                    );
                    break;
                };

                match cmd {
                    SyncCommand::SendRequest {
                        request,
                        reply,
                        broadcast_tx: req_broadcast_tx,
                    } => {
                        let result = send_request_impl(
                            &config.doc,
                            &mut reader,
                            &mut writer,
                            &config.snapshot_tx,
                            &config.broadcast_tx,
                            req_broadcast_tx.as_ref(),
                            &request,
                            &notebook_id,
                        )
                        .await;
                        let _ = reply.send(result);
                    }

                    SyncCommand::ConfirmSync { reply } => {
                        let result = confirm_sync_impl(
                            &config.doc,
                            &mut reader,
                            &mut writer,
                            &config.snapshot_tx,
                            &config.broadcast_tx,
                            &notebook_id,
                        )
                        .await;
                        let _ = reply.send(result);
                    }

                    SyncCommand::SendPresence { data, reply } => {
                        let result = connection::send_typed_frame(
                            &mut writer,
                            NotebookFrameType::Presence,
                            &data,
                        )
                        .await
                        .map_err(SyncError::Io);
                        let _ = reply.send(result);
                    }
                }
            }

            // ─── Incoming frame from daemon ────────────────────────────────
            SelectResult::Frame(frame_result) => match frame_result {
                Ok(Some(frame)) => {
                    handle_incoming_frame(
                        &frame,
                        &config.doc,
                        &mut writer,
                        &config.snapshot_tx,
                        &config.broadcast_tx,
                        &notebook_id,
                    )
                    .await;
                }
                Ok(None) => {
                    info!(
                        "[notebook-sync] Disconnected from daemon for {}, loop_count={}",
                        notebook_id, loop_count
                    );
                    break;
                }
                Err(e) => {
                    warn!(
                        "[notebook-sync] Socket error for {}: {}, loop_count={}",
                        notebook_id, e, loop_count
                    );
                    break;
                }
            },
        }
    }

    info!(
        "[notebook-sync] Stopped for {} after {} loop iterations",
        notebook_id, loop_count
    );
}

// =========================================================================
// Internal helpers
// =========================================================================

/// Handle an incoming typed frame from the daemon.
async fn handle_incoming_frame<W: AsyncWrite + Unpin>(
    frame: &connection::TypedNotebookFrame,
    doc: &Arc<Mutex<SharedDocState>>,
    writer: &mut W,
    snapshot_tx: &Arc<tokio::sync::watch::Sender<NotebookSnapshot>>,
    broadcast_tx: &broadcast::Sender<NotebookBroadcast>,
    notebook_id: &str,
) {
    match frame.frame_type {
        NotebookFrameType::AutomergeSync => {
            let msg = match sync::Message::decode(&frame.payload) {
                Ok(msg) => msg,
                Err(e) => {
                    warn!(
                        "[notebook-sync] Failed to decode sync message for {}: {}",
                        notebook_id, e
                    );
                    return;
                }
            };

            // Apply and generate ack — lock held only for Automerge operations
            let ack_bytes = {
                let mut state = doc.lock().unwrap_or_else(|e| e.into_inner());
                if let Err(e) = state.receive_sync_message(msg) {
                    warn!(
                        "[notebook-sync] Failed to apply sync message for {}: {}",
                        notebook_id, e
                    );
                    return;
                }
                state.generate_sync_message().map(|msg| msg.encode())
            };

            // Publish snapshot immediately (before sending ack — readers see changes fast)
            publish_snapshot(doc, snapshot_tx);

            // Send ack if needed (outside the lock — never hold across I/O)
            if let Some(bytes) = ack_bytes {
                if let Err(e) =
                    connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &bytes)
                        .await
                {
                    warn!(
                        "[notebook-sync] Failed to send sync ack for {}: {}",
                        notebook_id, e
                    );
                }
            }
        }

        NotebookFrameType::Broadcast => {
            match serde_json::from_slice::<NotebookBroadcast>(&frame.payload) {
                Ok(bc) => {
                    let _ = broadcast_tx.send(bc);
                }
                Err(e) => {
                    warn!(
                        "[notebook-sync] Failed to parse broadcast for {}: {}",
                        notebook_id, e
                    );
                }
            }
        }

        NotebookFrameType::Presence => {
            use notebook_doc::presence::{decode_message, validate_frame_size, PresenceMessage};

            if let Err(e) = validate_frame_size(&frame.payload) {
                debug!(
                    "[notebook-sync] Dropping oversized presence frame for {}: {}",
                    notebook_id, e
                );
                return;
            }

            match decode_message(&frame.payload) {
                Ok(msg) => {
                    let now_ms = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let mut state = doc.lock().unwrap_or_else(|e| e.into_inner());
                    match msg {
                        PresenceMessage::Update {
                            peer_id,
                            peer_label,
                            data,
                        } => {
                            state.presence.update_peer(
                                &peer_id,
                                peer_label.as_deref().unwrap_or("peer"),
                                data,
                                now_ms,
                            );
                        }
                        PresenceMessage::Snapshot { peers, .. } => {
                            state.presence.apply_snapshot(&peers, now_ms);
                        }
                        PresenceMessage::Left { peer_id } => {
                            state.presence.remove_peer(&peer_id);
                        }
                        PresenceMessage::Heartbeat { peer_id } => {
                            state.presence.mark_seen(&peer_id, now_ms);
                        }
                        PresenceMessage::ClearChannel { peer_id, channel } => {
                            state.presence.clear_channel(&peer_id, channel);
                        }
                    }
                }
                Err(e) => {
                    debug!(
                        "[notebook-sync] Failed to decode presence for {}: {}",
                        notebook_id, e
                    );
                }
            }
        }

        NotebookFrameType::Response => {
            // Unexpected outside of a request/response cycle
            warn!(
                "[notebook-sync] Unexpected Response frame for {} in background loop",
                notebook_id
            );
        }

        NotebookFrameType::Request => {
            warn!(
                "[notebook-sync] Unexpected Request frame from daemon for {}",
                notebook_id
            );
        }

        NotebookFrameType::RuntimeStateSync => {
            let msg = match sync::Message::decode(&frame.payload) {
                Ok(msg) => msg,
                Err(e) => {
                    warn!(
                        "[notebook-sync] Failed to decode RuntimeStateSync for {}: {}",
                        notebook_id, e
                    );
                    return;
                }
            };

            // Apply and generate reply — same pattern as AutomergeSync
            let reply_bytes = {
                let mut state = doc.lock().unwrap_or_else(|e| e.into_inner());
                if let Err(e) = state.receive_state_sync_message(msg) {
                    warn!(
                        "[notebook-sync] Failed to apply RuntimeStateSync for {}: {}",
                        notebook_id, e
                    );
                    return;
                }
                state.generate_state_sync_message().map(|msg| msg.encode())
            };

            if let Some(bytes) = reply_bytes {
                if let Err(e) = connection::send_typed_frame(
                    writer,
                    NotebookFrameType::RuntimeStateSync,
                    &bytes,
                )
                .await
                {
                    warn!(
                        "[notebook-sync] Failed to send RuntimeStateSync reply for {}: {}",
                        notebook_id, e
                    );
                }
            }
        }
    }
}

/// Send a request to the daemon and wait for a response.
///
/// While waiting, also processes AutomergeSync and Broadcast frames that arrive
/// interleaved with the response.
#[allow(clippy::too_many_arguments)]
async fn send_request_impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    doc: &Arc<Mutex<SharedDocState>>,
    reader: &mut R,
    writer: &mut W,
    snapshot_tx: &Arc<tokio::sync::watch::Sender<NotebookSnapshot>>,
    broadcast_tx: &broadcast::Sender<NotebookBroadcast>,
    req_broadcast_tx: Option<&broadcast::Sender<NotebookBroadcast>>,
    request: &NotebookRequest,
    notebook_id: &str,
) -> Result<NotebookResponse, SyncError> {
    // Serialize and send the request
    let payload =
        serde_json::to_vec(request).map_err(|e| SyncError::Serialization(e.to_string()))?;

    connection::send_typed_frame(writer, NotebookFrameType::Request, &payload)
        .await
        .map_err(SyncError::Io)?;

    // Determine timeout based on request type
    let timeout_secs = match request {
        NotebookRequest::LaunchKernel { .. } => 300, // 5 minutes for env creation
        _ => 30,
    };

    // Wait for a Response frame, processing other frames that arrive meanwhile
    let result = tokio::time::timeout(
        Duration::from_secs(timeout_secs),
        wait_for_response(
            doc,
            reader,
            writer,
            snapshot_tx,
            broadcast_tx,
            req_broadcast_tx,
            notebook_id,
        ),
    )
    .await;

    match result {
        Ok(Ok(response)) => Ok(response),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(SyncError::Timeout),
    }
}

/// Wait for a Response frame from the daemon, processing other frames.
async fn wait_for_response<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    doc: &Arc<Mutex<SharedDocState>>,
    reader: &mut R,
    writer: &mut W,
    snapshot_tx: &Arc<tokio::sync::watch::Sender<NotebookSnapshot>>,
    broadcast_tx: &broadcast::Sender<NotebookBroadcast>,
    req_broadcast_tx: Option<&broadcast::Sender<NotebookBroadcast>>,
    notebook_id: &str,
) -> Result<NotebookResponse, SyncError> {
    loop {
        let frame = connection::recv_typed_frame(reader)
            .await
            .map_err(SyncError::Io)?
            .ok_or_else(|| SyncError::Protocol("Connection closed waiting for response".into()))?;

        match frame.frame_type {
            NotebookFrameType::Response => {
                let response: NotebookResponse = serde_json::from_slice(&frame.payload)
                    .map_err(|e| SyncError::Serialization(e.to_string()))?;
                return Ok(response);
            }

            NotebookFrameType::AutomergeSync => {
                // Apply sync message while waiting for response
                let msg = sync::Message::decode(&frame.payload)
                    .map_err(|e| SyncError::Protocol(format!("Decode sync: {}", e)))?;

                let ack_bytes = {
                    let mut state = doc.lock().unwrap_or_else(|e| e.into_inner());
                    state
                        .receive_sync_message(msg)
                        .map_err(|e| SyncError::Protocol(format!("Apply sync: {}", e)))?;
                    state.generate_sync_message().map(|m| m.encode())
                };

                publish_snapshot(doc, snapshot_tx);

                if let Some(bytes) = ack_bytes {
                    connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &bytes)
                        .await
                        .map_err(SyncError::Io)?;
                }
            }

            NotebookFrameType::Broadcast => {
                if let Ok(bc) = serde_json::from_slice::<NotebookBroadcast>(&frame.payload) {
                    // Send to request-specific broadcast channel if provided (for real-time
                    // progress during long requests like LaunchKernel)
                    if let Some(tx) = req_broadcast_tx {
                        let _ = tx.send(bc.clone());
                    }
                    // Also send to the main broadcast channel
                    let _ = broadcast_tx.send(bc);
                }
            }

            _ => {
                // Ignore other frame types while waiting for response
                debug!(
                    "[notebook-sync] Ignoring {:?} frame while waiting for response ({})",
                    frame.frame_type, notebook_id
                );
            }
        }
    }
}

/// Confirm that the daemon has merged all our local changes.
///
/// Performs sync rounds until our local heads are in the peer's shared_heads,
/// or until we've done 5 rounds (best-effort).
async fn confirm_sync_impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    doc: &Arc<Mutex<SharedDocState>>,
    reader: &mut R,
    writer: &mut W,
    snapshot_tx: &Arc<tokio::sync::watch::Sender<NotebookSnapshot>>,
    broadcast_tx: &broadcast::Sender<NotebookBroadcast>,
    notebook_id: &str,
) -> Result<(), SyncError> {
    for round in 0..5 {
        // Generate and send sync message
        let (msg_bytes, our_heads, shared_heads) = {
            let mut state = doc.lock().unwrap_or_else(|e| e.into_inner());
            let our_heads = state.doc.get_heads();
            let shared = state.peer_state.shared_heads.clone();
            let msg = state.generate_sync_message().map(|m| m.encode());
            (msg, our_heads, shared)
        };

        // Check if already confirmed
        if our_heads.is_empty() || our_heads.iter().all(|h| shared_heads.contains(h)) {
            debug!(
                "[notebook-sync] Sync confirmed for {} after {} rounds",
                notebook_id, round
            );
            return Ok(());
        }

        // Send sync message if there is one
        if let Some(bytes) = msg_bytes {
            connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &bytes)
                .await
                .map_err(SyncError::Io)?;
        }

        // Wait for response
        match tokio::time::timeout(
            Duration::from_millis(2000),
            connection::recv_typed_frame(reader),
        )
        .await
        {
            Ok(Ok(Some(frame))) => {
                handle_incoming_frame(&frame, doc, writer, snapshot_tx, broadcast_tx, notebook_id)
                    .await;
            }
            Ok(Ok(None)) => {
                return Err(SyncError::Protocol(
                    "Connection closed during confirm_sync".into(),
                ));
            }
            Ok(Err(e)) => {
                return Err(SyncError::Io(e));
            }
            Err(_) => {
                // Timeout — try next round
                debug!(
                    "[notebook-sync] confirm_sync round {} timed out for {}",
                    round, notebook_id
                );
            }
        }
    }

    // Best-effort: likely confirmed even if heads don't fully match
    debug!(
        "[notebook-sync] confirm_sync: heads not fully confirmed after 5 rounds for {}",
        notebook_id
    );
    Ok(())
}

/// Publish a snapshot from the current document state.
fn publish_snapshot(
    doc: &Arc<Mutex<SharedDocState>>,
    snapshot_tx: &Arc<tokio::sync::watch::Sender<NotebookSnapshot>>,
) {
    let snapshot = {
        let state = doc.lock().unwrap_or_else(|e| e.into_inner());
        NotebookSnapshot::from_doc(&state.doc)
    };
    let _ = snapshot_tx.send(snapshot);
}
