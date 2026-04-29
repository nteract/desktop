use std::path::PathBuf;
use std::sync::Arc;

use notebook_protocol::connection::{send_typed_frame, FramedReader, NotebookFrameType};
use notebook_protocol::protocol::RuntimeAgentResponse;
use tracing::{debug, info, warn};

use super::peer_runtime_sync::persist_terminal_execution_records;
use super::peer_writer::spawn_peer_writer;
use super::{NotebookRoom, RuntimeAgentMessage, STATE_SYNC_COMPACT_THRESHOLD};

/// Handle a runtime agent subprocess that connected back to the daemon's Unix socket.
///
/// The runtime agent is a special peer that owns the kernel for this notebook
/// room. It receives RPC requests (LaunchKernel, Interrupt, etc.) via frame
/// 0x01 and watches RuntimeStateDoc for queued executions via frame 0x05.
///
/// This handler:
/// 1. Performs initial NotebookDoc + RuntimeStateDoc sync
/// 2. Sets up the `runtime_agent_request_tx` channel on the room
/// 3. Fires `runtime_agent_connected` to unblock LaunchKernel
/// 4. Enters a sync loop relaying frames bidirectionally
pub async fn handle_runtime_agent_sync_connection<R, W>(
    reader: R,
    mut writer: W,
    room: Arc<NotebookRoom>,
    notebook_id: String,
    runtime_agent_id: String,
    execution_store_dir: PathBuf,
) where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    // Frames are received on a dedicated task so the busy `select!`
    // below stays cancel-safe — `recv_typed_frame`'s internal
    // `read_exact` calls would otherwise drop bytes mid-read whenever
    // another arm wins, desyncing the runtime-agent ↔ daemon stream.
    let mut framed_reader = FramedReader::spawn(reader, 16);

    info!(
        "[notebook-sync] Runtime agent sync connection: notebook={} runtime_agent={}",
        notebook_id, runtime_agent_id
    );

    // Validate provenance — reject stale agents.
    // None means no agent is expected (room was reset or no spawn in progress),
    // so reject unconditionally. Only the exact current agent ID is accepted.
    {
        let expected = room.current_runtime_agent_id.read().await;
        match expected.as_deref() {
            Some(expected_id) if expected_id == runtime_agent_id => {
                // Match — this is the agent we're waiting for.
            }
            other => {
                warn!(
                    "[notebook-sync] Rejecting runtime agent {} (provenance is {:?})",
                    runtime_agent_id, other
                );
                return;
            }
        }
    }

    // ── 1. Initial NotebookDoc sync ──────────────────────────────────
    // Scope the doc write guard so it drops before the async send
    // (deadlock prevention: no lock held across `.await`).
    let mut doc_sync_state = automerge::sync::State::new();
    let doc_sync_msg = {
        let mut doc = room.doc.write().await;
        // Generate our sync message (full doc state for fresh peer)
        doc.generate_sync_message(&mut doc_sync_state)
            .map(|msg| msg.encode())
    };
    if let Some(encoded) = doc_sync_msg {
        if let Err(e) =
            send_typed_frame(&mut writer, NotebookFrameType::AutomergeSync, &encoded).await
        {
            warn!("[notebook-sync] Agent initial doc sync send failed: {}", e);
            return;
        }
    }

    // ── 2. Initial RuntimeStateDoc sync ──────────────────────────────
    // Uses bounded generation to compact if oversized (same 80 MiB threshold).
    let mut state_sync_state = automerge::sync::State::new();
    let state_sync_msg = room
        .state
        .with_doc(|sd| {
            Ok(sd.generate_sync_message_bounded_encoded(
                &mut state_sync_state,
                STATE_SYNC_COMPACT_THRESHOLD,
            ))
        })
        .ok()
        .flatten();
    if let Some(encoded) = state_sync_msg {
        if let Err(e) =
            send_typed_frame(&mut writer, NotebookFrameType::RuntimeStateSync, &encoded).await
        {
            warn!(
                "[notebook-sync] Agent initial state sync send failed: {}",
                e
            );
            return;
        }
    }

    // ── 3. Set up request channel ────────────────────────────────────
    let (ra_tx, mut ra_rx) = tokio::sync::mpsc::channel::<RuntimeAgentMessage>(16);
    {
        let mut tx_guard = room.runtime_agent_request_tx.lock().await;
        *tx_guard = Some(ra_tx);
    }

    // ── 4. Signal connected ─────────────────────────────────────────
    // Provenance is already set by the spawn site (before spawn).
    // We do NOT re-set it here — doing so after the async sync work above
    // would create a window where a newer spawn's provenance could be
    // clobbered by this (potentially stale) connect handler.
    //
    // take() ensures at most one signal per spawn generation — a stale
    // runtime agent that passes provenance finds None here (no-op).
    if let Some(tx) = room.pending_runtime_agent_connect_tx.lock().await.take() {
        let _ = tx.send(());
    }
    info!(
        "[notebook-sync] Runtime agent connected and ready: {}",
        runtime_agent_id
    );

    // ── 5. Sync loop ─────────────────────────────────────────────────
    let mut changed_rx = room.broadcasts.changed_tx.subscribe();
    let mut state_changed_rx = room.state.subscribe();
    let execution_store =
        runtimed_client::execution_store::ExecutionStore::new(execution_store_dir);
    let mut persisted_execution_records: std::collections::HashMap<
        String,
        runtimed_client::execution_store::ExecutionRecord,
    > = std::collections::HashMap::new();
    let mut pending_replies: std::collections::HashMap<
        String,
        tokio::sync::oneshot::Sender<RuntimeAgentResponse>,
    > = std::collections::HashMap::new();
    let (agent_writer, mut writer_task) = spawn_peer_writer(
        writer,
        notebook_id.clone(),
        format!("runtime-agent:{runtime_agent_id}"),
    );

    loop {
        tokio::select! {
            biased;

            writer_result = &mut writer_task.handle => {
                match writer_result {
                    Ok(Ok(())) => {
                        info!("[notebook-sync] Runtime agent writer closed cleanly: {}", runtime_agent_id);
                    }
                    Ok(Err(e)) => {
                        warn!("[notebook-sync] Runtime agent writer failed for {}: {}", runtime_agent_id, e);
                    }
                    Err(e) => {
                        warn!("[notebook-sync] Runtime agent writer task stopped for {}: {}", runtime_agent_id, e);
                    }
                }
                break;
            }

            // Frames from runtime agent (cancel-safe via FramedReader actor)
            maybe_frame = framed_reader.recv() => {
                let typed_frame = match maybe_frame {
                    Some(Ok(frame)) => frame,
                    Some(Err(e)) => {
                        info!("[notebook-sync] Agent disconnected: {}", e);
                        break;
                    }
                    None => {
                        info!("[notebook-sync] Agent disconnected (EOF)");
                        break;
                    }
                };
                match typed_frame.frame_type {
                    NotebookFrameType::AutomergeSync => {
                        if let Ok(msg) = automerge::sync::Message::decode(&typed_frame.payload) {
                            let mut doc = room.doc.write().await;
                            if doc.receive_sync_message(&mut doc_sync_state, msg).is_ok() {
                                let _ = room.broadcasts.changed_tx.send(());
                            }
                            // Send sync reply
                            if let Some(reply) = doc.generate_sync_message(&mut doc_sync_state) {
                                let encoded = reply.encode();
                                if let Err(e) = agent_writer.send_frame(
                                    NotebookFrameType::AutomergeSync,
                                    encoded,
                                ) {
                                    warn!("[notebook-sync] Failed to queue doc sync reply to runtime agent: {}", e);
                                    break;
                                }
                            }
                        }
                    }
                    NotebookFrameType::RuntimeStateSync => {
                        if let Ok(msg) = automerge::sync::Message::decode(&typed_frame.payload) {
                            let mut state_changed = false;
                            let reply_encoded = room.state.with_doc(|sd| {
                                if let Ok(changed) = sd.receive_sync_message_with_changes(
                                    &mut state_sync_state, msg,
                                ) {
                                    if changed {
                                        state_changed = true;
                                        // Notification handled by with_doc heads check
                                    }
                                }
                                Ok(sd.generate_sync_message(&mut state_sync_state)
                                    .map(|reply| reply.encode()))
                            }).ok().flatten();
                            if let Some(encoded) = reply_encoded {
                                if let Err(e) = agent_writer.send_frame(
                                    NotebookFrameType::RuntimeStateSync,
                                    encoded,
                                ) {
                                    warn!("[notebook-sync] Failed to queue state sync reply to runtime agent: {}", e);
                                    break;
                                }
                            }
                            if state_changed {
                                persist_terminal_execution_records(
                                    &room,
                                    &execution_store,
                                    &mut persisted_execution_records,
                                ).await;
                            }
                        }
                    }
                    NotebookFrameType::Response => {
                        if let Ok(envelope) = serde_json::from_slice::<
                            notebook_protocol::protocol::RuntimeAgentResponseEnvelope,
                        >(&typed_frame.payload) {
                            if let Some(reply) = pending_replies.remove(&envelope.id) {
                                let _ = reply.send(envelope.response);
                            } else {
                                debug!("[notebook-sync] Agent response for unknown id: {}", envelope.id);
                            }
                        }
                    }
                    _ => {
                        debug!("[notebook-sync] Agent sent unexpected frame type: {:?}", typed_frame.frame_type);
                    }
                }
            }

            // NotebookDoc changes (from other peers) → sync to runtime agent
            _ = changed_rx.recv() => {
                while changed_rx.try_recv().is_ok() {}
                let mut doc = room.doc.write().await;
                if let Some(msg) = doc.generate_sync_message(&mut doc_sync_state) {
                    let encoded = msg.encode();
                    if let Err(e) = agent_writer.send_frame(
                        NotebookFrameType::AutomergeSync,
                        encoded,
                    ) {
                        warn!("[notebook-sync] Failed to queue doc sync to runtime agent: {}", e);
                        break;
                    }
                }
            }

            // RuntimeStateDoc changes → sync to runtime agent
            _ = state_changed_rx.recv() => {
                while state_changed_rx.try_recv().is_ok() {}
                let encoded = room.state.with_doc(|sd| {
                    Ok(sd.generate_sync_message(&mut state_sync_state)
                        .map(|msg| msg.encode()))
                }).ok().flatten();
                if let Some(encoded) = encoded {
                    if let Err(e) = agent_writer.send_frame(
                        NotebookFrameType::RuntimeStateSync,
                        encoded,
                    ) {
                        warn!("[notebook-sync] Failed to queue state sync to runtime agent: {}", e);
                        break;
                    }
                }
            }

            // Forward requests to the runtime agent. Commands are fire-and-forget;
            // queries register a pending reply keyed by correlation ID.
            Some(msg) = ra_rx.recv() => {
                let (envelope, reply_tx) = match msg {
                    RuntimeAgentMessage::Command(env) => (env, None),
                    RuntimeAgentMessage::Query(env, tx) => (env, Some(tx)),
                };
                let json = match serde_json::to_vec(&envelope) {
                    Ok(j) => j,
                    Err(e) => {
                        if let Some(tx) = reply_tx {
                            let _ = tx.send(RuntimeAgentResponse::Error {
                                error: format!("Serialize error: {}", e),
                            });
                        }
                        continue;
                    }
                };
                let reply_id = envelope.id.clone();
                if let Some(tx) = reply_tx {
                    pending_replies.insert(reply_id.clone(), tx);
                }
                if let Err(e) = agent_writer.send_frame(
                    NotebookFrameType::Request,
                    json,
                ) {
                    if let Some(tx) = pending_replies.remove(&reply_id) {
                        let _ = tx.send(RuntimeAgentResponse::Error {
                            error: format!("Send error: {}", e),
                        });
                    }
                    break;
                }
            }
        }
    }

    // Drain any pending query replies so callers get an error instead of hanging.
    for (_id, reply_tx) in pending_replies.drain() {
        let _ = reply_tx.send(RuntimeAgentResponse::Error {
            error: "Runtime agent disconnected".to_string(),
        });
    }

    // Cleanup: only clear state if we're still the current runtime agent.
    // A stale runtime agent disconnecting after a new one connected must not
    // clobber the new runtime agent's channel.
    //
    // Scope the id read guard so it drops before acquiring other locks
    // (deadlock prevention: no lock held across `.await`).
    let is_current = {
        let expected = room.current_runtime_agent_id.read().await;
        expected.as_deref() == Some(&runtime_agent_id)
    };
    if is_current {
        {
            let mut tx_guard = room.runtime_agent_request_tx.lock().await;
            *tx_guard = None;
        }
        // No need to signal "disconnected" — the oneshot was consumed on
        // connect. If the runtime agent dies before connecting, the oneshot
        // sender is dropped when pending_runtime_agent_connect_tx is replaced
        // by the next spawn, which resolves the receiver with Err.
        //
        // Clear runtime_agent_handle so LaunchKernel spawns a new runtime agent
        let mut guard = room.runtime_agent_handle.lock().await;
        *guard = None;
    }
    info!(
        "[notebook-sync] Runtime agent sync connection closed: {}",
        runtime_agent_id
    );
}
