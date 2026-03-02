//! Integration tests for runtimed daemon and client.
//!
//! These tests spawn a real daemon and test client interactions.

use std::time::Duration;

use runtimed::client::PoolClient;
use runtimed::daemon::{Daemon, DaemonConfig};
use runtimed::notebook_sync_client::NotebookSyncClient;
use runtimed::EnvType;
use tempfile::TempDir;
use tokio::time::sleep;

/// Create a test daemon configuration with a unique socket and lock path.
fn test_config(temp_dir: &TempDir) -> DaemonConfig {
    DaemonConfig {
        socket_path: temp_dir.path().join("test-runtimed.sock"),
        cache_dir: temp_dir.path().join("envs"),
        blob_store_dir: temp_dir.path().join("blobs"),
        notebook_docs_dir: temp_dir.path().join("notebook-docs"),
        uv_pool_size: 0, // Don't create real envs in tests
        conda_pool_size: 0,
        max_age_secs: 3600,
        lock_dir: Some(temp_dir.path().to_path_buf()),
        room_eviction_delay_ms: Some(50), // Fast eviction for tests
    }
}

/// Wait for the daemon to be ready by polling the client.
async fn wait_for_daemon(client: &PoolClient, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if client.ping().await.is_ok() {
            return true;
        }
        sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test]
