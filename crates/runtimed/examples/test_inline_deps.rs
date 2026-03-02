//! Test inline deps detection in daemon mode.
//!
//! Run with: cargo run -p runtimed --example test_inline_deps
//!
//! This starts an isolated daemon and tests that it correctly detects
//! inline dependencies from notebook metadata.

use std::io::Write;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::sleep;

use runtimed::client::PoolClient;
use runtimed::daemon::{Daemon, DaemonConfig};
use runtimed::notebook_sync_client::NotebookSyncClient;
use runtimed::protocol::{NotebookRequest, NotebookResponse};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let temp_dir = TempDir::new()?;
    println!("Test directory: {:?}", temp_dir.path());

    // Create test notebook with UV inline deps
    let notebook_path = temp_dir.path().join("test-notebook.ipynb");
    let mut f = std::fs::File::create(&notebook_path)?;
    writeln!(
        f,
        r#"{{
  "metadata": {{
    "uv": {{
      "dependencies": ["requests", "numpy"]
    }}
  }},
  "cells": [
    {{
      "cell_type": "code",
      "source": "print('hello')",
      "metadata": {{}},
      "outputs": [],
      "execution_count": null
    }}
  ],
  "nbformat": 4,
  "nbformat_minor": 5
}}"#
    )?;
    println!("Created test notebook with UV deps at: {:?}", notebook_path);

    // Create isolated daemon config
    let config = DaemonConfig {
        socket_path: temp_dir.path().join("test-daemon.sock"),
        cache_dir: temp_dir.path().join("envs"),
        blob_store_dir: temp_dir.path().join("blobs"),
        notebook_docs_dir: temp_dir.path().join("notebook-docs"),
        uv_pool_size: 0, // Don't create real envs
        conda_pool_size: 0,
        max_age_secs: 3600,
        lock_dir: Some(temp_dir.path().to_path_buf()),
        room_eviction_delay_ms: None,
    };
    let socket_path = config.socket_path.clone();
    println!("Socket path: {:?}", socket_path);

    // Start daemon
    let daemon = Daemon::new(config)?;
    let daemon_handle = tokio::spawn(async move {
        daemon.run().await.ok();
    });

    // Wait for daemon
    let pool_client = PoolClient::new(socket_path.clone());
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if pool_client.ping().await.is_ok() {
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }
    println!("Daemon ready!");

    // Connect to notebook room
    let notebook_id = format!("test-{}", uuid::Uuid::new_v4());
    let mut client = NotebookSyncClient::connect(socket_path.clone(), notebook_id.clone()).await?;
    println!("Connected to notebook room: {}", notebook_id);

    // Send LaunchKernel request with env_source: "auto"
    println!("\n=== Sending LaunchKernel with env_source: auto ===");
    println!("Notebook path: {:?}", notebook_path);

    let request = NotebookRequest::LaunchKernel {
        kernel_type: "python".to_string(),
        env_source: "auto".to_string(),
        notebook_path: Some(notebook_path.to_string_lossy().to_string()),
    };

    let response = client.send_request(&request).await;
    println!("\n=== Response ===");
    match response {
        Ok(NotebookResponse::KernelLaunched {
            kernel_type,
            env_source,
            ..
        }) => {
            println!("✅ Kernel launched!");
            println!("   kernel_type: {}", kernel_type);
            println!("   env_source: {}", env_source);
            if env_source == "uv:inline" {
                println!("\n🎉 SUCCESS: Inline deps detected correctly!");
            } else {
                println!(
                    "\n⚠️  UNEXPECTED: Expected env_source 'uv:inline', got '{}'",
                    env_source
                );
            }
        }
        Ok(NotebookResponse::Error { error }) => {
            // This is expected if Python/uv isn't available
            println!(
                "⚠️  Kernel launch error (expected without Python): {}",
                error
            );
            println!("\nCheck daemon logs above for '[notebook-sync] Found inline deps' message");
        }
        Ok(other) => {
            println!("Unexpected response: {:?}", other);
        }
        Err(e) => {
            println!("Request error: {}", e);
        }
    }

    // Shutdown
    pool_client.shutdown().await.ok();
    let _ = tokio::time::timeout(Duration::from_secs(2), daemon_handle).await;

    println!("\nTest complete!");
    Ok(())
}
