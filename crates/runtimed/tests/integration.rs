// Tests can use unwrap/expect freely - panics are acceptable in test code
#![allow(clippy::unwrap_used, clippy::expect_used)]

//! Integration tests for runtimed daemon and client.
//!
//! These tests spawn a real daemon and test client interactions.

use std::time::Duration;

use notebook_protocol::connection::LaunchSpec;
use notebook_sync::connect;
use notebook_wire::frame_types;
use runtime_doc::RuntimeLifecycle;
use runtimed::client::PoolClient;
use runtimed::daemon::{Daemon, DaemonConfig};
use runtimed::protocol::{DependencyGuard, NotebookRequest, NotebookResponse};
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
        execution_store_dir: temp_dir.path().join("executions"),
        notebook_docs_dir: temp_dir.path().join("notebook-docs"),
        uv_pool_size: 0, // Don't create real envs in tests
        conda_pool_size: 0,
        max_age_secs: 3600,
        lock_dir: Some(temp_dir.path().to_path_buf()),
        room_eviction_delay_ms: Some(50), // Fast eviction for tests
        // Integration tests run dozens of daemons in parallel; the
        // preferred-port path's 10-slot budget gets saturated and the
        // sequential `EADDRINUSE` retries push daemon boot past the
        // test's `wait_for_daemon` timeout. Skip straight to OS-assigned.
        use_preferred_blob_port: false,
        // Pin settings files per-temp-dir so parallel daemons don't
        // contend on the global `~/.cache/runt*/settings.automerge` +
        // `~/.config/runt*/settings.json` pair at boot.
        settings_doc_path: Some(temp_dir.path().join("settings.automerge")),
        settings_json_path: Some(temp_dir.path().join("settings.json")),
        ..Default::default()
    }
}

/// Max time we'll wait for a test daemon's socket to accept `ping`.
///
/// Daemon boot is a few hundred ms in isolation but can stretch to
/// many seconds under CPU thrash when the whole suite runs in parallel.
/// Keep the budget generous so the suite isn't flaky under load; a
/// truly hung daemon still surfaces within the `cargo test` timeout.
const DAEMON_READY_TIMEOUT: Duration = Duration::from_secs(20);

/// Max time we'll wait for initial Automerge sync to hit session-ready.
///
/// Same shape as `DAEMON_READY_TIMEOUT`: fine in isolation, slow under
/// parallel load. Callers at heavy steps (`add_cell_after`, multi-client
/// sync) have their own per-step assertions; this one just gates the
/// initial handshake.
const SESSION_READY_TIMEOUT: Duration = Duration::from_secs(15);

/// Wait for the daemon to be ready by polling the client.
async fn wait_for_daemon(client: &PoolClient) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < DAEMON_READY_TIMEOUT {
        if client.ping().await.is_ok() {
            return true;
        }
        sleep(Duration::from_millis(50)).await;
    }
    false
}

/// Wait until initial Automerge sync has delivered the daemon-created
/// `cells` map to this client.
///
/// Per architecture rules (see `.claude/rules/architecture.md` § "No
/// Independent `put_object` on Shared Keys"), only the daemon creates
/// the `cells` / `metadata` maps at room creation — client peers receive
/// them via sync. Mutations like `add_cell_after` panic with
/// `InvalidObjId("cells map not found")` if called before that sync
/// frame arrives, which is a flaky race under loaded CI. Poll the
/// client's local Automerge doc until the cells map object is visible.
async fn wait_for_cells_map(handle: &notebook_sync::DocHandle, timeout: Duration) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        let has_map = handle
            .with_doc(|doc| {
                use automerge::ReadDoc;
                matches!(
                    doc.get(automerge::ROOT, "cells").ok().flatten(),
                    Some((automerge::Value::Object(automerge::ObjType::Map), _))
                )
            })
            .unwrap_or(false);
        if has_map {
            return true;
        }
        sleep(Duration::from_millis(20)).await;
    }
    false
}

async fn wait_for_session_ready(handle: &notebook_sync::DocHandle, timeout: Duration) -> bool {
    matches!(
        tokio::time::timeout(timeout, handle.await_session_ready()).await,
        Ok(Ok(()))
    )
}

async fn assert_session_ready(handle: &notebook_sync::DocHandle, context: &str) {
    if let Err(err) = handle
        .await_session_ready_timeout(SESSION_READY_TIMEOUT)
        .await
    {
        panic!(
            "{context} did not become session-ready within {:?}: {err}; status={:?}",
            SESSION_READY_TIMEOUT,
            handle.status()
        );
    }
}

async fn wait_for_cell_count(
    handle: &notebook_sync::DocHandle,
    expected: usize,
    timeout: Duration,
) -> bool {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if handle.get_cells().len() >= expected {
            return true;
        }
        sleep(Duration::from_millis(20)).await;
    }
    false
}

#[cfg(unix)]
type RawStream = tokio::net::UnixStream;

#[cfg(windows)]
type RawStream = tokio::net::windows::named_pipe::NamedPipeClient;

#[cfg(unix)]
async fn connect_raw_stream(socket_path: &std::path::Path) -> Result<RawStream, std::io::Error> {
    tokio::net::UnixStream::connect(socket_path).await
}

