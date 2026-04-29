use std::sync::Arc;

use automerge::sync;
use tokio::io::AsyncWrite;
use tracing::{info, warn};

use crate::connection::{self, NotebookFrameType};

use super::{catch_automerge_panic, NotebookRoom, STATE_SYNC_COMPACT_THRESHOLD};

pub(crate) async fn send_session_status<W>(
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

/// Generate and send the initial RuntimeStateDoc sync frame.
///
/// The caller owns `state_peer_state` because the steady-state peer loop uses
/// the same sync state to compute later RuntimeStateDoc deltas.
pub(crate) async fn send_initial_runtime_state_sync<W>(
    writer: &mut W,
    room: &Arc<NotebookRoom>,
    state_peer_state: &mut sync::State,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    // Encode inside the RuntimeStateDoc lock, then send outside it to avoid
    // holding state while awaiting socket I/O.
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
                    state_peer_state,
                    STATE_SYNC_COMPACT_THRESHOLD,
                )
            }) {
                Ok(encoded) => Ok(encoded),
                Err(e) => {
                    warn!("{}", e);
                    state_doc.rebuild_from_save();
                    *state_peer_state = sync::State::new();
                    Ok(state_doc
                        .generate_sync_message(state_peer_state)
                        .map(|msg| msg.encode()))
                }
            }
        })
        .ok()
        .flatten();
    if let Some(encoded) = initial_state_encoded {
        connection::send_typed_frame(writer, NotebookFrameType::RuntimeStateSync, &encoded).await?;
    }

    Ok(())
}
