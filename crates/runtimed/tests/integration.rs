// Tests can use unwrap/expect freely - panics are acceptable in test code
#![allow(clippy::unwrap_used, clippy::expect_used)]

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

/// Write a test .ipynb notebook file with the given cells.
/// Each cell is a tuple of (id, cell_type, source, outputs_json_strings).
fn write_test_ipynb(path: &std::path::Path, cells: &[(&str, &str, &str, Vec<&str>)]) {
    let cells_json: Vec<serde_json::Value> = cells
        .iter()
        .enumerate()
        .map(|(i, (id, cell_type, source, outputs))| {
            let mut cell = serde_json::json!({
                "id": id,
                "cell_type": cell_type,
                "source": source,
                "metadata": {},
            });
            if *cell_type == "code" {
                cell["execution_count"] = serde_json::json!(i + 1);
                let output_values: Vec<serde_json::Value> = outputs
                    .iter()
                    .map(|o| serde_json::from_str(o).unwrap())
                    .collect();
                cell["outputs"] = serde_json::Value::Array(output_values);
            }
            cell
        })
        .collect();

    let notebook = serde_json::json!({
        "nbformat": 4,
        "nbformat_minor": 5,
        "metadata": {
            "kernelspec": {
                "display_name": "Python 3",
                "language": "python",
                "name": "python3"
            }
        },
        "cells": cells_json,
    });

    std::fs::write(path, serde_json::to_string_pretty(&notebook).unwrap()).unwrap();
}

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

/// Test that opening a .ipynb file via OpenNotebook streams cells to the client.
///
/// This exercises the full streaming load path:
/// 1. Daemon receives OpenNotebook handshake
/// 2. Handshake responds with cell_count=0 (load is deferred)
/// 3. Sync loop calls streaming_load_cells which parses the file,
///    adds cells in batches, and sends sync messages
/// 4. Client receives cells via Automerge sync protocol
#[tokio::test]
async fn test_streaming_load_via_open_notebook() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    // Create a notebook with 7 cells (enough for 3 batches of 3 + partial)
    let nb_path = temp_dir.path().join("streaming_test.ipynb");
    write_test_ipynb(
        &nb_path,
        &[
            (
                "c1",
                "code",
                "x = 1",
                vec![
                    r#"{"output_type":"execute_result","data":{"text/plain":"1"},"metadata":{},"execution_count":1}"#,
                ],
            ),
            ("c2", "markdown", "# Header", vec![]),
            ("c3", "code", "y = 2", vec![]),
            (
                "c4",
                "code",
                "print('hello')",
                vec![r#"{"output_type":"stream","name":"stdout","text":"hello\n"}"#],
            ),
            ("c5", "markdown", "Some text", vec![]),
            (
                "c6",
                "code",
                "z = x + y",
                vec![
                    r#"{"output_type":"execute_result","data":{"text/plain":"3"},"metadata":{},"execution_count":4}"#,
                ],
            ),
            ("c7", "code", "import os", vec![]),
        ],
    );

    // Open via OpenNotebook handshake — triggers streaming load
    let (handle, _receiver, _broadcast_rx, initial_cells, _metadata, info) =
        NotebookSyncClient::connect_open_split(socket_path.clone(), nb_path.clone(), None)
            .await
            .expect("should connect and open notebook");

    // Handshake reports 0 cells (streaming load is deferred)
    assert_eq!(info.cell_count, 0);
    assert!(info.error.is_none());

    // The sync task runs in the background. Wait for cells to arrive.
    // In pipe mode, initial_cells may be empty; cells come via sync.
    let start = std::time::Instant::now();
    let mut cells = initial_cells;
    while cells.len() < 7 && start.elapsed() < Duration::from_secs(5) {
        sleep(Duration::from_millis(50)).await;
        cells = handle.get_cells();
    }

    assert_eq!(
        cells.len(),
        7,
        "should receive all 7 cells via streaming load"
    );

    // Verify cell ordering
    let ids: Vec<&str> = cells.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, vec!["c1", "c2", "c3", "c4", "c5", "c6", "c7"]);

    // Verify cell types
    assert_eq!(cells[0].cell_type, "code");
    assert_eq!(cells[1].cell_type, "markdown");

    // Verify source content
    assert_eq!(cells[0].source, "x = 1");
    assert_eq!(cells[1].source, "# Header");
    assert_eq!(cells[3].source, "print('hello')");
    assert_eq!(cells[6].source, "import os");

    // Verify outputs are manifest hashes (64-char hex), not raw JSON
    assert_eq!(cells[0].outputs.len(), 1, "c1 should have 1 output");
    let hash = &cells[0].outputs[0];
    assert_eq!(
        hash.len(),
        64,
        "output should be a manifest hash, got: {}",
        hash
    );
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "output should be hex, got: {}",
        hash
    );

    // Verify stream output on c4
    assert_eq!(cells[3].outputs.len(), 1, "c4 should have 1 output");
    let hash = &cells[3].outputs[0];
    assert_eq!(hash.len(), 64, "c4 output should be a manifest hash");

    // Verify execution counts
    assert_eq!(cells[0].execution_count, "1");
    assert_eq!(cells[2].execution_count, "3");

    // Verify c7 (no outputs) has empty outputs list
    assert!(cells[6].outputs.is_empty(), "c7 should have no outputs");

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

/// Test that a second client joining during/after streaming load gets all cells.
#[tokio::test]
async fn test_streaming_load_second_client_joins() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    // Create notebook
    let nb_path = temp_dir.path().join("multi_client.ipynb");
    write_test_ipynb(
        &nb_path,
        &[
            ("a1", "code", "first", vec![]),
            ("a2", "code", "second", vec![]),
            ("a3", "code", "third", vec![]),
        ],
    );

    // First client opens — triggers streaming load
    let (handle1, _rx1, _brx1, _cells1, _meta1, _info1) =
        NotebookSyncClient::connect_open_split(socket_path.clone(), nb_path.clone(), None)
            .await
            .expect("client1 should connect");

    // Wait for streaming load to complete
    let start = std::time::Instant::now();
    while handle1.get_cells().len() < 3 && start.elapsed() < Duration::from_secs(5) {
        sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        handle1.get_cells().len(),
        3,
        "client1 should have all cells"
    );

    // Second client opens the same file — should join the existing room
    let (handle2, _rx2, _brx2, _cells2, _meta2, info2) =
        NotebookSyncClient::connect_open_split(socket_path.clone(), nb_path.clone(), None)
            .await
            .expect("client2 should connect");

    // Room already loaded, so handshake may report cells > 0 or 0 depending
    // on whether the room was found with existing cells. Either way, the
    // second client should converge to the full cell set via sync.
    let start = std::time::Instant::now();
    let mut cells2 = handle2.get_cells();
    while cells2.len() < 3 && start.elapsed() < Duration::from_secs(5) {
        sleep(Duration::from_millis(50)).await;
        cells2 = handle2.get_cells();
    }

    assert_eq!(cells2.len(), 3, "client2 should see all 3 cells");
    let ids: Vec<&str> = cells2.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, vec!["a1", "a2", "a3"]);
    assert_eq!(cells2[0].source, "first");

    // Both clients see the same notebook_id (canonical path)
    assert_eq!(
        handle1.notebook_id(),
        handle2.notebook_id(),
        "both clients should share the same room"
    );

    // Shutdown
    drop(handle1);
    drop(handle2);
    let _ = info2; // suppress unused warning
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}
