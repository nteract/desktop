use automerge::sync;
use tracing::warn;

use crate::connection::NotebookFrameType;

use super::catch_automerge_panic;
use super::peer_writer::PeerWriter;
use super::{
    check_and_broadcast_sync_state, check_and_update_trust_state, process_markdown_assets,
    NotebookRoom,
};
use notebook_doc::diff::diff_metadata_touched;

pub(super) async fn handle_notebook_doc_frame(
    room: &NotebookRoom,
    peer_state: &mut sync::State,
    writer: &PeerWriter,
    payload: &[u8],
) -> anyhow::Result<NotebookDocFrameOutcome> {
    let message =
        sync::Message::decode(payload).map_err(|e| anyhow::anyhow!("decode error: {}", e))?;

    // Complete all document mutations inside the lock, encode the reply, then
    // release the lock before performing async I/O.
    let (persist_bytes, reply_encoded, metadata_changed) = {
        let mut doc = room.doc.write().await;

        let heads_before = doc.get_heads();

        let recv_result = catch_automerge_panic("doc-receive-sync", || {
            doc.receive_sync_message(peer_state, message)
        });
        match recv_result {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                warn!("[notebook-sync] receive_sync_message error: {}", e);
                return Ok(NotebookDocFrameOutcome::Skipped);
            }
            Err(e) => {
                warn!("{}", e);
                doc.rebuild_from_save();
                *peer_state = sync::State::new();
                return Ok(NotebookDocFrameOutcome::Skipped);
            }
        }

        let heads_after = doc.get_heads();
        let metadata_changed = diff_metadata_touched(doc.doc_mut(), &heads_before, &heads_after);

        let bytes = doc.save();

        // Notify other peers in this room.
        let _ = room.broadcasts.changed_tx.send(());

        let encoded = match catch_automerge_panic("doc-sync-reply", || {
            doc.generate_sync_message(peer_state)
                .map(|reply| reply.encode())
        }) {
            Ok(encoded) => encoded,
            Err(e) => {
                warn!("{}", e);
                *peer_state = sync::State::new();
                if doc.rebuild_from_save() {
                    doc.generate_sync_message(peer_state)
                        .map(|reply| reply.encode())
                } else {
                    None
                }
            }
        };

        (bytes, encoded, metadata_changed)
    };

    // Queue the reply outside the lock so other peers can acquire it while the
    // writer task drains the socket.
    if let Some(encoded) = reply_encoded {
        writer.send_frame(NotebookFrameType::AutomergeSync, encoded)?;
    }

    Ok(NotebookDocFrameOutcome::Applied(NotebookDocSideEffects {
        persist_bytes,
        metadata_changed,
    }))
}

pub(super) enum NotebookDocFrameOutcome {
    Applied(NotebookDocSideEffects),
    Skipped,
}

pub(super) struct NotebookDocSideEffects {
    persist_bytes: Vec<u8>,
    metadata_changed: bool,
}

pub(super) async fn finish_notebook_doc_frame(
    room: &NotebookRoom,
    effects: NotebookDocSideEffects,
) {
    if let Some(ref d) = room.persistence.debouncer {
        let _ = d.persist_tx.send(Some(effects.persist_bytes));
    }

    if effects.metadata_changed {
        check_and_broadcast_sync_state(room).await;
    }

    check_and_update_trust_state(room).await;
    process_markdown_assets(room).await;
}

pub(super) async fn forward_notebook_doc_broadcast(
    room: &NotebookRoom,
    peer_state: &mut sync::State,
    writer: &PeerWriter,
) -> anyhow::Result<()> {
    let encoded = {
        let mut doc = room.doc.write().await;
        match catch_automerge_panic("doc-broadcast", || {
            doc.generate_sync_message(peer_state)
                .map(|msg| msg.encode())
        }) {
            Ok(encoded) => encoded,
            Err(e) => {
                warn!("{}", e);
                *peer_state = sync::State::new();
                if doc.rebuild_from_save() {
                    doc.generate_sync_message(peer_state)
                        .map(|msg| msg.encode())
                } else {
                    None
                }
            }
        }
    };
    if let Some(encoded) = encoded {
        writer.send_frame(NotebookFrameType::AutomergeSync, encoded)?;
    }
    Ok(())
}

/// Queue a doc sync message to a peer if there are pending changes.
///
/// Generates an Automerge sync message from the room's doc and hands it to the
/// ordered peer writer after broadcast lag recovery.
pub(super) async fn queue_doc_sync(
    room: &NotebookRoom,
    peer_state: &mut sync::State,
    writer: &PeerWriter,
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
        writer.send_frame(NotebookFrameType::AutomergeSync, encoded)?;
    }
    Ok(())
}
