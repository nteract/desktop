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

    // Peer disconnected — decrement and possibly evict the room
    let remaining = room
        .connections
        .active_peers
        .fetch_sub(1, Ordering::Relaxed)
        - 1;
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
            "[notebook-sync] All peers disconnected from room {} (uuid={}, peer_id={}), scheduling eviction check in {:?}",
            notebook_id,
            room.id,
            peer_id,
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
                if room_for_eviction
                    .connections
                    .active_peers
                    .load(Ordering::Relaxed)
                    > 0
                {
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
                if let Some(ref d) = room_for_eviction.persistence.debouncer {
                    let (ack_tx, ack_rx) = oneshot::channel::<bool>();
                    if d.flush_request_tx.send(ack_tx).is_ok() {
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
                if flush_retries > 0 {
                    info!(
                        "[notebook-sync] Eviction flush succeeded for {} after {} retr{}",
                        notebook_id_for_eviction,
                        flush_retries,
                        if flush_retries == 1 { "y" } else { "ies" }
                    );
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
                if room_for_eviction
                    .connections
                    .active_peers
                    .load(Ordering::Relaxed)
                    == 0
                {
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
            // Use remove_by_uuid rather than reading room.identity.path — a concurrent writer
            // A concurrent save-path-update could hold room.identity.path.write() and a
            // try_read() would silently return None, leaking the path_index entry.
            if should_teardown {
                if let Some(uuid) = evicted_uuid {
                    path_index_for_eviction.lock().await.remove_by_uuid(uuid);
                }
            }

            if should_teardown {
                info!(
                    "[notebook-sync] Eviction teardown starting for {} (uuid={:?})",
                    notebook_id_for_eviction, evicted_uuid
                );
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
                        // Drop the handle so it tears down the runtime-agent ownership group
                        // and removes the matching manifest only after cleanup succeeds.
                        {
                            let mut guard = room_for_eviction.runtime_agent_handle.lock().await;
                            *guard = None;
                        }
                        {
                            let mut tx = room_for_eviction.runtime_agent_request_tx.lock().await;
                            *tx = None;
                        }
                    }
                }

                // Stop file watcher if running. `watcher_shutdown_tx` is
                // always present on `RoomPersistence`, but the Option inside
                // is None until a watcher is actually spawned.
                if let Some(shutdown_tx) = room_for_eviction
                    .persistence
                    .watcher_shutdown_tx
                    .lock()
                    .await
                    .take()
                {
                    let _ = shutdown_tx.send(());
                    debug!(
                        "[notebook-sync] Stopped file watcher for {}",
                        notebook_id_for_eviction
                    );
                }

                // Stop the project-file watcher if one is armed. Armed only
                // when `refresh_project_context` actually found a project
                // file to watch; untitled / bare-dir notebooks leave it
                // unset.
                if let Some(shutdown_tx) = room_for_eviction
                    .persistence
                    .project_file_watcher_shutdown_tx
                    .lock()
                    .await
                    .take()
                {
                    let _ = shutdown_tx.send(());
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
                    let has_saved_path = room_for_eviction.identity.path.read().await.is_some();
                    let env_source = room_for_eviction
                        .state
                        .read(|sd| sd.read_state().kernel.env_source.clone())
                        .unwrap_or_default();
                    let project_backed = matches!(
                        env_source.as_str(),
                        "pixi:toml" | "uv:pyproject" | "conda:env_yml"
                    );
                    if has_saved_path && !project_backed {
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
                    } else if project_backed {
                        debug!(
                            "[notebook-sync] Skipping launched dep metadata flush for project-backed env {}",
                            env_source
                        );
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
                        let has_saved_path = room_for_eviction.identity.path.read().await.is_some();
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
                    "[notebook-sync] Eviction teardown finished for {} (idle timeout)",
                    notebook_id_for_eviction
                );
            }
        });
    } else {
        info!(
            "[notebook-sync] Client disconnected from room {} (uuid={}, peer_id={}): {} peer{} remaining",
            notebook_id,
            room.id,
            peer_id,
            remaining,
            if remaining == 1 { "" } else { "s" }
        );
    }

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
