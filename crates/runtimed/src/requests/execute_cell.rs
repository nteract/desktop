//! `NotebookRequest::ExecuteCell` handler.

use std::sync::Arc;

use runtime_doc::RuntimeLifecycle;
use tracing::warn;

use crate::notebook_sync_server::{
    catch_automerge_panic, detect_room_runtime, format_source, formatter_actor, NotebookRoom,
};
use crate::protocol::NotebookResponse;
use crate::requests::guarded;
use crate::task_supervisor::spawn_best_effort;

pub(crate) async fn handle(room: &Arc<NotebookRoom>, cell_id: String) -> NotebookResponse {
    handle_inner(room, cell_id, None).await
}

pub(crate) async fn handle_guarded(
    room: &Arc<NotebookRoom>,
    cell_id: String,
    observed_heads: Vec<String>,
) -> NotebookResponse {
    if let Err(rejection) = guarded::ensure_trusted(room).await {
        return rejection.into_response();
    }
    handle_inner(room, cell_id, Some(observed_heads)).await
}

async fn handle_inner(
    room: &Arc<NotebookRoom>,
    cell_id: String,
    observed_heads: Option<Vec<String>>,
) -> NotebookResponse {
    // Agent-backed kernel: write execution to RuntimeStateDoc queue.
    // The runtime agent discovers it via CRDT sync and executes.
    // Check runtime_agent_request_tx (not runtime_agent_handle) to ensure the runtime agent's
    // sync connection is still live — a stale handle with no connection
    // would leave queued executions orphaned.
    {
        let has_runtime_agent = room.runtime_agent_request_tx.lock().await.is_some();
        if has_runtime_agent {
            // Check if kernel is shut down — return NoKernel instead
            // of silently queuing into a dead kernel.
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

            let (source, execution_id) =
                match queue_cell_if_current(room, &cell_id, observed_heads.as_deref()).await {
                    QueueCellResult::Queued {
                        source,
                        execution_id,
                    } => (source, execution_id),
                    QueueCellResult::AlreadyActive { execution_id } => {
                        return NotebookResponse::CellQueued {
                            cell_id,
                            execution_id,
                        };
                    }
                    QueueCellResult::Response(response) => return *response,
                };

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

enum QueueCellResult {
    Queued {
        source: String,
        execution_id: String,
    },
    AlreadyActive {
        execution_id: String,
    },
    Response(Box<NotebookResponse>),
}

async fn queue_cell_if_current(
    room: &Arc<NotebookRoom>,
    cell_id: &str,
    observed_heads: Option<&[String]>,
) -> QueueCellResult {
    let mut doc = room.doc.write().await;
    if let Some(observed_heads) = observed_heads {
        if let Err(rejection) = guarded::validate_execute_cell(&mut doc, cell_id, observed_heads) {
            return QueueCellResult::Response(Box::new(rejection.into_response()));
        }
    }

    let cell = match doc.get_cell(cell_id) {
        Some(c) => c,
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
            return QueueCellResult::Response(Box::new(NotebookResponse::Error {
                error: format!("Cell not found in document: {}", cell_id),
            }));
        }
    };

    if cell.cell_type != "code" {
        return QueueCellResult::Response(Box::new(NotebookResponse::Error {
            error: format!(
                "Cannot execute non-code cell: {} (type: {})",
                cell_id, cell.cell_type
            ),
        }));
    }

    let current_execution_id = doc.get_execution_id(cell_id);
    if let Some(eid) = current_execution_id {
        let is_active = room
            .state
            .read(|sd| {
                sd.get_execution(&eid)
                    .is_some_and(|exec| exec.status == "queued" || exec.status == "running")
            })
            .unwrap_or(false);
        if is_active {
            return QueueCellResult::AlreadyActive { execution_id: eid };
        }
    }

    let execution_id = uuid::Uuid::new_v4().to_string();
    let seq = room
        .next_queue_seq
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

    if let Err(e) = room
        .state
        .with_doc(|sd| sd.create_execution_with_source(&execution_id, cell_id, &cell.source, seq))
    {
        warn!(
            "[notebook-sync] Failed to create_execution_with_source for {}: {}",
            execution_id, e
        );
        return QueueCellResult::Response(Box::new(NotebookResponse::Error {
            error: format!("failed to queue execution: {e}"),
        }));
    }

    let source = cell.source;
    let _ = doc.set_execution_id(cell_id, Some(&execution_id));
    let _ = room.broadcasts.changed_tx.send(());

    QueueCellResult::Queued {
        source,
        execution_id,
    }
}