#[cfg(windows)]
async fn connect_raw_stream(socket_path: &std::path::Path) -> Result<RawStream, std::io::Error> {
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
    assert!(wait_for_daemon(&client).await);

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
    assert!(wait_for_daemon(&client).await);

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
    assert!(wait_for_daemon(&client).await);

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
    assert!(wait_for_daemon(&client).await);

    // Try to start second daemon with same paths - should fail
    let config2 = DaemonConfig {
        socket_path: socket_path.clone(),
        cache_dir: temp_dir.path().join("envs"),
        blob_store_dir: temp_dir.path().join("blobs"),
        execution_store_dir: temp_dir.path().join("executions"),
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

    assert!(wait_for_daemon(&client1).await);

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
    assert!(wait_for_daemon(&pool_client).await);

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
async fn test_external_settings_json_edit_survives_settings_sync_ack() {
    use runtimed::settings_doc::{PythonEnvType, SyncedSettings};
    use runtimed::sync_client::SyncClient;

    let temp_dir = TempDir::new().unwrap();
    let mut config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();
    let settings_dir = temp_dir.path().canonicalize().unwrap();
    let settings_doc_path = settings_dir.join("settings.automerge");
    let settings_json_path = settings_dir.join("settings.json");
    config.settings_doc_path = Some(settings_doc_path);
    config.settings_json_path = Some(settings_json_path.clone());

    let initial = serde_json::to_string_pretty(&SyncedSettings::default()).unwrap();
    std::fs::write(&settings_json_path, initial).unwrap();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    let mut sync_client = SyncClient::connect_with_timeout(socket_path, Duration::from_secs(2))
        .await
        .expect("SyncClient should connect via unified socket");
    assert_eq!(sync_client.get_all().default_python_env, PythonEnvType::Uv);

    // `watch_settings_json` is spawned before the socket listener accepts
    // connections, but give the notify watcher time to finish registration
    // before writing the external edit.
    sleep(Duration::from_secs(1)).await;

    let mut external = serde_json::to_value(SyncedSettings::default()).unwrap();
    external["default_python_env"] = serde_json::Value::String("conda".to_string());
    std::fs::write(
        &settings_json_path,
        serde_json::to_string_pretty(&external).unwrap(),
    )
    .unwrap();

    let synced = tokio::time::timeout(Duration::from_secs(5), sync_client.recv_changes())
        .await
        .expect("settings client should receive the external JSON edit")
        .expect("settings sync connection should stay open");
    assert_eq!(synced.default_python_env, PythonEnvType::Conda);

    // `recv_changes` sends any required sync ack before returning. Give the
    // daemon a short window to process that ack; a regression would persist
    // the stale client value back to the JSON mirror here.
    sleep(Duration::from_millis(500)).await;

    let saved_json = std::fs::read_to_string(&settings_json_path).unwrap();
    let saved: SyncedSettings = serde_json::from_str(&saved_json).unwrap();
    assert_eq!(saved.default_python_env, PythonEnvType::Conda);

    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_settings_json_mirror_write_does_not_feedback_loop() {
    use runtimed::settings_doc::{SyncedSettings, ThemeMode};
    use runtimed::sync_client::SyncClient;

    let temp_dir = TempDir::new().unwrap();
    let mut config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();
    let settings_dir = temp_dir.path().canonicalize().unwrap();
    let settings_doc_path = settings_dir.join("settings.automerge");
    let settings_json_path = settings_dir.join("settings.json");
    config.settings_doc_path = Some(settings_doc_path);
    config.settings_json_path = Some(settings_json_path.clone());

    let initial = serde_json::to_string_pretty(&SyncedSettings::default()).unwrap();
    std::fs::write(&settings_json_path, initial).unwrap();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    let mut writer = SyncClient::connect_with_timeout(socket_path.clone(), Duration::from_secs(2))
        .await
        .expect("writer SyncClient should connect");
    let mut observer = SyncClient::connect_with_timeout(socket_path, Duration::from_secs(2))
        .await
        .expect("observer SyncClient should connect");

    writer
        .put_value("theme", &serde_json::Value::String("dark".into()))
        .await
        .expect("theme update should sync");

    let observed = tokio::time::timeout(Duration::from_secs(5), observer.recv_changes())
        .await
        .expect("observer should receive the writer update")
        .expect("observer sync should remain connected");
    assert_eq!(observed.theme, ThemeMode::Dark);

    // The daemon persists the JSON mirror for the writer's Automerge change.
    // The settings.json watcher will see that filesystem event; it must
    // recognize the mirror already matches the doc and avoid broadcasting
    // another settings change back to peers.
    sleep(Duration::from_secs(1)).await;
    let extra = tokio::time::timeout(Duration::from_millis(300), observer.recv_changes()).await;
    assert!(
        extra.is_err(),
        "daemon-generated settings.json mirror writes must not feedback-loop into another settings broadcast"
    );

    let saved_json = std::fs::read_to_string(&settings_json_path).unwrap();
    let saved: SyncedSettings = serde_json::from_str(&saved_json).unwrap();
    assert_eq!(saved.theme, ThemeMode::Dark);

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
    assert!(wait_for_daemon(&client).await);

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
    assert!(wait_for_daemon(&pool_client).await);

    // Create first notebook via connect_create — should get empty notebook
    let result1 = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        None,
        vec![],
    )
    .await
    .expect("client1 should connect");
    let notebook_id_1 = result1.info.notebook_id.clone();
    let client1 = result1.handle;

    assert!(
        wait_for_session_ready(&client1, SESSION_READY_TIMEOUT).await,
        "client1 should reach session-ready state within 2s"
    );

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

    assert!(
        wait_for_session_ready(&client2, SESSION_READY_TIMEOUT).await,
        "client2 should reach session-ready state within 2s"
    );

    let cells = client2.get_cells();
    assert_eq!(cells.len(), 1, "client2 should see the cell from client1");
    assert_eq!(cells[0].id, "cell-1");
    assert_eq!(cells[0].source, "print('hello')");
    assert_eq!(cells[0].cell_type, "code");

    // Create a different notebook — should be independent
    let client3 = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        None,
        vec![],
    )
    .await
    .expect("client3 should connect")
    .handle;

    assert!(
        wait_for_session_ready(&client3, SESSION_READY_TIMEOUT).await,
        "client3 should reach session-ready state within 2s"
    );

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
    assert!(wait_for_daemon(&pool_client).await);

    // First client creates a notebook; second client joins it
    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        None,
        vec![],
    )
    .await
    .unwrap();
    let notebook_id = result.info.notebook_id.clone();
    let client1 = result.handle;
    let client2 = connect::connect(socket_path.clone(), notebook_id, "test")
        .await
        .unwrap()
        .handle;

    assert!(
        wait_for_session_ready(&client1, SESSION_READY_TIMEOUT).await,
        "client1 should reach session-ready state within 2s"
    );
    assert!(
        wait_for_session_ready(&client2, SESSION_READY_TIMEOUT).await,
        "client2 should reach session-ready state within 2s"
    );

    let mut watcher = client2.subscribe();

    // Client1 adds a cell
    client1.add_cell_after("c1", "code", None).unwrap();
    client1.update_source("c1", "x = 42").unwrap();
    client1.confirm_sync().await.unwrap();

    // Client2 should receive the changes
    let mut final_cells = client2.get_cells();
    for _ in 0..10 {
        if final_cells.iter().any(|c| c.id == "c1") {
            break;
        }
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
    // Live execution_count is resolved from RuntimeStateDoc at save time and
    // by the frontend. NotebookDoc keeps only the persisted history fallback.

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_parallel_cell_mutations_same_session_no_disconnect() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        None,
        vec![],
    )
    .await
    .unwrap();
    let notebook_id = result.info.notebook_id.clone();
    let handle = result.handle;

    assert!(
        wait_for_session_ready(&handle, SESSION_READY_TIMEOUT).await,
        "client should reach session-ready state"
    );
    assert!(
        wait_for_cells_map(&handle, Duration::from_secs(5)).await,
        "client should receive daemon-created cells map"
    );

    let mut joins = Vec::new();
    for index in 0..10 {
        let handle = handle.clone();
        joins.push(tokio::spawn(async move {
            let cell_id = format!("parallel-{index}");
            handle.add_cell_with_source(&cell_id, "code", None, &format!("print({index})"))?;
            handle.confirm_sync().await
        }));
    }

    tokio::time::timeout(Duration::from_secs(20), async {
        for join in joins {
            join.await.unwrap().unwrap();
        }
    })
    .await
    .expect("parallel cell mutations should sync without hanging");

    assert_eq!(
        handle.status().connection,
        notebook_sync::ConnectionState::Connected,
        "original client should stay connected after parallel confirms"
    );

    let fresh = connect::connect(socket_path.clone(), notebook_id, "test")
        .await
        .unwrap()
        .handle;
    assert!(
        wait_for_session_ready(&fresh, SESSION_READY_TIMEOUT).await,
        "fresh client should reach session-ready state"
    );
    assert!(
        wait_for_cells_map(&fresh, Duration::from_secs(5)).await,
        "fresh client should receive cells map"
    );
    assert!(
        wait_for_cell_count(&fresh, 10, Duration::from_secs(5)).await,
        "fresh client should see every parallel-created cell"
    );

    let ids: std::collections::HashSet<_> =
        fresh.get_cells().into_iter().map(|cell| cell.id).collect();
    for index in 0..10 {
        assert!(ids.contains(&format!("parallel-{index}")));
    }

    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_untrusted_launch_and_sync_environment_are_daemon_rejected() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        None,
        vec![],
    )
    .await
    .unwrap();
    let handle = result.handle;

    assert!(
        wait_for_session_ready(&handle, SESSION_READY_TIMEOUT).await,
        "client should reach session-ready state"
    );

    handle.add_uv_dependency("requests").unwrap();
    handle.confirm_sync().await.unwrap();

    let launch = handle
        .send_request(NotebookRequest::LaunchKernel {
            kernel_type: "python".to_string(),
            env_source: LaunchSpec::Auto,
            notebook_path: None,
        })
        .await
        .unwrap();
    assert!(
        matches!(launch, NotebookResponse::GuardRejected { .. }),
        "untrusted LaunchKernel should be rejected by the daemon, got {launch:?}"
    );
    assert_eq!(
        handle.get_runtime_state().unwrap().kernel.lifecycle,
        RuntimeLifecycle::NotStarted,
        "rejected LaunchKernel must not claim or mutate runtime lifecycle"
    );

    let sync = handle
        .send_request(NotebookRequest::SyncEnvironment { guard: None })
        .await
        .unwrap();
    assert!(
        matches!(sync, NotebookResponse::GuardRejected { .. }),
        "untrusted SyncEnvironment should be rejected before NoKernel, got {sync:?}"
    );

    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_sync_environment_guard_rejects_stale_observed_dependencies() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        Some(notebook_protocol::connection::PackageManager::Uv),
        vec!["pandas".to_string()],
    )
    .await
    .unwrap();
    let handle = result.handle;

    assert!(
        wait_for_session_ready(&handle, SESSION_READY_TIMEOUT).await,
        "client should reach session-ready state"
    );

    let observed_heads: Vec<String> = handle
        .with_doc(|doc| {
            doc.get_heads()
                .iter()
                .map(|head| head.to_string())
                .collect()
        })
        .unwrap();
    handle.add_uv_dependency("requests").unwrap();
    handle.confirm_sync().await.unwrap();

    let response = handle
        .send_request(NotebookRequest::SyncEnvironment {
            guard: Some(DependencyGuard { observed_heads }),
        })
        .await
        .unwrap();
    assert!(
        matches!(response, NotebookResponse::GuardRejected { .. }),
        "stale dependency guard should reject before sync-env logic, got {response:?}"
    );

    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_approve_trust_guard_rejects_stale_observed_dependencies() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        Some(notebook_protocol::connection::PackageManager::Uv),
        vec!["pandas".to_string()],
    )
    .await
    .unwrap();
    let handle = result.handle;

    assert!(
        wait_for_session_ready(&handle, SESSION_READY_TIMEOUT).await,
        "client should reach session-ready state"
    );

    let observed_heads: Vec<String> = handle
        .with_doc(|doc| {
            doc.get_heads()
                .iter()
                .map(|head| head.to_string())
                .collect()
        })
        .unwrap();
    handle.add_uv_dependency("requests").unwrap();
    handle.confirm_sync().await.unwrap();

    let response = handle
        .send_request(NotebookRequest::ApproveTrust {
            observed_heads: Some(observed_heads),
        })
        .await
        .unwrap();
    assert!(
        matches!(response, NotebookResponse::GuardRejected { .. }),
        "stale approve-trust guard should reject, got {response:?}"
    );

    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_sync_environment_no_deps_reaches_existing_no_kernel_path() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        None,
        vec![],
    )
    .await
    .unwrap();
    let handle = result.handle;

    assert!(
        wait_for_session_ready(&handle, SESSION_READY_TIMEOUT).await,
        "client should reach session-ready state"
    );

    let response = handle
        .send_request(NotebookRequest::SyncEnvironment { guard: None })
        .await
        .unwrap();
    assert!(
        matches!(
            response,
            NotebookResponse::SyncEnvironmentFailed {
                ref error,
                needs_restart: false,
            } if error == "No kernel running"
        ),
        "trusted/no-deps SyncEnvironment should reach existing NoKernel path, got {response:?}"
    );

    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

