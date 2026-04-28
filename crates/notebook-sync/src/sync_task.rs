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

use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use automerge::{sync, AutoCommit, ChangeHash};
use log::{debug, info, warn};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

use notebook_protocol::connection::{self, NotebookFrameType};
use notebook_protocol::protocol::{
    NotebookBroadcast, NotebookRequest, NotebookRequestEnvelope, NotebookResponse,
    NotebookResponseEnvelope, SessionControlMessage, SessionSyncStatusWire,
};

use crate::error::SyncError;
use crate::shared::SharedDocState;
use crate::snapshot::NotebookSnapshot;
use crate::status::{ConnectionState, InitialLoadPhase, SyncStatus};

const CONFIRM_SYNC_TIMEOUT: Duration = Duration::from_secs(10);
const CONFIRM_SYNC_RETRY: Duration = Duration::from_millis(200);
const STATE_SYNC_QUIET_TIMEOUT: Duration = Duration::from_millis(200);
const STATE_SYNC_MAX_TIMEOUT: Duration = Duration::from_secs(2);
const MAINTENANCE_TICK: Duration = Duration::from_millis(50);

/// Rebuild a `SharedDocState` after an automerge panic by round-tripping
/// save→load to clear corrupted internal indices, then resetting the
/// peer sync state to force a fresh handshake with the daemon.
///
/// Includes a defensive cell-count guard: if the rebuilt doc would have
/// fewer cells than the original, the rebuild is skipped (only sync state
/// is reset). This prevents silent cell loss when `save()` on a
/// panic-corrupted doc drops ops from the serialized bytes.
fn rebuild_shared_doc_state(state: &mut SharedDocState) {
    let actor = state.doc.get_actor().clone();
    let pre_cell_count = notebook_doc::get_cells_from_doc(&state.doc).len();
    let bytes = state.doc.save();
    match AutoCommit::load(&bytes) {
        Ok(mut doc) => {
            let post_cell_count = notebook_doc::get_cells_from_doc(&doc).len();
            if post_cell_count < pre_cell_count {
                warn!(
                    "[notebook-sync] rebuild_shared_doc_state would lose cells \
                     ({} → {}), keeping original doc and resetting sync state only",
                    pre_cell_count, post_cell_count
                );
                state.peer_state = sync::State::new();
                return;
            }
            doc.set_actor(actor);
            state.doc = doc;
            state.peer_state = sync::State::new();
            info!("[notebook-sync] Rebuilt doc and reset sync state after automerge panic");
        }
        Err(e) => {
            warn!("[notebook-sync] Failed to rebuild doc after panic: {}", e);
            state.peer_state = sync::State::new();
        }
    }
}

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
    /// The caller captures the target heads before enqueueing this command.
    /// The sync task registers a passive waiter and resolves it once inbound
    /// sync frames advance the daemon's `shared_heads` to include the target.
    /// Timeout remains best-effort: an expired waiter returns `Ok(())` so
    /// callers keep the historical non-strict durability semantics.
    ConfirmSync {
        target_heads: Vec<ChangeHash>,
        reply: oneshot::Sender<Result<(), SyncError>>,
    },

    /// Flush pending RuntimeStateDoc sync frames from the daemon.
    ///
    /// Generates and sends a state sync message, then waits for a quiet
    /// window while the main frame pump keeps processing every inbound frame.
    /// Since the client is read-only for the state doc, this is a flush — not
    /// a convergence handshake.
    ConfirmStateSync {
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

    /// Watch sender for publishing connection/bootstrap status.
    pub status_tx: watch::Sender<SyncStatus>,

    /// Broadcast sender for kernel/execution events from the daemon.
    pub broadcast_tx: broadcast::Sender<NotebookBroadcast>,
}

struct PendingRequest {
    reply: oneshot::Sender<Result<NotebookResponse, SyncError>>,
    broadcast_tx: Option<broadcast::Sender<NotebookBroadcast>>,
    deadline: Instant,
}

struct ConfirmWaiter {
    target_heads: Vec<ChangeHash>,
    sent_generation: Option<u64>,
    reply: oneshot::Sender<Result<(), SyncError>>,
    deadline: Instant,
}

struct StateSyncWaiter {
    reply: oneshot::Sender<Result<(), SyncError>>,
    quiet_deadline: Instant,
    deadline: Instant,
}

struct ReactorIo {
    doc: Arc<Mutex<SharedDocState>>,
    snapshot_tx: Arc<tokio::sync::watch::Sender<NotebookSnapshot>>,
    status_tx: watch::Sender<SyncStatus>,
    broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    notebook_id: String,
}

struct ReactorState {
    loop_count: u64,
    saw_session_status: bool,
    pending_requests: HashMap<String, PendingRequest>,
    confirm_waiters: Vec<ConfirmWaiter>,
    state_sync_waiters: Vec<StateSyncWaiter>,
    next_confirm_sync_attempt: Instant,
    sync_generation: u64,
    acked_sync_generation: u64,
}

impl ReactorState {
    fn new() -> Self {
        Self {
            loop_count: 0,
            saw_session_status: false,
            pending_requests: HashMap::new(),
            confirm_waiters: Vec::new(),
            state_sync_waiters: Vec::new(),
            next_confirm_sync_attempt: Instant::now(),
            sync_generation: 0,
            acked_sync_generation: 0,
        }
    }
}

struct SyncReactor {
    io: ReactorIo,
    state: ReactorState,
}

impl SyncReactor {
    fn new(
        doc: Arc<Mutex<SharedDocState>>,
        snapshot_tx: Arc<tokio::sync::watch::Sender<NotebookSnapshot>>,
        status_tx: watch::Sender<SyncStatus>,
        broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    ) -> Self {
        let notebook_id = {
            let state = doc.lock().unwrap_or_else(|e| e.into_inner());
            state.notebook_id().to_string()
        };

        Self {
            io: ReactorIo {
                doc,
                snapshot_tx,
                status_tx,
                broadcast_tx,
                notebook_id,
            },
            state: ReactorState::new(),
        }
    }
}

