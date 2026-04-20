//! Execution state machine for the runtime agent.
//!
//! `KernelState` owns the execution queue, currently-executing cell, and
//! kernel lifecycle status. It is designed to be held as a plain local
//! variable in the runtime agent's `select!` loop — no mutex needed.
//!
//! Async methods (`queue_cell`, `execution_done`, `process_next`) accept a
//! `&mut impl KernelConnection` so they can send execute requests without
//! owning the connection.

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::Result;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, info, warn};

use crate::kernel_connection::KernelConnection;
use crate::kernel_manager::{KernelStatus, QueuedCell};
use crate::protocol::{NotebookBroadcast, QueueEntry};
use notebook_doc::runtime_state::{QueueEntry as DocQueueEntry, RuntimeStateDoc};

/// Maximum execution entries retained in the RuntimeStateDoc.
///
/// Matches the constant in `kernel_manager.rs`. `trim_executions` preserves the
/// latest execution per cell, so the actual count can exceed this if the
/// notebook has more unique cells than this limit.
const MAX_EXECUTION_ENTRIES: usize = 64;

// ── Helpers ────────────────────────────────────────────────────────────────

/// Convert a protocol `QueueEntry` to a `RuntimeStateDoc` `QueueEntry`.
fn to_doc_entry(e: &QueueEntry) -> DocQueueEntry {
    DocQueueEntry {
        cell_id: e.cell_id.clone(),
        execution_id: e.execution_id.clone(),
    }
}

/// Convert a slice of protocol `QueueEntry`s to doc entries.
fn to_doc_entries(entries: &[QueueEntry]) -> Vec<DocQueueEntry> {
    entries.iter().map(to_doc_entry).collect()
}

// ── KernelState ────────────────────────────────────────────────────────────

/// Execution state machine for the runtime agent.
///
/// Owns the queue, executing-cell slot, and kernel status. The runtime agent
/// holds this as a local variable — all mutation goes through `&mut self`.
pub struct KernelState {
    /// Cells pending execution.
    queue: VecDeque<QueuedCell>,
    /// `(cell_id, execution_id)` of the currently running cell.
    executing: Option<(String, String)>,
    /// Whether the current execution produced an error output.
    /// Read by `execution_done` to record success/failure in the state doc.
    execution_had_error: bool,
    /// Kernel lifecycle status.
    status: KernelStatus,

    // ── Shared references (not owned, just held) ───────────────────────
    /// Per-notebook runtime state document (daemon-authoritative).
    state_doc: Arc<RwLock<RuntimeStateDoc>>,
    /// Notification channel for RuntimeStateDoc changes.
    state_changed_tx: broadcast::Sender<()>,
    /// Broadcast channel for execution events to connected peers.
    broadcast_tx: broadcast::Sender<NotebookBroadcast>,
}

impl KernelState {
    /// Create a new `KernelState` with initial status `Starting`.
    pub fn new(
        state_doc: Arc<RwLock<RuntimeStateDoc>>,
        state_changed_tx: broadcast::Sender<()>,
        broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    ) -> Self {
        Self {
            queue: VecDeque::new(),
            executing: None,
            execution_had_error: false,
            status: KernelStatus::Starting,
            state_doc,
            state_changed_tx,
            broadcast_tx,
        }
    }

    // ── Lifecycle ──────────────────────────────────────────────────────

    /// Clear all execution state for a new kernel launch.
    pub fn reset(&mut self) {
        self.queue.clear();
        self.executing = None;
        self.execution_had_error = false;
        self.status = KernelStatus::Starting;
    }

    /// Transition to idle after kernel launch completes.
    pub fn set_idle(&mut self) {
        self.status = KernelStatus::Idle;
    }

    // ── Queue operations ───────────────────────────────────────────────