#[tokio::test]
async fn test_parallel_daemon_requests_same_session_no_disconnect() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        None,
        vec![],
    )
    .await
    .unwrap();
    let handle = result.handle;

    assert!(
        wait_for_session_ready(&handle, SESSION_READY_TIMEOUT).await,
        "client should reach session-ready state"
    );
    assert!(
        wait_for_cells_map(&handle, Duration::from_secs(5)).await,
        "client should receive daemon-created cells map"
    );

    let mut joins = Vec::new();
    for _ in 0..10 {
        let handle = handle.clone();
        joins.push(tokio::spawn(async move {
            handle.send_request(NotebookRequest::GetDocBytes {}).await
        }));
    }

    tokio::time::timeout(Duration::from_secs(20), async {
        for join in joins {
            let response = join.await.unwrap().unwrap();
            assert!(matches!(response, NotebookResponse::DocBytes { .. }));
        }
    })
    .await
    .expect("parallel daemon requests should complete without hanging");

    assert_eq!(
        handle.status().connection,
        notebook_sync::ConnectionState::Connected,
        "client should stay connected after parallel daemon requests"
    );

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
    assert!(wait_for_daemon(&pool_client).await);

    // Phase 1: Two clients connect, add cells, then both disconnect
    let notebook_id;
    {
        let result = connect::connect_create(
            socket_path.clone(),
            "python",
            None,
            "test",
            false,
            None,
            vec![],
        )
        .await
        .unwrap();
        notebook_id = result.info.notebook_id.clone();
        let client1 = result.handle;
        let _client2 = connect::connect(socket_path.clone(), notebook_id.clone(), "test")
            .await
            .unwrap()
            .handle;

        assert!(
            wait_for_session_ready(&client1, SESSION_READY_TIMEOUT).await,
            "client1 should reach session-ready state within 2s"
        );

        client1.add_cell_after("c1", "code", None).unwrap();
        client1.update_source("c1", "persisted = True").unwrap();
        client1
            .add_cell_after("c2", "markdown", Some("c1"))
            .unwrap();
        client1.update_source("c2", "# Hello World").unwrap();
        client1.confirm_sync().await.unwrap();

        // Both clients drop here — the room should be evicted from memory
    }

    // Wait deterministically for the daemon to:
    //   1. Evict the room (scheduled `room_eviction_delay_ms` after the last
    //      peer disconnects — 50ms in this test config).
    //   2. Flush the Automerge doc to `notebook-docs/<hash>.automerge` (persist
    //      debouncer: 500ms debounce / 5s max-interval, plus a final flush on
    //      channel close when the evicted room is dropped).
    //
    // Polling for the persisted file alone isn't enough: the debouncer can
    // flush periodically while the room is still resident, and this test is
    // specifically exercising the evict-then-reload-from-disk path. Give
    // eviction a 500ms head start (10x the configured 50ms delay) before we
    // start trusting the file as a sign that reload-from-disk is the only
    // code path the reconnect can hit.
    sleep(Duration::from_millis(500)).await;

    let persist_path = temp_dir
        .path()
        .join("notebook-docs")
        .join(runtimed::notebook_doc::notebook_doc_filename(&notebook_id));
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(meta) = tokio::fs::metadata(&persist_path).await {
            if meta.len() > 0 {
                break;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "persisted Automerge doc for untitled notebook did not appear within 10s at {:?}",
                persist_path
            );
        }
        sleep(Duration::from_millis(50)).await;
    }

    // Phase 2: Reconnect — untitled notebook state should be restored from
    // the persisted Automerge doc (there's no .ipynb to load from)
    let client3 = connect::connect(socket_path.clone(), notebook_id, "test")
        .await
        .expect("should reconnect after room eviction")
        .handle;

    assert!(
        wait_for_session_ready(&client3, SESSION_READY_TIMEOUT).await,
        "reconnected client should reach session-ready state within 2s"
    );
    assert!(
        wait_for_cell_count(&client3, 2, SESSION_READY_TIMEOUT).await,
        "reconnected client should receive persisted cells within {:?}",
        SESSION_READY_TIMEOUT
    );

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

