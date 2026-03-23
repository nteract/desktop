//! Tests for notebook-sync.
//!
//! Unit tests verify the DocHandle pattern without a daemon connection.
//! Integration tests (behind `#[ignore]`) connect to a running daemon.

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::module_inception)]
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
                let _source_id = doc.put_object(&cell_id, "source", ObjType::Text).unwrap();
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
                nd.add_cell_after("cell-1", "code", None).unwrap();
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
                nd.add_cell_after("cell-1", "code", None).unwrap();
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
                nd.add_cell_after("cell-a", "code", None).unwrap();
                nd.add_cell_after("cell-b", "markdown", Some("cell-a"))
                    .unwrap();
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
                nd.add_cell_after("cell-1", "code", None).unwrap();
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
                nd.add_cell_after("cell-1", "code", None).unwrap();
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
                nd.add_cell_after("cell-1", "code", None).unwrap();
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
                        nd.add_cell_after(&cell_id, "code", None).unwrap();
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
                        nd.add_cell_after(&cell_id, "code", None).unwrap();
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

    // =====================================================================
    // Convenience method tests
    // =====================================================================

    #[test]
    fn test_convenience_add_cell() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        handle.add_cell_after("cell-1", "code", None).unwrap();

        let cells = handle.get_cells();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].id, "cell-1");
        assert_eq!(cells[0].cell_type, "code");
    }

    #[test]
    fn test_convenience_add_cell_with_source() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        handle
            .add_cell_with_source("cell-1", "code", None, "x = 42")
            .unwrap();

        let cell = handle.snapshot().get_cell("cell-1").unwrap().clone();
        assert_eq!(cell.source, "x = 42");
    }

    #[test]
    fn test_convenience_update_source() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        handle.add_cell_after("cell-1", "code", None).unwrap();
        handle.update_source("cell-1", "y = 100").unwrap();

        let cell = handle.snapshot().get_cell("cell-1").unwrap().clone();
        assert_eq!(cell.source, "y = 100");
    }

    #[test]
    fn test_convenience_append_source() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        handle
            .add_cell_with_source("cell-1", "code", None, "x = 1")
            .unwrap();
        handle.append_source("cell-1", "\ny = 2").unwrap();

        let cell = handle.snapshot().get_cell("cell-1").unwrap().clone();
        assert_eq!(cell.source, "x = 1\ny = 2");
    }

    #[test]
    fn test_convenience_delete_cell() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        handle.add_cell_after("cell-1", "code", None).unwrap();
        handle
            .add_cell_after("cell-2", "code", Some("cell-1"))
            .unwrap();
        assert_eq!(handle.get_cells().len(), 2);

        let deleted = handle.delete_cell("cell-1").unwrap();
        assert!(deleted);
        assert_eq!(handle.get_cells().len(), 1);
        assert_eq!(handle.get_cells()[0].id, "cell-2");
    }

    #[test]
    fn test_convenience_set_metadata_snapshot() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        let snapshot = notebook_doc::metadata::NotebookMetadataSnapshot {
            kernelspec: Some(notebook_doc::metadata::KernelspecSnapshot {
                name: "deno".into(),
                display_name: "Deno".into(),
                language: Some("typescript".into()),
            }),
            language_info: None,
            runt: notebook_doc::metadata::RuntMetadata::default(),
        };

        handle.set_metadata_snapshot(&snapshot).unwrap();

        let meta = handle.get_notebook_metadata().unwrap();
        assert_eq!(meta.kernelspec.unwrap().name, "deno");
    }

    #[test]
    fn test_convenience_cell_visibility() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        handle.add_cell_after("cell-1", "code", None).unwrap();

        handle.set_cell_source_hidden("cell-1", true).unwrap();
        assert!(handle
            .snapshot()
            .get_cell("cell-1")
            .unwrap()
            .is_source_hidden());

        handle.set_cell_outputs_hidden("cell-1", true).unwrap();
        assert!(handle
            .snapshot()
            .get_cell("cell-1")
            .unwrap()
            .is_outputs_hidden());
    }

    #[test]
    fn test_convenience_uv_dependencies() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();

        handle.add_uv_dependency("pandas>=2.0").unwrap();
        handle.add_uv_dependency("requests").unwrap();

        let meta = handle.get_notebook_metadata().unwrap();
        let deps = meta.runt.uv.unwrap().dependencies;
        assert_eq!(deps.len(), 2);
        assert!(deps.contains(&"pandas>=2.0".to_string()));
        assert!(deps.contains(&"requests".to_string()));

        let removed = handle.remove_uv_dependency("pandas").unwrap();
        assert!(removed);

        let meta = handle.get_notebook_metadata().unwrap();
        let deps = meta.runt.uv.unwrap().dependencies;
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0], "requests");
    }

    #[test]
    fn test_atomic_compound_operation() {
        let (handle, mut changed_rx, _cmd_rx) = test_handle();

        // Drain any existing notifications
        while changed_rx.try_recv().is_ok() {}

        // Compound operation: add cell + set source + set metadata, all in one lock
        handle
            .with_doc(|doc| {
                let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
                nd.add_cell_after("cell-1", "code", None).unwrap();
                nd.update_source("cell-1", "print('atomic')").unwrap();
                nd.set_cell_source_hidden("cell-1", true).unwrap();
                *doc = nd.into_inner();
            })
            .unwrap();

        // Only ONE change notification (atomic)
        let mut count = 0;
        while changed_rx.try_recv().is_ok() {
            count += 1;
        }
        assert_eq!(
            count, 1,
            "compound with_doc should produce exactly one notification"
        );

        // All mutations visible
        let cell = handle.snapshot().get_cell("cell-1").unwrap().clone();
        assert_eq!(cell.source, "print('atomic')");
        assert!(cell.is_source_hidden());
    }

    #[test]
    fn test_set_actor_and_get_actor_id() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();
        handle.set_actor("agent:claude:abc123").unwrap();
        assert_eq!(handle.get_actor_id().unwrap(), "agent:claude:abc123");
    }

    #[test]
    fn test_default_actor_is_not_empty() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();
        let actor = handle.get_actor_id().unwrap();
        assert!(!actor.is_empty());
    }

    #[test]
    fn test_actor_persists_through_mutations() {
        let (handle, _changed_rx, _cmd_rx) = test_handle();
        handle.set_actor("agent:test:session1").unwrap();

        // Mutate the doc
        handle.add_cell_after("cell-1", "code", None).unwrap();
        handle.update_source("cell-1", "print('hello')").unwrap();

        // Actor should still be the same
        assert_eq!(handle.get_actor_id().unwrap(), "agent:test:session1");
    }
}

