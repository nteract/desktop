//! `NotebookRequest::ClearOutputs` handler.

use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(room: &NotebookRoom, cell_ids: Vec<String>) -> NotebookResponse {
    // Clear outputs by nulling execution_id pointers on cells.
    // Outputs live in RuntimeStateDoc keyed by execution_id; cells without
    // an execution_id render as having no outputs. Old executions remain in
    // RuntimeStateDoc until natural trim.
    let persist_bytes = {
        let mut doc = room.doc.write().await;
        for cell_id in &cell_ids {
            let _ = doc.set_execution_id(cell_id, None);
        }
        let bytes = doc.save();
        let _ = room.broadcasts.changed_tx.send(());
        bytes
    };

    // Send to debounced persistence task
    if let Some(ref d) = room.persistence.debouncer {
        let _ = d.persist_tx.send(Some(persist_bytes));
    }

    NotebookResponse::OutputsCleared { cell_ids }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use uuid::Uuid;

    use crate::blob_store::BlobStore;
    use crate::notebook_sync_server::NotebookRoom;
    use crate::protocol::NotebookResponse;

    #[tokio::test]
    async fn clears_cell_execution_pointers_without_erasing_runtime_executions() {
        let tmp = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(BlobStore::new(tmp.path().join("blobs")));
        let room = NotebookRoom::new_fresh(Uuid::new_v4(), None, tmp.path(), blob_store, true);

        {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell-1", "code").unwrap();
            doc.add_cell(1, "cell-2", "code").unwrap();
            doc.set_execution_id("cell-1", Some("exec-1")).unwrap();
            doc.set_execution_id("cell-2", Some("exec-2")).unwrap();
        }

        room.state
            .with_doc(|state| {
                let output = serde_json::json!({
                    "output_type": "stream",
                    "output_id": "out-1",
                    "name": "stdout",
                    "text": {"inline": "hello"}
                });
                state.create_execution("exec-1", "cell-1")?;
                state.create_execution("exec-2", "cell-2")?;
                state.append_output("exec-1", &output)?;
                state.append_output("exec-2", &output)?;
                Ok(())
            })
            .unwrap();

        let response = super::handle(&room, vec!["cell-1".into(), "cell-2".into()]).await;
        assert!(matches!(
            response,
            NotebookResponse::OutputsCleared { cell_ids } if cell_ids == vec![
                "cell-1".to_string(),
                "cell-2".to_string()
            ]
        ));

        {
            let doc = room.doc.read().await;
            assert_eq!(doc.get_execution_id("cell-1"), None);
            assert_eq!(doc.get_execution_id("cell-2"), None);
        }

        room.state
            .with_doc(|state| {
                assert_eq!(state.get_outputs("exec-1").len(), 1);
                assert_eq!(state.get_outputs("exec-2").len(), 1);
                Ok(())
            })
            .unwrap();
    }
}
