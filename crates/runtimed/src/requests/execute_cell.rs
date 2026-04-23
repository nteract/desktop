//! `NotebookRequest::ExecuteCell` handler.

use std::sync::Arc;

use tracing::warn;

use crate::notebook_sync_server::{
    catch_automerge_panic, detect_room_runtime, format_source, formatter_actor, NotebookRoom,
};
use crate::protocol::NotebookResponse;
use crate::task_supervisor::spawn_best_effort;

pub(crate) async fn handle(room: &Arc<NotebookRoom>, cell_id: String) -> NotebookResponse {
    // Read cell source FIRST (before kernel lock) to avoid holding
    // kernel mutex while waiting on doc lock
    let (source, cell_type) = {
        let doc = room.doc.read().await;
        match doc.get_cell(&cell_id) {
            Some(c) => (c.source, c.cell_type),
            None => {
                let cells = doc.get_cells();
                let cell_ids: Vec<&str> = cells.iter().map(|c| c.id.as_str()).collect();
                warn!(
                    "[notebook-sync] ExecuteCell: cell {} not found in document \
                     (doc has {} cells: {:?})",
                    cell_id,
                    cells.len(),
                    cell_ids,
                );
                return NotebookResponse::Error {
                    error: format!("Cell not found in document: {}", cell_id),
                };
            }
        }
    }; // doc lock released here

    // Only execute code cells
    if cell_type != "code" {
        return NotebookResponse::Error {
            error: format!(
                "Cannot execute non-code cell: {} (type: {})",
                cell_id, cell_type
            ),
        };
    }

    // Agent-backed kernel: write execution to RuntimeStateDoc queue.
    // The runtime agent discovers it via CRDT sync and executes.
    // Check runtime_agent_request_tx (not runtime_agent_handle) to ensure the runtime agent's
    // sync connection is still live — a stale handle with no connection
    // would leave queued executions orphaned.
    {
        let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
        if has_runtime_agent {
            // Check if kernel is shut down — return NoKernel instead
            // of silently queuing into a dead kernel
            {
                let status = room
                    .state
                    .read(|sd| sd.read_state().kernel.status.clone())
                    .unwrap_or_default();
                if status == "shutdown" || status == "error" {
                    return NotebookResponse::NoKernel {};
                }
            }

            // Idempotency: if the cell already has an active (queued or
            // running) execution, return the existing execution_id instead
            // of creating a new one. Lookup follows the ownership model:
            // NotebookDoc owns the cell→execution_id mapping,
            // RuntimeStateDoc owns execution lifecycle state.
            {
                let eid = {
                    let doc = room.doc.read().await;
                    doc.get_execution_id(&cell_id)
                };
                if let Some(eid) = eid {
                    let is_active = room
                        .state
                        .read(|sd| {
                            sd.get_execution(&eid).is_some_and(|exec| {
                                exec.status == "queued" || exec.status == "running"
                            })
                        })
                        .unwrap_or(false);
                    if is_active {
                        return NotebookResponse::CellQueued {
                            cell_id,
                            execution_id: eid,
                        };
                    }
                }
            }

            let execution_id = uuid::Uuid::new_v4().to_string();
            let seq = room
                .next_queue_seq
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

            // Write execution entry with source to RuntimeStateDoc first
            // so that NotebookDoc's cell→execution_id pointer never
            // dangles. If this fails we bail before stamping the cell.
            if let Err(e) = room.state.with_doc(|sd| {
                sd.create_execution_with_source(&execution_id, &cell_id, &source, seq)
            }) {
                warn!(
                    "[notebook-sync] Failed to create_execution_with_source for {}: {}",
                    execution_id, e
                );
                return NotebookResponse::Error {
                    error: format!("failed to queue execution: {e}"),
                };
            }

            // Stamp execution_id on the cell in NotebookDoc
            {
                let mut doc = room.doc.write().await;
                let _ = doc.set_execution_id(&cell_id, Some(&execution_id));
                let _ = room.broadcasts.changed_tx.send(());
            }

            // Best-effort background formatting via fork+merge
            let fork = {
                let mut doc = room.doc.write().await;
                doc.fork()
            };
            let room_clone = Arc::clone(room);
            let cell_id_clone = cell_id.clone();
            let source_clone = source.clone();
            spawn_best_effort("cell-formatter", async move {
                if let Some(runtime) = detect_room_runtime(&room_clone).await {
                    if let Some(formatted) = format_source(&source_clone, &runtime).await {
                        // Actor is assigned here (not via fork_with_actor)
                        // because the formatter identity depends on the
                        // runtime, which is detected after the fork was
                        // created. The UUID suffix keeps concurrent
                        // formatter forks from colliding on `(actor, seq)`.
                        let mut fork = fork;
                        fork.set_actor(&format!(
                            "{}:{}",
                            formatter_actor(&runtime),
                            uuid::Uuid::new_v4()
                        ));
                        if fork.update_source(&cell_id_clone, &formatted).is_ok() {
                            let mut doc = room_clone.doc.write().await;
                            if let Err(e) =
                                catch_automerge_panic("format-merge", || doc.merge(&mut fork))
                            {
                                warn!("{}", e);
                                doc.rebuild_from_save();
                            }
                            let _ = room_clone.broadcasts.changed_tx.send(());
                        }
                    }
                }
            });

            return NotebookResponse::CellQueued {
                cell_id,
                execution_id,
            };
        }
    }

    // No runtime agent available — kernel not running
    NotebookResponse::NoKernel {}
}