// =========================================================================
// Integration tests (require a running daemon)
// =========================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod integration_tests {
    use std::path::PathBuf;
    use std::time::Duration;

    use notebook_protocol::protocol::{NotebookBroadcast, NotebookRequest, NotebookResponse};

    /// Get the daemon socket path for the current worktree.
    fn daemon_socket_path() -> PathBuf {
        // Use the same logic as the Python tests: check env vars first
        if let Ok(path) = std::env::var("RUNTIMED_SOCKET_PATH") {
            return PathBuf::from(path);
        }
        runtimed::default_socket_path()
    }

    /// Check if a daemon is running and available.
    fn daemon_available() -> bool {
        let path = daemon_socket_path();
        path.exists()
    }

    #[tokio::test]
    #[ignore] // Run with: cargo test -p notebook-sync -- --ignored
    async fn test_connect_to_daemon() {
        if !daemon_available() {
            eprintln!("Skipping: no daemon running");
            return;
        }

        let result =
            crate::connect::connect(daemon_socket_path(), "test-connect".into(), "test").await;

        assert!(result.is_ok(), "Failed to connect: {:?}", result.err());

        let conn = result.unwrap();
        assert_eq!(conn.handle.notebook_id(), "test-connect");
    }

    #[tokio::test]
    #[ignore]
    async fn test_create_cell_and_read_back() {
        if !daemon_available() {
            eprintln!("Skipping: no daemon running");
            return;
        }

        let conn = crate::connect::connect(
            daemon_socket_path(),
            format!("test-cell-{}", uuid::Uuid::new_v4()),
            "test",
        )
        .await
        .expect("connect");

        let handle = conn.handle;

        // Create a cell with source
        handle
            .add_cell_with_source("cell-1", "code", None, "x = 42")
            .expect("add_cell_with_source");

        // Confirm the daemon has our changes
        handle.confirm_sync().await.expect("confirm_sync");

        // Read back via snapshot
        let snap = handle.snapshot();
        let cell = snap.get_cell("cell-1").expect("cell should exist");
        assert_eq!(cell.source, "x = 42");
        assert_eq!(cell.cell_type, "code");
    }

    #[tokio::test]
    #[ignore]
    async fn test_execute_cell_and_get_outputs() {
        if !daemon_available() {
            eprintln!("Skipping: no daemon running");
            return;
        }

        let notebook_id = format!("test-exec-{}", uuid::Uuid::new_v4());
        let conn = crate::connect::connect(daemon_socket_path(), notebook_id, "test")
            .await
            .expect("connect");

        let handle = conn.handle;
        let mut broadcast_rx = conn.broadcast_rx;

        // Start a kernel
        let response = handle
            .send_request(NotebookRequest::LaunchKernel {
                kernel_type: "python".into(),
                env_source: "auto".into(),
                notebook_path: None,
            })
            .await
            .expect("launch kernel");

        match response {
            NotebookResponse::KernelLaunched { .. }
            | NotebookResponse::KernelAlreadyRunning { .. } => {}
            other => panic!("Unexpected response: {:?}", other),
        }

        // Create a cell that prints something
        handle
            .add_cell_with_source(
                "cell-exec",
                "code",
                None,
                "print('hello from notebook-sync')",
            )
            .expect("add_cell");

        // Confirm sync so daemon has the cell
        handle.confirm_sync().await.expect("confirm_sync");

        // Execute
        let response = handle
            .send_request(NotebookRequest::ExecuteCell {
                cell_id: "cell-exec".into(),
            })
            .await
            .expect("execute");

        match response {
            NotebookResponse::CellQueued { .. } => {}
            other => panic!("Expected CellQueued, got: {:?}", other),
        }

        // Wait for ExecutionDone via broadcast
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut got_done = false;

        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), broadcast_rx.recv()).await {
                Ok(Some(NotebookBroadcast::ExecutionDone { cell_id, .. }))
                    if cell_id == "cell-exec" =>
                {
                    got_done = true;
                    break;
                }
                Ok(Some(_)) => continue, // other broadcasts
                Ok(None) => break,       // channel closed
                Err(_) => continue,      // timeout, keep waiting
            }
        }

        assert!(got_done, "Did not receive ExecutionDone within 30s");

        // Confirm sync to ensure outputs are in our doc
        handle
            .confirm_sync()
            .await
            .expect("confirm_sync after exec");

        // Read outputs from the Automerge doc (source of truth)
        // Outputs are stored as manifest hashes (blob references) or raw JSON —
        // the important thing is that they exist after execution.
        let snap = handle.snapshot();
        let cell = snap.get_cell("cell-exec").expect("cell should exist");

        assert!(
            !cell.outputs.is_empty(),
            "Cell should have outputs after execution"
        );

        // Verify execution_count was set (proves the cell actually ran)
        assert_ne!(
            cell.execution_count, "null",
            "execution_count should be set after execution, got: {}",
            cell.execution_count
        );

        // Shutdown kernel
        let _ = handle
            .send_request(NotebookRequest::ShutdownKernel {})
            .await;
    }

    #[tokio::test]
    #[ignore]
    async fn test_two_handles_share_state() {
        if !daemon_available() {
            eprintln!("Skipping: no daemon running");
            return;
        }

        let notebook_id = format!("test-share-{}", uuid::Uuid::new_v4());

        // First handle connects and creates a cell
        let conn1 = crate::connect::connect(daemon_socket_path(), notebook_id.clone(), "test")
            .await
            .expect("connect 1");

        conn1
            .handle
            .add_cell_with_source("shared-cell", "code", None, "shared = True")
            .expect("add_cell");

        conn1.handle.confirm_sync().await.expect("confirm_sync 1");

        // Second handle connects to the same notebook
        let conn2 = crate::connect::connect(daemon_socket_path(), notebook_id, "test")
            .await
            .expect("connect 2");

        // Second handle should see the cell created by the first
        let snap = conn2.handle.snapshot();
        let cell = snap.get_cell("shared-cell");
        assert!(
            cell.is_some(),
            "Second handle should see cell created by first"
        );
        assert_eq!(cell.unwrap().source, "shared = True");
    }

    #[tokio::test]
    #[ignore]
    async fn test_metadata_round_trip_via_daemon() {
        if !daemon_available() {
            eprintln!("Skipping: no daemon running");
            return;
        }

        let notebook_id = format!("test-meta-{}", uuid::Uuid::new_v4());
        let conn = crate::connect::connect(daemon_socket_path(), notebook_id, "test")
            .await
            .expect("connect");

        let handle = conn.handle;

        // Set metadata
        let snapshot = notebook_doc::metadata::NotebookMetadataSnapshot {
            kernelspec: Some(notebook_doc::metadata::KernelspecSnapshot {
                name: "python3".into(),
                display_name: "Python 3".into(),
                language: Some("python".into()),
            }),
            language_info: None,
            runt: notebook_doc::metadata::RuntMetadata::default(),
        };

        handle
            .set_metadata_snapshot(&snapshot)
            .expect("set_metadata_snapshot");

        handle.confirm_sync().await.expect("confirm_sync");

        // Read back
        let meta = handle
            .get_notebook_metadata()
            .expect("should have metadata");
        let ks = meta.kernelspec.expect("should have kernelspec");
        assert_eq!(ks.name, "python3");
        assert_eq!(ks.display_name, "Python 3");
    }

    #[tokio::test]
    #[ignore]
    async fn test_actor_identity_round_trip() {
        if !daemon_available() {
            eprintln!("Skipping: no daemon running");
            return;
        }

        let notebook_id = format!("test-actor-{}", uuid::Uuid::new_v4());
        let conn = crate::connect::connect(daemon_socket_path(), notebook_id, "test")
            .await
            .expect("connect");

        let handle = conn.handle;

        // Set a meaningful actor label
        handle
            .set_actor("agent:claude:test1234")
            .expect("set_actor");

        // Verify it sticks
        assert_eq!(handle.get_actor_id().unwrap(), "agent:claude:test1234");

        // Make an edit — this op should be tagged with our actor
        handle
            .add_cell_with_source("prov-cell", "code", None, "# written by agent")
            .expect("add_cell");

        // Sync to daemon
        handle.confirm_sync().await.expect("confirm_sync");

        // Actor should still be ours after sync
        assert_eq!(handle.get_actor_id().unwrap(), "agent:claude:test1234");
    }

    #[tokio::test]
    #[ignore]
    async fn test_contributing_actors_across_peers() {
        if !daemon_available() {
            eprintln!("Skipping: no daemon running");
            return;
        }

        let notebook_id = format!("test-contrib-{}", uuid::Uuid::new_v4());

        // Peer 1: "agent:alice"
        let conn1 = crate::connect::connect(daemon_socket_path(), notebook_id.clone(), "test")
            .await
            .expect("connect 1");
        conn1
            .handle
            .set_actor("agent:alice:aaa")
            .expect("set_actor 1");
        conn1
            .handle
            .add_cell_with_source("cell-alice", "code", None, "# alice")
            .expect("add_cell alice");
        conn1.handle.confirm_sync().await.expect("confirm_sync 1");

        // Peer 2: "agent:bob"
        let conn2 = crate::connect::connect(daemon_socket_path(), notebook_id, "test")
            .await
            .expect("connect 2");
        conn2
            .handle
            .set_actor("agent:bob:bbb")
            .expect("set_actor 2");
        conn2
            .handle
            .add_cell_with_source("cell-bob", "code", None, "# bob")
            .expect("add_cell bob");
        conn2.handle.confirm_sync().await.expect("confirm_sync 2");

        // Give sync a moment to propagate
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Both peers should see both cells
        let snap1 = conn1.handle.snapshot();
        assert!(
            snap1.get_cell("cell-alice").is_some(),
            "peer1 should see alice's cell"
        );
        assert!(
            snap1.get_cell("cell-bob").is_some(),
            "peer1 should see bob's cell"
        );

        // Walk the change history — both agents + runtimed should appear
        let actors = conn2
            .handle
            .with_doc(|doc| {
                let mut nd = notebook_doc::NotebookDoc::wrap(std::mem::take(doc));
                let actors = nd.contributing_actors();
                *doc = nd.into_inner();
                actors
            })
            .expect("with_doc");

        assert!(
            actors.contains(&"agent:alice:aaa".to_string()),
            "should see alice in contributors: {:?}",
            actors
        );
        assert!(
            actors.contains(&"agent:bob:bbb".to_string()),
            "should see bob in contributors: {:?}",
            actors
        );
        // runtimed is also a contributor (it creates the room/doc structure)
        assert!(
            actors.contains(&"runtimed".to_string()),
            "should see runtimed in contributors: {:?}",
            actors
        );
    }

    #[tokio::test]
    async fn test_connect_to_missing_socket_returns_daemon_unavailable() {
        use std::error::Error;
        use std::path::PathBuf;

        let bogus_path = PathBuf::from("/tmp/nonexistent-runtimed-test.sock");
        let result =
            crate::connect::connect(bogus_path.clone(), "test-notebook".to_string(), "test").await;

        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("should fail to connect to nonexistent socket"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("Daemon is not running"),
            "expected DaemonUnavailable, got: {msg}"
        );
        assert!(
            msg.contains("nonexistent-runtimed-test.sock"),
            "expected path in error message, got: {msg}"
        );
        // Verify the source error is preserved
        assert!(
            err.source().is_some(),
            "expected source io::Error to be preserved"
        );
    }
}