    /// Queue a cell for execution.
    ///
    /// Idempotent: if the cell is already executing or queued, returns the
    /// existing `execution_id` instead of generating a new one.
    ///
    /// After queuing, calls `process_next` to start execution if nothing is
    /// currently running.
    pub async fn queue_cell(
        &mut self,
        cell_id: String,
        execution_id: String,
        source: String,
        conn: &mut impl KernelConnection,
    ) -> Result<String> {
        // Idempotent: return existing execution_id if already executing or queued
        if let Some((ref cid, ref eid)) = self.executing {
            if cid == &cell_id {
                info!(
                    "[kernel-state] Cell {} already executing ({}), skipping",
                    cell_id, eid
                );
                return Ok(eid.clone());
            }
        }
        if let Some(existing) = self.queue.iter().find(|c| c.cell_id == cell_id) {
            info!(
                "[kernel-state] Cell {} already queued ({}), skipping",
                cell_id, existing.execution_id
            );
            return Ok(existing.execution_id.clone());
        }

        info!(
            "[kernel-state] Queuing cell: {} (execution_id={})",
            cell_id, execution_id
        );

        // Add to queue (clone cell_id before move for state_doc write below)
        let cell_id_ref = cell_id.clone();
        self.queue.push_back(QueuedCell {
            cell_id,
            execution_id: execution_id.clone(),
            code: source,
        });

        // Broadcast queue state
        let _ = self.broadcast_tx.send(NotebookBroadcast::QueueChanged {
            executing: self.executing_entry(),
            queued: self.queued_entries(),
        });

        // Write to state doc
        {
            let doc_exec = self.executing_entry().as_ref().map(to_doc_entry);
            let doc_queued = to_doc_entries(&self.queued_entries());
            let mut sd = self.state_doc.write().await;
            let mut changed = sd.create_execution(&execution_id, &cell_id_ref);
            changed |= sd.set_queue(doc_exec.as_ref(), &doc_queued);
            if changed {
                let _ = self.state_changed_tx.send(());
            }
        }

        // Try to process if nothing executing
        self.process_next(conn).await?;
        Ok(execution_id)
    }

    /// Mark a cell execution as complete and process the next queued cell.
    ///
    /// Only acts if `cell_id` matches the currently executing cell.
    pub async fn execution_done(
        &mut self,
        cell_id: &str,
        execution_id: &str,
        conn: &mut impl KernelConnection,
    ) -> Result<()> {
        let matches = self
            .executing
            .as_ref()
            .is_some_and(|(cid, _)| cid == cell_id);
        if matches {
            let success = !self.execution_had_error;
            self.executing = None;
            self.execution_had_error = false;
            self.status = KernelStatus::Idle;

            // Broadcast done
            let _ = self.broadcast_tx.send(NotebookBroadcast::ExecutionDone {
                cell_id: cell_id.to_string(),
                execution_id: execution_id.to_string(),
            });

            // Broadcast queue state
            let _ = self.broadcast_tx.send(NotebookBroadcast::QueueChanged {
                executing: None,
                queued: self.queued_entries(),
            });

            // Write to state doc
            {
                let doc_queued = to_doc_entries(&self.queued_entries());
                let mut sd = self.state_doc.write().await;
                let mut changed = sd.set_execution_done(execution_id, success);
                changed |= sd.set_queue(None, &doc_queued);
                let trimmed = sd.trim_executions(MAX_EXECUTION_ENTRIES);
                changed |= trimmed > 0;
                if trimmed > 0 {
                    sd.rebuild_from_save();
                    debug!(
                        "[kernel-state] Compacted RuntimeStateDoc after trimming {} executions",
                        trimmed
                    );
                }
                if changed {
                    let _ = self.state_changed_tx.send(());
                }
            }

            // Process next
            self.process_next(conn).await?;
        }
        Ok(())
    }

    /// Record that the current execution produced an error output.
    ///
    /// Called before `execution_done` so it can determine success/failure.
    pub fn mark_execution_error(&mut self) {
        self.execution_had_error = true;
    }

