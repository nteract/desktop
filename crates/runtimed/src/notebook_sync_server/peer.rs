use super::peer_eviction::handle_peer_disconnect;
use super::peer_notebook_sync::{
    finish_notebook_doc_frame, forward_notebook_doc_broadcast, handle_notebook_doc_frame,
    queue_doc_sync, NotebookDocFrameOutcome,
};
use super::peer_pool_sync::{
    forward_pool_state_broadcast, handle_pool_state_frame, send_initial_pool_sync,
};
use super::peer_presence::{
    cleanup_presence_on_disconnect, forward_presence_broadcast, handle_presence_frame,
    prune_stale_presence, send_initial_presence_snapshot,
};
use super::peer_runtime_sync::{forward_runtime_state_broadcast, handle_runtime_state_frame};
use super::peer_session::{send_initial_notebook_doc_sync, send_session_status, InitialSyncState};
use super::peer_writer::{
    enqueue_notebook_request, queue_session_status, spawn_peer_request_worker, spawn_peer_writer,
};
use super::*;
use runtime_doc::RuntimeLifecycle;

/// Handle a single notebook sync client connection.
///
/// The caller has already consumed the handshake frame and resolved the room.
/// This function runs the Automerge sync protocol:
/// 1. Initial sync: server sends first message
/// 2. Watch loop: wait for changes (from other peers or from this client),
///    exchange sync messages to propagate
///
/// When the connection closes (client disconnect or error), the peer count
/// is decremented. If it reaches zero, the room is evicted and any pending
/// doc bytes are flushed via debounced persistence.
///
/// If `skip_capabilities` is true, the ProtocolCapabilities frame is not sent.
/// This is used for OpenNotebook/CreateNotebook handshakes where the protocol
/// is already communicated in the NotebookConnectionInfo response.
#[allow(clippy::too_many_arguments)]
pub async fn handle_notebook_sync_connection<R, W>(
    reader: R,
    mut writer: W,
    room: Arc<NotebookRoom>,
    rooms: NotebookRooms,
    notebook_id: String,
    default_runtime: crate::runtime::Runtime,
    default_python_env: crate::settings_doc::PythonEnvType,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
    working_dir: Option<PathBuf>,
    initial_metadata: Option<String>,
    skip_capabilities: bool,
    needs_load: Option<PathBuf>,
    // True if this is a newly-created notebook at a non-existent path.
    // Used to enable auto-launch for notebooks created via `runt notebook newfile.ipynb`.
    created_new_at_path: bool,
    // Protocol version from the client preamble. v4 is required at connection
    // setup, so SessionControl frames are always supported.
    client_protocol_version: u8,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    // Set working_dir on the room if provided (for untitled notebook project detection)
    if let Some(wd) = working_dir {
        let mut room_wd = room.identity.working_dir.write().await;
        *room_wd = Some(wd);
    }

    // Seed initial metadata into the Automerge doc if provided and doc has no metadata yet.
    // This ensures the kernelspec is available before auto-launch decides which kernel to use.
    if let Some(ref metadata_json) = initial_metadata {
        match serde_json::from_str::<NotebookMetadataSnapshot>(metadata_json) {
            Ok(snapshot) => {
                let mut doc = room.doc.write().await;
                if doc.get_metadata_snapshot().is_none() {
                    match doc.set_metadata_snapshot(&snapshot) {
                        Ok(()) => {
                            info!(
                                "[notebook-sync] Seeded initial metadata from handshake for {}",
                                notebook_id
                            );
                        }
                        Err(e) => {
                            warn!("[notebook-sync] Failed to seed initial metadata: {}", e);
                        }
                    }
                }
            }
            Err(e) => {
                warn!(
                    "[notebook-sync] Failed to parse initial metadata JSON for {}: {}",
                    notebook_id, e
                );
            }
        }
    }

    // Write trust state to RuntimeStateDoc so frontend can read it reactively.
    // Start with room.trust_state (from disk at room creation), then re-verify
    // from the doc in case initial_metadata was just seeded with a trust signature.
    //
    // Scope the trust_state read guard so it drops before acquiring state_doc
    // write lock (deadlock prevention: no lock held across `.await`).
    {
        let trust_state = room.trust_state.read().await;
        write_trust_to_runtime_state(&room, &trust_state);
    }
    // Re-verify trust from doc metadata — picks up trust signatures that were
    // written to the Automerge doc (e.g., from a previous approval or from
    // initial_metadata seeded above).
    check_and_update_trust_state(&room).await;

    room.connections
        .active_peers
        .fetch_add(1, Ordering::Relaxed);
    room.connections.had_peers.store(true, Ordering::Relaxed);
    let peers = room.connections.active_peers.load(Ordering::Relaxed);
    info!(
        "[notebook-sync] Client connected to room {} ({} peer{})",
        notebook_id,
        peers,
        if peers == 1 { "" } else { "s" }
    );

    // Auto-launch kernel if this is the first peer and notebook is trusted
    if peers == 1 {
        // Check if notebook_id is a UUID (new unsaved notebook) vs a file path
        let path_snapshot = room.identity.path.read().await.clone();
        let is_new_notebook = path_snapshot.as_ref().is_none_or(|p| !p.exists())
            && uuid::Uuid::parse_str(&notebook_id).is_ok();

        // Scope the trust_state read guard so it drops before
        // `has_kernel()` which acquires another lock (deadlock prevention).
        let trust_status = {
            let trust_state = room.trust_state.read().await;
            trust_state.status.clone()
        };
        let has_kernel = room.has_kernel().await;
        let should_auto_launch = !has_kernel
            && matches!(
                trust_status,
                runt_trust::TrustStatus::Trusted | runt_trust::TrustStatus::NoDependencies
            )
            // For existing files: trust must be verified (Trusted or NoDependencies)
            // For new notebooks (UUID, no file): NoDependencies is safe to auto-launch
            // For newly-created notebooks at a path: also safe to auto-launch
            && (path_snapshot.as_ref().is_some_and(|p| p.exists()) || is_new_notebook || created_new_at_path);

        if should_auto_launch {
            info!(
                "[notebook-sync] Auto-launching kernel for notebook {} (trust: {:?}, new: {})",
                notebook_id, trust_status, is_new_notebook
            );
            // Write Resolving immediately so clients never see stale NotStarted
            if let Err(e) = room
                .state
                .with_doc(|sd| sd.set_lifecycle(&RuntimeLifecycle::Resolving))
            {
                warn!("[runtime-state] {}", e);
            }
            // Spawn auto-launch in background so we don't block sync
            let room_clone = room.clone();
            let panic_room = room.clone();
            let notebook_id_clone = notebook_id.clone();
            let daemon_clone = daemon.clone();
            spawn_supervised(
                "auto-launch-kernel",
                async move {
                    auto_launch_kernel(
                        &room_clone,
                        &notebook_id_clone,
                        default_runtime,
                        default_python_env,
                        daemon_clone,
                    )
                    .await;
                },
                move |_| {
                    let r = panic_room;
                    // with_doc is sync (std::sync::Mutex), so no need for tokio::spawn
                    // to acquire the lock. But spawn_supervised's panic handler runs
                    // outside async context, so we still need spawn for the closure.
                    tokio::spawn(async move {
                        // Auto-launch panic — no specific typed reason. Clear
                        // any stale error_reason so the frontend prompt isn't
                        // stuck on an earlier missing_ipykernel, etc.
                        if let Err(e) = r.state.with_doc(|sd| {
                            sd.set_lifecycle_with_error(&RuntimeLifecycle::Error, None)
                        }) {
                            tracing::warn!("[runtime-state] {}", e);
                        }
                    });
                },
            );
        } else if !has_kernel
            && matches!(
                trust_status,
                runt_trust::TrustStatus::Untrusted | runt_trust::TrustStatus::SignatureInvalid
            )
        {
            // Kernel blocked on trust approval — write this to RuntimeStateDoc
            // so the frontend shows "Awaiting Trust Approval" instead of "Initializing"
            info!(
                "[notebook-sync] Kernel blocked on trust approval for {} (trust: {:?})",
                notebook_id, trust_status
            );
            if let Err(e) = room
                .state
                .with_doc(|sd| sd.set_lifecycle(&RuntimeLifecycle::AwaitingTrust))
            {
                warn!("[runtime-state] {}", e);
            }
        } else {
            info!(
                "[notebook-sync] Auto-launch skipped for {} (trust: {:?}, has_kernel: {}, path_exists: {}, is_new: {}, created_at_path: {})",
                notebook_id, trust_status, has_kernel,
                path_snapshot.as_ref().is_some_and(|p| p.exists()), is_new_notebook, created_new_at_path
            );
        }
    }

    // Send capabilities response unless already sent via NotebookConnectionInfo.
    if !skip_capabilities {
        let (proto_str, proto_ver) = (connection::PROTOCOL_V4, connection::PROTOCOL_VERSION);
        let caps = connection::ProtocolCapabilities {
            protocol: proto_str.to_string(),
            protocol_version: Some(proto_ver),
            daemon_version: Some(crate::daemon_version().to_string()),
        };
        connection::send_json_frame(&mut writer, &caps).await?;
    }

    // Generate peer_id here so it's available for cleanup regardless of
    // whether the sync loop exits with Ok or Err.
    let peer_id = uuid::Uuid::new_v4().to_string();

    let result = run_sync_loop_v2(
        reader,
        writer,
        &room,
        rooms.clone(),
        notebook_id.clone(),
        daemon.clone(),
        needs_load.as_deref(),
        &peer_id,
        client_protocol_version,
    )
    .await;

    // Always clean up presence on disconnect, whether the sync loop
    // exited cleanly (Ok) or with an error (Err). The peer_id was
    // generated before starting the sync loop, so it is always
    // available here. remove_peer is a no-op for unknown peers
    // (e.g. error before any presence was registered).
    cleanup_presence_on_disconnect(&room, &peer_id).await;

    handle_peer_disconnect(&room, rooms.clone(), &notebook_id, &peer_id, &daemon).await;

    result
}

