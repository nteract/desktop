//! `NotebookRequest::GetQueueState` handler.

use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::{NotebookResponse, QueueEntry};

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    // Read from RuntimeStateDoc (source of truth for runtime agent)
    let sd = room.state_doc.read().await;
    let state = sd.read_state();
    NotebookResponse::QueueState {
        executing: state.queue.executing.map(|e| QueueEntry {
            cell_id: e.cell_id,
            execution_id: e.execution_id,
        }),
        queued: state
            .queue
            .queued
            .into_iter()
            .map(|e| QueueEntry {
                cell_id: e.cell_id,
                execution_id: e.execution_id,
            })
            .collect(),
    }
}
