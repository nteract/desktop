//! Automerge sync protocol handler for settings synchronization.
//!
//! Handles a single client connection that has already been routed by the
//! daemon's unified socket. Exchanges Automerge sync messages to keep a
//! shared settings document in sync across all notebook windows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use automerge::sync;
use notebook_protocol::connection::{SettingsRpcClientMessage, SettingsRpcServerMessage};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, info, warn};

use crate::connection;
use crate::settings_doc::SettingsDoc;

/// Check if an error is just a normal connection close.
pub(crate) fn is_connection_closed(e: &anyhow::Error) -> bool {
    if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
        matches!(
            io_err.kind(),
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::NotConnected
        )
    } else {
        false
    }
}

/// Handle a single settings sync client connection.
///
/// The caller has already consumed the handshake frame. This function
/// runs the Automerge sync protocol:
/// 1. Initial sync: exchange messages until both sides converge
/// 2. Watch loop: wait for changes (from other peers or from this client),
///    exchange sync messages to propagate
pub async fn handle_settings_sync_connection<R, W>(
    mut reader: R,
    mut writer: W,
    settings: Arc<RwLock<SettingsDoc>>,
    changed_tx: broadcast::Sender<()>,
    mut changed_rx: broadcast::Receiver<()>,
    automerge_path: PathBuf,
    json_path: PathBuf,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut peer_state = sync::State::new();
    info!("[sync] New client connected, starting initial sync");

    // Phase 1: Initial sync -- server sends first
    {
        let encoded = {
            let mut doc = settings.write().await;
            doc.generate_sync_message(&mut peer_state)
                .map(|msg| msg.encode())
        };
        if let Some(data) = encoded {
            connection::send_frame(&mut writer, &data).await?;
        }
    }

    // Phase 2: Exchange messages until sync is complete, then watch for changes
    loop {
        tokio::select! {
            // Incoming message from this client
            result = connection::recv_frame(&mut reader) => {
                match result? {
                    Some(data) => {
                        let message = sync::Message::decode(&data)
                            .map_err(|e| anyhow::anyhow!("decode error: {}", e))?;

                        let mut doc = settings.write().await;
                        // Compare heads before/after so pure acks or duplicate
                        // messages don't fire `settings_changed`. Without this
                        // the pool warming loops wake up on every sync-protocol
                        // round-trip, which thrashes the pools when several
                        // per-`invoke` clients land back-to-back (#2120).
                        let before = doc.heads();
                        doc.receive_sync_message(&mut peer_state, message)?;
                        let after = doc.heads();
                        let doc_changed = before != after;

                        if doc_changed {
                            persist_settings(&mut doc, &automerge_path, &json_path);
                            let _ = changed_tx.send(());
                        }

                        // Send our response
                        if let Some(reply) = doc.generate_sync_message(&mut peer_state) {
                            connection::send_frame(&mut writer, &reply.encode()).await?;
                        }
                    }
                    None => {
                        // Client disconnected
                        return Ok(());
                    }
                }
            }

            // Another peer changed settings -- push update to this client
            _ = changed_rx.recv() => {
                let mut doc = settings.write().await;
                if let Some(msg) = doc.generate_sync_message(&mut peer_state) {
                    connection::send_frame(&mut writer, &msg.encode()).await?;
                }
            }
        }
    }
}

/// Persist the settings document to disk (both Automerge binary and JSON mirror).
///
/// The Automerge sync path treats persist failures as best-effort warnings
/// (the in-memory CRDT is still authoritative; the next change retries).
/// The RPC path needs the failure surfaced to the writing client, so it
/// uses `try_persist_settings` instead.
fn persist_settings(doc: &mut SettingsDoc, automerge_path: &Path, json_path: &Path) {
    if let Err(e) = doc.save_to_file(automerge_path) {
        warn!("[sync] Failed to save Automerge doc: {}", e);
    }
    if let Err(e) = doc.save_json_mirror(json_path) {
        warn!("[sync] Failed to write JSON mirror: {}", e);
    }
}

/// Persist the settings document, surfacing the first failure as `Err`.
///
/// Used by the RPC `SetSetting` path so the ack can carry `ok: false`
/// when the on-disk write fails. The in-memory doc is left as the
/// caller wrote it; rollback is the caller's call. We still attempt
/// both writes so a partially recoverable state (Automerge ok, JSON
/// mirror failed) doesn't silently leave only one side persisted.
fn try_persist_settings(
    doc: &mut SettingsDoc,
    automerge_path: &Path,
    json_path: &Path,
) -> Result<(), String> {
    let mut first_error: Option<String> = None;
    if let Err(e) = doc.save_to_file(automerge_path) {
        let msg = format!("save Automerge doc: {e}");
        warn!("[settings-rpc] {msg}");
        first_error.get_or_insert(msg);
    }
    if let Err(e) = doc.save_json_mirror(json_path) {
        let msg = format!("write JSON mirror: {e}");
        warn!("[settings-rpc] {msg}");
        first_error.get_or_insert(msg);
    }
    match first_error {
        None => Ok(()),
        Some(msg) => Err(msg),
    }
}