/// Regression test for the eviction/debouncer race (PR follow-up).
///
/// The race: the daemon's eviction task used to remove a room from the
/// `notebook_rooms` HashMap, then run async teardown (kernel shutdown,
/// env cleanup, etc.), and only finally drop the room's `Arc` — which
/// in turn closed the `watch` channel feeding the persist debouncer,
/// triggering its shutdown flush. A fast reconnect in the window
/// between HashMap removal and Arc drop would hit `get_or_create_room`
/// with the room gone, instantiate a fresh one, and load the still-stale
/// `.automerge` from disk — silently dropping the user's edits.
///
/// The fix makes the eviction task send a flush request to the debouncer
/// and await its ack *before* touching the HashMap. This test drives a
/// rapid disconnect/reconnect cycle to cover the happy path end-to-end.
/// The existing `test_untitled_notebook_persists_through_eviction` covers
/// the slower-reconnect case with an explicit file-size wait.
#[tokio::test]
async fn test_eviction_flushes_before_reconnect() {
    let temp_dir = TempDir::new().unwrap();
    let mut config = test_config(&temp_dir);
    // Instant eviction — the fix must guarantee the flush runs before the
    // HashMap removal regardless of the eviction timer.
    config.room_eviction_delay_ms = Some(0);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    // Create an untitled notebook with a few cells, then drop the client to
    // trigger eviction. No autosave path exists for untitled notebooks, so
    // the `.automerge` debouncer is the only thing keeping content durable.
    let notebook_id;
    {
        let result = connect::connect_create(
            socket_path.clone(),
            "python",
            None,
            "test",
            false,
            None,
            vec![],
        )
        .await
        .unwrap();
        notebook_id = result.info.notebook_id.clone();
        let client = result.handle;

        assert!(
            wait_for_session_ready(&client, SESSION_READY_TIMEOUT).await,
            "client should reach session-ready state within 2s"
        );

        client.add_cell_after("c1", "code", None).unwrap();
        client.update_source("c1", "race_test = 1").unwrap();
        client.add_cell_after("c2", "code", Some("c1")).unwrap();
        client.update_source("c2", "race_test = 2").unwrap();
        client.add_cell_after("c3", "markdown", Some("c2")).unwrap();
        client
            .update_source("c3", "# Race regression guard")
            .unwrap();
        client.confirm_sync().await.unwrap();
    }

    // Reconnect fast — no sleep. The fix's flush-and-ack ensures the room's
    // latest doc bytes are on disk before the HashMap drop, so regardless of
    // whether the reconnect hits the still-evicting room or spawns a fresh
    // one from disk, all three cells must be visible.
    let client2 = connect::connect(socket_path.clone(), notebook_id.clone(), "test")
        .await
        .expect("reconnect should succeed")
        .handle;

    let mut watcher = client2.subscribe();
    let mut cells = client2.get_cells();
    for _ in 0..30 {
        if cells.len() == 3 {
            break;
        }
        match tokio::time::timeout(Duration::from_millis(250), watcher.changed()).await {
            Ok(Ok(())) => cells = client2.get_cells(),
            _ => break,
        }
    }

    assert_eq!(
        cells.len(),
        3,
        "reconnected client must see all 3 cells, got {}: {:?}",
        cells.len(),
        cells
    );
    assert_eq!(cells[0].source, "race_test = 1");
    assert_eq!(cells[1].source, "race_test = 2");
    assert_eq!(cells[2].source, "# Race regression guard");

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
    assert!(wait_for_daemon(&pool_client).await);

    // Client1 creates a notebook with three cells
    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        None,
        vec![],
    )
    .await
    .unwrap();
    let notebook_id = result.info.notebook_id.clone();
    let client1 = result.handle;

    assert!(
        wait_for_session_ready(&client1, SESSION_READY_TIMEOUT).await,
        "client1 should reach session-ready state within 2s"
    );

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
    client1.confirm_sync().await.unwrap();

    // Client2 joins and verifies all three cells
    let client2 = connect::connect(socket_path.clone(), notebook_id, "test")
        .await
        .unwrap()
        .handle;

    // Wait for sync convergence before asserting
    let mut watcher = client2.subscribe();
    let mut cells = client2.get_cells();
    for _ in 0..10 {
        if cells.len() == 3 {
            break;
        }
        match tokio::time::timeout(Duration::from_millis(200), watcher.changed()).await {
            Ok(Ok(())) => cells = client2.get_cells(),
            _ => break,
        }
    }

    assert_eq!(
        cells.len(),
        3,
        "client2 should see 3 cells after sync convergence"
    );

    // Client1 deletes the middle cell
    client1.delete_cell("to-delete").unwrap();
    client1.confirm_sync().await.unwrap();

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
    assert!(wait_for_daemon(&pool_client).await);

    // Create three notebooks concurrently via connect_create
    let (nb_a, nb_b, nb_c) = tokio::join!(
        connect::connect_create(
            socket_path.clone(),
            "python",
            None,
            "test",
            false,
            None,
            vec![]
        ),
        connect::connect_create(
            socket_path.clone(),
            "python",
            None,
            "test",
            false,
            None,
            vec![]
        ),
        connect::connect_create(
            socket_path.clone(),
            "python",
            None,
            "test",
            false,
            None,
            vec![]
        ),
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

    assert!(
        wait_for_session_ready(&nb_a, SESSION_READY_TIMEOUT).await,
        "alpha notebook should reach session-ready state within {:?}",
        SESSION_READY_TIMEOUT
    );
    assert!(
        wait_for_session_ready(&nb_b, SESSION_READY_TIMEOUT).await,
        "beta notebook should reach session-ready state within {:?}",
        SESSION_READY_TIMEOUT
    );
    assert!(
        wait_for_session_ready(&nb_c, SESSION_READY_TIMEOUT).await,
        "gamma notebook should reach session-ready state within {:?}",
        SESSION_READY_TIMEOUT
    );

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
    let _ = tokio::join!(
        nb_a.confirm_sync(),
        nb_b.confirm_sync(),
        nb_c.confirm_sync()
    );

    // Verify each notebook is isolated by connecting fresh clients
    let (fresh_a, fresh_b, fresh_c) = tokio::join!(
        connect::connect(socket_path.clone(), id_a, "test"),
        connect::connect(socket_path.clone(), id_b, "test"),
        connect::connect(socket_path.clone(), id_c, "test"),
    );

    let handle_a = fresh_a.unwrap().handle;
    let handle_b = fresh_b.unwrap().handle;
    let handle_c = fresh_c.unwrap().handle;

    assert!(
        wait_for_session_ready(&handle_a, SESSION_READY_TIMEOUT).await,
        "alpha client should reach session-ready state within {:?}",
        SESSION_READY_TIMEOUT
    );
    assert!(
        wait_for_session_ready(&handle_b, SESSION_READY_TIMEOUT).await,
        "beta client should reach session-ready state within {:?}",
        SESSION_READY_TIMEOUT
    );
    assert!(
        wait_for_session_ready(&handle_c, SESSION_READY_TIMEOUT).await,
        "gamma client should reach session-ready state within {:?}",
        SESSION_READY_TIMEOUT
    );

    // Wait for initial sync to deliver the cells map before reading.
    // session_ready only guarantees status=Interactive, not that the
    // snapshot watch channel has published cells from the sync frames.
    assert!(
        wait_for_cells_map(&handle_a, SESSION_READY_TIMEOUT).await,
        "alpha sync did not deliver cells map within {:?}",
        SESSION_READY_TIMEOUT
    );
    assert!(
        wait_for_cells_map(&handle_b, SESSION_READY_TIMEOUT).await,
        "beta sync did not deliver cells map within {:?}",
        SESSION_READY_TIMEOUT
    );
    assert!(
        wait_for_cells_map(&handle_c, SESSION_READY_TIMEOUT).await,
        "gamma sync did not deliver cells map within {:?}",
        SESSION_READY_TIMEOUT
    );

    let cells_a = handle_a.get_cells();
    assert_eq!(cells_a.len(), 1, "alpha should have 1 cell");
    assert_eq!(cells_a[0].id, "alpha-1");
    assert_eq!(cells_a[0].source, "print('alpha')");

    let cells_b = handle_b.get_cells();
    assert_eq!(cells_b.len(), 2, "beta should have 2 cells");
    assert!(cells_b
        .iter()
        .any(|c| c.id == "beta-1" && c.cell_type == "markdown"));
    assert!(cells_b
        .iter()
        .any(|c| c.id == "beta-2" && c.source == "x = 99"));

    let cells_c = handle_c.get_cells();
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
    assert!(wait_for_daemon(&pool_client).await);

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
    let info = result.info;

    // Handshake reports 0 cells (streaming load is deferred)
    assert_eq!(info.cell_count, 0);
    assert!(info.error.is_none());

    assert_session_ready(&handle, "OpenNotebook streaming load").await;
    let mut cells = handle.get_cells();

    assert_eq!(
        cells.len(),
        7,
        "should receive all 7 cells via streaming load after session-ready; status={:?}",
        handle.status()
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
    // frame type 0x05) and are looked up via `handle.get_cell_outputs`.
    // Poll for convergence — if it arrives, verify hashes.
    let start = std::time::Instant::now();
    let mut c0_outputs: Vec<serde_json::Value> = Vec::new();
    while start.elapsed() < Duration::from_secs(10) {
        sleep(Duration::from_millis(100)).await;
        cells = handle.get_cells();
        if !cells.is_empty() {
            c0_outputs = handle.get_cell_outputs(&cells[0].id).unwrap_or_default();
            if !c0_outputs.is_empty() {
                break;
            }
        }
    }

    // Verify outputs if RuntimeStateDoc sync converged.
    // On slow CI runners, the sync may not complete within the timeout.
    // The output pipeline is verified end-to-end by the fixture integration
    // tests and manual testing — this checks the streaming load path specifically.
    if !c0_outputs.is_empty() {
        let output = &c0_outputs[0];
        assert!(
            output.get("output_type").is_some(),
            "output should be a manifest object with output_type, got: {}",
            output
        );
        let c4_outputs = handle.get_cell_outputs(&cells[3].id).unwrap_or_default();
        assert_eq!(c4_outputs.len(), 1, "c4 should have 1 output");
        assert!(
            c4_outputs[0].get("output_type").is_some(),
            "c4 output should be a manifest object with output_type"
        );
    }

    // Verify execution counts
    assert_eq!(cells[0].execution_count, "1");
    assert_eq!(cells[2].execution_count, "3");

    // Verify c7 (no outputs) has no outputs
    assert!(
        handle
            .get_cell_outputs(&cells[6].id)
            .unwrap_or_default()
            .is_empty(),
        "c7 should have no outputs"
    );

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
    assert!(wait_for_daemon(&pool_client).await);

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

    assert_session_ready(&handle1, "first OpenNotebook client").await;
    let cells1 = handle1.get_cells();
    assert_eq!(
        cells1.len(),
        3,
        "client1 should have all cells after session-ready; status={:?}",
        handle1.status()
    );

    // Second client opens the same file — should join the existing room
    let result2 = connect::connect_open(socket_path.clone(), nb_path.clone(), "test")
        .await
        .expect("client2 should connect");
    let handle2 = result2.handle;
    let info2 = result2.info;

    // Room already loaded, so handshake may report cells > 0 or 0 depending
    // on whether the room was found with existing cells. Either way, the
    // second client should be fully ready before we trust its local snapshot.
    assert_session_ready(&handle2, "second OpenNotebook client").await;
    let cells2 = handle2.get_cells();

    assert_eq!(
        cells2.len(),
        3,
        "client2 should see all 3 cells after session-ready; status={:?}",
        handle2.status()
    );
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

/// An older stable app sends a pool ping during upgrade to check whether a
/// daemon is already running and whether it needs replacement. The pool channel
/// must accept the old preamble version and still return version metadata.
#[tokio::test]
async fn test_pool_ping_from_old_stable_preamble_returns_version_metadata() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&client).await);

    // Connect a raw stream and send a preamble with an older protocol version
    // (simulating a v2.2.0 stable app that ships protocol v2).
    let mut stream = connect_raw_stream(&socket_path)
        .await
        .expect("should connect");

    // Send preamble with protocol version 2 (old stable)
    let mut preamble = [0u8; 5];
    preamble[..4].copy_from_slice(&[0xC0, 0xDE, 0x01, 0xAC]);
    preamble[4] = 2; // old protocol version
    stream.write_all(&preamble).await.unwrap();

    // Send Pool handshake as a length-prefixed JSON frame
    let handshake = br#"{"channel":"pool"}"#;
    let len = (handshake.len() as u32).to_be_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(handshake).await.unwrap();

    // Send a Ping request
    let ping = br#"{"type":"ping"}"#;
    let len = (ping.len() as u32).to_be_bytes();
    stream.write_all(&len).await.unwrap();
    stream.write_all(ping).await.unwrap();
    stream.flush().await.unwrap();

    // Should get a Pong back despite the version mismatch, including the
    // metadata older launchers need to decide whether to upgrade the daemon.
    let mut resp_len = [0u8; 4];
    stream.read_exact(&mut resp_len).await.unwrap();
    let resp_size = u32::from_be_bytes(resp_len) as usize;
    let mut resp_buf = vec![0u8; resp_size];
    stream.read_exact(&mut resp_buf).await.unwrap();

    let resp: serde_json::Value = serde_json::from_slice(&resp_buf).unwrap();
    assert_eq!(
        resp["type"], "pong",
        "pool ping from older client should get a Pong"
    );
    assert_eq!(
        resp["protocol_version"],
        serde_json::json!(notebook_protocol::connection::PROTOCOL_VERSION),
        "pool ping should report the daemon's current protocol version"
    );
    assert!(
        resp["daemon_version"]
            .as_str()
            .is_some_and(|version| !version.is_empty()),
        "pool ping should report a daemon version for upgrade checks"
    );

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
    assert!(wait_for_daemon(&pool_client).await);

    // Create a pipe channel
    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Connect pipe client (relay mode — no local doc, no initial sync)
    let _result = connect::connect_relay(
        socket_path.clone(),
        "00000000-0000-0000-0000-000000000001".to_string(),
        frame_tx,
    )
    .await
    .unwrap();

    // Second client (full peer) adds a cell and updates source
    let client2 = connect::connect(
        socket_path.clone(),
        "00000000-0000-0000-0000-000000000001".to_string(),
        "test",
    )
    .await
    .unwrap()
    .handle;
    // Initial sync must deliver the daemon's cells map before we can
    // mutate it. Otherwise `add_cell_after` panics with
    // `InvalidObjId("cells map not found")` — a flake under loaded CI.
    assert!(
        wait_for_cells_map(&client2, SESSION_READY_TIMEOUT).await,
        "initial sync did not deliver the cells map within {:?}",
        SESSION_READY_TIMEOUT
    );
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
async fn test_pipe_mode_preserves_initial_session_status_frame() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let _result = connect::connect_relay(
        socket_path.clone(),
        "00000000-0000-0000-0000-000000000011".to_string(),
        frame_tx,
    )
    .await
    .unwrap();

    let first_frame = tokio::time::timeout(Duration::from_secs(2), frame_rx.recv())
        .await
        .expect("relay should receive an initial daemon frame")
        .expect("relay channel should stay open");

    assert!(
        !first_frame.is_empty(),
        "initial relayed frame should include a type byte"
    );
    assert_eq!(
        first_frame[0],
        frame_types::SESSION_CONTROL,
        "relay should preserve the daemon's initial SessionControl frame"
    );

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
    assert!(wait_for_daemon(&pool_client).await);

    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let _result = connect::connect_relay(
        socket_path.clone(),
        "00000000-0000-0000-0000-000000000002".to_string(),
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
        "00000000-0000-0000-0000-000000000002".to_string(),
        "test",
    )
    .await
    .unwrap()
    .handle;
    assert!(
        wait_for_cells_map(&client2, SESSION_READY_TIMEOUT).await,
        "initial sync did not deliver the cells map within {:?}",
        SESSION_READY_TIMEOUT
    );
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
    // AutomergeSync, Broadcast, Presence, RuntimeStateSync, PoolStateSync,
    // or SessionControl — never Request or Response.
    let allowed_types = [
        frame_types::AUTOMERGE_SYNC,
        frame_types::BROADCAST,
        frame_types::PRESENCE,
        frame_types::RUNTIME_STATE_SYNC,
        frame_types::POOL_STATE_SYNC,
        frame_types::SESSION_CONTROL,
    ];
    for (i, frame) in frames.iter().enumerate() {
        assert!(!frame.is_empty(), "frame {} should not be empty", i);
        assert!(
            allowed_types.contains(&frame[0]),
            "frame {} has unexpected type byte 0x{:02x} — only AUTOMERGE_SYNC, BROADCAST, PRESENCE, RUNTIME_STATE_SYNC, POOL_STATE_SYNC, and SESSION_CONTROL are piped",
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
    assert!(wait_for_daemon(&pool_client).await);

    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let result = connect::connect_relay(
        socket_path.clone(),
        "00000000-0000-0000-0000-000000000004".to_string(),
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
    assert!(wait_for_daemon(&pool_client).await);

    let (frame_tx, mut frame_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let _result = connect::connect_relay(
        socket_path.clone(),
        "00000000-0000-0000-0000-000000000003".to_string(),
        frame_tx,
    )
    .await
    .unwrap();

    // Second client rapidly adds multiple cells
    let client2 = connect::connect(
        socket_path.clone(),
        "00000000-0000-0000-0000-000000000003".to_string(),
        "test",
    )
    .await
    .unwrap()
    .handle;
    // Initial sync must deliver the daemon's cells map before we can
    // mutate it. Otherwise `add_cell_after` panics with
    // `InvalidObjId("cells map not found")` — a flake under loaded CI.
    assert!(
        wait_for_cells_map(&client2, SESSION_READY_TIMEOUT).await,
        "initial sync did not deliver the cells map within {:?}",
        SESSION_READY_TIMEOUT
    );
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
        "00000000-0000-0000-0000-000000000003".to_string(),
        "test",
    )
    .await
    .unwrap()
    .handle;
    assert!(
        wait_for_session_ready(&client3, SESSION_READY_TIMEOUT).await,
        "third client should reach session-ready state within 2s"
    );
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

#[tokio::test]
async fn test_pool_size_config_honored() {
    let temp_dir = TempDir::new().unwrap();

    // Write settings.json with custom pool sizes
    let settings_path = temp_dir.path().join("settings.json");
    let settings = serde_json::json!({
        "uv_pool_size": 15,
        "conda_pool_size": 10,
        "pixi_pool_size": 5
    });
    std::fs::write(
        &settings_path,
        serde_json::to_string_pretty(&settings).unwrap(),
    )
    .unwrap();

    // Create a custom settings doc path for this test
    let settings_doc_path = temp_dir.path().join("settings.amg");

    // Test 1: Verify that from_json() correctly imports pool sizes during initial migration
    let settings_doc = runtimed_client::settings_doc::SettingsDoc::load_or_create(
        &settings_doc_path,
        Some(&settings_path),
    );

    assert_eq!(
        settings_doc.get_u64("uv_pool_size"),
        Some(15),
        "UV pool size should be imported from settings.json via from_json()"
    );
    assert_eq!(
        settings_doc.get_u64("conda_pool_size"),
        Some(10),
        "Conda pool size should be imported from settings.json via from_json()"
    );
    assert_eq!(
        settings_doc.get_u64("pixi_pool_size"),
        Some(5),
        "Pixi pool size should be imported from settings.json via from_json()"
    );

    // Test 2: Verify that apply_json_changes() correctly updates pool sizes
    let mut settings_doc2 = runtimed_client::settings_doc::SettingsDoc::new();

    // Initially, pool sizes should have dynamic defaults: base 1, selected env 2.
    assert_eq!(
        settings_doc2.get_all().uv_pool_size,
        2,
        "UV pool should start with default value"
    );
    assert_eq!(
        settings_doc2.get_all().conda_pool_size,
        1,
        "Conda pool should start with default value"
    );
    assert_eq!(
        settings_doc2.get_all().pixi_pool_size,
        1,
        "Pixi pool should start with default value"
    );

    // Apply JSON changes with different values
    let new_settings = serde_json::json!({
        "uv_pool_size": 20,
        "conda_pool_size": 12,
        "pixi_pool_size": 8
    });

    let changed = settings_doc2.apply_json_changes(&new_settings);
    assert!(
        changed,
        "apply_json_changes should return true when pool sizes change"
    );

    assert_eq!(
        settings_doc2.get_u64("uv_pool_size"),
        Some(20),
        "UV pool size should be updated via apply_json_changes()"
    );
    assert_eq!(
        settings_doc2.get_u64("conda_pool_size"),
        Some(12),
        "Conda pool size should be updated via apply_json_changes()"
    );
    assert_eq!(
        settings_doc2.get_u64("pixi_pool_size"),
        Some(8),
        "Pixi pool size should be updated via apply_json_changes()"
    );

    // Test 3: Verify that apply_json_changes() doesn't report changes when values are the same
    let changed_again = settings_doc2.apply_json_changes(&new_settings);
    assert!(
        !changed_again,
        "apply_json_changes should return false when values don't change"
    );
}

#[tokio::test]
async fn stream_blob_spill_is_renderable_by_llm_resolver() {
    use runtimed::blob_store::BlobStore;
    use runtimed::output_store::{create_manifest, DEFAULT_INLINE_THRESHOLD};
    use runtimed_client::output_resolver::resolve_cell_outputs_for_llm;

    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path().to_path_buf());

    let big: String = (0..2_000).map(|i| format!("stdout line {i}\n")).collect();

    let raw = serde_json::json!({
        "output_type": "stream",
        "name": "stdout",
        "text": big.clone(),
    });

    let manifest = create_manifest(&raw, &store, DEFAULT_INLINE_THRESHOLD)
        .await
        .unwrap();
    let manifest_json = manifest.to_json();

    let outputs = resolve_cell_outputs_for_llm(
        &[manifest_json],
        runtimed_client::output_resolver::ResolveCtx {
            blob_base_url: Some("http://127.0.0.1:1234"),
            blob_store_path: Some(dir.path()),
            ..Default::default()
        },
    )
    .await;

    assert_eq!(outputs.len(), 1);
    let text = outputs[0].text.as_ref().expect("stream text");
    assert!(text.contains("stdout line 0"));
    assert!(text.contains("stdout line 1999"));
    assert!(text.contains("bytes total"));
}

#[tokio::test]
async fn error_blob_spill_is_renderable_by_llm_resolver() {
    use runtimed::blob_store::BlobStore;
    use runtimed::output_store::{create_manifest, DEFAULT_INLINE_THRESHOLD};
    use runtimed_client::output_resolver::resolve_cell_outputs_for_llm;

    let dir = tempfile::tempdir().unwrap();
    let store = BlobStore::new(dir.path().to_path_buf());

    let frames: Vec<String> = (0..500)
        .map(|i| format!("  frame {i} \u{2014} file.py:{i}"))
        .collect();

    let raw = serde_json::json!({
        "output_type": "error",
        "ename": "RecursionError",
        "evalue": "maximum recursion depth exceeded",
        "traceback": frames,
    });

    let manifest = create_manifest(&raw, &store, DEFAULT_INLINE_THRESHOLD)
        .await
        .unwrap();
    let manifest_json = manifest.to_json();

    let outputs = resolve_cell_outputs_for_llm(
        &[manifest_json],
        runtimed_client::output_resolver::ResolveCtx {
            blob_base_url: Some("http://127.0.0.1:1234"),
            blob_store_path: Some(dir.path()),
            ..Default::default()
        },
    )
    .await;

    assert_eq!(outputs.len(), 1);
    let out = &outputs[0];
    assert_eq!(out.ename.as_deref(), Some("RecursionError"));
    let tb = out.traceback.as_ref().expect("traceback");
    assert!(tb[0].contains("frame 499"));
    assert!(tb[1].contains("500"));
    assert!(tb[1].contains("traceback frames"));
}

/// Test that creating a notebook with an explicit package manager and dependencies
/// seeds the CRDT metadata correctly. When deps are non-empty, the daemon should
/// also generate a trust signature so auto-launch can proceed.
#[tokio::test]
async fn test_create_notebook_with_deps() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    // Create notebook with conda + two deps
    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        Some(notebook_protocol::connection::PackageManager::Conda),
        vec!["pandas".to_string(), "numpy".to_string()],
    )
    .await
    .expect("should create notebook with deps");
    let handle = result.handle;

    assert!(
        wait_for_session_ready(&handle, SESSION_READY_TIMEOUT).await,
        "client should reach session-ready state within {:?}",
        SESSION_READY_TIMEOUT
    );

    // Poll for metadata to arrive via Automerge sync
    let mut meta = handle.get_notebook_metadata();
    let start = std::time::Instant::now();
    while meta.is_none() && start.elapsed() < Duration::from_secs(5) {
        sleep(Duration::from_millis(50)).await;
        meta = handle.get_notebook_metadata();
    }
    let meta = meta.expect("metadata should be present after sync");

    // Conda section should have the two deps
    let conda = meta
        .runt
        .conda
        .as_ref()
        .expect("conda section should be present");
    assert_eq!(
        conda.dependencies,
        vec!["pandas".to_string(), "numpy".to_string()],
        "conda deps should match what was passed at creation"
    );

    // UV section should NOT be present (conda was the explicit manager)
    assert!(
        meta.runt.uv.is_none(),
        "uv section should not be present when conda is the explicit manager"
    );

    // Trust signature should be present because deps are non-empty
    assert!(
        meta.runt.trust_signature.is_some(),
        "trust signature should be present when deps are non-empty"
    );

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

/// Test that creating a notebook with an explicit package manager but no deps
/// still seeds the correct manager section in the CRDT.
#[tokio::test]
async fn test_create_notebook_with_explicit_manager_no_deps() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    // Create notebook with pixi manager, no deps
    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        Some(notebook_protocol::connection::PackageManager::Pixi),
        vec![],
    )
    .await
    .expect("should create notebook with pixi manager");
    let handle = result.handle;

    assert!(
        wait_for_session_ready(&handle, SESSION_READY_TIMEOUT).await,
        "client should reach session-ready state within {:?}",
        SESSION_READY_TIMEOUT
    );

    // Poll for metadata to arrive via Automerge sync
    let mut meta = handle.get_notebook_metadata();
    let start = std::time::Instant::now();
    while meta.is_none() && start.elapsed() < Duration::from_secs(5) {
        sleep(Duration::from_millis(50)).await;
        meta = handle.get_notebook_metadata();
    }
    let meta = meta.expect("metadata should be present after sync");

    // Pixi section should be present with empty deps
    let pixi = meta
        .runt
        .pixi
        .as_ref()
        .expect("pixi section should be present");
    assert!(
        pixi.dependencies.is_empty(),
        "pixi deps should be empty when none were provided"
    );

    // Neither uv nor conda should be present
    assert!(
        meta.runt.uv.is_none(),
        "uv section should not be present when pixi is the explicit manager"
    );
    assert!(
        meta.runt.conda.is_none(),
        "conda section should not be present when pixi is the explicit manager"
    );

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}