/// Run the sync task.
///
/// This is spawned as a background tokio task. It runs until the socket
/// closes or all handles are dropped (channels close).
///
/// The document mutex is held briefly for sync message generation/application.
/// It is NEVER held across `.await` points (socket I/O).
pub async fn run<R, W>(config: SyncTaskConfig, reader: R, writer: W)
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    // Hand the read half to a dedicated FramedReader actor so the
    // busy `select!` stays cancel-safe. `recv_typed_frame`'s internal
    // `read_exact` drops bytes mid-read whenever its future is cancelled —
    // the exact failure mode that desyncs the wire under stream-output
    // pressure.
    let buffered = tokio::io::BufReader::new(reader);
    let mut framed_reader = connection::FramedReader::spawn(buffered, 64);
    let mut writer = tokio::io::BufWriter::new(writer);

    let SyncTaskConfig {
        doc,
        mut changed_rx,
        mut cmd_rx,
        snapshot_tx,
        status_tx,
        broadcast_tx,
    } = config;
    let mut reactor = SyncReactor::new(doc, snapshot_tx, status_tx, broadcast_tx);
    let mut maintenance = tokio::time::interval(MAINTENANCE_TICK);
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        reactor.state.loop_count += 1;

        // Keep the inbound frame pump hot. Commands must only register work
        // and write immediate outbound frames; waits resolve from the normal
        // frame path so the daemon never backs up behind a command subloop.
        enum SelectResult {
            Frame(Option<std::io::Result<connection::TypedNotebookFrame>>),
            Changed(Option<()>),
            Command(Option<SyncCommand>),
            Maintenance,
        }

        let select_result = tokio::select! {
            biased;

            // Incoming frame from daemon (cancel-safe: actor owns read_exact)
            frame = framed_reader.recv() => SelectResult::Frame(frame),

            // Local document was mutated by a handle
            result = changed_rx.recv() => SelectResult::Changed(result),

            // Protocol command (request/response, confirm_sync, etc.)
            cmd = cmd_rx.recv() => SelectResult::Command(cmd),

            _ = maintenance.tick() => SelectResult::Maintenance,
        };

        match select_result {
            // ─── Incoming frame from daemon ────────────────────────────────
            SelectResult::Frame(frame_result) => match frame_result {
                Some(Ok(frame)) => {
                    if let Err(e) = reactor.on_frame(&frame, &mut writer).await {
                        warn!(
                            "[notebook-sync] Failed to continue confirm_sync for {}: {}",
                            reactor.io.notebook_id, e
                        );
                        break;
                    }
                }
                None => {
                    info!(
                        "[notebook-sync] Disconnected from daemon for {}, loop_count={}",
                        reactor.io.notebook_id, reactor.state.loop_count
                    );
                    break;
                }
                Some(Err(e)) => {
                    warn!(
                        "[notebook-sync] Socket error for {}: {}, loop_count={}",
                        reactor.io.notebook_id, e, reactor.state.loop_count
                    );
                    break;
                }
            },

            // ─── Local changes: generate sync message and send to daemon ───
            SelectResult::Changed(Some(())) => {
                // Drain any additional notifications (coalesce multiple mutations)
                while changed_rx.try_recv().is_ok() {}

                if let Err(e) = reactor.on_changed(&mut writer).await {
                    warn!(
                        "[notebook-sync] Failed to send sync message for {}: {}",
                        reactor.io.notebook_id, e
                    );
                    break;
                }
            }
            SelectResult::Changed(None) => {
                // All handles dropped — shut down
                info!(
                    "[notebook-sync] All handles dropped for {}, shutting down",
                    reactor.io.notebook_id
                );
                break;
            }

            // ─── Protocol commands ─────────────────────────────────────────
            SelectResult::Command(cmd) => {
                let Some(cmd) = cmd else {
                    // Command channel closed — shut down
                    info!(
                        "[notebook-sync] Command channel closed for {}, shutting down",
                        reactor.io.notebook_id
                    );
                    break;
                };

                reactor.on_command(cmd, &mut writer).await;
            }

            SelectResult::Maintenance => {
                if let Err(e) = reactor.on_maintenance(&mut writer).await {
                    warn!(
                        "[notebook-sync] Failed to retry confirm_sync for {}: {}",
                        reactor.io.notebook_id, e
                    );
                    break;
                }
            }
        }
    }

    reactor.disconnect_pending();
    mark_disconnected(&reactor.io.status_tx);

    info!(
        "[notebook-sync] Stopped for {} after {} loop iterations",
        reactor.io.notebook_id, reactor.state.loop_count
    );
}

// =========================================================================
// Internal helpers
// =========================================================================

impl SyncReactor {
    async fn on_frame<W: AsyncWrite + Unpin>(
        &mut self,
        frame: &connection::TypedNotebookFrame,
        writer: &mut W,
    ) -> Result<(), SyncError> {
        self.note_frame_activity();
        self.handle_task_frame(frame, writer).await;
        if frame.frame_type == NotebookFrameType::AutomergeSync {
            self.state.acked_sync_generation = self.state.sync_generation;
        }
        self.resolve_confirm_waiters();
        self.drive_confirm_sync_round(writer).await?;
        self.resolve_state_sync_waiters();
        Ok(())
    }

    async fn on_changed<W: AsyncWrite + Unpin>(&mut self, writer: &mut W) -> Result<(), SyncError> {
        if let Some(generation) = self.send_doc_sync_round(writer).await? {
            mark_unsent_confirm_waiters(&mut self.state.confirm_waiters, generation);
        }
        self.resolve_confirm_waiters();
        Ok(())
    }

