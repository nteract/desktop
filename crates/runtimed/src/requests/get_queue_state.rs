//! Compatibility handler for `NotebookRequest::GetQueueState`.
//!
//! Current clients should read `RuntimeStateDoc` directly. This remains on the
//! wire for older SDK clients until the next major protocol cleanup.

use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::{NotebookResponse, QueueEntry};

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    let state = room.state.read(|sd| sd.read_state()).unwrap_or_default();
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
