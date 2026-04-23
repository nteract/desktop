//! `NotebookRequest::SetMetadataSnapshot` handler.

use crate::notebook_metadata::NotebookMetadataSnapshot;
use crate::notebook_sync_server::{
    check_and_broadcast_sync_state, check_and_update_trust_state, NotebookRoom,
};
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom, snapshot: String) -> NotebookResponse {
    match serde_json::from_str::<NotebookMetadataSnapshot>(&snapshot) {
        Ok(snap) => {
            // Scope the doc write guard so it drops before the async
            // sync/trust checks (deadlock prevention).
            let result = {
                let mut doc = room.doc.write().await;
                match doc.set_metadata_snapshot(&snap) {
                    Ok(()) => {
                        // Notify peers of the change
                        let _ = room.broadcasts.changed_tx.send(());
                        // Persist
                        if let Some(ref tx) = room.persist_tx {
                            let bytes = doc.save();
                            let _ = tx.send(Some(bytes));
                        }
                        Ok(())
                    }
                    Err(e) => Err(format!("Failed to set metadata snapshot: {e}")),
                }
            };
            match result {
                Ok(()) => {
                    // Check for env sync state and trust changes
                    check_and_broadcast_sync_state(room).await;
                    check_and_update_trust_state(room).await;
                    NotebookResponse::MetadataSet {}
                }
                Err(error) => NotebookResponse::Error { error },
            }
        }
        Err(e) => NotebookResponse::Error {
            error: format!("Failed to parse metadata snapshot: {e}"),
        },
    }
}
