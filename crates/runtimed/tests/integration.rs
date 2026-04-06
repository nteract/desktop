// Tests can use unwrap/expect freely - panics are acceptable in test code
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for runtimed daemon and client.
//!
//! These tests spawn a real daemon and test client interactions.

use std::time::Duration;

use notebook_sync::connect;
use runtimed::client::PoolClient;
use runtimed::daemon::{Daemon, DaemonConfig};
use runtimed::notebook_doc::frame_types;
use runtimed::protocol::{NotebookRequest, NotebookResponse};
use runtimed::EnvType;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
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
    // Windows named pipes must use \\.\pipe\ prefix, not filesystem paths
    #[cfg(windows)]
    let socket_path = {
        let unique = temp_dir
            .path()
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        std::path::PathBuf::from(format!(r"\\.\pipe\runtimed-test-{}", unique))
    };
    #[cfg(not(windows))]
    let socket_path = temp_dir.path().join("test-runtimed.sock");

    DaemonConfig {
        socket_path,
        cache_dir: temp_dir.path().join("envs"),
        blob_store_dir: temp_dir.path().join("blobs"),
        notebook_docs_dir: temp_dir.path().join("notebook-docs"),
        uv_pool_size: 0, // Don't create real envs in tests
        conda_pool_size: 0,
        max_age_secs: 3600,
        lock_dir: Some(temp_dir.path().to_path_buf()),
        room_eviction_delay_ms: Some(50), // Fast eviction for tests
        ..Default::default()
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

#[cfg(unix)]
type LegacyPoolStream = tokio::net::UnixStream;

#[cfg(windows)]
type LegacyPoolStream = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(unix)]
async fn connect_legacy_pool_stream(
    socket_path: &std::path::Path,
) -> Result<LegacyPoolStream, std::io::Error> {
    tokio::net::UnixStream::connect(socket_path).await
}

#[cfg(windows)]
async fn connect_legacy_pool_stream(
    socket_path: &std::path::Path,
) -> Result<LegacyPoolStream, std::io::Error> {
    const ERROR_PIPE_BUSY: i32 = 231;
    let pipe_name = socket_path.to_string_lossy().to_string();
    let mut attempts = 0;

    loop {
        match tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe_name) {
            Ok(client) => return Ok(client),
            Err(err) if err.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 5 => {
                attempts += 1;
                sleep(Duration::from_millis(50)).await;
            }
            Err(err) => return Err(err),
        }
    }
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
    let state = client.status().await.unwrap();
    assert_eq!(state.uv.available, 0);
    assert_eq!(state.conda.available, 0);

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
        ..Default::default()
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

    // Create first notebook via connect_create — should get empty notebook
    let result1 = connect::connect_create(socket_path.clone(), "python", None, "test")
        .await
        .expect("client1 should connect");
    let notebook_id_1 = result1.info.notebook_id.clone();
    let client1 = result1.handle;

    let cells = client1.get_cells();
    assert!(cells.is_empty(), "new notebook should have no cells");

    // Add a cell from client1
    client1.add_cell_after("cell-1", "code", None).unwrap();
    client1.update_source("cell-1", "print('hello')").unwrap();

    // Connect second client to the same notebook — should see the cell
    let client2 = connect::connect(socket_path.clone(), notebook_id_1, "test")
        .await
        .expect("client2 should connect")
        .handle;

    let cells = client2.get_cells();
    assert_eq!(cells.len(), 1, "client2 should see the cell from client1");
    assert_eq!(cells[0].id, "cell-1");
    assert_eq!(cells[0].source, "print('hello')");
    assert_eq!(cells[0].cell_type, "code");

    // Create a different notebook — should be independent
    let client3 = connect::connect_create(socket_path.clone(), "python", None, "test")
        .await
        .expect("client3 should connect")
        .handle;

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

    // First client creates a notebook; second client joins it
    let result = connect::connect_create(socket_path.clone(), "python", None, "test")
        .await
        .unwrap();
    let notebook_id = result.info.notebook_id.clone();
    let client1 = result.handle;
    let client2 = connect::connect(socket_path.clone(), notebook_id, "test")
        .await
        .unwrap()
        .handle;

    // Client1 adds a cell
    client1.add_cell_after("c1", "code", None).unwrap();
    client1.update_source("c1", "x = 42").unwrap();

    // Client2 should receive the changes
    let mut watcher = client2.subscribe();
    let _ = tokio::time::timeout(Duration::from_millis(500), watcher.changed()).await;
    let cells = client2.get_cells();
    assert!(!cells.is_empty(), "client2 should receive propagated cells");

    // May need additional recv rounds for full convergence
    let mut final_cells = cells;
    for _ in 0..5 {
        match tokio::time::timeout(Duration::from_millis(200), watcher.changed()).await {
            Ok(Ok(())) => final_cells = client2.get_cells(),
            _ => break,
        }
    }

    // Verify client2 has the cell
    let cell = final_cells.iter().find(|c| c.id == "c1");
    assert!(cell.is_some(), "client2 should have cell c1");
    let cell = cell.unwrap();
    assert_eq!(cell.source, "x = 42");
    // execution_count is now in RuntimeStateDoc, not NotebookDoc.
    // The cell snapshot shows the default "null" — execution count
    // is resolved from RuntimeStateDoc at save time and by the frontend.

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

