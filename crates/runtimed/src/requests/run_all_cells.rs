//! `NotebookRequest::RunAllCells` handler.

use runtime_doc::RuntimeLifecycle;
use tracing::warn;

use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::{NotebookResponse, QueueEntry};

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    // Agent path — write all cells to RuntimeStateDoc queue
    {
        let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
        if has_runtime_agent {
            // Check if kernel is shut down.
            {
                let lifecycle = room
                    .state
                    .read(|sd| sd.read_state().kernel.lifecycle)
                    .unwrap_or(RuntimeLifecycle::NotStarted);
                if matches!(
                    lifecycle,
                    RuntimeLifecycle::Shutdown | RuntimeLifecycle::Error
                ) {
                    return NotebookResponse::NoKernel {};
                }
            }

            let cells = {
                let doc = room.doc.read().await;
                doc.get_cells()
            };

            // Pre-compute execution entries so we can write to
            // state_doc and doc in separate scoped blocks, avoiding
            // holding one lock across the other's `.await` (deadlock
            // prevention).
            let mut queued = Vec::new();
            let mut entries: Vec<(String, String, String, u64)> = Vec::new();
            for cell in &cells {
                if cell.cell_type == "code" {
                    let execution_id = uuid::Uuid::new_v4().to_string();
                    let seq = room
                        .next_queue_seq
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    entries.push((
                        execution_id.clone(),
                        cell.id.clone(),
                        cell.source.clone(),
                        seq,
                    ));
                    queued.push(QueueEntry {
                        cell_id: cell.id.clone(),
                        execution_id,
                    });
                }
            }
            // Write RuntimeStateDoc entries first; on failure bail
            // before stamping NotebookDoc so cell→execution_id pointers
            // cannot dangle. Any single failure aborts the whole batch.
            if let Err(e) = room.state.with_doc(|sd| {
                for (execution_id, cell_id, source, seq) in &entries {
                    sd.create_execution_with_source(execution_id, cell_id, source, *seq)?;
                }
                Ok(())
            }) {
                warn!(
                    "[notebook-sync] Failed to create_execution_with_source: {}",
                    e
                );
                return NotebookResponse::Error {
                    error: format!("failed to queue execution: {e}"),
                };
            }
            {
                let mut doc = room.doc.write().await;
                for (execution_id, cell_id, _, _) in &entries {
                    let _ = doc.set_execution_id(cell_id, Some(execution_id));
                }
                let _ = room.broadcasts.changed_tx.send(());
            }

            return NotebookResponse::AllCellsQueued { queued };
        }
    }

    // No runtime agent available — kernel not running
    NotebookResponse::NoKernel {}
}