    /// Drain the execution queue.
    ///
    /// Returns the cleared entries. Does NOT clear the executing cell — that
    /// is the caller's responsibility (e.g., via interrupt or kernel_died).
    pub fn clear_queue(&mut self) -> Vec<QueueEntry> {
        let cleared: Vec<QueueEntry> = self
            .queue
            .drain(..)
            .map(|c| QueueEntry {
                cell_id: c.cell_id,
                execution_id: c.execution_id,
            })
            .collect();

        // Broadcast queue state
        let _ = self.broadcast_tx.send(NotebookBroadcast::QueueChanged {
            executing: self.executing_entry(),
            queued: vec![],
        });

        cleared
    }

    /// Handle kernel death (process exit or heartbeat failure).
    ///
    /// Sets status to `Dead`, clears the executing cell and queue, broadcasts
    /// error status. Returns the interrupted execution (if any) and cleared
    /// queue entries.
    ///
    /// Idempotent — multiple calls (e.g., from both process watcher and
    /// heartbeat monitor) are safe.
    pub fn kernel_died(&mut self) -> (Option<(String, String)>, Vec<QueueEntry>) {
        // Idempotent: if already dead, don't re-broadcast
        if self.status == KernelStatus::Dead {
            debug!("[kernel-state] kernel_died called but already dead, ignoring");
            return (None, vec![]);
        }

        warn!(
            "[kernel-state] Kernel died, executing={:?}, queued={}",
            self.executing,
            self.queue.len()
        );

        // Capture the interrupted execution before clearing
        let interrupted = self.executing.take();
        self.status = KernelStatus::Dead;

        // Clear any queued cells — they can't execute without a kernel
        let cleared = self.clear_queue();
        if !cleared.is_empty() {
            info!(
                "[kernel-state] Cleared {} queued cells due to kernel death",
                cleared.len()
            );
        }

        // Broadcast error status to all peers
        let _ = self.broadcast_tx.send(NotebookBroadcast::KernelStatus {
            status: "error".to_string(),
            cell_id: None,
        });

        // Broadcast empty queue state
        let _ = self.broadcast_tx.send(NotebookBroadcast::QueueChanged {
            executing: None,
            queued: vec![],
        });

        // Note: state_doc writes for kernel_died happen in the async command
        // processor (notebook_sync_server.rs QueueCommand::KernelDied handler).
        // state_doc.set_kernel_status("error") + set_queue(None, &[])
        // + set_execution_done for interrupted + cleared entries

        (interrupted, cleared)
    }

    // ── Internal: process next ─────────────────────────────────────────

    /// Pop the next cell from the queue and send an execute request.
    ///
    /// No-op if something is already executing or the queue is empty.
    async fn process_next(&mut self, conn: &mut impl KernelConnection) -> Result<()> {
        // Already executing?
        if self.executing.is_some() {
            return Ok(());
        }

        // Get next cell
        let Some(cell) = self.queue.pop_front() else {
            return Ok(());
        };

        self.executing = Some((cell.cell_id.clone(), cell.execution_id.clone()));
        self.status = KernelStatus::Busy;

        // Broadcast queue state
        let executing = self.executing_entry();
        let queued = self.queued_entries();
        let _ = self
            .broadcast_tx
            .send(NotebookBroadcast::QueueChanged { executing, queued });

        // Write to state doc
        {
            let doc_exec = self.executing_entry().as_ref().map(to_doc_entry);
            let doc_queued = to_doc_entries(&self.queued_entries());
            let mut sd = self.state_doc.write().await;
            let mut changed = sd.set_execution_running(&cell.execution_id);
            changed |= sd.set_queue(doc_exec.as_ref(), &doc_queued);
            if changed {
                let _ = self.state_changed_tx.send(());
            }
        }

        // Send execute request via the connection
        conn.execute(&cell.cell_id, &cell.execution_id, &cell.code)
            .await?;

        info!(
            "[kernel-state] Sent execute_request: cell_id={} execution_id={}",
            cell.cell_id, cell.execution_id
        );

        Ok(())
    }