    async fn on_command<W: AsyncWrite + Unpin>(&mut self, cmd: SyncCommand, writer: &mut W) {
        match cmd {
            SyncCommand::SendRequest {
                request,
                reply,
                broadcast_tx,
            } => {
                self.register_request(writer, request, reply, broadcast_tx)
                    .await;
            }

            SyncCommand::ConfirmSync {
                target_heads,
                reply,
            } => {
                if self.target_heads_confirmed(&target_heads) {
                    let _ = reply.send(Ok(()));
                } else {
                    let sent_generation = match self.send_doc_sync_round(writer).await {
                        Ok(generation) => generation,
                        Err(e) => {
                            let _ = reply.send(Err(e));
                            return;
                        }
                    };
                    self.state.confirm_waiters.push(ConfirmWaiter {
                        target_heads,
                        sent_generation,
                        reply,
                        deadline: Instant::now() + CONFIRM_SYNC_TIMEOUT,
                    });
                    self.state.next_confirm_sync_attempt = Instant::now() + CONFIRM_SYNC_RETRY;
                    self.resolve_confirm_waiters();
                }
            }

            SyncCommand::ConfirmStateSync { reply } => {
                if let Err(e) = send_state_sync_message(&self.io.doc, writer).await {
                    let _ = reply.send(Err(e));
                } else {
                    let now = Instant::now();
                    self.state.state_sync_waiters.push(StateSyncWaiter {
                        reply,
                        quiet_deadline: now + STATE_SYNC_QUIET_TIMEOUT,
                        deadline: now + STATE_SYNC_MAX_TIMEOUT,
                    });
                }
            }

            SyncCommand::SendPresence { data, reply } => {
                let result =
                    connection::send_typed_frame(writer, NotebookFrameType::Presence, &data)
                        .await
                        .map_err(SyncError::Io);
                let _ = reply.send(result);
            }
        }
    }

    async fn on_maintenance<W: AsyncWrite + Unpin>(
        &mut self,
        writer: &mut W,
    ) -> Result<(), SyncError> {
        self.resolve_confirm_waiters();
        if !self.state.confirm_waiters.is_empty()
            && Instant::now() >= self.state.next_confirm_sync_attempt
        {
            self.drive_confirm_sync_round(writer).await?;
        }
        self.resolve_state_sync_waiters();
        self.expire_pending_requests();
        Ok(())
    }

    async fn send_doc_sync_round<W: AsyncWrite + Unpin>(
        &mut self,
        writer: &mut W,
    ) -> Result<Option<u64>, SyncError> {
        if send_doc_sync_message(&self.io.doc, writer).await? {
            self.state.sync_generation += 1;
            Ok(Some(self.state.sync_generation))
        } else {
            Ok(None)
        }
    }

    /// Handle an incoming typed frame from the daemon.
    async fn handle_incoming_frame<W: AsyncWrite + Unpin>(
        &mut self,
        frame: &connection::TypedNotebookFrame,
        writer: &mut W,
    ) {
        match frame.frame_type {
            NotebookFrameType::AutomergeSync => {
                let msg = match sync::Message::decode(&frame.payload) {
                    Ok(msg) => msg,
                    Err(e) => {
                        warn!(
                            "[notebook-sync] Failed to decode sync message for {}: {}",
                            self.io.notebook_id, e
                        );
                        return;
                    }
                };

                // Apply and generate ack — lock held only for Automerge operations.
                //
                // The receive_sync_message call is wrapped in catch_unwind because
                // automerge 0.7 has a known panic in BatchApply::apply when the
                // internal patch log actor table gets out of order during concurrent
                // sync (automerge/automerge#1187). Without catch_unwind, the panic
                // poisons the Mutex and renders the session permanently unusable.
                // By catching it, the MutexGuard drops normally and subsequent
                // operations can still succeed.
                let ack_bytes = {
                    let mut state = self.io.doc.lock().unwrap_or_else(|e| e.into_inner());
                    let recv_result = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        state.receive_sync_message(msg)
                    }));
                    match recv_result {
                        Ok(Ok(())) => {}
                        Ok(Err(e)) => {
                            warn!(
                                "[notebook-sync] Failed to apply sync message for {}: {}",
                                self.io.notebook_id, e
                            );
                            return;
                        }
                        Err(panic_payload) => {
                            let msg = if let Some(s) = panic_payload.downcast_ref::<String>() {
                                s.as_str()
                            } else if let Some(s) = panic_payload.downcast_ref::<&str>() {
                                s
                            } else {
                                "unknown panic"
                            };
                            warn!(
                                "[notebook-sync] Automerge panicked during sync for {} \
                             (upstream bug automerge/automerge#1187): {}",
                                self.io.notebook_id, msg
                            );
                            // Rebuild doc via save→load to clear corrupted indices,
                            // then reset peer state to force a fresh sync handshake.
                            rebuild_shared_doc_state(&mut state);
                            return;
                        }
                    }
                    // Guard generate_sync_message — the collector can also panic
                    // with MissingOps even after a successful receive.
                    match std::panic::catch_unwind(AssertUnwindSafe(|| {
                        state.generate_sync_message().map(|msg| msg.encode())
                    })) {
                        Ok(bytes) => bytes,
                        Err(_) => {
                            warn!(
                            "[notebook-sync] Automerge panicked in generate_sync_message for {} \
                             (upstream MissingOps bug)",
                            self.io.notebook_id
                        );
                            rebuild_shared_doc_state(&mut state);
                            None
                        }
                    }
                };

                // Publish snapshot immediately (before sending ack — readers see changes fast)
                publish_snapshot(&self.io.doc, &self.io.snapshot_tx);

