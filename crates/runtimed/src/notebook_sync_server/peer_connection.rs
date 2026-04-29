use super::peer_eviction::handle_peer_disconnect;
use super::peer_loop::run_sync_loop_v2;
use super::peer_presence::cleanup_presence_on_disconnect;
use super::*;
use runtime_doc::RuntimeLifecycle;

/// Handle a single notebook sync client connection.
///
/// The caller has already consumed the handshake frame and resolved the room.
/// This function owns connection setup, initial metadata/trust seeding,
/// capability negotiation, optional autolaunch, and disconnect cleanup. The
/// steady-state typed-frame select loop lives in `peer_loop`.
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
            && (path_snapshot.as_ref().is_some_and(|p| p.exists())
                || is_new_notebook
                || created_new_at_path);

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