async fn test_daemon_ping_pong() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    // Spawn daemon
    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    // Create client and wait for daemon
    let client = PoolClient::new(socket_path);
    assert!(wait_for_daemon(&client, Duration::from_secs(5)).await);

    // Test ping
    let result = client.ping().await;
    assert!(result.is_ok());

    // Shutdown
    let shutdown_result = client.shutdown().await;
    assert!(shutdown_result.is_ok());

    // Wait for daemon to exit
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_daemon_status() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let client = PoolClient::new(socket_path);
    assert!(wait_for_daemon(&client, Duration::from_secs(5)).await);

    // Get status
    let stats = client.status().await.unwrap();
    assert_eq!(stats.uv_available, 0);
    assert_eq!(stats.conda_available, 0);

    // Shutdown
    client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_daemon_take_empty_pool() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let client = PoolClient::new(socket_path);
    assert!(wait_for_daemon(&client, Duration::from_secs(5)).await);

    // Try to take from empty pool
    let result = client.take(EnvType::Uv).await.unwrap();
    assert!(result.is_none());

    let result = client.take(EnvType::Conda).await.unwrap();
    assert!(result.is_none());

    // Shutdown
    client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_singleton_prevents_second_daemon() {
    let temp_dir = TempDir::new().unwrap();
    let config1 = test_config(&temp_dir);
    let socket_path = config1.socket_path.clone();

    // Start first daemon
    let daemon1 = Daemon::new(config1).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon1.run().await.ok();
    });

    let client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&client, Duration::from_secs(5)).await);

    // Try to start second daemon with same paths - should fail
    let config2 = DaemonConfig {
        socket_path: socket_path.clone(),
        cache_dir: temp_dir.path().join("envs"),
        blob_store_dir: temp_dir.path().join("blobs"),
        notebook_docs_dir: temp_dir.path().join("notebook-docs"),
        uv_pool_size: 0,
        conda_pool_size: 0,
        max_age_secs: 3600,
        lock_dir: Some(temp_dir.path().to_path_buf()),
        room_eviction_delay_ms: Some(50),
    };

    let result = Daemon::new(config2);
    assert!(result.is_err());

    // Shutdown first daemon
    client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_client_timeout_when_no_daemon() {
    let temp_dir = TempDir::new().unwrap();
    let socket_path = temp_dir.path().join("nonexistent.sock");

    let client = PoolClient::new(socket_path).with_timeout(Duration::from_millis(100));

    // Should fail to connect
    let result = client.ping().await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_multiple_client_connections() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let client1 = PoolClient::new(socket_path.clone());
    let client2 = PoolClient::new(socket_path.clone());
    let client3 = PoolClient::new(socket_path.clone());

    assert!(wait_for_daemon(&client1, Duration::from_secs(5)).await);

    // All clients should be able to ping concurrently
    let (r1, r2, r3) = tokio::join!(client1.ping(), client2.ping(), client3.ping());

    assert!(r1.is_ok());
    assert!(r2.is_ok());
    assert!(r3.is_ok());

    // Shutdown
    client1.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_try_get_pooled_env_no_daemon() {
    // Use a temp dir so we don't accidentally connect to a real running daemon
    let temp_dir = TempDir::new().unwrap();
    let socket_path = temp_dir.path().join("nonexistent.sock");

    let client = PoolClient::new(socket_path);
    let result = client.take(EnvType::Uv).await;
    assert!(result.is_err(), "should fail when daemon is not running");
}

#[tokio::test]
async fn test_settings_sync_via_unified_socket() {
    use runtimed::sync_client::SyncClient;

    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    // Wait for daemon to be ready (via pool channel)
    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    // Connect a SyncClient through the unified socket
    let sync_client = SyncClient::connect_with_timeout(socket_path, Duration::from_secs(2))
        .await
        .expect("SyncClient should connect via unified socket");

    // Read settings — verifies the sync handshake completed and we have
    // a valid local replica. Exact values depend on persisted state.
    let settings = sync_client.get_all();
    // Smoke check: theme field is populated (any valid variant)
    let _ = serde_json::to_string(&settings.theme).unwrap();

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_blob_server_health() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let client = PoolClient::new(socket_path);
    assert!(wait_for_daemon(&client, Duration::from_secs(5)).await);

    // Read daemon info to find blob port
    let info_path = temp_dir.path().join("daemon.json");
    let info_json = tokio::fs::read_to_string(&info_path)
        .await
        .expect("daemon.json should exist");
    let info: serde_json::Value = serde_json::from_str(&info_json).unwrap();
    let blob_port = info["blob_port"].as_u64().expect("blob_port should be set");

    // Hit the health endpoint
    let resp = reqwest::get(format!("http://127.0.0.1:{}/health", blob_port))
        .await
        .expect("health request should succeed");
    assert_eq!(resp.status(), 200);
    let body = resp.text().await.unwrap();
    assert_eq!(body, "OK");

    // Hit a non-existent blob — should 404
    let fake_hash = "a".repeat(64);
    let resp = reqwest::get(format!("http://127.0.0.1:{}/blob/{}", blob_port, fake_hash))
        .await
        .expect("blob request should succeed");
    assert_eq!(resp.status(), 404);

    // Shutdown
    client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_notebook_sync_via_unified_socket() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    // Wait for daemon to be ready
    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    // Connect first client — should get empty notebook
    let mut client1 = NotebookSyncClient::connect(socket_path.clone(), "test-notebook".to_string())
        .await
        .expect("client1 should connect");

    let cells = client1.get_cells();
    assert!(cells.is_empty(), "new notebook should have no cells");

    // Add a cell from client1
    client1.add_cell(0, "cell-1", "code").await.unwrap();
    client1
        .update_source("cell-1", "print('hello')")
        .await
        .unwrap();

    // Connect second client to the same notebook — should see the cell
    let client2 = NotebookSyncClient::connect(socket_path.clone(), "test-notebook".to_string())
        .await
        .expect("client2 should connect");

    let cells = client2.get_cells();
    assert_eq!(cells.len(), 1, "client2 should see the cell from client1");
    assert_eq!(cells[0].id, "cell-1");
    assert_eq!(cells[0].source, "print('hello')");
    assert_eq!(cells[0].cell_type, "code");

    // Connect to a different notebook — should be independent
    let client3 = NotebookSyncClient::connect(socket_path.clone(), "other-notebook".to_string())
        .await
        .expect("client3 should connect");

    let cells = client3.get_cells();
    assert!(cells.is_empty(), "different notebook should have no cells");

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_notebook_sync_cross_window_propagation() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    // Both clients connect to the same notebook
    let mut client1 = NotebookSyncClient::connect(socket_path.clone(), "shared-nb".to_string())
        .await
        .unwrap();
    let mut client2 = NotebookSyncClient::connect(socket_path.clone(), "shared-nb".to_string())
        .await
        .unwrap();

    // Client1 adds a cell
    client1.add_cell(0, "c1", "code").await.unwrap();
    client1.update_source("c1", "x = 42").await.unwrap();
    client1.set_execution_count("c1", "1").await.unwrap();

    // Client2 should receive the changes
    let cells = client2.recv_changes().await.unwrap();
    assert!(!cells.is_empty(), "client2 should receive propagated cells");

    // May need additional recv rounds for full convergence
    let mut final_cells = cells;
    for _ in 0..5 {
        match tokio::time::timeout(Duration::from_millis(200), client2.recv_changes()).await {
            Ok(Ok(cells)) => final_cells = cells,
            _ => break,
        }
    }

    // Verify client2 has the cell
    let cell = final_cells.iter().find(|c| c.id == "c1");
    assert!(cell.is_some(), "client2 should have cell c1");
    let cell = cell.unwrap();
    assert_eq!(cell.source, "x = 42");
    assert_eq!(cell.execution_count, "1");

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

/// Test that room eviction creates a fresh room on reconnection.
///
/// Design: The .ipynb file is the source of truth, not persisted Automerge docs.
/// When all clients disconnect and the room is evicted, a new connection should
/// get a fresh empty room. The client will populate it from their local .ipynb.
#[tokio::test]
async fn test_notebook_room_eviction_and_persistence() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    // Phase 1: Two clients connect, add cells, then both disconnect
    {
        let mut client1 =
            NotebookSyncClient::connect(socket_path.clone(), "evict-test".to_string())
                .await
                .unwrap();
        let _client2 = NotebookSyncClient::connect(socket_path.clone(), "evict-test".to_string())
            .await
            .unwrap();

        client1.add_cell(0, "c1", "code").await.unwrap();
        client1
            .update_source("c1", "persisted = True")
            .await
            .unwrap();
        client1.add_cell(1, "c2", "markdown").await.unwrap();
        client1.update_source("c2", "# Hello World").await.unwrap();

        // Both clients drop here — the room should be evicted
    }

    // Give the daemon time to process disconnects and evict the room
    sleep(Duration::from_millis(200)).await;

    // Phase 2: Reconnect — the room should be fresh (not loaded from persisted state)
    // This matches the design: .ipynb is source of truth, Automerge is just sync layer
    let client3 = NotebookSyncClient::connect(socket_path.clone(), "evict-test".to_string())
        .await
        .expect("should reconnect after room eviction");

    let cells = client3.get_cells();
    assert_eq!(
        cells.len(),
        0,
        "reconnected client should get fresh empty room (client populates from .ipynb), got: {:?}",
        cells
    );

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_notebook_cell_delete_propagation() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    // Client1 creates three cells
    let mut client1 = NotebookSyncClient::connect(socket_path.clone(), "delete-test".to_string())
        .await
        .unwrap();

    client1.add_cell(0, "keep-1", "code").await.unwrap();
    client1.add_cell(1, "to-delete", "code").await.unwrap();
    client1.add_cell(2, "keep-2", "code").await.unwrap();
    client1.update_source("keep-1", "a = 1").await.unwrap();
    client1.update_source("to-delete", "b = 2").await.unwrap();
    client1.update_source("keep-2", "c = 3").await.unwrap();

    // Client2 joins and verifies all three cells
    let mut client2 = NotebookSyncClient::connect(socket_path.clone(), "delete-test".to_string())
        .await
        .unwrap();

    assert_eq!(client2.get_cells().len(), 3);

    // Client1 deletes the middle cell
    client1.delete_cell("to-delete").await.unwrap();

    // Client2 receives the deletion
    let mut final_cells = client2.get_cells();
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_millis(200), client2.recv_changes()).await {
            Ok(Ok(cells)) => {
                final_cells = cells;
                if final_cells.len() == 2 {
                    break;
                }
            }
            _ => break,
        }
    }

    assert_eq!(final_cells.len(), 2, "should have 2 cells after deletion");
    assert!(
        final_cells.iter().any(|c| c.id == "keep-1"),
        "keep-1 should remain"
    );
    assert!(
        final_cells.iter().any(|c| c.id == "keep-2"),
        "keep-2 should remain"
    );
    assert!(
        !final_cells.iter().any(|c| c.id == "to-delete"),
        "to-delete should be gone"
    );

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_multiple_notebooks_concurrent_isolation() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    // Create three notebooks concurrently
    let (nb_a, nb_b, nb_c) = tokio::join!(
        NotebookSyncClient::connect(socket_path.clone(), "nb-alpha".to_string()),
        NotebookSyncClient::connect(socket_path.clone(), "nb-beta".to_string()),
        NotebookSyncClient::connect(socket_path.clone(), "nb-gamma".to_string()),
    );
    let mut nb_a = nb_a.unwrap();
    let mut nb_b = nb_b.unwrap();
    let mut nb_c = nb_c.unwrap();

    // Add cells to each notebook concurrently
    tokio::join!(
        async {
            nb_a.add_cell(0, "alpha-1", "code").await.unwrap();
            nb_a.update_source("alpha-1", "print('alpha')")
                .await
                .unwrap();
        },
        async {
            nb_b.add_cell(0, "beta-1", "markdown").await.unwrap();
            nb_b.update_source("beta-1", "# Beta").await.unwrap();
            nb_b.add_cell(1, "beta-2", "code").await.unwrap();
            nb_b.update_source("beta-2", "x = 99").await.unwrap();
        },
        async {
            nb_c.add_cell(0, "gamma-1", "code").await.unwrap();
            nb_c.update_source("gamma-1", "import os").await.unwrap();
            nb_c.add_cell(1, "gamma-2", "code").await.unwrap();
            nb_c.add_cell(2, "gamma-3", "code").await.unwrap();
        },
    );

    // Verify each notebook is isolated by connecting fresh clients
    let (fresh_a, fresh_b, fresh_c) = tokio::join!(
        NotebookSyncClient::connect(socket_path.clone(), "nb-alpha".to_string()),
        NotebookSyncClient::connect(socket_path.clone(), "nb-beta".to_string()),
        NotebookSyncClient::connect(socket_path.clone(), "nb-gamma".to_string()),
    );

    let cells_a = fresh_a.unwrap().get_cells();
    assert_eq!(cells_a.len(), 1, "nb-alpha should have 1 cell");
    assert_eq!(cells_a[0].id, "alpha-1");
    assert_eq!(cells_a[0].source, "print('alpha')");

    let cells_b = fresh_b.unwrap().get_cells();
    assert_eq!(cells_b.len(), 2, "nb-beta should have 2 cells");
    assert!(cells_b
        .iter()
        .any(|c| c.id == "beta-1" && c.cell_type == "markdown"));
    assert!(cells_b
        .iter()
        .any(|c| c.id == "beta-2" && c.source == "x = 99"));

    let cells_c = fresh_c.unwrap().get_cells();
    assert_eq!(cells_c.len(), 3, "nb-gamma should have 3 cells");
    assert!(cells_c
        .iter()
        .any(|c| c.id == "gamma-1" && c.source == "import os"));

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_notebook_append_and_clear_outputs() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    // Client1 creates a cell and appends outputs incrementally
    let mut client1 = NotebookSyncClient::connect(socket_path.clone(), "output-test".to_string())
        .await
        .unwrap();

    client1.add_cell(0, "c1", "code").await.unwrap();
    client1.set_execution_count("c1", "1").await.unwrap();
    client1
        .append_output(
            "c1",
            r#"{"output_type":"stream","name":"stdout","text":"line 1\n"}"#,
        )
        .await
        .unwrap();
    client1
        .append_output(
            "c1",
            r#"{"output_type":"stream","name":"stdout","text":"line 2\n"}"#,
        )
        .await
        .unwrap();
    client1
        .append_output(
            "c1",
            r#"{"output_type":"execute_result","data":{"text/plain":"42"}}"#,
        )
        .await
        .unwrap();

    // Client2 connects and should see all 3 outputs
    let client2 = NotebookSyncClient::connect(socket_path.clone(), "output-test".to_string())
        .await
        .unwrap();

    let cell = client2.get_cell("c1").expect("should have c1");
    assert_eq!(cell.outputs.len(), 3, "should have 3 outputs");
    assert_eq!(cell.execution_count, "1");

    // Client1 clears outputs (simulating re-execution)
    client1.clear_outputs("c1").await.unwrap();

    // Client2 receives the clear
    let mut client2 = client2;
    let mut final_cell = client2.get_cell("c1").unwrap();
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_millis(200), client2.recv_changes()).await {
            Ok(Ok(cells)) => {
                if let Some(c) = cells.iter().find(|c| c.id == "c1") {
                    final_cell = c.clone();
                    if c.outputs.is_empty() {
                        break;
                    }
                }
            }
            _ => break,
        }
    }

    assert!(
        final_cell.outputs.is_empty(),
        "outputs should be cleared, got: {:?}",
        final_cell.outputs
    );
    assert_eq!(
        final_cell.execution_count, "null",
        "execution_count should be reset to null"
    );

    // Client1 appends new outputs after clear
    client1
        .append_output(
            "c1",
            r#"{"output_type":"stream","name":"stdout","text":"fresh\n"}"#,
        )
        .await
        .unwrap();

    // Verify via a fresh client
    let client3 = NotebookSyncClient::connect(socket_path.clone(), "output-test".to_string())
        .await
        .unwrap();
    let cell = client3.get_cell("c1").expect("should have c1");
    assert_eq!(
        cell.outputs.len(),
        1,
        "should have 1 output after re-append"
    );

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}