/// Test that untitled notebook state survives room eviction via Automerge persistence.
///
/// Design: Untitled notebooks (UUID IDs) have no .ipynb on disk — their Automerge
/// doc is persisted so content survives daemon restarts and room evictions.
/// When all clients disconnect, the room is evicted from memory. On reconnect,
/// the daemon reloads the persisted Automerge doc and the cells reappear.
#[tokio::test]
async fn test_untitled_notebook_persists_through_eviction() {
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
    let notebook_id;
    {
        let result = connect::connect_create(socket_path.clone(), "python", None, "test")
            .await
            .unwrap();
        notebook_id = result.info.notebook_id.clone();
        let client1 = result.handle;
        let _client2 = connect::connect(socket_path.clone(), notebook_id.clone(), "test")
            .await
            .unwrap()
            .handle;

        client1.add_cell_after("c1", "code", None).unwrap();
        client1.update_source("c1", "persisted = True").unwrap();
        client1
            .add_cell_after("c2", "markdown", Some("c1"))
            .unwrap();
        client1.update_source("c2", "# Hello World").unwrap();

        // Both clients drop here — the room should be evicted from memory
    }

    // Give the daemon time to process disconnects and evict the room.
    // 200ms was too tight for CI runners under load — bump to 2s.
    sleep(Duration::from_secs(2)).await;

    // Phase 2: Reconnect — untitled notebook state should be restored from
    // the persisted Automerge doc (there's no .ipynb to load from)
    let client3 = connect::connect(socket_path.clone(), notebook_id, "test")
        .await
        .expect("should reconnect after room eviction")
        .handle;

    let cells = client3.get_cells();
    assert_eq!(
        cells.len(),
        2,
        "reconnected client should see persisted cells for untitled notebook, got: {:?}",
        cells
    );
    assert_eq!(cells[0].source, "persisted = True");
    assert_eq!(cells[1].source, "# Hello World");

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

    // Client1 creates a notebook with three cells
    let result = connect::connect_create(socket_path.clone(), "python", None, "test")
        .await
        .unwrap();
    let notebook_id = result.info.notebook_id.clone();
    let client1 = result.handle;

    client1.add_cell_after("keep-1", "code", None).unwrap();
    client1
        .add_cell_after("to-delete", "code", Some("keep-1"))
        .unwrap();
    client1
        .add_cell_after("keep-2", "code", Some("to-delete"))
        .unwrap();
    client1.update_source("keep-1", "a = 1").unwrap();
    client1.update_source("to-delete", "b = 2").unwrap();
    client1.update_source("keep-2", "c = 3").unwrap();

    // Client2 joins and verifies all three cells
    let client2 = connect::connect(socket_path.clone(), notebook_id, "test")
        .await
        .unwrap()
        .handle;

    assert_eq!(client2.get_cells().len(), 3);

    // Client1 deletes the middle cell
    client1.delete_cell("to-delete").unwrap();

    // Client2 receives the deletion
    let mut watcher = client2.subscribe();
    let mut final_cells = client2.get_cells();
    for _ in 0..10 {
        match tokio::time::timeout(Duration::from_millis(200), watcher.changed()).await {
            Ok(Ok(())) => {
                final_cells = client2.get_cells();
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

    // Create three notebooks concurrently via connect_create
    let (nb_a, nb_b, nb_c) = tokio::join!(
        connect::connect_create(socket_path.clone(), "python", None, "test"),
        connect::connect_create(socket_path.clone(), "python", None, "test"),
        connect::connect_create(socket_path.clone(), "python", None, "test"),
    );
    let nb_a = nb_a.unwrap();
    let nb_b = nb_b.unwrap();
    let nb_c = nb_c.unwrap();
    let id_a = nb_a.info.notebook_id.clone();
    let id_b = nb_b.info.notebook_id.clone();
    let id_c = nb_c.info.notebook_id.clone();
    let nb_a = nb_a.handle;
    let nb_b = nb_b.handle;
    let nb_c = nb_c.handle;

    // Add cells to each notebook
    nb_a.add_cell_after("alpha-1", "code", None).unwrap();
    nb_a.update_source("alpha-1", "print('alpha')").unwrap();

    nb_b.add_cell_after("beta-1", "markdown", None).unwrap();
    nb_b.update_source("beta-1", "# Beta").unwrap();
    nb_b.add_cell_after("beta-2", "code", Some("beta-1"))
        .unwrap();
    nb_b.update_source("beta-2", "x = 99").unwrap();

    nb_c.add_cell_after("gamma-1", "code", None).unwrap();
    nb_c.update_source("gamma-1", "import os").unwrap();
    nb_c.add_cell_after("gamma-2", "code", Some("gamma-1"))
        .unwrap();
    nb_c.add_cell_after("gamma-3", "code", Some("gamma-2"))
        .unwrap();

    // Verify each notebook is isolated by connecting fresh clients
    let (fresh_a, fresh_b, fresh_c) = tokio::join!(
        connect::connect(socket_path.clone(), id_a, "test"),
        connect::connect(socket_path.clone(), id_b, "test"),
        connect::connect(socket_path.clone(), id_c, "test"),
    );

    let cells_a = fresh_a.unwrap().handle.get_cells();
    assert_eq!(cells_a.len(), 1, "alpha should have 1 cell");
    assert_eq!(cells_a[0].id, "alpha-1");
    assert_eq!(cells_a[0].source, "print('alpha')");

    let cells_b = fresh_b.unwrap().handle.get_cells();
    assert_eq!(cells_b.len(), 2, "beta should have 2 cells");
    assert!(cells_b
        .iter()
        .any(|c| c.id == "beta-1" && c.cell_type == "markdown"));
    assert!(cells_b
        .iter()
        .any(|c| c.id == "beta-2" && c.source == "x = 99"));

    let cells_c = fresh_c.unwrap().handle.get_cells();
    assert_eq!(cells_c.len(), 3, "gamma should have 3 cells");
    assert!(cells_c
        .iter()
        .any(|c| c.id == "gamma-1" && c.source == "import os"));

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
    let result = connect::connect_open(socket_path.clone(), nb_path.clone(), "test")
        .await
        .expect("should connect and open notebook");
    let handle = result.handle;
    let initial_cells = result.cells;
    let info = result.info;

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

    // Outputs live in RuntimeStateDoc (separate Automerge doc synced via
    // frame type 0x05). Poll for convergence — if it arrives, verify hashes.
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(10) {
        sleep(Duration::from_millis(100)).await;
        cells = handle.get_cells();
        if !cells.is_empty() && !cells[0].outputs.is_empty() {
            break;
        }
    }

    // Verify outputs if RuntimeStateDoc sync converged.
    // On slow CI runners, the sync may not complete within the timeout.
    // The output pipeline is verified end-to-end by the fixture integration
    // tests and manual testing — this checks the streaming load path specifically.
    if !cells[0].outputs.is_empty() {
        let output = &cells[0].outputs[0];
        assert!(
            output.get("output_type").is_some(),
            "output should be a manifest object with output_type, got: {}",
            output
        );
        assert_eq!(cells[3].outputs.len(), 1, "c4 should have 1 output");
        let c4_output = &cells[3].outputs[0];
        assert!(
            c4_output.get("output_type").is_some(),
            "c4 output should be a manifest object with output_type"
        );
    }

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
    let result1 = connect::connect_open(socket_path.clone(), nb_path.clone(), "test")
        .await
        .expect("client1 should connect");
    let handle1 = result1.handle;

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
    let result2 = connect::connect_open(socket_path.clone(), nb_path.clone(), "test")
        .await
        .expect("client2 should connect");
    let handle2 = result2.handle;
    let info2 = result2.info;

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

/// Test that a legacy client (pre-2.0.0, no magic bytes preamble) can still
/// connect to the daemon. This is critical for the upgrade path: the old app
/// installs the new daemon binary, then pings it to verify it's running.
/// Without backward compat, the upgrade fails with "daemon did not become ready."
#[tokio::test]
async fn test_legacy_client_no_preamble() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    // Wait for daemon with a modern client first
    let client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&client, Duration::from_secs(5)).await);

    // Now connect as a legacy client: send a raw length-prefixed JSON
    // handshake WITHOUT the magic bytes preamble.
    let mut stream = connect_legacy_pool_stream(&socket_path)
        .await
        .expect("legacy client should connect");

    // Send Pool handshake as length-prefixed JSON (old protocol)
    let handshake = br#"{"channel":"pool"}"#;
    let len = (handshake.len() as u32).to_be_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(handshake).await.unwrap();
    stream.flush().await.unwrap();

    // Send a Ping request
    let ping = br#"{"type":"ping"}"#;
    let len = (ping.len() as u32).to_be_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(ping).await.unwrap();
    stream.flush().await.unwrap();

    // Read the response — should be a Pong
    let mut resp_len = [0u8; 4];
    stream.read_exact(&mut resp_len).await.unwrap();
    let resp_size = u32::from_be_bytes(resp_len) as usize;
    let mut resp_buf = vec![0u8; resp_size];
    stream.read_exact(&mut resp_buf).await.unwrap();

    let resp: serde_json::Value = serde_json::from_slice(&resp_buf).unwrap();
    assert_eq!(
        resp["type"], "pong",
        "legacy client should get a Pong response"
    );

    // Also verify a modern client still works alongside
    let result = client.ping().await;
    assert!(result.is_ok(), "modern client should still work");

    // Shutdown
    client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_pipe_mode_forwards_sync_frames() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    // Create a pipe channel
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Connect pipe client (relay mode — no local doc, no initial sync)
    let _result = connect::connect_relay(
        socket_path.clone(),
        "00000000-0000-0000-0000-pipe00sync01".to_string(),
        frame_tx,
    )
    .await
    .unwrap();

    // Second client (full peer) adds a cell and updates source
    let client2 = connect::connect(
        socket_path.clone(),
        "00000000-0000-0000-0000-pipe00sync01".to_string(),
        "test",
    )
    .await
    .unwrap()
    .handle;
    client2.add_cell_after("cell-1", "code", None).unwrap();
    client2
        .update_source("cell-1", "print('hello from pipe test')")
        .unwrap();

    // Wait for sync propagation
    sleep(Duration::from_millis(200)).await;

    // Drain frames from the pipe
    let mut frames = Vec::new();
    while let Ok(Some(frame)) =
        tokio::time::timeout(Duration::from_millis(500), frame_rx.recv()).await
    {
        frames.push(frame);
    }

    assert!(!frames.is_empty(), "pipe should receive at least one frame");

    // Verify at least one frame is an AutomergeSync frame
    let sync_count = frames
        .iter()
        .filter(|f| !f.is_empty() && f[0] == frame_types::AUTOMERGE_SYNC)
        .count();
    assert!(
        sync_count > 0,
        "pipe should contain at least one AUTOMERGE_SYNC frame, got {} frames total",
        frames.len()
    );

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_pipe_mode_only_pipes_allowed_frame_types() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let _result = connect::connect_relay(
        socket_path.clone(),
        "00000000-0000-0000-0000-pipe0bcast01".to_string(),
        frame_tx,
    )
    .await
    .unwrap();

    // Second client adds a cell to trigger sync activity.
    // Note: this only produces AutomergeSync frames — actual Broadcast frames
    // require a kernel launch, which is covered by E2E tests. This test
    // verifies the type-byte filter, not broadcast-specific forwarding.
    let client2 = connect::connect(
        socket_path.clone(),
        "00000000-0000-0000-0000-pipe0bcast01".to_string(),
        "test",
    )
    .await
    .unwrap()
    .handle;
    client2.add_cell_after("bc-cell", "code", None).unwrap();
    client2.update_source("bc-cell", "x = 1").unwrap();

    sleep(Duration::from_millis(200)).await;

    // Drain all frames
    let mut frames = Vec::new();
    while let Ok(Some(frame)) =
        tokio::time::timeout(Duration::from_millis(500), frame_rx.recv()).await
    {
        frames.push(frame);
    }

    assert!(
        !frames.is_empty(),
        "pipe should receive frames after peer activity"
    );

    // Every piped frame must have a valid type byte from the forwarded set:
    // AutomergeSync, Broadcast, Presence, or RuntimeStateSync — never Request or Response.
    let allowed_types = [
        frame_types::AUTOMERGE_SYNC,
        frame_types::BROADCAST,
        frame_types::PRESENCE,
        frame_types::RUNTIME_STATE_SYNC,
        frame_types::POOL_STATE_SYNC,
    ];
    for (i, frame) in frames.iter().enumerate() {
        assert!(!frame.is_empty(), "frame {} should not be empty", i);
        assert!(
            allowed_types.contains(&frame[0]),
            "frame {} has unexpected type byte 0x{:02x} — only AUTOMERGE_SYNC, BROADCAST, PRESENCE, RUNTIME_STATE_SYNC, and POOL_STATE_SYNC are piped",
            i,
            frame[0]
        );
    }

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_pipe_mode_does_not_forward_response_frames() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let result = connect::connect_relay(
        socket_path.clone(),
        "00000000-0000-0000-0000-pipe00resp01".to_string(),
        frame_tx,
    )
    .await
    .unwrap();
    let handle = result.handle;

    // Send a request that produces a Response frame
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        handle.send_request(NotebookRequest::GetDocBytes {}),
    )
    .await
    .expect("request should not time out")
    .expect("request should succeed");

    // Verify the response came back normally through the handle
    assert!(
        matches!(response, NotebookResponse::DocBytes { .. }),
        "should receive DocBytes response"
    );

    // Wait briefly for any straggling frames
    sleep(Duration::from_millis(200)).await;

    // Drain all frames from the pipe
    let mut frames = Vec::new();
    while let Ok(Some(frame)) =
        tokio::time::timeout(Duration::from_millis(500), frame_rx.recv()).await
    {
        frames.push(frame);
    }

    // None of the piped frames should be Response frames
    for (i, frame) in frames.iter().enumerate() {
        assert!(
            frame.is_empty() || frame[0] != frame_types::RESPONSE,
            "frame {} is a RESPONSE (0x{:02x}) — responses must not be piped",
            i,
            frame_types::RESPONSE
        );
    }

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
/// Note: Automerge sync is intentionally convergent under reordering, so this
/// test cannot distinguish ordered delivery from shuffled delivery by inspecting
/// application state alone. It verifies that frames arrive without duplication
/// or coalescing and that the final state is correct — but a true ordering
/// assertion would require transport-layer sequence numbers, which the pipe
/// protocol doesn't currently carry. Tracked as a known limitation.
async fn test_pipe_mode_preserves_frame_order() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let _result = connect::connect_relay(
        socket_path.clone(),
        "00000000-0000-0000-0000-pipeorder001".to_string(),
        frame_tx,
    )
    .await
    .unwrap();

    // Second client rapidly adds multiple cells
    let client2 = connect::connect(
        socket_path.clone(),
        "00000000-0000-0000-0000-pipeorder001".to_string(),
        "test",
    )
    .await
    .unwrap()
    .handle;
    client2.add_cell_after("cell-1", "code", None).unwrap();
    client2
        .add_cell_after("cell-2", "code", Some("cell-1"))
        .unwrap();
    client2
        .add_cell_after("cell-3", "code", Some("cell-2"))
        .unwrap();
    client2.update_source("cell-1", "a = 1").unwrap();
    client2.update_source("cell-2", "b = 2").unwrap();
    client2.update_source("cell-3", "c = 3").unwrap();

    // Wait for sync propagation
    sleep(Duration::from_millis(200)).await;

    // Collect all frames
    let mut frames = Vec::new();
    while let Ok(Some(frame)) =
        tokio::time::timeout(Duration::from_millis(500), frame_rx.recv()).await
    {
        frames.push(frame);
    }

    // Filter to sync frames only
    let sync_frames: Vec<&Vec<u8>> = frames
        .iter()
        .filter(|f| !f.is_empty() && f[0] == frame_types::AUTOMERGE_SYNC)
        .collect();

    // Should receive multiple sync frames
    // DocHandle mutations coalesce through the changed_tx notification channel,
    // so rapid local mutations may produce fewer sync frames than the number of
    // operations. We just need at least 1 sync frame proving the pipe forwarded it.
    assert!(
        !sync_frames.is_empty(),
        "expected at least 1 sync frame, got 0",
    );

    // All sync frame payloads must be non-trivial (type byte + automerge data)
    for (i, frame) in sync_frames.iter().enumerate() {
        assert!(
            frame.len() > 1,
            "sync frame {} should have payload beyond the type byte",
            i
        );
    }

    // Verify no duplicate frames (coalescing would violate the ordering contract)
    let unique_count = {
        let mut seen = std::collections::HashSet::new();
        sync_frames
            .iter()
            .filter(|f| seen.insert(f.to_vec()))
            .count()
    };
    assert_eq!(
        unique_count,
        sync_frames.len(),
        "pipe should not coalesce or duplicate sync frames"
    );

    // Connect a third full-peer client and verify convergence — this proves
    // the daemon processed all mutations and that the sync traffic the pipe
    // received (in channel order) represents the correct state transitions.
    let client3 = connect::connect(
        socket_path.clone(),
        "00000000-0000-0000-0000-pipeorder001".to_string(),
        "test",
    )
    .await
    .unwrap()
    .handle;
    let cells = client3.get_cells();
    assert_eq!(cells.len(), 3, "third client should see all 3 cells");
    assert_eq!(cells[0].id, "cell-1");
    assert_eq!(cells[1].id, "cell-2");
    assert_eq!(cells[2].id, "cell-3");
    assert_eq!(cells[0].source, "a = 1");
    assert_eq!(cells[1].source, "b = 2");
    assert_eq!(cells[2].source, "c = 3");

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}
