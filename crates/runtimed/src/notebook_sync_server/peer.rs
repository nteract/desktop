use super::*;

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
/// Uses v2 typed frames protocol (with first-byte type indicator).
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
    mut reader: R,
    mut writer: W,
    room: Arc<NotebookRoom>,
    notebook_id: String,
    runtime_agent_id: String,
) where
    R: tokio::io::AsyncRead + Unpin + Send,
    W: tokio::io::AsyncWrite + Unpin + Send,
{
    use notebook_protocol::connection::{recv_typed_frame, send_typed_frame, NotebookFrameType};
    use notebook_protocol::protocol::RuntimeAgentResponse;

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
    // Scope the state_doc write guard so it drops before the async send.
    // Uses bounded generation to compact if oversized (same 80 MiB threshold).
    let mut state_sync_state = automerge::sync::State::new();
    let state_sync_msg = {
        let mut sd = room.state_doc.write().await;
        sd.generate_sync_message_bounded_encoded(
            &mut state_sync_state,
            STATE_SYNC_COMPACT_THRESHOLD,
        )
    };
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
    let mut changed_rx = room.changed_tx.subscribe();
    let mut state_changed_rx = room.state_changed_tx.subscribe();
    let mut pending_replies: std::collections::HashMap<
        String,
        tokio::sync::oneshot::Sender<RuntimeAgentResponse>,
    > = std::collections::HashMap::new();

    loop {
        tokio::select! {
            // Frames from runtime agent
            frame = recv_typed_frame(&mut reader) => {
                match frame {
                    Ok(Some(typed_frame)) => {
                        match typed_frame.frame_type {
                            NotebookFrameType::AutomergeSync => {
                                if let Ok(msg) = automerge::sync::Message::decode(&typed_frame.payload) {
                                    let mut doc = room.doc.write().await;
                                    if doc.receive_sync_message(&mut doc_sync_state, msg).is_ok() {
                                        let _ = room.changed_tx.send(());
                                    }
                                    // Send sync reply
                                    if let Some(reply) = doc.generate_sync_message(&mut doc_sync_state) {
                                        let encoded = reply.encode();
                                        let _ = send_typed_frame(
                                            &mut writer,
                                            NotebookFrameType::AutomergeSync,
                                            &encoded,
                                        ).await;
                                    }
                                }
                            }
                            NotebookFrameType::RuntimeStateSync => {
                                if let Ok(msg) = automerge::sync::Message::decode(&typed_frame.payload) {
                                    let mut sd = room.state_doc.write().await;
                                    if let Ok(changed) = sd.receive_sync_message_with_changes(
                                        &mut state_sync_state, msg,
                                    ) {
                                        if changed {
                                            let _ = room.state_changed_tx.send(());
                                        }
                                    }
                                    // Send sync reply
                                    if let Some(reply) = sd.generate_sync_message(&mut state_sync_state) {
                                        let encoded = reply.encode();
                                        let _ = send_typed_frame(
                                            &mut writer,
                                            NotebookFrameType::RuntimeStateSync,
                                            &encoded,
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
                    Ok(None) => {
                        info!("[notebook-sync] Agent disconnected (EOF)");
                        break;
                    }
                    Err(e) => {
                        info!("[notebook-sync] Agent disconnected: {}", e);
                        break;
                    }
                }
            }

            // NotebookDoc changes (from other peers) → sync to runtime agent
            _ = changed_rx.recv() => {
                while changed_rx.try_recv().is_ok() {}
                let mut doc = room.doc.write().await;
                if let Some(msg) = doc.generate_sync_message(&mut doc_sync_state) {
                    let encoded = msg.encode();
                    if let Err(e) = send_typed_frame(
                        &mut writer,
                        NotebookFrameType::AutomergeSync,
                        &encoded,
                    ).await {
                        warn!("[notebook-sync] Failed to sync doc to runtime agent: {}", e);
                        break;
                    }
                }
            }

            // RuntimeStateDoc changes → sync to runtime agent
            _ = state_changed_rx.recv() => {
                while state_changed_rx.try_recv().is_ok() {}
                let mut sd = room.state_doc.write().await;
                if let Some(msg) = sd.generate_sync_message(&mut state_sync_state) {
                    let encoded = msg.encode();
                    if let Err(e) = send_typed_frame(
                        &mut writer,
                        NotebookFrameType::RuntimeStateSync,
                        &encoded,
                    ).await {
                        warn!("[notebook-sync] Failed to sync state to runtime agent: {}", e);
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
                if let Err(e) = send_typed_frame(
                    &mut writer,
                    NotebookFrameType::Request,
                    &json,
                ).await {
                    if let Some(tx) = reply_tx {
                        let _ = tx.send(RuntimeAgentResponse::Error {
                            error: format!("Send error: {}", e),
                        });
                    }
                    break;
                }
                if let Some(tx) = reply_tx {
                    pending_replies.insert(envelope.id, tx);
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

///
/// If `skip_capabilities` is true, the ProtocolCapabilities frame is not sent.
/// This is used for OpenNotebook/CreateNotebook handshakes where the protocol
/// is already communicated in the NotebookConnectionInfo response.
#[allow(clippy::too_many_arguments)]
pub async fn handle_notebook_sync_connection<R, W>(
    mut reader: R,
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
    // Protocol version from the client preamble. v2 clients don't understand
    // SessionControl frames, so we skip them when this is < 3.
    client_protocol_version: u8,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Set working_dir on the room if provided (for untitled notebook project detection)
    if let Some(wd) = working_dir {
        let mut room_wd = room.working_dir.write().await;
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
    let (status_str, needs_approval) = {
        let trust_state = room.trust_state.read().await;
        let needs_approval = !matches!(
            trust_state.status,
            runt_trust::TrustStatus::Trusted | runt_trust::TrustStatus::NoDependencies
        );
        let status_str = match &trust_state.status {
            runt_trust::TrustStatus::Trusted => "trusted",
            runt_trust::TrustStatus::Untrusted => "untrusted",
            runt_trust::TrustStatus::SignatureInvalid => "signature_invalid",
            runt_trust::TrustStatus::NoDependencies => "no_dependencies",
        };
        (status_str, needs_approval)
    };
    {
        let mut sd = room.state_doc.write().await;
        if sd.set_trust(status_str, needs_approval) {
            let _ = room.state_changed_tx.send(());
        }
    }
    // Re-verify trust from doc metadata — picks up trust signatures that were
    // written to the Automerge doc (e.g., from a previous approval or from
    // initial_metadata seeded above).
    check_and_update_trust_state(&room).await;

    room.active_peers.fetch_add(1, Ordering::Relaxed);
    room.had_peers.store(true, Ordering::Relaxed);
    let peers = room.active_peers.load(Ordering::Relaxed);
    info!(
        "[notebook-sync] Client connected to room {} ({} peer{})",
        notebook_id,
        peers,
        if peers == 1 { "" } else { "s" }
    );

    // Auto-launch kernel if this is the first peer and notebook is trusted
    if peers == 1 {
        // Check if notebook_id is a UUID (new unsaved notebook) vs a file path
        let path_snapshot = room.path.read().await.clone();
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
            // Record auto-launch time for grace period on eviction
            {
                let mut auto_launch_at = room.auto_launch_at.write().await;
                *auto_launch_at = Some(std::time::Instant::now());
            }
            // Write "starting" immediately so clients never see stale "not_started"
            {
                let mut sd = room.state_doc.write().await;
                let mut changed = false;
                changed |= sd.set_kernel_status("starting");
                changed |= sd.set_starting_phase("resolving");
                if changed {
                    let _ = room.state_changed_tx.send(());
                }
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
                    tokio::spawn(async move {
                        let mut sd = r.state_doc.write().await;
                        sd.set_kernel_status("error");
                        sd.set_starting_phase("");
                        let _ = r.state_changed_tx.send(());
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
            let mut sd = room.state_doc.write().await;
            let mut changed = false;
            changed |= sd.set_kernel_status("awaiting_trust");
            changed |= sd.set_starting_phase("");
            if changed {
                let _ = room.state_changed_tx.send(());
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
    // v2 clients expect PROTOCOL_V2 and don't understand session-control frames.
    if !skip_capabilities {
        let (proto_str, proto_ver) = if client_protocol_version >= 3 {
            (connection::PROTOCOL_V3, connection::PROTOCOL_VERSION)
        } else {
            (connection::PROTOCOL_V2, 2)
        };
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
        &mut reader,
        &mut writer,
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
    room.presence.write().await.remove_peer(&peer_id);
    match presence::encode_left(&peer_id) {
        Ok(left_bytes) => {
            let _ = room.presence_tx.send((peer_id, left_bytes));
        }
        Err(e) => warn!("[notebook-sync] Failed to encode 'left' presence: {}", e),
    }

    // Peer disconnected — decrement and possibly evict the room
    let remaining = room.active_peers.fetch_sub(1, Ordering::Relaxed) - 1;
    if remaining == 0 {
        // Schedule delayed eviction check. This handles:
        // 1. Grace period during auto-launch (client may reconnect)
        // 2. Kernel running with no peers (idle timeout)
        // Without this, rooms with kernels would leak indefinitely.
        let eviction_delay = daemon.room_eviction_delay().await;
        let rooms_for_eviction = rooms.clone();
        let path_index_for_eviction = daemon.path_index.clone();
        let room_for_eviction = room.clone();
        let notebook_id_for_eviction = notebook_id.clone();

        info!(
            "[notebook-sync] All peers disconnected from room {}, scheduling eviction check in {:?}",
            notebook_id,
            eviction_delay
        );

        spawn_best_effort("room-eviction", async move {
            // Outer loop wraps the eviction attempt so a flush timeout can
            // back off and retry rather than leak the room (and any attached
            // kernel / watcher) indefinitely. The loop exits either by
            // cancelling (peers reconnected) or by completing teardown.
            let mut delay = eviction_delay;
            let mut flush_retries: u32 = 0;
            loop {
                tokio::time::sleep(delay).await;

                // Check if peers reconnected during the delay
                if room_for_eviction.active_peers.load(Ordering::Relaxed) > 0 {
                    info!(
                        "[notebook-sync] Eviction cancelled for {} (peers reconnected)",
                        notebook_id_for_eviction
                    );
                    return;
                }

                // Force a synchronous flush of the persist debouncer BEFORE removing
                // the room from the map. Without this, a fast reconnect lands in
                // the window between HashMap removal and the debouncer's shutdown
                // flush (which only fires when the last Arc to the room drops, and
                // the eviction task still holds one while running kernel/env
                // teardown). In that window get_or_create_room creates a fresh
                // room that loads stale bytes from the .automerge file — or no
                // file at all for brand-new untitled notebooks — silently losing
                // cells and edits.
                //
                // Request/ack over a dedicated channel. The debouncer has a
                // select! arm that writes the latest doc bytes and replies on
                // the oneshot with the I/O result.
                //
                // On timeout or write failure we back off and retry indefinitely.
                // Proceeding with HashMap removal on a failed flush reopens the
                // race: either the write is still in flight, or the latest bytes
                // are only in the soon-to-be-dropped room. We'd rather leak a
                // room than silently lose user edits. A reconnect still finds
                // the live in-memory room and recovers; a genuinely wedged
                // filesystem will surface through other signals, and daemon
                // shutdown still tries a last flush on persist_tx drop.
                const FLUSH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
                const FLUSH_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(30);
                let mut flush_ok = true;
                let mut flush_failure_kind: Option<&'static str> = None;
                if let Some(ref flush_tx) = room_for_eviction.flush_request_tx {
                    let (ack_tx, ack_rx) = oneshot::channel::<bool>();
                    if flush_tx.send(ack_tx).is_ok() {
                        match tokio::time::timeout(FLUSH_TIMEOUT, ack_rx).await {
                            Ok(Ok(true)) => {}
                            Ok(Ok(false)) => {
                                flush_ok = false;
                                flush_failure_kind = Some("write error");
                            }
                            Ok(Err(_)) => {
                                // Debouncer dropped the ack sender without
                                // replying — task already exited (e.g. a
                                // previous eviction flushed and closed). Any
                                // pending bytes went through the shutdown path.
                                debug!(
                                    "[notebook-sync] Eviction flush ack dropped for {} (debouncer exited)",
                                    notebook_id_for_eviction
                                );
                            }
                            Err(_) => {
                                flush_ok = false;
                                flush_failure_kind = Some("timeout");
                            }
                        }
                    }
                }
                if !flush_ok {
                    flush_retries += 1;
                    warn!(
                        "[notebook-sync] Eviction flush failed for {} ({}; attempt {}); keeping room resident, retrying in {:?}",
                        notebook_id_for_eviction,
                        flush_failure_kind.unwrap_or("unknown"),
                        flush_retries,
                        FLUSH_RETRY_DELAY
                    );
                    delay = FLUSH_RETRY_DELAY;
                    continue;
                }
                break;
            }

            // Remove room from the map under the lock, then drop the lock
            // BEFORE async teardown. Holding the lock across runtime agent
            // shutdown RPCs causes a convoy deadlock when the agent is
            // unresponsive — all notebook operations block on the lock.
            //
            // Look up the room by Arc pointer — UUID key is stable, but this
            // guards against double-eviction races.
            let (should_teardown, evicted_uuid) = {
                let mut rooms_guard = rooms_for_eviction.lock().await;
                if room_for_eviction.active_peers.load(Ordering::Relaxed) == 0 {
                    // Find the room's UUID key by Arc pointer identity
                    let current_key = rooms_guard
                        .iter()
                        .find(|(_, r)| Arc::ptr_eq(r, &room_for_eviction))
                        .map(|(k, _)| *k);
                    if let Some(uuid) = current_key {
                        rooms_guard.remove(&uuid);
                        (true, Some(uuid))
                    } else {
                        debug!(
                            "[notebook-sync] Eviction skipped for {} (room already removed)",
                            notebook_id_for_eviction
                        );
                        (false, None)
                    }
                } else {
                    (false, None)
                }
            }; // rooms lock dropped here

            // Clean up path_index entry (separate lock, after rooms lock is dropped).
            // Use remove_by_uuid rather than reading room.path — a concurrent writer
            // A concurrent save-path-update could hold room.path.write() and a
            // try_read() would silently return None, leaking the path_index entry.
            if should_teardown {
                if let Some(uuid) = evicted_uuid {
                    path_index_for_eviction.lock().await.remove_by_uuid(uuid);
                }
            }

            if should_teardown {
                // Shut down runtime agent subprocess if running. RuntimeAgentHandle::spawn
                // moves Child into a background task, so kill_on_drop doesn't
                // trigger on room drop — we need explicit shutdown via RPC.
                {
                    let has_runtime_agent = room_for_eviction
                        .runtime_agent_request_tx
                        .lock()
                        .await
                        .is_some();
                    if has_runtime_agent {
                        // Timeout the shutdown RPC — a dead/stuck agent shouldn't
                        // block teardown forever.
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(5),
                            send_runtime_agent_request(
                                &room_for_eviction,
                                notebook_protocol::protocol::RuntimeAgentRequest::ShutdownKernel,
                            ),
                        )
                        .await
                        {
                            Ok(_) => {}
                            Err(_) => {
                                warn!(
                                    "[notebook-sync] Runtime agent shutdown timed out for {}, force-dropping",
                                    notebook_id_for_eviction
                                );
                            }
                        }
                        // Unregister from process group registry and drop handle
                        {
                            let mut guard = room_for_eviction.runtime_agent_handle.lock().await;
                            if let Some(ref handle) = *guard {
                                handle.unregister();
                            }
                            *guard = None;
                        }
                        {
                            let mut tx = room_for_eviction.runtime_agent_request_tx.lock().await;
                            *tx = None;
                        }
                    }
                }

                // Stop file watcher if running
                if let Some(shutdown_tx) = room_for_eviction.watcher_shutdown_tx.lock().await.take()
                {
                    let _ = shutdown_tx.send(());
                    debug!(
                        "[notebook-sync] Stopped file watcher for {}",
                        notebook_id_for_eviction
                    );
                }

                // Flush launched_config deps → metadata.runt.{uv,conda}.dependencies
                // before env cleanup and final save. This captures any packages
                // the user hot-installed during the session so they land in
                // the .ipynb, and feeds the preserve-predicate below with the
                // up-to-date dep list so the unified-hash path check points
                // at the right directory.
                //
                // The launched config carries deps for at most one runtime
                // (UV xor Conda), and `effective_user_deps_from_launched`
                // gates strictly on that — so at most one flush happens per
                // eviction. We record which runtime flushed so the rename
                // step below uses the right hash function.
                let launched_snapshot = room_for_eviction
                    .runtime_agent_launched_config
                    .read()
                    .await
                    .clone();
                let mut flushed_runtime: Option<CapturedEnvRuntime> = None;
                let mut save_succeeded = false;
                if let Some(ref launched) = launched_snapshot {
                    let has_saved_path = room_for_eviction.path.read().await.is_some();
                    if has_saved_path {
                        for runtime in [CapturedEnvRuntime::Uv, CapturedEnvRuntime::Conda] {
                            if flush_launched_deps_to_metadata(
                                &room_for_eviction,
                                launched,
                                runtime,
                            )
                            .await
                            {
                                flushed_runtime = Some(runtime);
                            }
                        }
                        if flushed_runtime.is_some() {
                            info!(
                                "[notebook-sync] Flushed hot-sync deps into metadata for {}",
                                notebook_id_for_eviction
                            );
                            // Persist to disk now — the autosave debouncer
                            // has already fired for this eviction, and the
                            // daemon is about to tear the room down.
                            match save_notebook_to_disk(&room_for_eviction, None).await {
                                Ok(_) => save_succeeded = true,
                                Err(e) => warn!(
                                    "[notebook-sync] Failed to persist hot-sync deps to {}: {} — skipping env-dir rename",
                                    notebook_id_for_eviction, e
                                ),
                            }
                        }
                    }
                }

                // Rename the env dir to match the post-flush unified
                // hash so the next reopen's `unified_env_on_disk` lookup
                // finds it. Skip the rename when save failed — leaving
                // disk metadata on the old hash while the env moved to
                // the new one would defeat the next reopen. Kernel is
                // already dead at this point (runtime agent was shut
                // down above), so the rename is safe.
                if let Some(runtime) = flushed_runtime {
                    if save_succeeded {
                        let current = room_for_eviction
                            .runtime_agent_env_path
                            .read()
                            .await
                            .clone();
                        if let Some(current_path) = current {
                            let metadata_after = {
                                let doc = room_for_eviction.doc.read().await;
                                doc.get_metadata_snapshot()
                            };
                            let new_path = rename_env_dir_to_unified_hash(
                                &current_path,
                                metadata_after.as_ref(),
                                runtime,
                                &kernel_env::uv::default_cache_dir_uv(),
                                &kernel_env::conda::default_cache_dir_conda(),
                            )
                            .await;
                            if new_path != current_path {
                                let mut ep = room_for_eviction.runtime_agent_env_path.write().await;
                                *ep = Some(new_path);
                            }
                        }
                    }
                }

                // Clean up the environment directory on eviction — unless
                // the room holds a captured env bound to a saved .ipynb.
                //
                // Pool envs (`runtimed-{uv,conda,pixi}-*`) and captured envs
                // for untitled notebooks are orphaned once the room is gone:
                // pool envs were mutated with the notebook's deps and can't
                // be returned, and captured envs with no saved .ipynb have
                // no persistent `env_id` reference. Both delete eagerly.
                //
                // Captured envs for saved notebooks are the reopen cache.
                // Preserve them so the next daemon session's first open
                // hits `unified_env_on_disk` instead of rebuilding from the
                // pool. A future age-based GC sweeps envs whose notebook
                // hasn't been opened in a long time.
                //
                // Use pool_env_root() to normalise pixi paths — their
                // venv_path is nested (e.g. .pixi/envs/default) but we
                // operate on the top-level runtimed-pixi-* directory.
                {
                    let env_path = room_for_eviction
                        .runtime_agent_env_path
                        .read()
                        .await
                        .clone();
                    if let Some(ref path) = env_path {
                        let has_saved_path = room_for_eviction.path.read().await.is_some();
                        let metadata = {
                            let doc = room_for_eviction.doc.read().await;
                            doc.get_metadata_snapshot()
                        };
                        let preserve = should_preserve_env_on_eviction(
                            has_saved_path,
                            path,
                            metadata.as_ref(),
                            &kernel_env::uv::default_cache_dir_uv(),
                            &kernel_env::conda::default_cache_dir_conda(),
                        );
                        if preserve {
                            info!(
                                "[notebook-sync] Preserving captured env {:?} on eviction (saved notebook)",
                                path
                            );
                        } else {
                            let root = crate::paths::pool_env_root(path);
                            let cache_dir = crate::paths::default_cache_dir();
                            if !crate::is_within_cache_dir(&root, &cache_dir) {
                                warn!(
                                    "[notebook-sync] Refusing to delete env {:?} on eviction (not within cache dir)",
                                    root
                                );
                            } else if root.exists() {
                                info!(
                                    "[notebook-sync] Cleaning up env {:?} on room eviction",
                                    root
                                );
                                if let Err(e) = tokio::fs::remove_dir_all(&root).await {
                                    warn!(
                                        "[notebook-sync] Failed to clean up env {:?} on eviction: {}",
                                        root, e
                                    );
                                }
                            }
                        }
                    }
                }

                info!(
                    "[notebook-sync] Evicted room {} (idle timeout)",
                    notebook_id_for_eviction
                );
            }
        });
    } else {
        info!(
            "[notebook-sync] Client disconnected from room {} ({} peer{} remaining)",
            notebook_id,
            remaining,
            if remaining == 1 { "" } else { "s" }
        );
    }

    result
}

/// Sanitize a peer label from the wire.
///
/// - Strips zero-width and control characters (ZWJ, ZWNJ, ZWSP, etc.)
/// - Trims whitespace
/// - Clamps to 64 Unicode scalar values
/// - Falls back to `fallback` if empty/missing
pub(crate) fn sanitize_peer_label(raw: Option<&str>, fallback: &str) -> String {
    const MAX_LABEL_CHARS: usize = 64;

    fn is_allowed(c: char) -> bool {
        !c.is_control()
            && !matches!(
                c,
                '\u{200B}' // zero-width space
                | '\u{200C}' // zero-width non-joiner
                | '\u{200D}' // zero-width joiner
                | '\u{200E}' // left-to-right mark
                | '\u{200F}' // right-to-left mark
                | '\u{2060}' // word joiner
                | '\u{FEFF}' // BOM / zero-width no-break space
                | '\u{00AD}' // soft hyphen
                | '\u{034F}' // combining grapheme joiner
                | '\u{061C}' // arabic letter mark
                | '\u{115F}' // hangul choseong filler
                | '\u{1160}' // hangul jungseong filler
                | '\u{17B4}' // khmer vowel inherent aq
                | '\u{17B5}' // khmer vowel inherent aa
                | '\u{180E}' // mongolian vowel separator
            )
            && !('\u{2066}'..='\u{2069}').contains(&c) // bidi isolates
            && !('\u{202A}'..='\u{202E}').contains(&c) // bidi overrides
            && !('\u{FE00}'..='\u{FE0F}').contains(&c) // variation selectors
            && !('\u{E0100}'..='\u{E01EF}').contains(&c) // variation selectors supplement
    }

    match raw {
        Some(s) => {
            // Filter and take at most MAX_LABEL_CHARS in one pass — avoids
            // allocating proportional to attacker-controlled input size.
            let cleaned: String = s
                .trim()
                .chars()
                .filter(|c| is_allowed(*c))
                .take(MAX_LABEL_CHARS)
                .collect();
            let trimmed = cleaned.trim();
            if trimmed.is_empty() {
                fallback.to_string()
            } else {
                trimmed.to_string()
            }
        }
        None => fallback.to_string(),
    }
}

async fn send_session_status<W>(
    writer: &mut W,
    notebook_doc: notebook_protocol::protocol::NotebookDocPhaseWire,
    runtime_state: notebook_protocol::protocol::RuntimeStatePhaseWire,
    initial_load: notebook_protocol::protocol::InitialLoadPhaseWire,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    connection::send_typed_json_frame(
        writer,
        NotebookFrameType::SessionControl,
        &notebook_protocol::protocol::SessionControlMessage::SyncStatus(
            notebook_protocol::protocol::SessionSyncStatusWire {
                notebook_doc,
                runtime_state,
                initial_load,
            },
        ),
    )
    .await?;
    Ok(())
}

/// State carried from the initial notebook-doc sync into the steady-state loop.
///
/// See [`send_initial_notebook_doc_sync`]. `peer_state` tracks what the
/// daemon has already advertised about the notebook doc so subsequent
/// generate_sync_message calls compute correct deltas (including deltas
/// emitted by `streaming_load_cells`).
pub(crate) struct InitialSyncState {
    pub(crate) peer_state: sync::State,
}

impl InitialSyncState {
    fn new() -> Self {
        Self {
            peer_state: sync::State::new(),
        }
    }
}

/// Generate and send the initial notebook-doc AutomergeSync frame.
///
/// Returns the `peer_state` so the rest of bootstrap (and streaming load)
/// continues from the same baseline and emits correct deltas.
pub(crate) async fn send_initial_notebook_doc_sync<W>(
    writer: &mut W,
    room: &Arc<NotebookRoom>,
) -> anyhow::Result<InitialSyncState>
where
    W: AsyncWrite + Unpin,
{
    let mut sync_state = InitialSyncState::new();

    // Encode the sync message inside the lock, then send outside it to avoid
    // holding the write lock across async I/O.
    let initial_encoded = {
        let mut doc = room.doc.write().await;
        match catch_automerge_panic("initial-doc-sync", || {
            doc.generate_sync_message(&mut sync_state.peer_state)
                .map(|msg| msg.encode())
        }) {
            Ok(encoded) => encoded,
            Err(e) => {
                warn!("{}", e);
                sync_state.peer_state = sync::State::new();
                if doc.rebuild_from_save() {
                    doc.generate_sync_message(&mut sync_state.peer_state)
                        .map(|msg| msg.encode())
                } else {
                    // Cell-count guard prevented rebuild — skip sync message,
                    // fresh peer_state will trigger full re-sync on next exchange
                    None
                }
            }
        }
    };
    if let Some(encoded) = initial_encoded {
        connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &encoded).await?;
    }

    Ok(sync_state)
}

/// Typed frames sync loop with first-byte type indicator.
///
/// Handles both Automerge sync messages and NotebookRequest messages.
/// This protocol supports daemon-owned kernel execution (Phase 8).
#[allow(clippy::too_many_arguments)]
async fn run_sync_loop_v2<R, W>(
    reader: &mut R,
    writer: &mut W,
    room: &Arc<NotebookRoom>,
    _rooms: NotebookRooms,
    _notebook_id: String,
    daemon: std::sync::Arc<crate::daemon::Daemon>,
    needs_load: Option<&Path>,
    peer_id: &str,
    client_protocol_version: u8,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Subscribe before sending bootstrap traffic so any writes that land
    // during connection setup are still observed as steady-state deltas.
    let mut changed_rx = room.changed_tx.subscribe();
    let mut kernel_broadcast_rx = room.kernel_broadcast_tx.subscribe();
    let mut presence_rx = room.presence_tx.subscribe();
    let mut state_changed_rx = room.state_changed_tx.subscribe();

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
            writer,
            notebook_doc_phase,
            runtime_state_phase,
            initial_load_phase.clone(),
        )
        .await?;
    }

    let InitialSyncState { mut peer_state } = send_initial_notebook_doc_sync(writer, room).await?;
    notebook_doc_phase = notebook_protocol::protocol::NotebookDocPhaseWire::Syncing;
    if client_protocol_version >= 3 {
        send_session_status(
            writer,
            notebook_doc_phase,
            runtime_state_phase,
            initial_load_phase.clone(),
        )
        .await?;
    }

    let mut state_peer_state = sync::State::new();
    let mut pool_peer_state = sync::State::new();

    // Initial RuntimeStateDoc sync — encode inside lock, send outside.
    // Uses bounded generation to compact atomically if the message would exceed
    // the 100 MiB frame limit.
    let initial_state_encoded = {
        let mut state_doc = room.state_doc.write().await;
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
            Ok(encoded) => encoded,
            Err(e) => {
                warn!("{}", e);
                state_doc.rebuild_from_save();
                state_peer_state = sync::State::new();
                state_doc
                    .generate_sync_message(&mut state_peer_state)
                    .map(|msg| msg.encode())
            }
        }
    };
    if let Some(encoded) = initial_state_encoded {
        connection::send_typed_frame(writer, NotebookFrameType::RuntimeStateSync, &encoded).await?;
    }
    runtime_state_phase = notebook_protocol::protocol::RuntimeStatePhaseWire::Syncing;
    if client_protocol_version >= 3 {
        send_session_status(
            writer,
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
            match streaming_load_cells(reader, writer, room, load_path, &mut peer_state).await {
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
                            writer,
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
                    let _ = room.changed_tx.send(());
                    warn!(
                        "[notebook-sync] Streaming load failed for {}: {}",
                        load_path.display(),
                        e
                    );
                    if client_protocol_version >= 3 {
                        send_session_status(
                            writer,
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

    // Initial PoolDoc sync — global pool state
    let initial_pool_encoded = {
        let mut pool_doc = daemon.pool_doc.write().await;
        match catch_automerge_panic("initial-pool-sync", || {
            pool_doc
                .generate_sync_message(&mut pool_peer_state)
                .map(|msg| msg.encode())
        }) {
            Ok(encoded) => encoded,
            Err(e) => {
                warn!("{}", e);
                pool_doc.rebuild_from_save();
                pool_peer_state = sync::State::new();
                pool_doc
                    .generate_sync_message(&mut pool_peer_state)
                    .map(|msg| msg.encode())
            }
        }
    };
    if let Some(encoded) = initial_pool_encoded {
        connection::send_typed_frame(writer, NotebookFrameType::PoolStateSync, &encoded).await?;
    }

    // Phase 1.5 (removed): CommSync broadcast is no longer needed.
    // Late joiners receive widget state via RuntimeStateDoc CRDT sync,
    // and the frontend CRDT watcher synthesizes comm_open messages.

    // Phase 1.6: Send presence snapshot so late joiners see current peer state
    // (kernel status, cursors, selections from other connected peers).
    // The snapshot's peer_id field identifies the sender (daemon), not the receiver.
    // We filter out the receiver's own peer_id to prevent them from rendering
    // their own cursor as a remote peer (clients don't know their server-assigned ID).
    {
        let snapshot_bytes = {
            let presence_state = room.presence.read().await;
            if presence_state.peer_count() > 0 {
                // Build snapshot excluding this peer (they shouldn't see themselves)
                let other_peers: Vec<presence::PeerSnapshot> = presence_state
                    .peers()
                    .values()
                    .filter(|p| p.peer_id != peer_id)
                    .map(|p| presence::PeerSnapshot {
                        peer_id: p.peer_id.clone(),
                        peer_label: p.peer_label.clone(),
                        actor_label: p.actor_label.clone(),
                        channels: p.channels.values().cloned().collect(),
                    })
                    .collect();
                if !other_peers.is_empty() {
                    match presence::encode_snapshot("daemon", &other_peers) {
                        Ok(bytes) => Some(bytes),
                        Err(e) => {
                            warn!("[notebook-sync] Failed to encode presence snapshot: {}", e);
                            None
                        }
                    }
                } else {
                    None
                }
            } else {
                None
            }
        }; // presence read guard dropped
        if let Some(snapshot_bytes) = snapshot_bytes {
            connection::send_typed_frame(writer, NotebookFrameType::Presence, &snapshot_bytes)
                .await?;
        }
    }

    // Periodic pruning of stale presence peers (e.g. clients that silently dropped).
    let prune_period = std::time::Duration::from_millis(presence::DEFAULT_HEARTBEAT_MS);
    let mut prune_interval = tokio::time::interval(prune_period);
    prune_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Phase 2: Exchange messages until sync is complete, then watch for changes
    loop {
        tokio::select! {
            // Incoming message from this client
            result = connection::recv_typed_frame(reader) => {
                match result? {
                    Some(frame) => {
                        match frame.frame_type {
                            NotebookFrameType::AutomergeSync => {
                                // Handle Automerge sync message
                                let message = sync::Message::decode(&frame.payload)
                                    .map_err(|e| anyhow::anyhow!("decode error: {}", e))?;

                                // Complete all document mutations inside the lock, encode the
                                // reply, then release the lock before performing async I/O.
                                let (persist_bytes, reply_encoded, metadata_changed) = {
                                    let mut doc = room.doc.write().await;

                                    let heads_before = doc.get_heads();

                                    // Guard receive_sync_message against automerge panics
                                    let recv_result = catch_automerge_panic("doc-receive-sync", || {
                                        doc.receive_sync_message(&mut peer_state, message)
                                    });
                                    match recv_result {
                                        Ok(Ok(())) => {}
                                        Ok(Err(e)) => {
                                            warn!("[notebook-sync] receive_sync_message error: {}", e);
                                            continue;
                                        }
                                        Err(e) => {
                                            warn!("{}", e);
                                            doc.rebuild_from_save();
                                            peer_state = sync::State::new();
                                            continue;
                                        }
                                    }

                                    let heads_after = doc.get_heads();
                                    let metadata_changed = diff_metadata_touched(
                                        doc.doc_mut(),
                                        &heads_before,
                                        &heads_after,
                                    );

                                    let bytes = doc.save();

                                    // Notify other peers in this room
                                    let _ = room.changed_tx.send(());

                                    let encoded = match catch_automerge_panic("doc-sync-reply", || {
                                        doc.generate_sync_message(&mut peer_state)
                                            .map(|reply| reply.encode())
                                    }) {
                                        Ok(encoded) => encoded,
                                        Err(e) => {
                                            warn!("{}", e);
                                            peer_state = sync::State::new();
                                            if doc.rebuild_from_save() {
                                                doc.generate_sync_message(&mut peer_state)
                                                    .map(|reply| reply.encode())
                                            } else {
                                                None
                                            }
                                        }
                                    };

                                    (bytes, encoded, metadata_changed)
                                };

                                // Send reply outside the lock so other peers can
                                // acquire it while we wait on the socket.
                                if let Some(encoded) = reply_encoded {
                                    connection::send_typed_frame(
                                        writer,
                                        NotebookFrameType::AutomergeSync,
                                        &encoded,
                                    )
                                    .await?;
                                }

                                if notebook_doc_phase
                                    != notebook_protocol::protocol::NotebookDocPhaseWire::Interactive
                                {
                                    notebook_doc_phase =
                                        notebook_protocol::protocol::NotebookDocPhaseWire::Interactive;
                                    if client_protocol_version >= 3 {
                                        send_session_status(
                                            writer,
                                            notebook_doc_phase,
                                            runtime_state_phase,
                                            initial_load_phase.clone(),
                                        )
                                        .await?;
                                    }
                                }

                                // Send to debounced persistence task
                                if let Some(ref tx) = room.persist_tx {
                                    let _ = tx.send(Some(persist_bytes));
                                }

                                // Check if metadata changed and kernel is running - broadcast sync state
                                if metadata_changed {
                                    check_and_broadcast_sync_state(room).await;
                                }

                                // Re-verify trust from doc metadata (detects trust approval)
                                check_and_update_trust_state(room).await;

                                // Rebuild markdown asset refs after source sync.
                                process_markdown_assets(room).await;
                            }

                            NotebookFrameType::Request => {
                                // Decode the envelope, dispatch the inner request,
                                // echo the id on the response envelope so the caller
                                // can correlate multiple in-flight requests.
                                let envelope: notebook_protocol::protocol::NotebookRequestEnvelope =
                                    serde_json::from_slice(&frame.payload)?;
                                let response = handle_notebook_request(
                                    room,
                                    envelope.request,
                                    daemon.clone(),
                                )
                                .await;

                                // Promotion from untitled → file-backed is now handled
                                // entirely inside handle_notebook_request (SaveNotebook arm).
                                // Save path update is handled inside handle_notebook_request.

                                let reply = notebook_protocol::protocol::NotebookResponseEnvelope {
                                    id: envelope.id,
                                    response,
                                };
                                connection::send_typed_json_frame(
                                    writer,
                                    NotebookFrameType::Response,
                                    &reply,
                                )
                                .await?;
                            }

                            NotebookFrameType::Presence => {
                                // Client sent a presence update (cursor, selection, etc.)
                                // Reject oversized frames — presence data is small (~20-30 bytes).
                                if frame.payload.len() > presence::MAX_PRESENCE_FRAME_SIZE {
                                    warn!(
                                        "[notebook-sync] Oversized presence frame ({} bytes, max {}), dropping",
                                        frame.payload.len(),
                                        presence::MAX_PRESENCE_FRAME_SIZE
                                    );
                                    continue;
                                }

                                // Decode, update room state, relay to other peers.
                                let now_ms = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .unwrap_or_default()
                                    .as_millis() as u64;

                                match presence::decode_message(&frame.payload) {
                                    Ok(presence::PresenceMessage::Update { data, peer_label, actor_label, .. }) => {
                                        // Reject daemon-owned channels before updating shared state.
                                        // This prevents clients from spoofing kernel status.
                                        if matches!(data, presence::ChannelData::KernelState(_)) {
                                            warn!("[notebook-sync] Client tried to publish KernelState presence, ignoring");
                                        } else {
                                            let data_for_relay = data.clone();
                                            let actor_label_for_relay = actor_label.clone();
                                            // Sanitize peer_label: trim whitespace, clamp length,
                                            // treat empty as fallback. Prevents UI/memory issues
                                            // from malicious or buggy clients.
                                            let label = sanitize_peer_label(peer_label.as_deref(), peer_id);
                                            let sanitized_label = Some(label.clone());
                                            // Update the room's presence state (using our known peer_id,
                                            // not the one in the frame — clients don't know their peer_id).
                                            let is_new = room.presence.write().await.update_peer(
                                                peer_id,
                                                &label,
                                                actor_label.as_deref(),
                                                data,
                                                now_ms,
                                            );

                                            if is_new {
                                                // New peer — send snapshot of everyone else (excluding self)
                                                let other_peers: Vec<presence::PeerSnapshot> = room
                                                    .presence
                                                    .read()
                                                    .await
                                                    .peers()
                                                    .values()
                                                    .filter(|p| p.peer_id != peer_id)
                                                    .map(|p| presence::PeerSnapshot {
                                                        peer_id: p.peer_id.clone(),
                                                        peer_label: p.peer_label.clone(),
                                                        actor_label: p.actor_label.clone(),
                                                        channels: p.channels.values().cloned().collect(),
                                                    })
                                                    .collect();
                                                if !other_peers.is_empty() {
                                                    match presence::encode_snapshot(
                                                        "daemon",
                                                        &other_peers,
                                                    ) {
                                                        Ok(snapshot_bytes) => {
                                                            connection::send_typed_frame(
                                                                writer,
                                                                NotebookFrameType::Presence,
                                                                &snapshot_bytes,
                                                            )
                                                            .await?;
                                                        }
                                                        Err(e) => warn!(
                                                            "[notebook-sync] Failed to encode presence snapshot for new peer: {}",
                                                            e
                                                        ),
                                                    }
                                                }
                                            }

                                            // Re-encode with server-assigned peer_id and sanitized label
                                            if let Ok(bytes) = presence::encode_message(
                                                &presence::PresenceMessage::Update {
                                                    peer_id: peer_id.to_string(),
                                                    peer_label: sanitized_label,
                                                    actor_label: actor_label_for_relay,
                                                    data: data_for_relay,
                                                },
                                            ) {
                                                let _ = room.presence_tx.send((peer_id.to_string(), bytes));
                                            }
                                        }
                                    }
                                    Ok(presence::PresenceMessage::Heartbeat { .. }) => {
                                        room.presence.write().await.mark_seen(peer_id, now_ms);
                                    }
                                    Ok(presence::PresenceMessage::ClearChannel { channel, .. }) => {
                                        room.presence.write().await.clear_channel(peer_id, channel);
                                        match presence::encode_clear_channel(peer_id, channel) {
                                            Ok(bytes) => {
                                                let _ = room.presence_tx.send((peer_id.to_string(), bytes));
                                            }
                                            Err(e) => warn!(
                                                "[notebook-sync] Failed to encode clear_channel presence: {}",
                                                e
                                            ),
                                        }
                                    }
                                    Ok(_) => {
                                        // Snapshot/Left from a client — ignore
                                    }
                                    Err(e) => {
                                        warn!(
                                            "[notebook-sync] Failed to decode presence frame: {}",
                                            e
                                        );
                                    }
                                }
                            }

                            NotebookFrameType::RuntimeStateSync => {
                                // Client sync — accept changes (frontend may write
                                // to comms/*/state/* for widget state updates).
                                let message = sync::Message::decode(&frame.payload)
                                    .map_err(|e| anyhow::anyhow!("decode state sync: {}", e))?;
                                let reply_encoded = {
                                    let mut state_doc = room.state_doc.write().await;

                                    let recv_result = catch_automerge_panic("state-receive-sync", || {
                                        state_doc.receive_sync_message_with_changes(
                                            &mut state_peer_state,
                                            message,
                                        )
                                    });
                                    let had_changes = match recv_result {
                                        Ok(Ok(changed)) => changed,
                                        Ok(Err(e)) => {
                                            warn!("[notebook-sync] state receive_sync_message error: {}", e);
                                            continue;
                                        }
                                        Err(e) => {
                                            warn!("{}", e);
                                            state_doc.rebuild_from_save();
                                            state_peer_state = sync::State::new();
                                            continue;
                                        }
                                    };

                                    // If client sent changes, notify all peers.
                                    // Comm state forwarding to kernel: the runtime agent diffs
                                    // comm state before/after each RuntimeStateSync and
                                    // sends changed properties to the kernel via send_comm_update.
                                    if had_changes {
                                        let _ = room.state_changed_tx.send(());
                                    }

                                    match catch_automerge_panic("state-sync-reply", || {
                                        state_doc
                                            .generate_sync_message(&mut state_peer_state)
                                            .map(|msg| msg.encode())
                                    }) {
                                        Ok(encoded) => encoded,
                                        Err(e) => {
                                            warn!("{}", e);
                                            state_doc.rebuild_from_save();
                                            state_peer_state = sync::State::new();
                                            state_doc
                                                .generate_sync_message(&mut state_peer_state)
                                                .map(|msg| msg.encode())
                                        }
                                    }
                                };
                                if let Some(encoded) = reply_encoded {
                                    connection::send_typed_frame(
                                        writer,
                                        NotebookFrameType::RuntimeStateSync,
                                        &encoded,
                                    )
                                    .await?;
                                }

                                if runtime_state_phase
                                    != notebook_protocol::protocol::RuntimeStatePhaseWire::Ready
                                {
                                    runtime_state_phase =
                                        notebook_protocol::protocol::RuntimeStatePhaseWire::Ready;
                                    if client_protocol_version >= 3 {
                                        send_session_status(
                                            writer,
                                            notebook_doc_phase,
                                            runtime_state_phase,
                                            initial_load_phase.clone(),
                                        )
                                        .await?;
                                    }
                                }
                            }

                            NotebookFrameType::PoolStateSync => {
                                // Client's pool sync reply — apply with change stripping
                                let message = sync::Message::decode(&frame.payload)
                                    .map_err(|e| anyhow::anyhow!("decode pool sync: {}", e))?;
                                let reply_encoded = {
                                    let mut pool_doc = daemon.pool_doc.write().await;

                                    let recv_result = catch_automerge_panic("pool-receive-sync", || {
                                        pool_doc.receive_sync_message(
                                            &mut pool_peer_state,
                                            message,
                                        )
                                    });
                                    match recv_result {
                                        Ok(Ok(())) => {}
                                        Ok(Err(e)) => {
                                            warn!("[notebook-sync] pool receive_sync_message error: {}", e);
                                            continue;
                                        }
                                        Err(e) => {
                                            warn!("{}", e);
                                            pool_doc.rebuild_from_save();
                                            pool_peer_state = sync::State::new();
                                            continue;
                                        }
                                    }

                                    match catch_automerge_panic("pool-sync-reply", || {
                                        pool_doc
                                            .generate_sync_message(&mut pool_peer_state)
                                            .map(|msg| msg.encode())
                                    }) {
                                        Ok(encoded) => encoded,
                                        Err(e) => {
                                            warn!("{}", e);
                                            pool_doc.rebuild_from_save();
                                            pool_peer_state = sync::State::new();
                                            pool_doc
                                                .generate_sync_message(&mut pool_peer_state)
                                                .map(|msg| msg.encode())
                                        }
                                    }
                                };
                                if let Some(encoded) = reply_encoded {
                                    connection::send_typed_frame(
                                        writer,
                                        NotebookFrameType::PoolStateSync,
                                        &encoded,
                                    )
                                    .await?;
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
                    None => {
                        // Client disconnected
                        return Ok(());
                    }
                }
            }

            // Another peer changed the document — push update to this client
            _ = changed_rx.recv() => {
                // Encode inside the lock, send outside it to avoid holding the
                // write lock across async I/O.
                let encoded = {
                    let mut doc = room.doc.write().await;
                    match catch_automerge_panic("doc-broadcast", || {
                        doc.generate_sync_message(&mut peer_state)
                            .map(|msg| msg.encode())
                    }) {
                        Ok(encoded) => encoded,
                        Err(e) => {
                            warn!("{}", e);
                            peer_state = sync::State::new();
                            if doc.rebuild_from_save() {
                                doc.generate_sync_message(&mut peer_state)
                                    .map(|msg| msg.encode())
                            } else {
                                None
                            }
                        }
                    }
                };
                if let Some(encoded) = encoded {
                    connection::send_typed_frame(
                        writer,
                        NotebookFrameType::AutomergeSync,
                        &encoded,
                    )
                    .await?;
                }

                if matches!(
                    initial_load_phase,
                    notebook_protocol::protocol::InitialLoadPhaseWire::Streaming
                ) && !room
                    .is_loading
                    .load(std::sync::atomic::Ordering::Acquire)
                {
                    initial_load_phase =
                        notebook_protocol::protocol::InitialLoadPhaseWire::Ready;
                    if client_protocol_version >= 3 {
                        send_session_status(
                            writer,
                            notebook_doc_phase,
                            runtime_state_phase,
                            initial_load_phase.clone(),
                        )
                        .await?;
                    }
                }
            }

            // RuntimeStateDoc changed — push update to this client
            result = state_changed_rx.recv() => {
                match result {
                    Ok(()) => {
                        let encoded = {
                            let mut state_doc = room.state_doc.write().await;
                            match catch_automerge_panic("state-broadcast", || {
                                state_doc
                                    .generate_sync_message(&mut state_peer_state)
                                    .map(|msg| msg.encode())
                            }) {
                                Ok(encoded) => encoded,
                                Err(e) => {
                                    warn!("{}", e);
                                    state_doc.rebuild_from_save();
                                    state_peer_state = sync::State::new();
                                    state_doc
                                        .generate_sync_message(&mut state_peer_state)
                                        .map(|msg| msg.encode())
                                }
                            }
                        };
                        if let Some(encoded) = encoded {
                            connection::send_typed_frame(
                                writer,
                                NotebookFrameType::RuntimeStateSync,
                                &encoded,
                            )
                            .await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(
                            "[notebook-sync] Peer {} lagged {} runtime state updates",
                            peer_id, n
                        );
                        // Send a full sync to catch up
                        let encoded = {
                            let mut state_doc = room.state_doc.write().await;
                            match catch_automerge_panic("state-broadcast-lagged", || {
                                state_doc
                                    .generate_sync_message(&mut state_peer_state)
                                    .map(|msg| msg.encode())
                            }) {
                                Ok(encoded) => encoded,
                                Err(e) => {
                                    warn!("{}", e);
                                    state_doc.rebuild_from_save();
                                    state_peer_state = sync::State::new();
                                    state_doc
                                        .generate_sync_message(&mut state_peer_state)
                                        .map(|msg| msg.encode())
                                }
                            }
                        };
                        if let Some(encoded) = encoded {
                            connection::send_typed_frame(
                                writer,
                                NotebookFrameType::RuntimeStateSync,
                                &encoded,
                            )
                            .await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // State change channel closed — room is being evicted
                        return Ok(());
                    }
                }
            }

            // PoolDoc changed — push update to this client
            result = pool_changed_rx.recv() => {
                match result {
                    Ok(()) => {
                        let encoded = {
                            let mut pool_doc = daemon.pool_doc.write().await;
                            match catch_automerge_panic("pool-broadcast", || {
                                pool_doc
                                    .generate_sync_message(&mut pool_peer_state)
                                    .map(|msg| msg.encode())
                            }) {
                                Ok(encoded) => encoded,
                                Err(e) => {
                                    warn!("{}", e);
                                    pool_doc.rebuild_from_save();
                                    pool_peer_state = sync::State::new();
                                    pool_doc
                                        .generate_sync_message(&mut pool_peer_state)
                                        .map(|msg| msg.encode())
                                }
                            }
                        };
                        if let Some(encoded) = encoded {
                            connection::send_typed_frame(
                                writer,
                                NotebookFrameType::PoolStateSync,
                                &encoded,
                            )
                            .await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::debug!(
                            "[notebook-sync] Peer {} lagged {} pool state updates",
                            peer_id, n
                        );
                        let encoded = {
                            let mut pool_doc = daemon.pool_doc.write().await;
                            match catch_automerge_panic("pool-broadcast-lagged", || {
                                pool_doc
                                    .generate_sync_message(&mut pool_peer_state)
                                    .map(|msg| msg.encode())
                            }) {
                                Ok(encoded) => encoded,
                                Err(e) => {
                                    warn!("{}", e);
                                    pool_doc.rebuild_from_save();
                                    pool_peer_state = sync::State::new();
                                    pool_doc
                                        .generate_sync_message(&mut pool_peer_state)
                                        .map(|msg| msg.encode())
                                }
                            }
                        };
                        if let Some(encoded) = encoded {
                            connection::send_typed_frame(
                                writer,
                                NotebookFrameType::PoolStateSync,
                                &encoded,
                            )
                            .await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Pool doc channel closed — daemon is shutting down
                        return Ok(());
                    }
                }
            }

            // Presence update from another peer — forward to this client
            result = presence_rx.recv() => {
                match result {
                    Ok((ref sender_peer_id, ref bytes)) => {
                        // Don't echo back to the sender
                        if sender_peer_id != peer_id {
                            connection::send_typed_frame(
                                writer,
                                NotebookFrameType::Presence,
                                bytes,
                            )
                            .await?;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Missed some presence updates — send a full snapshot to catch up
                        tracing::debug!(
                            "[notebook-sync] Peer {} lagged {} presence updates, sending snapshot",
                            peer_id, n
                        );
                        match room.presence.read().await.encode_snapshot(peer_id) {
                            Ok(snapshot_bytes) => {
                                connection::send_typed_frame(
                                    writer,
                                    NotebookFrameType::Presence,
                                    &snapshot_bytes,
                                )
                                .await?;
                            }
                            Err(e) => warn!(
                                "[notebook-sync] Failed to encode lag-recovery snapshot: {}",
                                e
                            ),
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        // Presence channel closed — room is being evicted
                        return Ok(());
                    }
                }
            }

            // Kernel broadcast event — forward to this client
            result = kernel_broadcast_rx.recv() => {
                match result {
                    Ok(broadcast) => {
                        // Drop broadcasts that are redundant with RuntimeStateDoc
                        // (synced via frame 0x05). The daemon still emits these
                        // internally (e.g. ExecutionDone drives the command loop),
                        // but peers no longer need them — RuntimeStateDoc is the
                        // single source of truth for kernel status, queue, execution
                        // lifecycle, and env sync state.
                        if matches!(
                            &broadcast,
                            NotebookBroadcast::KernelStatus { .. }
                                | NotebookBroadcast::ExecutionStarted { .. }
                                | NotebookBroadcast::ExecutionDone { .. }
                                | NotebookBroadcast::QueueChanged { .. }
                                | NotebookBroadcast::EnvSyncState { .. }
                        ) {
                            // ExecutionDone previously triggered a doc sync flush
                            // to ensure outputs arrived before the signal. Now that
                            // the broadcast is dropped, the sync still happens via
                            // the RuntimeStateDoc update path — the daemon writes
                            // execution status to the RuntimeStateDoc *after*
                            // writing outputs to the notebook doc, so the
                            // frame-0x05 sync message is the new "outputs ready"
                            // signal for peers.
                            continue;
                        }

                        connection::send_typed_json_frame(
                            writer,
                            NotebookFrameType::Broadcast,
                            &broadcast,
                        )
                        .await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!(
                            "[notebook-sync] Peer lagged {} kernel broadcasts, sending doc sync to catch up",
                            n
                        );
                        // The peer missed some broadcasts (outputs, status changes).
                        // The Automerge doc contains the persisted state, so send a
                        // sync message to catch the peer up on any missed output data.
                        send_doc_sync(
                            room,
                            &mut peer_state,
                            writer,
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
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let mut presence_state = room.presence.write().await;
                presence_state.mark_seen(peer_id, now_ms);
                let pruned = presence_state.prune_stale(now_ms, presence::DEFAULT_PEER_TTL_MS);
                drop(presence_state);
                for pruned_peer_id in pruned {
                    match presence::encode_left(&pruned_peer_id) {
                        Ok(left_bytes) => {
                            let _ = room.presence_tx.send((pruned_peer_id, left_bytes));
                        }
                        Err(e) => warn!(
                            "[notebook-sync] Failed to encode 'left' for pruned peer: {}",
                            e
                        ),
                    }
                }
            }
        }
    }
}

/// Send a doc sync message to a peer if there are pending changes.
///
/// Generates an Automerge sync message from the room's doc and sends it
/// as a typed frame. Used before forwarding ExecutionDone (to ensure
/// outputs are synced) and after broadcast lag recovery.
async fn send_doc_sync<W: tokio::io::AsyncWrite + Unpin>(
    room: &NotebookRoom,
    peer_state: &mut automerge::sync::State,
    writer: &mut W,
) -> anyhow::Result<()> {
    let encoded = {
        let mut doc = room.doc.write().await;
        match catch_automerge_panic("broadcast-doc-changes", || {
            doc.generate_sync_message(peer_state)
                .map(|msg| msg.encode())
        }) {
            Ok(encoded) => encoded,
            Err(e) => {
                warn!("{}", e);
                doc.rebuild_from_save();
                *peer_state = sync::State::new();
                doc.generate_sync_message(peer_state)
                    .map(|msg| msg.encode())
            }
        }
    };
    if let Some(encoded) = encoded {
        connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &encoded).await?;
    }
    Ok(())
}
