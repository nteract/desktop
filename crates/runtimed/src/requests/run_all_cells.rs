//! `NotebookRequest::RunAllCells` handler.

use runtime_doc::RuntimeLifecycle;
use tracing::warn;

use crate::notebook_sync_server::NotebookRoom;
use crate::protocol::{NotebookResponse, QueueEntry};
use crate::requests::guarded;

pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    handle_inner(room, None).await
}

pub(crate) async fn handle_guarded(
    room: &NotebookRoom,
    observed_heads: Vec<String>,
) -> NotebookResponse {
    if let Err(rejection) = guarded::ensure_trusted(room).await {
        return rejection.into_response();
    }
    handle_inner(room, Some(observed_heads)).await
}

async fn handle_inner(
    room: &NotebookRoom,
    observed_heads: Option<Vec<String>>,
) -> NotebookResponse {
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

            let queued = {
                let mut doc = room.doc.write().await;
                if let Some(observed_heads) = observed_heads.as_deref() {
                    if let Err(rejection) = guarded::validate_run_all(&mut doc, observed_heads) {
                        return rejection.into_response();
                    }
                }

                let cells = doc.get_cells();
                let code_cells: Vec<_> = cells
                    .iter()
                    .filter(|cell| cell.cell_type == "code")
                    .cloned()
                    .collect();

                // Pre-compute execution entries while holding the doc write
                // lock so guarded requests cannot be invalidated before the
                // cell→execution_id pointers are stamped.
                let mut queued = Vec::new();
                let mut entries: Vec<(String, String, String, u64)> = Vec::new();
                for cell in &code_cells {
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

                for (execution_id, cell_id, _, _) in &entries {
                    let _ = doc.set_execution_id(cell_id, Some(execution_id));
                }
                let _ = room.broadcasts.changed_tx.send(());

                queued
            };

            return NotebookResponse::AllCellsQueued { queued };
        }
    }

    // No runtime agent available — kernel not running
    NotebookResponse::NoKernel {}
}
