use std::sync::Arc;

use automerge::sync;
use tokio::io::AsyncWrite;
use tracing::warn;

use crate::connection::{self, NotebookFrameType};

use super::{catch_automerge_panic, NotebookRoom};

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
