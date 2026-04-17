//! In-memory test runtime for deterministic E2E tests.
//!
//! Implements `KernelConnection` without spawning any process. On execute,
//! echoes the cell source as `text/plain` output through the same
//! RuntimeStateDoc + BlobStore path that real kernels use.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use notebook_doc::runtime_state::RuntimeStateDoc;
use notebook_protocol::protocol::LaunchedEnvConfig;
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::info;

use crate::kernel_connection::{KernelConnection, KernelLaunchConfig, KernelSharedRefs};
use crate::kernel_manager::QueueCommand;
use crate::output_store::{ContentRef, OutputManifest, TransientData};
use crate::protocol::{CompletionItem, HistoryEntry};

pub struct TestRuntime {
    cmd_tx: mpsc::Sender<QueueCommand>,
    state_doc: Arc<RwLock<RuntimeStateDoc>>,
    state_changed_tx: broadcast::Sender<()>,
    execution_count: u64,
    launched_config: LaunchedEnvConfig,
}

impl KernelConnection for TestRuntime {
    async fn launch(
        _config: KernelLaunchConfig,
        shared: KernelSharedRefs,
    ) -> Result<(Self, mpsc::Receiver<QueueCommand>)> {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);

        info!("[test-runtime] Launched (in-memory echo backend)");

        Ok((
            TestRuntime {
                cmd_tx,
                state_doc: shared.state_doc,
                state_changed_tx: shared.state_changed_tx,
                execution_count: 0,
                launched_config: LaunchedEnvConfig::default(),
            },
            cmd_rx,
        ))
    }

    async fn execute(&mut self, cell_id: &str, execution_id: &str, source: &str) -> Result<()> {
        self.execution_count += 1;
        let count = self.execution_count as i64;

        {
            let mut sd = self.state_doc.write().await;
            sd.create_execution(execution_id, cell_id);
            sd.set_execution_running(execution_id);
            sd.set_execution_count(execution_id, count);

            let manifest = OutputManifest::ExecuteResult {
                data: HashMap::from([(
                    "text/plain".to_string(),
                    ContentRef::Inline {
                        inline: source.to_string(),
                    },
                )]),
                metadata: HashMap::new(),
                execution_count: Some(count as i32),
                transient: TransientData::default(),
            };
            let _ = sd.append_output(execution_id, &manifest.to_json());

            sd.set_execution_done(execution_id, true);
        }
        let _ = self.state_changed_tx.send(());

        let _ = self
            .cmd_tx
            .send(QueueCommand::ExecutionDone {
                cell_id: cell_id.to_string(),
                execution_id: execution_id.to_string(),
            })
            .await;

        Ok(())
    }

    async fn interrupt(&mut self) -> Result<()> {
        Ok(())
    }

    async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }

    async fn send_comm_message(&mut self, _: serde_json::Value) -> Result<()> {
        Ok(())
    }

    async fn send_comm_update(&mut self, _: &str, _: serde_json::Value) -> Result<()> {
        Ok(())
    }

    async fn complete(&mut self, _: &str, _: usize) -> Result<(Vec<CompletionItem>, usize, usize)> {
        Ok((vec![], 0, 0))
    }

    async fn get_history(&mut self, _: Option<&str>, _: i32, _: bool) -> Result<Vec<HistoryEntry>> {
        Ok(vec![])
    }

    fn kernel_type(&self) -> &str {
        "test"
    }

    fn env_source(&self) -> &str {
        "test:builtin"
    }

    fn launched_config(&self) -> &LaunchedEnvConfig {
        &self.launched_config
    }

    fn env_path(&self) -> Option<&PathBuf> {
        None
    }

    fn is_connected(&self) -> bool {
        true
    }

    fn update_launched_uv_deps(&mut self, _: Vec<String>) {}
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::blob_store::BlobStore;

    #[tokio::test]
    async fn test_runtime_execute_echoes_source() {
        let state_doc = Arc::new(RwLock::new(RuntimeStateDoc::new()));
        let (state_changed_tx, _) = broadcast::channel(16);
        let (broadcast_tx, _) = broadcast::channel(16);
        let (presence_tx, _) = broadcast::channel(16);
        let tmp = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(BlobStore::new(tmp.path().join("blobs")));

        let shared = KernelSharedRefs {
            state_doc: state_doc.clone(),
            state_changed_tx,
            blob_store,
            broadcast_tx,
            presence: Arc::new(RwLock::new(notebook_doc::presence::PresenceState::new())),
            presence_tx,
        };

        let (mut runtime, mut cmd_rx) = TestRuntime::launch(
            KernelLaunchConfig {
                kernel_type: "test".to_string(),
                env_source: "test:builtin".to_string(),
                notebook_path: None,
                launched_config: LaunchedEnvConfig::default(),
                env_vars: vec![],
                pooled_env: None,
            },
            shared,
        )
        .await
        .unwrap();

        runtime.execute("c1", "e1", "hello world").await.unwrap();

        // Check state_doc recorded the execution
        let sd = state_doc.read().await;
        let state = sd.read_state();
        let exec = state.executions.get("e1").expect("execution e1 exists");
        assert_eq!(exec.status, "done");
        assert!(exec.success.unwrap_or(false));
        assert_eq!(exec.execution_count, Some(1));

        // Check output was written
        let outputs = sd.get_outputs("e1");
        assert_eq!(outputs.len(), 1);
        let output = &outputs[0];
        assert_eq!(output["output_type"], "execute_result");
        assert_eq!(output["data"]["text/plain"]["inline"], "hello world");

        // Check QueueCommand was sent
        let cmd = cmd_rx.try_recv().unwrap();
        match cmd {
            QueueCommand::ExecutionDone {
                cell_id,
                execution_id,
            } => {
                assert_eq!(cell_id, "c1");
                assert_eq!(execution_id, "e1");
            }
            _ => panic!("Expected ExecutionDone"),
        }
    }

    #[tokio::test]
    async fn test_runtime_increments_execution_count() {
        let state_doc = Arc::new(RwLock::new(RuntimeStateDoc::new()));
        let (state_changed_tx, _) = broadcast::channel(16);
        let (broadcast_tx, _) = broadcast::channel(16);
        let (presence_tx, _) = broadcast::channel(16);
        let tmp = tempfile::tempdir().unwrap();
        let blob_store = Arc::new(BlobStore::new(tmp.path().join("blobs")));

        let shared = KernelSharedRefs {
            state_doc: state_doc.clone(),
            state_changed_tx,
            blob_store,
            broadcast_tx,
            presence: Arc::new(RwLock::new(notebook_doc::presence::PresenceState::new())),
            presence_tx,
        };

        let (mut runtime, _cmd_rx) = TestRuntime::launch(
            KernelLaunchConfig {
                kernel_type: "test".to_string(),
                env_source: "test:builtin".to_string(),
                notebook_path: None,
                launched_config: LaunchedEnvConfig::default(),
                env_vars: vec![],
                pooled_env: None,
            },
            shared,
        )
        .await
        .unwrap();

        runtime.execute("c1", "e1", "first").await.unwrap();
        runtime.execute("c2", "e2", "second").await.unwrap();

        let sd = state_doc.read().await;
        let state = sd.read_state();
        assert_eq!(state.executions.get("e1").unwrap().execution_count, Some(1));
        assert_eq!(state.executions.get("e2").unwrap().execution_count, Some(2));
    }
}