                // Send ack if needed (outside the lock — never hold across I/O)
                if let Some(bytes) = ack_bytes {
                    if let Err(e) = connection::send_typed_frame(
                        writer,
                        NotebookFrameType::AutomergeSync,
                        &bytes,
                    )
                    .await
                    {
                        warn!(
                            "[notebook-sync] Failed to send sync ack for {}: {}",
                            self.io.notebook_id, e
                        );
                    }
                }
            }

            NotebookFrameType::Broadcast => {
                match serde_json::from_slice::<NotebookBroadcast>(&frame.payload) {
                    Ok(bc) => {
                        let _ = self.io.broadcast_tx.send(bc);
                    }
                    Err(e) => {
                        warn!(
                            "[notebook-sync] Failed to parse broadcast for {}: {}",
                            self.io.notebook_id, e
                        );
                    }
                }
            }

            NotebookFrameType::Presence => {
                use notebook_doc::presence::{
                    decode_message, validate_frame_size, PresenceMessage,
                };

                if let Err(e) = validate_frame_size(&frame.payload) {
                    debug!(
                        "[notebook-sync] Dropping oversized presence frame for {}: {}",
                        self.io.notebook_id, e
                    );
                    return;
                }

                match decode_message(&frame.payload) {
                    Ok(msg) => {
                        let now_ms = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        let mut state = self.io.doc.lock().unwrap_or_else(|e| e.into_inner());
                        match msg {
                            PresenceMessage::Update {
                                peer_id,
                                peer_label,
                                actor_label,
                                data,
                            } => {
                                let label = peer_label.as_deref().unwrap_or(&peer_id);
                                state.presence.update_peer(
                                    &peer_id,
                                    label,
                                    actor_label.as_deref(),
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
                            self.io.notebook_id, e
                        );
                    }
                }
            }

            NotebookFrameType::Response => {
                // Unexpected outside of a request/response cycle
                warn!(
                    "[notebook-sync] Unexpected Response frame for {} in background loop",
                    self.io.notebook_id
                );
            }

            NotebookFrameType::Request => {
                warn!(
                    "[notebook-sync] Unexpected Request frame from daemon for {}",
                    self.io.notebook_id
                );
            }

            NotebookFrameType::RuntimeStateSync => {
                let msg = match sync::Message::decode(&frame.payload) {
                    Ok(msg) => msg,
                    Err(e) => {
                        warn!(
                            "[notebook-sync] Failed to decode RuntimeStateSync for {}: {}",
                            self.io.notebook_id, e
                        );
                        return;
                    }
                };

                // Apply and generate reply — same pattern as AutomergeSync
                let reply_bytes = {
                    let mut state = self.io.doc.lock().unwrap_or_else(|e| e.into_inner());
                    if let Err(e) = state.receive_state_sync_message(msg) {
                        warn!(
                            "[notebook-sync] Failed to apply RuntimeStateSync for {}: {}",
                            self.io.notebook_id, e
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
                            self.io.notebook_id, e
                        );
                    }
                }
            }

            NotebookFrameType::PoolStateSync => {
                // PoolDoc sync is handled by the frontend WASM layer, not the Python client.
                // Ignore in the Python sync task.
                debug!(
                    "[notebook-sync] Ignoring PoolStateSync frame for {} (handled by frontend)",
                    self.io.notebook_id
                );
            }

            NotebookFrameType::SessionControl => {
                let message = match serde_json::from_slice::<SessionControlMessage>(&frame.payload)
                {
                    Ok(message) => message,
                    Err(e) => {
                        warn!(
                            "[notebook-sync] Failed to parse SessionControl for {}: {}",
                            self.io.notebook_id, e
                        );
                        return;
                    }
                };

                match message {
                    SessionControlMessage::SyncStatus(status) => {
                        let io = &self.io;
                        let state = &mut self.state;
                        apply_sync_status(
                            &io.status_tx,
                            &io.notebook_id,
                            &mut state.saw_session_status,
                            status,
                        );
                    }
                }
            }
        }
    }

    async fn register_request<W: AsyncWrite + Unpin>(
        &mut self,
        writer: &mut W,
        request: NotebookRequest,
        reply: oneshot::Sender<Result<NotebookResponse, SyncError>>,
        broadcast_tx: Option<broadcast::Sender<NotebookBroadcast>>,
    ) {
        let id = uuid::Uuid::new_v4().to_string();
        let deadline = Instant::now() + crate::relay_task::request_timeout(&request);
        let envelope = NotebookRequestEnvelope {
            id: Some(id.clone()),
            request,
        };

        let payload = match serde_json::to_vec(&envelope) {
            Ok(payload) => payload,
            Err(e) => {
                let _ = reply.send(Err(SyncError::Serialization(e.to_string())));
                return;
            }
        };

        // Register before sending so a fast daemon response cannot beat the
        // pending entry. The main frame loop owns all response routing.
        self.state.pending_requests.insert(
            id.clone(),
            PendingRequest {
                reply,
                broadcast_tx,
                deadline,
            },
        );

        if let Err(e) =
            connection::send_typed_frame(writer, NotebookFrameType::Request, &payload).await
        {
            if let Some(entry) = self.state.pending_requests.remove(&id) {
                let _ = entry.reply.send(Err(SyncError::Io(e)));
            }
        }
    }

    async fn handle_task_frame<W: AsyncWrite + Unpin>(
        &mut self,
        frame: &connection::TypedNotebookFrame,
        writer: &mut W,
    ) {
        match frame.frame_type {
            NotebookFrameType::Response => {
                match serde_json::from_slice::<NotebookResponseEnvelope>(&frame.payload) {
                    Ok(envelope) => {
                        let entry = envelope
                            .id
                            .as_deref()
                            .and_then(|id| self.state.pending_requests.remove(id));
                        if let Some(entry) = entry {
                            let _ = entry.reply.send(Ok(envelope.response));
                        } else {
                            warn!(
                                "[notebook-sync] Unknown Response id for {}: {:?}",
                                self.io.notebook_id, envelope.id
                            );
                        }
                    }
                    Err(e) => {
                        warn!(
                            "[notebook-sync] Malformed response envelope for {}: {}",
                            self.io.notebook_id, e
                        );
                    }
                }
            }

            NotebookFrameType::Broadcast => {
                match serde_json::from_slice::<NotebookBroadcast>(&frame.payload) {
                    Ok(bc) => {
                        for entry in self.state.pending_requests.values() {
                            if let Some(tx) = &entry.broadcast_tx {
                                let _ = tx.send(bc.clone());
                            }
                        }
                        let _ = self.io.broadcast_tx.send(bc);
                    }
                    Err(e) => {
                        warn!(
                            "[notebook-sync] Failed to parse broadcast for {}: {}",
                            self.io.notebook_id, e
                        );
                    }
                }
            }

            _ => self.handle_incoming_frame(frame, writer).await,
        }
    }

    fn target_heads_confirmed(&self, target_heads: &[ChangeHash]) -> bool {
        if target_heads.is_empty() {
            return true;
        }
        let state = self.io.doc.lock().unwrap_or_else(|e| e.into_inner());
        heads_confirmed_by_peer(target_heads, &state.peer_state.shared_heads)
    }

    fn resolve_confirm_waiters(&mut self) {
        if self.state.confirm_waiters.is_empty() {
            return;
        }

        let shared_heads = {
            let state = self.io.doc.lock().unwrap_or_else(|e| e.into_inner());
            state.peer_state.shared_heads.clone()
        };
        let now = Instant::now();
        let acked_sync_generation = self.state.acked_sync_generation;
        let mut pending = Vec::with_capacity(self.state.confirm_waiters.len());

        for waiter in self.state.confirm_waiters.drain(..) {
            if heads_confirmed_by_peer(&waiter.target_heads, &shared_heads)
                || waiter
                    .sent_generation
                    .map(|generation| generation <= acked_sync_generation)
                    .unwrap_or(false)
            {
                let _ = waiter.reply.send(Ok(()));
            } else if now >= waiter.deadline {
                debug!(
                    "[notebook-sync] confirm_sync timed out before heads fully confirmed for {}",
                    self.io.notebook_id
                );
                let _ = waiter.reply.send(Ok(()));
            } else {
                pending.push(waiter);
            }
        }

        self.state.confirm_waiters = pending;
    }

    async fn drive_confirm_sync_round<W: AsyncWrite + Unpin>(
        &mut self,
        writer: &mut W,
    ) -> Result<(), SyncError> {
        if self.state.confirm_waiters.is_empty() {
            return Ok(());
        }

        if let Some(generation) = self.send_doc_sync_round(writer).await? {
            mark_unsent_confirm_waiters(&mut self.state.confirm_waiters, generation);
        }
        self.state.next_confirm_sync_attempt = Instant::now() + CONFIRM_SYNC_RETRY;
        self.resolve_confirm_waiters();
        Ok(())
    }

    fn note_frame_activity(&mut self) {
        if self.state.state_sync_waiters.is_empty() {
            return;
        }
        let now = Instant::now();
        for waiter in &mut self.state.state_sync_waiters {
            waiter.quiet_deadline = (now + STATE_SYNC_QUIET_TIMEOUT).min(waiter.deadline);
        }
    }

    fn resolve_state_sync_waiters(&mut self) {
        if self.state.state_sync_waiters.is_empty() {
            return;
        }
        let now = Instant::now();
        let mut pending = Vec::with_capacity(self.state.state_sync_waiters.len());

        for waiter in self.state.state_sync_waiters.drain(..) {
            if now >= waiter.quiet_deadline || now >= waiter.deadline {
                let _ = waiter.reply.send(Ok(()));
            } else {
                pending.push(waiter);
            }
        }

        self.state.state_sync_waiters = pending;
    }

    fn expire_pending_requests(&mut self) {
        if self.state.pending_requests.is_empty() {
            return;
        }
        let now = Instant::now();
        let expired_ids: Vec<String> = self
            .state
            .pending_requests
            .iter()
            .filter(|(_, entry)| now >= entry.deadline)
            .map(|(id, _)| id.clone())
            .collect();

        for id in expired_ids {
            if let Some(entry) = self.state.pending_requests.remove(&id) {
                warn!(
                    "[notebook-sync] Request {} timed out for {}",
                    id, self.io.notebook_id
                );
                let _ = entry.reply.send(Err(SyncError::Timeout));
            }
        }
    }

    fn disconnect_pending(&mut self) {
        for (_, entry) in self.state.pending_requests.drain() {
            let _ = entry.reply.send(Err(SyncError::Disconnected));
        }
        for waiter in self.state.confirm_waiters.drain(..) {
            let _ = waiter.reply.send(Err(SyncError::Disconnected));
        }
        for waiter in self.state.state_sync_waiters.drain(..) {
            let _ = waiter.reply.send(Err(SyncError::Disconnected));
        }
    }
}

