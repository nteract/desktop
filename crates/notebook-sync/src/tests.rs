//! Unit tests for notebook-sync.
//!
//! These tests verify the DocHandle pattern without a daemon connection.
//! They exercise the shared state, snapshot publishing, and with_doc closure API.

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use automerge::AutoCommit;
    use tokio::sync::{mpsc, watch};

    use crate::handle::DocHandle;
    use crate::shared::SharedDocState;
    use crate::snapshot::NotebookSnapshot;

    /// Create a DocHandle wired up with channels but no sync task.
    /// Good for testing the handle's local behavior in isolation.
    fn test_handle() -> (
        DocHandle,
        mpsc::UnboundedReceiver<()>,
        mpsc::Receiver<crate::sync_task::SyncCommand>,
    ) {
        // Use NotebookDoc::new() to get a properly initialized doc with schema
        let nd = notebook_doc::NotebookDoc::new("test-notebook");
        let doc = nd.into_inner();
        let shared = Arc::new(Mutex::new(SharedDocState::new(doc, "test-notebook".into())));

        let initial_snapshot = NotebookSnapshot::empty();
        let (snapshot_tx, snapshot_rx) = watch::channel(initial_snapshot);
        let snapshot_tx = Arc::new(snapshot_tx);
        let (changed_tx, changed_rx) = mpsc::unbounded_channel();
        let (cmd_tx, cmd_rx) = mpsc::channel(32);

        let handle = DocHandle::new(
            shared,
            changed_tx,
            cmd_tx,
            snapshot_tx,
            snapshot_rx,
            "test-notebook".into(),
        );

        (handle, changed_rx, cmd_rx)
    }

    #[test]
    fn test_notebook_id() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();
        assert_eq!(handle.notebook_id(), "test-notebook");
    }

    #[test]
    fn test_with_doc_returns_value() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        let result = handle.with_doc(|_doc| 42).unwrap();
        assert_eq!(result, 42);
    }

    #[test]
    fn test_with_doc_can_return_result() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        let result: Result<Result<String, String>, _> =
            handle.with_doc(|_doc| Ok("hello".to_string()));

        assert_eq!(result.unwrap().unwrap(), "hello");
    }

    #[test]
    fn test_with_doc_notifies_changed() {
        let (handle, mut changed_rx, _cmd_rx) = test_handle();

        // No notification yet
        assert!(changed_rx.try_recv().is_err());

        // Mutate the doc
        handle.with_doc(|_doc| {}).unwrap();

        // Should have received a change notification
        assert!(changed_rx.try_recv().is_ok());
    }

    #[test]
    fn test_with_doc_publishes_snapshot() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        // Initial snapshot has no cells
        let snap = handle.snapshot();
        assert_eq!(snap.cell_count(), 0);

        // Add a cell via with_doc using raw Automerge operations
        handle
            .with_doc(|doc| {
                use automerge::transaction::Transactable;
                use automerge::ObjType;

                // Create the schema: cells map
                let cells_id = doc
                    .put_object(automerge::ROOT, "cells", ObjType::Map)
                    .unwrap();
                let cell_id = doc.put_object(&cells_id, "cell-1", ObjType::Map).unwrap();
                doc.put(&cell_id, "id", "cell-1").unwrap();
                doc.put(&cell_id, "cell_type", "code").unwrap();
                doc.put(&cell_id, "position", "80").unwrap();
                doc.put_object(&cell_id, "source", ObjType::Text).unwrap();
                doc.put(&cell_id, "execution_count", "null").unwrap();
                doc.put_object(&cell_id, "outputs", ObjType::List).unwrap();
                doc.put_object(&cell_id, "metadata", ObjType::Map).unwrap();
                doc.put_object(&cell_id, "resolved_assets", ObjType::Map)
                    .unwrap();
            })
            .unwrap();

        // Snapshot should now have the cell
        let snap = handle.snapshot();
        assert_eq!(snap.cell_count(), 1);
        assert_eq!(snap.cells()[0].id, "cell-1");
        assert_eq!(snap.cells()[0].cell_type, "code");
    }

    #[test]
    fn test_with_doc_using_notebook_doc() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        // Use NotebookDoc wrapper for typed operations
        handle
            .with_doc(|doc| {
                let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
                nd.add_cell(0, "cell-1", "code").unwrap();
                nd.update_source("cell-1", "print('hello')").unwrap();
                *doc = nd.into_inner();
            })
            .unwrap();

        let snap = handle.snapshot();
        assert_eq!(snap.cell_count(), 1);

        let cell = snap.get_cell("cell-1").unwrap();
        assert_eq!(cell.source, "print('hello')");
        assert_eq!(cell.cell_type, "code");
    }

    #[test]
    fn test_multiple_mutations_coalesce_notifications() {
        let (handle, mut changed_rx, _cmd_rx) = test_handle();

        // Multiple mutations
        handle.with_doc(|_doc| {}).unwrap();
        handle.with_doc(|_doc| {}).unwrap();
        handle.with_doc(|_doc| {}).unwrap();

        // Should have 3 notifications (one per with_doc call)
        // The sync task would coalesce these, but the channel has all of them
        let mut count = 0;
        while changed_rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(count, 3);
    }

    #[test]
    fn test_snapshot_reflects_latest_mutation() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        handle
            .with_doc(|doc| {
                let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
                nd.add_cell(0, "cell-1", "code").unwrap();
                nd.update_source("cell-1", "x = 1").unwrap();
                *doc = nd.into_inner();
            })
            .unwrap();

        assert_eq!(
            handle.snapshot().get_cell("cell-1").unwrap().source,
            "x = 1"
        );

        // Update the source
        handle
            .with_doc(|doc| {
                let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
                nd.update_source("cell-1", "x = 2").unwrap();
                *doc = nd.into_inner();
            })
            .unwrap();

        // Snapshot should reflect the latest mutation immediately
        assert_eq!(
            handle.snapshot().get_cell("cell-1").unwrap().source,
            "x = 2"
        );
    }

    #[test]
    fn test_get_cells_convenience() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        handle
            .with_doc(|doc| {
                let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
                nd.add_cell(0, "cell-a", "code").unwrap();
                nd.add_cell(1, "cell-b", "markdown").unwrap();
                *doc = nd.into_inner();
            })
            .unwrap();

        let cells = handle.get_cells();
        assert_eq!(cells.len(), 2);
        assert_eq!(cells[0].id, "cell-a");
        assert_eq!(cells[1].id, "cell-b");
    }

    #[test]
    fn test_handle_is_clone() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        let handle2 = handle.clone();

        // Mutate via first handle
        handle
            .with_doc(|doc| {
                let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
                nd.add_cell(0, "cell-1", "code").unwrap();
                *doc = nd.into_inner();
            })
            .unwrap();

        // Second handle sees the same state
        assert_eq!(handle2.snapshot().cell_count(), 1);
        assert_eq!(handle2.get_cells()[0].id, "cell-1");
    }

    #[test]
    fn test_subscribe_receives_updates() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        let mut subscriber = handle.subscribe();

        handle
            .with_doc(|doc| {
                let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
                nd.add_cell(0, "cell-1", "code").unwrap();
                *doc = nd.into_inner();
            })
            .unwrap();

        // The subscriber should see the change
        assert!(subscriber.has_changed().unwrap_or(false));
        let snap = subscriber.borrow_and_update().clone();
        assert_eq!(snap.cell_count(), 1);
    }

    #[test]
    fn test_metadata_operations() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        // Initially no metadata
        assert!(handle.get_notebook_metadata().is_none());

        // Set metadata via with_doc
        handle
            .with_doc(|doc| {
                let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));

                let snapshot = notebook_doc::metadata::NotebookMetadataSnapshot {
                    kernelspec: Some(notebook_doc::metadata::KernelspecSnapshot {
                        name: "python3".into(),
                        display_name: "Python 3".into(),
                        language: Some("python".into()),
                    }),
                    language_info: None,
                    runt: notebook_doc::metadata::RuntMetadata::default(),
                };
                nd.set_metadata_snapshot(&snapshot).unwrap();
                *doc = nd.into_inner();
            })
            .unwrap();

        // Should be readable from the snapshot
        let meta = handle.get_notebook_metadata().unwrap();
        let ks = meta.kernelspec.unwrap();
        assert_eq!(ks.name, "python3");
        assert_eq!(ks.display_name, "Python 3");
        assert_eq!(ks.language.unwrap(), "python");
    }

    #[test]
    fn test_cell_metadata_via_with_doc() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        handle
            .with_doc(|doc| {
                let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
                nd.add_cell(0, "cell-1", "code").unwrap();
                nd.set_cell_source_hidden("cell-1", true).unwrap();
                nd.set_cell_outputs_hidden("cell-1", true).unwrap();
                *doc = nd.into_inner();
            })
            .unwrap();

        let cell = handle.snapshot().get_cell("cell-1").unwrap().clone();
        assert!(cell.is_source_hidden());
        assert!(cell.is_outputs_hidden());
    }

    #[test]
    fn test_empty_snapshot() {
        let snap = NotebookSnapshot::empty();
        assert_eq!(snap.cell_count(), 0);
        assert!(snap.cells().is_empty());
        assert!(snap.notebook_metadata().is_none());
        assert!(snap.get_cell("nonexistent").is_none());
    }

    #[test]
    fn test_shared_doc_state_new() {
        let doc = AutoCommit::new();
        let state = SharedDocState::new(doc, "test-id".into());
        assert_eq!(state.notebook_id(), "test-id");
    }

    #[test]
    fn test_shared_doc_state_sync_message_empty_doc() {
        let doc = AutoCommit::new();
        let mut state = SharedDocState::new(doc, "test-id".into());

        // A fresh doc with no changes should have no sync message for a fresh peer
        // (both are empty, nothing to sync)
        let msg = state.generate_sync_message();
        // First sync message is always generated (contains bloom filter)
        assert!(msg.is_some());
    }

    #[test]
    fn test_with_doc_error_propagation() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        let result: Result<Result<(), String>, _> =
            handle.with_doc(|_doc| Err("something went wrong".to_string()));

        // The outer Result is Ok (no lock poison), inner is Err
        let inner = result.unwrap();
        assert!(inner.is_err());
        assert_eq!(inner.unwrap_err(), "something went wrong");
    }

    #[test]
    fn test_concurrent_access_from_multiple_threads() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        let handle1 = handle.clone();
        let handle2 = handle.clone();

        // Spawn two threads that mutate concurrently
        let t1 = std::thread::spawn(move || {
            for i in 0..10 {
                handle1
                    .with_doc(|doc| {
                        let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
                        let cell_id = format!("thread1-cell-{}", i);
                        nd.add_cell(0, &cell_id, "code").unwrap();
                        *doc = nd.into_inner();
                    })
                    .unwrap();
            }
        });

        let t2 = std::thread::spawn(move || {
            for i in 0..10 {
                handle2
                    .with_doc(|doc| {
                        let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
                        let cell_id = format!("thread2-cell-{}", i);
                        nd.add_cell(0, &cell_id, "code").unwrap();
                        *doc = nd.into_inner();
                    })
                    .unwrap();
            }
        });

        t1.join().unwrap();
        t2.join().unwrap();

        // All 20 cells should be present
        let cells = handle.get_cells();
        assert_eq!(cells.len(), 20);
    }
}
