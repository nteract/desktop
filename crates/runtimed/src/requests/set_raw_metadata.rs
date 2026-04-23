//! `NotebookRequest::SetRawMetadata` handler.

use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom, key: String, value: String) -> NotebookResponse {
    let mut doc = room.doc.write().await;
    match doc.set_metadata(&key, &value) {
        Ok(()) => {
            // Notify peers of the change
            let _ = room.broadcasts.changed_tx.send(());
            // Persist
            if let Some(ref d) = room.persistence.debouncer {
                let bytes = doc.save();
                let _ = d.persist_tx.send(Some(bytes));
            }
            NotebookResponse::MetadataSet {}
        }
        Err(e) => NotebookResponse::Error {
            error: format!("Failed to set metadata: {e}"),
        },
    }
}