    // ── Read-only accessors ────────────────────────────────────────────

    /// Current kernel lifecycle status.
    pub fn status(&self) -> KernelStatus {
        self.status
    }

    /// Whether the kernel is running (not Dead, not ShuttingDown).
    pub fn is_running(&self) -> bool {
        !matches!(self.status, KernelStatus::Dead | KernelStatus::ShuttingDown)
    }

    /// The `(cell_id, execution_id)` of the currently executing cell, if any.
    pub fn executing_cell(&self) -> Option<&(String, String)> {
        self.executing.as_ref()
    }

    /// Snapshot of queued entries as protocol `QueueEntry`s.
    pub fn queued_entries(&self) -> Vec<QueueEntry> {
        self.queue
            .iter()
            .map(|c| QueueEntry {
                cell_id: c.cell_id.clone(),
                execution_id: c.execution_id.clone(),
            })
            .collect()
    }

    /// The currently executing entry as a protocol `QueueEntry`, if any.
    pub fn executing_entry(&self) -> Option<QueueEntry> {
        self.executing.as_ref().map(|(cid, eid)| QueueEntry {
            cell_id: cid.clone(),
            execution_id: eid.clone(),
        })
    }

    /// Write current queue state to the RuntimeStateDoc.
    ///
    /// Used by callers that need to sync state doc after modifying the queue
    /// externally (e.g., interrupt handler).
    pub async fn write_queue_to_state_doc(&self) {
        let doc_exec = self.executing_entry().as_ref().map(to_doc_entry);
        let doc_queued = to_doc_entries(&self.queued_entries());
        let mut sd = self.state_doc.write().await;
        if sd.set_queue(doc_exec.as_ref(), &doc_queued) {
            let _ = self.state_changed_tx.send(());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel_connection::{KernelConnection, KernelLaunchConfig, KernelSharedRefs};
    use crate::kernel_manager::QueueCommand;
    use crate::protocol::CompletionItem;
    use anyhow::Result;
    use notebook_protocol::protocol::LaunchedEnvConfig;
    use std::path::PathBuf;

    /// Minimal mock that records execute calls and succeeds.
    struct MockKernel {
        executes: Vec<(String, String)>,
    }

    impl MockKernel {
        fn new() -> Self {
            Self {
                executes: Vec::new(),
            }
        }
    }

    impl KernelConnection for MockKernel {
        async fn launch(
            _config: KernelLaunchConfig,
            _shared: KernelSharedRefs,
        ) -> Result<(Self, tokio::sync::mpsc::Receiver<QueueCommand>)> {
            unimplemented!("tests create MockKernel directly")
        }

        async fn execute(
            &mut self,
            cell_id: &str,
            execution_id: &str,
            _source: &str,
        ) -> Result<()> {
            self.executes
                .push((cell_id.to_string(), execution_id.to_string()));
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
        async fn complete(
            &mut self,
            _: &str,
            _: usize,
        ) -> Result<(Vec<CompletionItem>, usize, usize)> {
            Ok((vec![], 0, 0))
        }
        async fn get_history(
            &mut self,
            _: Option<&str>,
            _: i32,
            _: bool,
        ) -> Result<Vec<crate::protocol::HistoryEntry>> {
            Ok(vec![])
        }
        fn kernel_type(&self) -> &str {
            "python"
        }
        fn env_source(&self) -> &str {
            "test"
        }
        fn launched_config(&self) -> &LaunchedEnvConfig {
            // Leak a static default — fine for tests.
            Box::leak(Box::new(LaunchedEnvConfig::default()))
        }
        fn env_path(&self) -> Option<&PathBuf> {
            None
        }
        fn is_connected(&self) -> bool {
            true
        }
        fn update_launched_uv_deps(&mut self, _: Vec<String>) {}
    }

    /// Build a `KernelState` wired to the given `RuntimeStateDoc`.
    fn test_state(
        state_doc: Arc<RwLock<RuntimeStateDoc>>,
    ) -> (KernelState, broadcast::Receiver<NotebookBroadcast>) {
        let (state_changed_tx, _) = broadcast::channel(64);
        let (broadcast_tx, broadcast_rx) = broadcast::channel(64);
        let state = KernelState::new(state_doc, state_changed_tx, broadcast_tx);
        (state, broadcast_rx)
    }

    // ── kernel_died tests ────────────────────────────────────────────────

    #[tokio::test]
    async fn kernel_died_returns_interrupted_execution_and_cleared_queue() {
        let sd = Arc::new(RwLock::new(RuntimeStateDoc::new()));
        let (mut state, _rx) = test_state(sd.clone());
        let mut mock = MockKernel::new();
        state.set_idle();

        // Queue two cells — first starts executing, second stays queued
        state
            .queue_cell("c1".into(), "e1".into(), "x=1".into(), &mut mock)
            .await
            .unwrap();
        state
            .queue_cell("c2".into(), "e2".into(), "x=2".into(), &mut mock)
            .await
            .unwrap();

        assert!(state.executing_cell().is_some());
        assert_eq!(state.queued_entries().len(), 1);

        let (interrupted, cleared) = state.kernel_died();

        // Should return the executing cell
        let (cid, eid) = interrupted.unwrap();
        assert_eq!(cid, "c1");
        assert_eq!(eid, "e1");

        // Should return the cleared queued entry
        assert_eq!(cleared.len(), 1);
        assert_eq!(cleared[0].cell_id, "c2");
        assert_eq!(cleared[0].execution_id, "e2");

        // State should be cleared
        assert!(state.executing_cell().is_none());
        assert!(state.queued_entries().is_empty());
    }

    #[tokio::test]
    async fn kernel_died_idempotent_when_already_dead() {
        let sd = Arc::new(RwLock::new(RuntimeStateDoc::new()));
        let (mut state, _rx) = test_state(sd.clone());
        let mut mock = MockKernel::new();
        state.set_idle();

        state
            .queue_cell("c1".into(), "e1".into(), "x=1".into(), &mut mock)
            .await
            .unwrap();

        // First call returns data
        let (interrupted, _) = state.kernel_died();
        assert!(interrupted.is_some());

        // Second call is a no-op
        let (interrupted2, cleared2) = state.kernel_died();
        assert!(interrupted2.is_none());
        assert!(cleared2.is_empty());
    }

    #[tokio::test]
    async fn kernel_died_with_empty_queue() {
        let sd = Arc::new(RwLock::new(RuntimeStateDoc::new()));
        let (mut state, _rx) = test_state(sd.clone());
        state.set_idle();

        let (interrupted, cleared) = state.kernel_died();
        assert!(interrupted.is_none());
        assert!(cleared.is_empty());
    }

    // ── reset tests ──────────────────────────────────────────────────────

    #[tokio::test]
    async fn reset_clears_executing_and_queue() {
        let sd = Arc::new(RwLock::new(RuntimeStateDoc::new()));
        let (mut state, _rx) = test_state(sd.clone());
        let mut mock = MockKernel::new();
        state.set_idle();

        state
            .queue_cell("c1".into(), "e1".into(), "x=1".into(), &mut mock)
            .await
            .unwrap();
        state
            .queue_cell("c2".into(), "e2".into(), "x=2".into(), &mut mock)
            .await
            .unwrap();

        assert!(state.executing_cell().is_some());
        assert_eq!(state.queued_entries().len(), 1);

        state.reset();

        assert!(state.executing_cell().is_none());
        assert!(state.queued_entries().is_empty());
    }
}
