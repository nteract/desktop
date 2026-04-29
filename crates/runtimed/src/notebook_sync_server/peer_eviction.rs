use std::sync::atomic::Ordering;
use std::sync::Arc;

use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use crate::task_supervisor::spawn_best_effort;

use super::{
    flush_launched_deps_to_metadata, rename_env_dir_to_unified_hash, save_notebook_to_disk,
    send_runtime_agent_request, should_preserve_env_on_eviction, shutdown_autosave_debouncer,
    CapturedEnvRuntime, NotebookRoom, NotebookRooms,
};

pub(super) async fn handle_peer_disconnect(
    room: &Arc<NotebookRoom>,
    rooms: NotebookRooms,
    notebook_id: &str,
    peer_id: &str,
    daemon: &Arc<crate::daemon::Daemon>,
) {
    // Peer disconnected — decrement and possibly evict the room.
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
        let notebook_id_for_eviction = notebook_id.to_string();

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

                const AUTOSAVE_SHUTDOWN_TIMEOUT: std::time::Duration =
                    std::time::Duration::from_secs(5);
                let autosave_shutdown_ok = shutdown_autosave_debouncer(
                    &room_for_eviction,
                    &notebook_id_for_eviction,
                    AUTOSAVE_SHUTDOWN_TIMEOUT,
                )
                .await;
                if !autosave_shutdown_ok {
                    warn!(
                        "[notebook-sync] Autosave shutdown did not complete final .ipynb save for {}; continuing eviction after .automerge flush",
                        notebook_id_for_eviction
                    );
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
}