async fn send_doc_sync_message<W: AsyncWrite + Unpin>(
    doc: &Arc<Mutex<SharedDocState>>,
    writer: &mut W,
) -> Result<bool, SyncError> {
    let msg_bytes = {
        let mut state = doc.lock().unwrap_or_else(|e| e.into_inner());
        state.generate_sync_message().map(|msg| msg.encode())
    };

    if let Some(bytes) = msg_bytes {
        connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &bytes)
            .await
            .map_err(SyncError::Io)?;
        return Ok(true);
    }

    Ok(false)
}

fn mark_unsent_confirm_waiters(waiters: &mut [ConfirmWaiter], generation: u64) {
    for waiter in waiters {
        if waiter.sent_generation.is_none() {
            waiter.sent_generation = Some(generation);
        }
    }
}

async fn send_state_sync_message<W: AsyncWrite + Unpin>(
    doc: &Arc<Mutex<SharedDocState>>,
    writer: &mut W,
) -> Result<(), SyncError> {
    let msg_bytes = {
        let mut state = doc.lock().unwrap_or_else(|e| e.into_inner());
        state.generate_state_sync_message().map(|msg| msg.encode())
    };

    if let Some(bytes) = msg_bytes {
        connection::send_typed_frame(writer, NotebookFrameType::RuntimeStateSync, &bytes)
            .await
            .map_err(SyncError::Io)?;
    }

    Ok(())
}

