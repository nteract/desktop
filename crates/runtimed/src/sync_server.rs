//! Automerge sync protocol handler for settings synchronization.
//!
//! Handles a single client connection that has already been routed by the
//! daemon's unified socket. Exchanges Automerge sync messages to keep a
//! shared settings document in sync across all notebook windows.

use std::sync::Arc;

use automerge::sync;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, RwLock};
use tracing::{info, warn};

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