/// Typed frames sync loop with first-byte type indicator.
///
/// Handles both Automerge sync messages and NotebookRequest messages.
/// This protocol supports daemon-owned kernel execution (Phase 8).
///
/// Takes `reader` by value because the post-streaming-load main loop
/// hands it to a `FramedReader` actor; from that point the read half
/// belongs to the dedicated reader task, not this select loop.
#[allow(clippy::too_many_arguments)]
async fn run_sync_loop_v2<R, W>(
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

    // Initial RuntimeStateDoc sync — encode inside lock, send outside.
    // Uses bounded generation to compact atomically if the message would exceed
    // the 100 MiB frame limit.
    let initial_state_encoded = room
        .state
        .with_doc(|state_doc| {
            // Safety net: compact before initial sync if the doc grew too large.
            // 80 MiB leaves headroom under the 100 MiB frame limit.
            const COMPACTION_THRESHOLD: usize = 80 * 1024 * 1024;
            if state_doc.compact_if_oversized(COMPACTION_THRESHOLD) {
                info!("[notebook-sync] Compacted oversized RuntimeStateDoc before initial sync");
            }
            match catch_automerge_panic("initial-state-sync", || {
                state_doc.generate_sync_message_bounded_encoded(
                    &mut state_peer_state,
                    STATE_SYNC_COMPACT_THRESHOLD,
                )
            }) {
                Ok(encoded) => Ok(encoded),
                Err(e) => {
                    warn!("{}", e);
                    state_doc.rebuild_from_save();
                    state_peer_state = sync::State::new();
                    Ok(state_doc
                        .generate_sync_message(&mut state_peer_state)
                        .map(|msg| msg.encode()))
                }
            }
        })
        .ok()
        .flatten();
    if let Some(encoded) = initial_state_encoded {
        connection::send_typed_frame(&mut writer, NotebookFrameType::RuntimeStateSync, &encoded)
            .await?;
    }
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

    // Streaming load: add cells in batches and sync after each batch so the
    // frontend can observe progressive notebook-doc updates.
    if let Some(load_path) = needs_load {
        if room.try_start_loading() {
            let execution_store = runtimed_client::execution_store::ExecutionStore::new(
                daemon.config.execution_store_dir.clone(),
            );
            match streaming_load_cells(
                &mut reader,
                &mut writer,
                room,
                load_path,
                Some(&execution_store),
                &mut peer_state,
            )
            .await
            {
                Ok(count) => {
                    room.finish_loading();
                    info!(
                        "[notebook-sync] Streaming load complete: {} cells from {}",
                        count,
                        load_path.display()
                    );
                    initial_load_phase = notebook_protocol::protocol::InitialLoadPhaseWire::Ready;
                    if client_protocol_version >= 3 {
                        send_session_status(
                            &mut writer,
                            notebook_doc_phase,
                            runtime_state_phase,
                            initial_load_phase.clone(),
                        )
                        .await?;
                    }
                }
                Err(e) => {
                    room.finish_loading();
                    {
                        let mut doc = room.doc.write().await;
                        let _ = doc.clear_all_cells();
                    }
                    let _ = room.broadcasts.changed_tx.send(());
                    warn!(
                        "[notebook-sync] Streaming load failed for {}: {}",
                        load_path.display(),
                        e
                    );
                    if client_protocol_version >= 3 {
                        send_session_status(
                            &mut writer,
                            notebook_doc_phase,
                            runtime_state_phase,
                            notebook_protocol::protocol::InitialLoadPhaseWire::Failed {
                                reason: e.clone(),
                            },
                        )
                        .await?;
                    }
                    return Err(anyhow::anyhow!("Streaming load failed: {}", e));
                }
            }
        }
    }

    send_initial_pool_sync(&mut writer, &daemon, &mut pool_peer_state).await?;

    // Phase 1.5 (removed): CommSync broadcast is no longer needed.
    // Late joiners receive widget state via RuntimeStateDoc CRDT sync,
    // and the frontend CRDT watcher synthesizes comm_open messages.

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

    // Phase 2: Exchange messages until sync is complete, then watch for changes
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
