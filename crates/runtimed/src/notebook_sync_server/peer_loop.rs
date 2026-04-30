use super::peer_notebook_sync::{
    finish_notebook_doc_frame, forward_notebook_doc_broadcast, handle_notebook_doc_frame,
    queue_doc_sync, NotebookDocFrameOutcome,
};
use super::peer_pool_sync::{
    forward_pool_state_broadcast, handle_pool_state_frame, send_initial_pool_sync,
};
use super::peer_presence::{
    forward_presence_broadcast, handle_presence_frame, prune_stale_presence,
    send_initial_presence_snapshot,
};
use super::peer_runtime_sync::{forward_runtime_state_broadcast, handle_runtime_state_frame};
use super::peer_session::{
    send_initial_notebook_doc_sync, send_initial_runtime_state_sync, send_session_status,
    stream_initial_load, InitialSyncState,
};
use super::peer_writer::{
    enqueue_notebook_request, queue_session_status, spawn_peer_request_worker, spawn_peer_writer,
};
use super::*;

/// Typed frames sync loop with first-byte type indicator.
///
/// Handles both Automerge sync messages and NotebookRequest messages.
/// This protocol supports daemon-owned kernel execution (Phase 8).
///
/// Takes `reader` by value because the post-streaming-load main loop
/// hands it to a `FramedReader` actor; from that point the read half
/// belongs to the dedicated reader task, not this select loop.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_sync_loop_v2<R, W>(
    mut reader: R,
    mut writer: W,
    room: &Arc<NotebookRoom>,
    _rooms: NotebookRooms,
    notebook_id: String,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
    needs_load: Option<&Path>,
    peer_id: &str,
    client_protocol_version: u8,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    // Subscribe before sending bootstrap traffic so any writes that land
    // during connection setup are still observed as steady-state deltas.
    let mut changed_rx = room.broadcasts.changed_tx.subscribe();
    let mut kernel_broadcast_rx = room.broadcasts.kernel_broadcast_tx.subscribe();
    let mut presence_rx = room.broadcasts.presence_tx.subscribe();
    let mut state_changed_rx = room.state.subscribe();

    // PoolDoc — global daemon pool state (UV/Conda availability, errors).
    let mut pool_changed_rx = daemon.pool_doc_changed.subscribe();

    let mut notebook_doc_phase = notebook_protocol::protocol::NotebookDocPhaseWire::Pending;
    let mut runtime_state_phase = notebook_protocol::protocol::RuntimeStatePhaseWire::Pending;
    let mut initial_load_phase = if needs_load.is_some() {
        notebook_protocol::protocol::InitialLoadPhaseWire::Streaming
    } else {
        notebook_protocol::protocol::InitialLoadPhaseWire::NotNeeded
    };

    if client_protocol_version >= 3 {
        send_session_status(
            &mut writer,
            notebook_doc_phase,
            runtime_state_phase,
            initial_load_phase.clone(),
        )
        .await?;
    }

    let InitialSyncState { mut peer_state } =
        send_initial_notebook_doc_sync(&mut writer, room).await?;
    notebook_doc_phase = notebook_protocol::protocol::NotebookDocPhaseWire::Syncing;
    if client_protocol_version >= 3 {
        send_session_status(
            &mut writer,
            notebook_doc_phase,
            runtime_state_phase,
            initial_load_phase.clone(),
        )
        .await?;
    }

    let mut state_peer_state = sync::State::new();
    let mut pool_peer_state = sync::State::new();
    let mut persisted_execution_records: std::collections::HashMap<
        String,
        runtimed_client::execution_store::ExecutionRecord,
    > = std::collections::HashMap::new();
    let execution_store = runtimed_client::execution_store::ExecutionStore::new(
        daemon.config.execution_store_dir.clone(),
    );

    send_initial_runtime_state_sync(&mut writer, room, &mut state_peer_state).await?;
    runtime_state_phase = notebook_protocol::protocol::RuntimeStatePhaseWire::Syncing;
    if client_protocol_version >= 3 {
        send_session_status(
            &mut writer,
            notebook_doc_phase,
            runtime_state_phase,
            initial_load_phase.clone(),
        )
        .await?;
    }

    initial_load_phase = stream_initial_load(
        &mut reader,
        &mut writer,
        room,
        needs_load,
        &daemon.config.execution_store_dir,
        &mut peer_state,
        notebook_doc_phase,
        runtime_state_phase,
        initial_load_phase,
        client_protocol_version,
    )
    .await?;

    send_initial_pool_sync(&mut writer, &daemon, &mut pool_peer_state).await?;

    // CommSync broadcast is no longer needed. Late joiners receive widget
    // state via RuntimeStateDoc CRDT sync, and the frontend CRDT watcher
    // synthesizes comm_open messages.

    send_initial_presence_snapshot(&mut writer, room, peer_id).await?;

    // Periodic pruning of stale presence peers (e.g. clients that silently dropped).
    let prune_period = std::time::Duration::from_millis(presence::DEFAULT_HEARTBEAT_MS);
    let mut prune_interval = tokio::time::interval(prune_period);
    prune_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Bootstrap sends stay synchronous so initial load failures surface to the
    // caller. Once steady state starts, socket writes move to a single ordered
    // writer task; the peer loop must keep draining client frames even when the
    // client is temporarily slow to read daemon frames.
    let (peer_writer, mut writer_task) =
        spawn_peer_writer(writer, notebook_id.clone(), peer_id.to_string());
    let mut request_worker = spawn_peer_request_worker(
        room.clone(),
        daemon.clone(),
        peer_writer.clone(),
        notebook_id.clone(),
        peer_id.to_string(),
    );

    // Hand the reader off to a dedicated FramedReader actor before
    // entering the busy `select!` below. `recv_typed_frame`'s internal
    // `read_exact` calls are NOT cancel-safe — putting them directly
    // in a `select!` arm desyncs the framed stream the moment another
    // arm wins mid-payload (see issue + production diagnostics).
    let mut framed_reader = connection::FramedReader::spawn(reader, 16);

    // Steady state: exchange client frames and broadcast room changes.
    loop {
        tokio::select! {
            biased;

            writer_result = &mut writer_task.handle => {
                return match writer_result {
                    Ok(result) => result,
                    Err(e) => Err(anyhow::anyhow!(
                        "peer writer task stopped for {}: {}",
                        notebook_id,
                        e
                    )),
                };
            }

            request_worker_result = &mut request_worker.handle => {
                return match request_worker_result {
                    Ok(result) => result,
                    Err(e) => Err(anyhow::anyhow!(
                        "peer request worker stopped for {}: {}",
                        notebook_id,
                        e
                    )),
                };
            }

            // Incoming message from this client (cancel-safe via FramedReader actor)
            maybe_frame = framed_reader.recv() => {
                let frame = match maybe_frame {
                    Some(Ok(frame)) => frame,
                    Some(Err(e)) => return Err(e.into()),
                    None => return Ok(()), // clean EOF
                };
                match frame.frame_type {
                            NotebookFrameType::AutomergeSync => {
                                let notebook_doc_effects = match handle_notebook_doc_frame(
                                    room,
                                    &mut peer_state,
                                    &peer_writer,
                                    &frame.payload,
                                )
                                .await?
                                {
                                    NotebookDocFrameOutcome::Applied(effects) => effects,
                                    NotebookDocFrameOutcome::Skipped => continue,
                                };

                                if notebook_doc_phase
                                    != notebook_protocol::protocol::NotebookDocPhaseWire::Interactive
                                {
                                    notebook_doc_phase =
                                        notebook_protocol::protocol::NotebookDocPhaseWire::Interactive;
                                    if client_protocol_version >= 3 {
                                        queue_session_status(
                                            &peer_writer,
                                            notebook_doc_phase,
                                            runtime_state_phase,
                                            initial_load_phase.clone(),
                                        )?;
                                    }
                                }

                                // Keep session status queued before these awaits so the sync reply
                                // and readiness transition remain adjacent on the peer writer.
                                finish_notebook_doc_frame(room, notebook_doc_effects).await;
                            }

                            NotebookFrameType::Request => {
                                enqueue_notebook_request(
                                    &request_worker,
                                    &peer_writer,
                                    &frame.payload,
                                    &notebook_id,
                                    peer_id,
                                )?;
                            }

                            NotebookFrameType::Presence => {
                                handle_presence_frame(room, peer_id, &peer_writer, &frame.payload)
                                    .await?;
                            }

                            NotebookFrameType::RuntimeStateSync => {
                                if !handle_runtime_state_frame(
                                    room,
                                    &mut state_peer_state,
                                    &peer_writer,
                                    &frame.payload,
                                    &execution_store,
                                    &mut persisted_execution_records,
                                )
                                .await?
                                {
                                    continue;
                                }

                                if runtime_state_phase
                                    != notebook_protocol::protocol::RuntimeStatePhaseWire::Ready
                                {
                                    runtime_state_phase =
                                        notebook_protocol::protocol::RuntimeStatePhaseWire::Ready;
                                    if client_protocol_version >= 3 {
                                        queue_session_status(
                                            &peer_writer,
                                            notebook_doc_phase,
                                            runtime_state_phase,
                                            initial_load_phase.clone(),
                                        )?;
                                    }
                                }
                            }

                            NotebookFrameType::PoolStateSync => {
                                if !handle_pool_state_frame(
                                    &daemon,
                                    &mut pool_peer_state,
                                    &peer_writer,
                                    &frame.payload,
                                )
                                .await?
                                {
                                    continue;
                                }
                            }

                            NotebookFrameType::Response
                            | NotebookFrameType::Broadcast
                            | NotebookFrameType::SessionControl => {
                                // Clients shouldn't send these
                                warn!(
                                    "[notebook-sync] Unexpected frame type from client: {:?}",
                                    frame.frame_type
                                );
                            }
                        }
            }

            // Another peer changed the document — push update to this client
            _ = changed_rx.recv() => {
                forward_notebook_doc_broadcast(room, &mut peer_state, &peer_writer).await?;

                if matches!(
                    initial_load_phase,
                    notebook_protocol::protocol::InitialLoadPhaseWire::Streaming
                ) && !room.is_loading()
                {
                    initial_load_phase =
                        notebook_protocol::protocol::InitialLoadPhaseWire::Ready;
                    if client_protocol_version >= 3 {
                        queue_session_status(
                            &peer_writer,
                            notebook_doc_phase,
                            runtime_state_phase,
                            initial_load_phase.clone(),
                        )?;
                    }
                }
            }

            // RuntimeStateDoc changed — push update to this client
            result = state_changed_rx.recv() => {
                if !forward_runtime_state_broadcast(
                    room,
                    peer_id,
                    &mut state_peer_state,
                    &peer_writer,
                    result,
                )
                .await?
                {
                    return Ok(());
                }
            }

            // PoolDoc changed — push update to this client
            result = pool_changed_rx.recv() => {
                if !forward_pool_state_broadcast(
                    &daemon,
                    peer_id,
                    &mut pool_peer_state,
                    &peer_writer,
                    result,
                )
                .await?
                {
                    return Ok(());
                }
            }

            // Presence update from another peer — forward to this client
            result = presence_rx.recv() => {
                if !forward_presence_broadcast(room, peer_id, &peer_writer, result).await? {
                    return Ok(());
                }
            }

            // Kernel broadcast event — forward to this client
            result = kernel_broadcast_rx.recv() => {
                match result {
                    Ok(broadcast) => {
                        peer_writer.send_json(NotebookFrameType::Broadcast, &broadcast)?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            "[notebook-sync] Peer lagged {} kernel broadcasts, sending doc sync to catch up",
                            n
                        );
                        // The peer missed some broadcasts (outputs, status changes).
                        // The Automerge doc contains the persisted state, so send a
                        // sync message to catch the peer up on any missed output data.
                        queue_doc_sync(
                            room,
                            &mut peer_state,
                            &peer_writer,
                        )
                        .await?;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Broadcast channel closed — room is being evicted
                        return Ok(());
                    }
                }
            }

            // Prune stale presence peers that haven't heartbeated within the TTL.
            // Each connection's loop is proof-of-life for its own peer, so we
            // mark ourselves seen before pruning to avoid false self-eviction
            // (idle-but-connected peers don't send frames).
            _ = prune_interval.tick() => {
                prune_stale_presence(room, peer_id).await;
            }
        }
    }
}