fn heads_confirmed_by_peer(target_heads: &[ChangeHash], shared_heads: &[ChangeHash]) -> bool {
    target_heads.is_empty() || target_heads.iter().all(|head| shared_heads.contains(head))
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

fn mark_disconnected(status_tx: &watch::Sender<SyncStatus>) {
    let mut next = status_tx.borrow().clone();
    if next.connection != ConnectionState::Disconnected {
        next.connection = ConnectionState::Disconnected;
        let _ = status_tx.send(next);
    }
}

fn initial_load_transition_valid(current: &InitialLoadPhase, next: &InitialLoadPhase) -> bool {
    match current {
        InitialLoadPhase::NotNeeded => matches!(next, InitialLoadPhase::NotNeeded),
        InitialLoadPhase::Streaming => matches!(
            next,
            InitialLoadPhase::Streaming | InitialLoadPhase::Ready | InitialLoadPhase::Failed { .. }
        ),
        InitialLoadPhase::Ready => matches!(next, InitialLoadPhase::Ready),
        InitialLoadPhase::Failed { .. } => matches!(next, InitialLoadPhase::Failed { .. }),
    }
}

fn apply_sync_status(
    status_tx: &watch::Sender<SyncStatus>,
    notebook_id: &str,
    saw_session_status: &mut bool,
    incoming_wire: SessionSyncStatusWire,
) {
    let current = status_tx.borrow().clone();
    let mut incoming: SyncStatus = incoming_wire.into();
    incoming.connection = current.connection;

    if !*saw_session_status {
        let _ = status_tx.send(incoming);
        *saw_session_status = true;
        return;
    }

    let mut next = current.clone();

    if incoming.notebook_doc >= current.notebook_doc {
        next.notebook_doc = incoming.notebook_doc;
    } else {
        warn!(
            "[notebook-sync] Ignoring regressing notebook_doc status for {}: {:?} -> {:?}",
            notebook_id, current.notebook_doc, incoming.notebook_doc
        );
    }

    if incoming.runtime_state >= current.runtime_state {
        next.runtime_state = incoming.runtime_state;
    } else {
        warn!(
            "[notebook-sync] Ignoring regressing runtime_state status for {}: {:?} -> {:?}",
            notebook_id, current.runtime_state, incoming.runtime_state
        );
    }

    if initial_load_transition_valid(&current.initial_load, &incoming.initial_load) {
        next.initial_load = incoming.initial_load;
    } else {
        warn!(
            "[notebook-sync] Ignoring invalid initial_load status for {}: {:?} -> {:?}",
            notebook_id, current.initial_load, incoming.initial_load
        );
    }

    if next != current {
        let _ = status_tx.send(next);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use notebook_protocol::connection::send_typed_json_frame;
    use notebook_protocol::protocol::{
        InitialLoadPhaseWire, NotebookDocPhaseWire, NotebookRequestEnvelope,
        NotebookResponseEnvelope, RuntimeStatePhaseWire,
    };
    use serde_json::json;
    use tokio::io::{BufReader, BufWriter};
    use tokio::sync::{broadcast, mpsc, watch};
    use tokio::time::timeout;

    fn test_handle_and_config() -> (crate::DocHandle, SyncTaskConfig) {
        let shared = Arc::new(Mutex::new(SharedDocState::new(
            notebook_doc::NotebookDoc::new("test-notebook").into_inner(),
            "test-notebook".into(),
        )));
        let initial_snapshot = {
            let state = shared.lock().unwrap();
            NotebookSnapshot::from_doc(&state.doc)
        };
        let (snapshot_tx, snapshot_rx) = watch::channel(initial_snapshot);
        let snapshot_tx = Arc::new(snapshot_tx);
        let (status_tx, status_rx) = watch::channel(SyncStatus::connected_pending());
        let (changed_tx, changed_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (broadcast_tx, _broadcast_rx) = broadcast::channel::<NotebookBroadcast>(32);

        let handle = crate::DocHandle::new(
            Arc::clone(&shared),
            changed_tx,
            cmd_tx,
            Arc::clone(&snapshot_tx),
            snapshot_rx,
            status_rx,
            "test-notebook".to_string(),
        );
        let config = SyncTaskConfig {
            doc: shared,
            changed_rx,
            cmd_rx,
            snapshot_tx,
            status_tx,
            broadcast_tx,
        };

        (handle, config)
    }

    fn test_reactor() -> SyncReactor {
        let (_handle, config) = test_handle_and_config();
        let SyncTaskConfig {
            doc,
            changed_rx: _,
            cmd_rx: _,
            snapshot_tx,
            status_tx,
            broadcast_tx,
        } = config;

        SyncReactor::new(doc, snapshot_tx, status_tx, broadcast_tx)
    }

    fn interactive_status() -> SessionControlMessage {
        SessionControlMessage::SyncStatus(SessionSyncStatusWire {
            notebook_doc: NotebookDocPhaseWire::Interactive,
            runtime_state: RuntimeStatePhaseWire::Ready,
            initial_load: InitialLoadPhaseWire::NotNeeded,
        })
    }

    #[test]
    fn select_loop_preserves_biased_frame_priority_shape() {
        let source = include_str!("sync_task.rs");
        let select = &source[source
            .find("let select_result = tokio::select!")
            .expect("select loop exists")..];
        let biased = select.find("biased;").expect("select loop stays biased");
        let frame = select
            .find("frame = framed_reader.recv()")
            .expect("frame arm exists");
        let changed = select
            .find("result = changed_rx.recv()")
            .expect("changed arm exists");
        let command = select
            .find("cmd = cmd_rx.recv()")
            .expect("command arm exists");
        let maintenance = select
            .find("_ = maintenance.tick()")
            .expect("maintenance arm exists");

        assert!(biased < frame, "biased select must precede all arms");
        assert!(
            frame < changed && changed < command && command < maintenance,
            "select arm priority must remain Frame > Changed > Command > Maintenance"
        );
    }

    #[test]
    fn request_expiry_times_out_only_expired_pending_requests() {
        let mut reactor = test_reactor();
        let now = Instant::now();
        let (expired_tx, mut expired_rx) = oneshot::channel();
        let (live_tx, mut live_rx) = oneshot::channel();

        reactor.state.pending_requests.insert(
            "expired".into(),
            PendingRequest {
                reply: expired_tx,
                broadcast_tx: None,
                deadline: now - Duration::from_secs(1),
            },
        );
        reactor.state.pending_requests.insert(
            "live".into(),
            PendingRequest {
                reply: live_tx,
                broadcast_tx: None,
                deadline: now + Duration::from_secs(30),
            },
        );

        reactor.expire_pending_requests();

        assert!(!reactor.state.pending_requests.contains_key("expired"));
        assert!(reactor.state.pending_requests.contains_key("live"));
        assert!(matches!(expired_rx.try_recv(), Ok(Err(SyncError::Timeout))));
        assert!(matches!(
            live_rx.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn confirm_waiter_timeout_preserves_best_effort_ok() {
        let mut reactor = test_reactor();
        let (reply, mut rx) = oneshot::channel();

        reactor.state.confirm_waiters.push(ConfirmWaiter {
            target_heads: vec![ChangeHash([1; 32])],
            sent_generation: None,
            reply,
            deadline: Instant::now() - Duration::from_secs(1),
        });

        reactor.resolve_confirm_waiters();

        assert!(reactor.state.confirm_waiters.is_empty());
        assert!(matches!(rx.try_recv(), Ok(Ok(()))));
    }

    #[test]
    fn one_confirm_resolution_event_resolves_all_waiters() {
        let mut reactor = test_reactor();
        let mut receivers = Vec::new();

        reactor.state.acked_sync_generation = 7;
        for index in 0..10 {
            let (reply, rx) = oneshot::channel();
            reactor.state.confirm_waiters.push(ConfirmWaiter {
                target_heads: vec![ChangeHash([index + 1; 32])],
                sent_generation: Some(7),
                reply,
                deadline: Instant::now() + Duration::from_secs(30),
            });
            receivers.push(rx);
        }

        reactor.resolve_confirm_waiters();

        assert!(reactor.state.confirm_waiters.is_empty());
        for mut rx in receivers {
            assert!(matches!(rx.try_recv(), Ok(Ok(()))));
        }
    }

    #[tokio::test]
    async fn first_session_status_accepts_streaming_initial_load() {
        let (status_tx, status_rx) = watch::channel(SyncStatus::connected_pending());
        let mut saw = false;

        apply_sync_status(
            &status_tx,
            "test-notebook",
            &mut saw,
            SessionSyncStatusWire {
                notebook_doc: NotebookDocPhaseWire::Pending,
                runtime_state: RuntimeStatePhaseWire::Pending,
                initial_load: InitialLoadPhaseWire::Streaming,
            },
        );

        let status = status_rx.borrow().clone();
        assert!(saw);
        assert_eq!(status.initial_load, InitialLoadPhase::Streaming);
    }

    #[tokio::test]
    async fn regressing_session_status_components_are_ignored() {
        let (status_tx, status_rx) = watch::channel(SyncStatus::connected_pending());
        let mut saw = false;

        apply_sync_status(
            &status_tx,
            "test-notebook",
            &mut saw,
            SessionSyncStatusWire {
                notebook_doc: NotebookDocPhaseWire::Interactive,
                runtime_state: RuntimeStatePhaseWire::Ready,
                initial_load: InitialLoadPhaseWire::Streaming,
            },
        );

        apply_sync_status(
            &status_tx,
            "test-notebook",
            &mut saw,
            SessionSyncStatusWire {
                notebook_doc: NotebookDocPhaseWire::Syncing,
                runtime_state: RuntimeStatePhaseWire::Syncing,
                initial_load: InitialLoadPhaseWire::NotNeeded,
            },
        );

        let status = status_rx.borrow().clone();
        assert_eq!(
            status.notebook_doc,
            crate::status::NotebookDocPhase::Interactive
        );
        assert_eq!(
            status.runtime_state,
            crate::status::RuntimeStatePhase::Ready
        );
        assert_eq!(status.initial_load, InitialLoadPhase::Streaming);

        apply_sync_status(
            &status_tx,
            "test-notebook",
            &mut saw,
            SessionSyncStatusWire {
                notebook_doc: NotebookDocPhaseWire::Interactive,
                runtime_state: RuntimeStatePhaseWire::Ready,
                initial_load: InitialLoadPhaseWire::Ready,
            },
        );

        assert_eq!(
            status_rx.borrow().initial_load.clone(),
            InitialLoadPhase::Ready
        );
    }

    #[tokio::test]
    async fn run_marks_status_disconnected_on_exit() {
        let shared = Arc::new(Mutex::new(SharedDocState::new(
            notebook_doc::NotebookDoc::new("test-notebook").into_inner(),
            "test-notebook".into(),
        )));
        let initial_snapshot = {
            let state = shared.lock().unwrap();
            NotebookSnapshot::from_doc(&state.doc)
        };
        let (snapshot_tx, _snapshot_rx) = watch::channel(initial_snapshot);
        let snapshot_tx = Arc::new(snapshot_tx);
        let (status_tx, status_rx) = watch::channel(SyncStatus::connected_pending());
        let (changed_tx, changed_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::channel(1);
        let (broadcast_tx, _broadcast_rx) = broadcast::channel::<NotebookBroadcast>(8);
        drop(changed_tx);
        drop(cmd_tx);

        run(
            SyncTaskConfig {
                doc: Arc::clone(&shared),
                changed_rx,
                cmd_rx,
                snapshot_tx,
                status_tx,
                broadcast_tx,
            },
            tokio::io::empty(),
            tokio::io::sink(),
        )
        .await;

        assert_eq!(status_rx.borrow().connection, ConnectionState::Disconnected);
    }

    #[tokio::test]
    async fn parallel_confirm_sync_does_not_starve_frame_pump() {
        let (handle, config) = test_handle_and_config();

        let mut after_cell_id: Option<String> = None;
        for index in 0..10 {
            let cell_id = format!("cell-{index}");
            handle
                .add_cell_with_source(
                    &cell_id,
                    "code",
                    after_cell_id.as_deref(),
                    &format!("print({index})"),
                )
                .expect("cell added");
            after_cell_id = Some(cell_id);
        }

        let (client, server) = tokio::io::duplex(128);
        let (client_read, client_write) = tokio::io::split(client);
        let (server_read, server_write) = tokio::io::split(server);

        let sync_task = tokio::spawn(run(config, client_read, client_write));
        let daemon = tokio::spawn(async move {
            let mut reader = connection::FramedReader::spawn(BufReader::new(server_read), 64);
            let mut writer = BufWriter::new(server_write);
            let mut daemon_state = SharedDocState::new(AutoCommit::new(), "test-notebook".into());

            loop {
                let frame = timeout(Duration::from_secs(15), reader.recv())
                    .await
                    .expect("daemon received sync frame")
                    .expect("client stayed connected")
                    .expect("sync frame read");

                if frame.frame_type != NotebookFrameType::AutomergeSync {
                    continue;
                }

                let msg = sync::Message::decode(&frame.payload).expect("valid sync message");
                daemon_state
                    .receive_sync_message(msg)
                    .expect("daemon receives client sync");

                if let Some(reply) = daemon_state.generate_sync_message() {
                    connection::send_typed_frame(
                        &mut writer,
                        NotebookFrameType::AutomergeSync,
                        &reply.encode(),
                    )
                    .await
                    .expect("daemon sends sync reply");
                }

                for _ in 0..32 {
                    send_typed_json_frame(
                        &mut writer,
                        NotebookFrameType::SessionControl,
                        &interactive_status(),
                    )
                    .await
                    .expect("daemon sends interleaved control frame");
                }

                let cell_count = notebook_doc::get_cells_from_doc(&daemon_state.doc).len();
                if cell_count >= 10 {
                    return cell_count;
                }
            }
        });

        let mut waiters = Vec::new();
        for _ in 0..10 {
            let handle = handle.clone();
            waiters.push(tokio::spawn(async move { handle.confirm_sync().await }));
        }

        timeout(Duration::from_secs(15), async {
            for waiter in waiters {
                waiter.await.expect("join").expect("confirm_sync");
            }
        })
        .await
        .expect("parallel confirm_sync waiters resolved without blocking frames");

        assert_eq!(daemon.await.expect("daemon task"), 10);
        drop(handle);
        sync_task.await.expect("sync task exits");
    }

    #[tokio::test]
    async fn concurrent_send_request_routes_responses_by_id() {
        let (handle, config) = test_handle_and_config();
        let (client, server) = tokio::io::duplex(256);
        let (client_read, client_write) = tokio::io::split(client);
        let (server_read, server_write) = tokio::io::split(server);

        let sync_task = tokio::spawn(run(config, client_read, client_write));
        let daemon = tokio::spawn(async move {
            let mut reader = connection::FramedReader::spawn(BufReader::new(server_read), 64);
            let mut writer = BufWriter::new(server_write);
            let mut ids = Vec::new();

            while ids.len() < 2 {
                let frame = timeout(Duration::from_secs(15), reader.recv())
                    .await
                    .expect("daemon received request")
                    .expect("client stayed connected")
                    .expect("request frame read");
                if frame.frame_type != NotebookFrameType::Request {
                    continue;
                }
                let envelope: NotebookRequestEnvelope =
                    serde_json::from_slice(&frame.payload).expect("request envelope");
                ids.push(envelope.id.expect("request id"));
            }

            send_typed_json_frame(
                &mut writer,
                NotebookFrameType::Broadcast,
                &NotebookBroadcast::Comm {
                    msg_type: "comm_msg".into(),
                    content: json!({"comm_id": "abc", "data": {}}),
                    buffers: Vec::new(),
                },
            )
            .await
            .expect("daemon sends broadcast");

            send_typed_json_frame(
                &mut writer,
                NotebookFrameType::Response,
                &NotebookResponseEnvelope {
                    id: Some(ids[1].clone()),
                    response: NotebookResponse::NoKernel {},
                },
            )
            .await
            .expect("daemon sends second response first");

            send_typed_json_frame(
                &mut writer,
                NotebookFrameType::Response,
                &NotebookResponseEnvelope {
                    id: Some(ids[0].clone()),
                    response: NotebookResponse::DocBytes {
                        bytes: vec![1, 2, 3],
                    },
                },
            )
            .await
            .expect("daemon sends first response second");
        });

        let (progress_tx, mut progress_rx) = broadcast::channel(8);
        let first =
            handle.send_request_with_broadcast(NotebookRequest::GetDocBytes {}, progress_tx);
        let second = handle.send_request(NotebookRequest::GetDocBytes {});

        let (first_response, second_response) = tokio::join!(first, second);
        let first_response = first_response.expect("first response");
        let second_response = second_response.expect("second response");
        let progress = progress_rx
            .recv()
            .await
            .expect("request progress broadcast");

        assert!(matches!(first_response, NotebookResponse::DocBytes { .. }));
        assert!(matches!(second_response, NotebookResponse::NoKernel {}));
        assert!(matches!(progress, NotebookBroadcast::Comm { .. }));

        daemon.await.expect("daemon task");
        drop(handle);
        sync_task.await.expect("sync task exits");
    }
}
