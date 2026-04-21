//! `NotebookRequest::ClearOutputs` handler.

use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::{NotebookBroadcast, NotebookResponse};

pub(crate) async fn handle(room: &NotebookRoom, cell_id: String) -> NotebookResponse {
    // Clear outputs by nulling the execution_id pointer on the cell.
    // Outputs live in RuntimeStateDoc keyed by execution_id — with no
    // execution_id, the frontend sees no outputs. The old execution's
    // outputs remain in RuntimeStateDoc until natural trim.
    let old_eid = {
        let doc = room.doc.read().await;
        doc.get_execution_id(&cell_id)
    };

    let persist_bytes = {
        let mut doc = room.doc.write().await;
        let _ = doc.set_execution_id(&cell_id, None);
        let bytes = doc.save();
        let _ = room.changed_tx.send(());
        bytes
    };

    // Optionally clean up the old execution's outputs in RuntimeStateDoc
    if let Some(ref eid) = old_eid {
        let mut sd = room.state_doc.write().await;
        let _ = sd.clear_execution_outputs(eid);
        let _ = room.state_changed_tx.send(());
    }

    // Send to debounced persistence task
    if let Some(ref tx) = room.persist_tx {
        let _ = tx.send(Some(persist_bytes));
    }

    // Broadcast for cross-window UI sync (fast path)
    let _ = room
        .kernel_broadcast_tx
        .send(NotebookBroadcast::OutputsCleared {
            cell_id: cell_id.clone(),
        });

    NotebookResponse::OutputsCleared { cell_id }
}
