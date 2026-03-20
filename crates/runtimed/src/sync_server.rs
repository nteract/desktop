//! Automerge sync protocol handlers for global documents.
//!
//! Handles client connections that have already been routed by the daemon's
//! unified socket. Exchanges Automerge sync messages to keep shared documents
//! in sync across all notebook windows.
//!
//! Two handlers:
//! - `handle_settings_sync_connection` — bidirectional sync for user settings
//! - `handle_pool_sync_connection` — daemon-authoritative sync for pool state

use std::sync::Arc;

use automerge::sync;
use log::{info, warn};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, RwLock};

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
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut peer_state = sync::State::new();
    info!("[sync] New client connected, starting initial sync");

    // Phase 1: Initial sync -- server sends first
    {
        let mut doc = settings.write().await;
        if let Some(msg) = doc.generate_sync_message(&mut peer_state) {
            connection::send_frame(&mut writer, &msg.encode()).await?;
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
                        doc.receive_sync_message(&mut peer_state, message)?;

                        // Persist and notify others
                        persist_settings(&mut doc);
                        let _ = changed_tx.send(());

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

/// Handle a pool state sync connection (daemon-authoritative, read-only for clients).
///
/// Same Automerge sync protocol as settings, but the daemon is the sole writer.
/// Client sync messages are processed (for protocol handshake / ACKs) but any
/// document mutations from the client are effectively no-ops — the daemon's
/// writes always win because the client has no meaningful changes to contribute.
pub async fn handle_pool_sync_connection<R, W>(
    mut reader: R,
    mut writer: W,
    pool_doc: Arc<RwLock<crate::pool_doc::PoolDoc>>,
    mut changed_rx: broadcast::Receiver<()>,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut peer_state = sync::State::new();
    info!("[sync] Pool state client connected");

    // Phase 1: Initial sync — server sends first
    let initial_data = {
        let mut doc = pool_doc.write().await;
        doc.generate_sync_message(&mut peer_state)
            .map(|msg| msg.encode())
    };
    if let Some(data) = initial_data {
        connection::send_frame(&mut writer, &data).await?;
    }

    // Phase 2: Exchange messages until sync is complete, then push on changes
    loop {
        tokio::select! {
            // Incoming message from client (protocol ACKs)
            result = connection::recv_frame(&mut reader) => {
                match result? {
                    Some(data) => {
                        let message = sync::Message::decode(&data)
                            .map_err(|e| anyhow::anyhow!("decode error: {}", e))?;

                        let reply_data = {
                            let mut doc = pool_doc.write().await;
                            // Process for sync protocol state, but client can't
                            // meaningfully mutate pool state — daemon is authoritative.
                            doc.receive_sync_message(&mut peer_state, message)?;
                            doc.generate_sync_message(&mut peer_state)
                                .map(|msg| msg.encode())
                        };
                        // Send our response (completes handshake) — lock released
                        if let Some(data) = reply_data {
                            connection::send_frame(&mut writer, &data).await?;
                        }
                    }
                    None => {
                        // Client disconnected
                        info!("[sync] Pool state client disconnected");
                        return Ok(());
                    }
                }
            }

            // Pool state changed — push update to this client
            recv_result = changed_rx.recv() => {
                match recv_result {
                    Ok(()) => {}
                    Err(broadcast::error::RecvError::Closed) => {
                        info!("[sync] Pool state broadcast channel closed, disconnecting");
                        return Ok(());
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        warn!("[sync] Pool state receiver lagged by {} messages", n);
                    }
                }
                let push_data = {
                    let mut doc = pool_doc.write().await;
                    doc.generate_sync_message(&mut peer_state).map(|msg| msg.encode())
                };
                if let Some(data) = push_data {
                    connection::send_frame(&mut writer, &data).await?;
                }
            }
        }
    }
}

/// Persist the settings document to disk (both Automerge binary and JSON mirror).
fn persist_settings(doc: &mut SettingsDoc) {
    let automerge_path = crate::default_settings_doc_path();
    let json_path = crate::settings_json_path();

    if let Err(e) = doc.save_to_file(&automerge_path) {
        warn!("[sync] Failed to save Automerge doc: {}", e);
    }
    if let Err(e) = doc.save_json_mirror(&json_path) {
        warn!("[sync] Failed to write JSON mirror: {}", e);
    }
}