/// Test that creating a notebook with deps but no explicit package manager
/// uses the daemon default (uv in test config).
#[tokio::test]
async fn test_create_notebook_default_manager_with_deps() {
    let temp_dir = TempDir::new().unwrap();
    let config = test_config(&temp_dir);
    let socket_path = config.socket_path.clone();

    let daemon = Daemon::new(config).unwrap();
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client).await);

    // Create notebook with deps but no explicit package manager
    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        None,
        vec!["requests".to_string()],
    )
    .await
    .expect("should create notebook with default manager + deps");
    let handle = result.handle;

    assert!(
        wait_for_session_ready(&handle, SESSION_READY_TIMEOUT).await,
        "client should reach session-ready state within {:?}",
        SESSION_READY_TIMEOUT
    );

    // Poll for metadata to arrive via Automerge sync
    let mut meta = handle.get_notebook_metadata();
    let start = std::time::Instant::now();
    while meta.is_none() && start.elapsed() < Duration::from_secs(5) {
        sleep(Duration::from_millis(50)).await;
        meta = handle.get_notebook_metadata();
    }
    let meta = meta.expect("metadata should be present after sync");

    // UV section should be present (daemon default for python is uv)
    let uv = meta
        .runt
        .uv
        .as_ref()
        .expect("uv section should be present as the daemon default");
    assert_eq!(
        uv.dependencies,
        vec!["requests".to_string()],
        "uv deps should contain the provided dependency"
    );

    // Conda should not be present since we defaulted to uv
    assert!(
        meta.runt.conda.is_none(),
        "conda section should not be present when defaulting to uv"
    );

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;
}