/// Build a `Snapshot` server message from the current `SettingsDoc`.
fn build_snapshot_message(doc: &SettingsDoc) -> anyhow::Result<SettingsRpcServerMessage> {
    let snapshot = doc.get_all();
    let value = serde_json::to_value(&snapshot)?;
    Ok(SettingsRpcServerMessage::Snapshot { settings: value })
}

/// Handle a single `Handshake::SettingsRpc` client.
///
/// Prototype channel for nteract/desktop#1598. Runs alongside the existing
/// Automerge `SettingsSync` handler against the same `SettingsDoc` and the
/// same `settings_changed` broadcast. The Automerge path is the source of
/// truth for now; this channel is opt-in and additive.
///
/// Wire shape:
/// 1. On connect: server sends one `Snapshot`.
/// 2. Loop: select between client `SetSetting` requests and `settings_changed`
///    broadcast ticks. Each `SetSetting` is applied via
///    `SettingsDoc::put_value`, persisted, broadcast, and acked. Each
///    broadcast tick causes a fresh `Snapshot` to go out.
pub async fn handle_settings_rpc_connection<R, W>(
    mut reader: R,
    mut writer: W,
    settings: Arc<RwLock<SettingsDoc>>,
    changed_tx: broadcast::Sender<()>,
    mut changed_rx: broadcast::Receiver<()>,
    automerge_path: PathBuf,
    json_path: PathBuf,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    info!("[settings-rpc] New client connected");

    // Initial snapshot. Build with a short read lock so we don't hold the
    // guard across the socket write.
    let initial = {
        let doc = settings.read().await;
        build_snapshot_message(&doc)?
    };
    connection::send_json_frame(&mut writer, &initial).await?;

    loop {
        tokio::select! {
            // Inbound client message.
            result = connection::recv_json_frame::<_, SettingsRpcClientMessage>(&mut reader) => {
                match result? {
                    Some(SettingsRpcClientMessage::SetSetting { key, value }) => {
                        debug!("[settings-rpc] SetSetting key={key} value={value}");

                        // Apply + persist + build the post-write snapshot
                        // under a single write-lock scope. Don't hold the
                        // RwLock guard across `.await`. Compare heads so
                        // no-op writes (same value, unsupported value
                        // shape silently ignored by `put_value`) don't
                        // wake the `settings_changed` subscribers — the
                        // existing Automerge handler enforces the same
                        // invariant against pool-warming churn (#2120).
                        type ApplyOk = (SettingsRpcServerMessage, bool);
                        let apply_result: Result<ApplyOk, String> = {
                            let mut doc = settings.write().await;
                            let before = doc.heads();
                            doc.put_value(&key, &value);
                            let after = doc.heads();
                            let doc_changed = before != after;

                            let persist_result = if doc_changed {
                                try_persist_settings(&mut doc, &automerge_path, &json_path)
                            } else {
                                Ok(())
                            };
                            persist_result.and_then(|()| {
                                build_snapshot_message(&doc)
                                    .map(|snapshot| (snapshot, doc_changed))
                                    .map_err(|e| e.to_string())
                            })
                        };

                        match apply_result {
                            Ok((snapshot, doc_changed)) => {
                                // Always echo the post-write snapshot to the
                                // writer so set-and-read patterns see a
                                // consistent view, even on no-op writes.
                                connection::send_json_frame(&mut writer, &snapshot).await?;
                                if doc_changed {
                                    // Fan out to peers; our own broadcast
                                    // tick will fire on the next select
                                    // iteration and resend the same
                                    // snapshot — harmless, the client
                                    // treats a duplicate snapshot as a
                                    // no-op refresh.
                                    let _ = changed_tx.send(());
                                }
                                let ack = SettingsRpcServerMessage::SetSettingAck {
                                    ok: true,
                                    error: None,
                                };
                                connection::send_json_frame(&mut writer, &ack).await?;
                            }
                            Err(e) => {
                                let ack = SettingsRpcServerMessage::SetSettingAck {
                                    ok: false,
                                    error: Some(e),
                                };
                                connection::send_json_frame(&mut writer, &ack).await?;
                            }
                        }
                    }
                    None => {
                        info!("[settings-rpc] Client disconnected");
                        return Ok(());
                    }
                }
            }

            // Settings changed elsewhere (this client's own write, the
            // Automerge sync handler, or the `settings.json` watcher).
            // Push a fresh snapshot.
            tick = changed_rx.recv() => {
                match tick {
                    Ok(()) => {
                        let snapshot = {
                            let doc = settings.read().await;
                            build_snapshot_message(&doc)?
                        };
                        connection::send_json_frame(&mut writer, &snapshot).await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Broadcast queue overflowed. Resync with current
                        // state instead of giving up.
                        debug!("[settings-rpc] broadcast lagged by {n}, resyncing");
                        let snapshot = {
                            let doc = settings.read().await;
                            build_snapshot_message(&doc)?
                        };
                        connection::send_json_frame(&mut writer, &snapshot).await?;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Ok(());
                    }
                }
            }
        }
    }
}
