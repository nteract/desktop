use std::collections::HashMap;

use automerge::sync;
use tokio::sync::broadcast;
use tracing::{debug, warn};

use crate::connection::NotebookFrameType;

use super::catch_automerge_panic;
use super::peer_writer::PeerWriter;
use super::NotebookRoom;

type PersistedExecutionRecords = HashMap<String, runtimed_client::execution_store::ExecutionRecord>;

pub(super) async fn handle_runtime_state_frame(
    room: &NotebookRoom,
    state_peer_state: &mut sync::State,
    writer: &PeerWriter,
    payload: &[u8],
    store: &runtimed_client::execution_store::ExecutionStore,
    persisted_records: &mut PersistedExecutionRecords,
) -> anyhow::Result<bool> {
    let message =
        sync::Message::decode(payload).map_err(|e| anyhow::anyhow!("decode state sync: {}", e))?;

    let result: Option<(Option<Vec<u8>>, bool)> = room
        .state
        .with_doc(|state_doc| {
            let recv_result = catch_automerge_panic("state-receive-sync", || {
                state_doc.receive_sync_message_with_changes(state_peer_state, message)
            });
            let had_changes = match recv_result {
                Ok(Ok(changed)) => changed,
                Ok(Err(e)) => {
                    warn!("[notebook-sync] state receive_sync_message error: {}", e);
                    return Ok(None);
                }
                Err(e) => {
                    warn!("{}", e);
                    state_doc.rebuild_from_save();
                    *state_peer_state = sync::State::new();
                    return Ok(None);
                }
            };

            let reply = generate_runtime_state_sync_message(
                state_doc,
                state_peer_state,
                "state-sync-reply",
            );
            Ok(Some((reply, had_changes)))
        })
        .ok()
        .flatten();

    let (reply_encoded, had_changes) = match result {
        Some(pair) => pair,
        None => return Ok(false),
    };
    if let Some(encoded) = reply_encoded {
        writer.send_frame(NotebookFrameType::RuntimeStateSync, encoded)?;
    }

    if had_changes {
        // Save-relevant state arrived from a peer (kernel agent forwarding
        // outputs, or another notebook client). Wake the autosave debouncer.
        let _ = room.broadcasts.notebook_file_dirty_tx.send(());
    }

    persist_terminal_execution_records(room, store, persisted_records).await;
    Ok(true)
}

pub(super) async fn forward_runtime_state_broadcast(
    room: &NotebookRoom,
    peer_id: &str,
    state_peer_state: &mut sync::State,
    writer: &PeerWriter,
    result: Result<(), broadcast::error::RecvError>,
) -> anyhow::Result<bool> {
    match result {
        Ok(()) => {
            send_runtime_state_sync_update(room, state_peer_state, writer, "state-broadcast")?;
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            debug!(
                "[notebook-sync] Peer {} lagged {} runtime state updates",
                peer_id, n
            );
            send_runtime_state_sync_update(
                room,
                state_peer_state,
                writer,
                "state-broadcast-lagged",
            )?;
        }
        Err(broadcast::error::RecvError::Closed) => {
            // State change channel closed — room is being evicted.
            return Ok(false);
        }
    }
    Ok(true)
}

fn send_runtime_state_sync_update(
    room: &NotebookRoom,
    state_peer_state: &mut sync::State,
    writer: &PeerWriter,
    label: &str,
) -> anyhow::Result<()> {
    let encoded = room
        .state
        .with_doc(|state_doc| {
            Ok(generate_runtime_state_sync_message(
                state_doc,
                state_peer_state,
                label,
            ))
        })
        .ok()
        .flatten();
    if let Some(encoded) = encoded {
        writer.send_frame(NotebookFrameType::RuntimeStateSync, encoded)?;
    }
    Ok(())
}

fn generate_runtime_state_sync_message(
    state_doc: &mut runtime_doc::RuntimeStateDoc,
    state_peer_state: &mut sync::State,
    label: &str,
) -> Option<Vec<u8>> {
    match catch_automerge_panic(label, || {
        state_doc
            .generate_sync_message(state_peer_state)
            .map(|msg| msg.encode())
    }) {
        Ok(encoded) => encoded,
        Err(e) => {
            warn!("{}", e);
            state_doc.rebuild_from_save();
            *state_peer_state = sync::State::new();
            state_doc
                .generate_sync_message(state_peer_state)
                .map(|msg| msg.encode())
        }
    }
}

pub(super) async fn persist_terminal_execution_records(
    room: &NotebookRoom,
    store: &runtimed_client::execution_store::ExecutionStore,
    persisted_records: &mut PersistedExecutionRecords,
) {
    let notebook_path = room
        .identity
        .path
        .read()
        .await
        .as_ref()
        .map(|p| p.to_string_lossy().to_string());
    let context_id = notebook_execution_context_id(room, notebook_path.as_deref());
    let records = room
        .state
        .read(|sd| {
            sd.read_state()
                .executions
                .into_iter()
                .filter_map(|(execution_id, exec)| {
                    if !matches!(exec.status.as_str(), "done" | "error") {
                        return None;
                    }
                    Some(
                        runtimed_client::execution_store::ExecutionRecord::from_execution_state(
                            &execution_id,
                            "notebook",
                            context_id.clone(),
                            notebook_path.clone(),
                            &exec,
                        ),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    for record in records {
        if persisted_records
            .get(&record.execution_id)
            .is_some_and(|existing| existing.payload_matches(&record))
        {
            continue;
        }
        if let Err(e) = store.write_record(record.clone()).await {
            warn!(
                "[execution-store] Failed to persist execution record: {}",
                e
            );
        } else {
            persisted_records.insert(record.execution_id.clone(), record);
        }
    }
}

pub(crate) fn notebook_execution_context_id(
    room: &NotebookRoom,
    notebook_path: Option<&str>,
) -> String {
    notebook_path
        .map(str::to_string)
        .unwrap_or_else(|| room.id.to_string())
}
