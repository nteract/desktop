use std::path::Path;
use std::sync::Arc;

use automerge::sync;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::{info, warn};

use crate::connection::{self, NotebookFrameType};

use super::{
    catch_automerge_panic, streaming_load_cells, NotebookRoom, STATE_SYNC_COMPACT_THRESHOLD,
};

pub(crate) async fn send_session_status<W>(
    writer: &mut W,
    notebook_doc: notebook_protocol::protocol::NotebookDocPhaseWire,
    runtime_state: notebook_protocol::protocol::RuntimeStatePhaseWire,
    initial_load: notebook_protocol::protocol::InitialLoadPhaseWire,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    connection::send_typed_json_frame(
        writer,
        NotebookFrameType::SessionControl,
        &notebook_protocol::protocol::SessionControlMessage::SyncStatus(
            notebook_protocol::protocol::SessionSyncStatusWire {
                notebook_doc,
                runtime_state,
                initial_load,
            },
        ),
    )
    .await?;
    Ok(())
}

/// State carried from the initial notebook-doc sync into the steady-state loop.
///
/// See [`send_initial_notebook_doc_sync`]. `peer_state` tracks what the
/// daemon has already advertised about the notebook doc so subsequent
/// generate_sync_message calls compute correct deltas (including deltas
/// emitted by `streaming_load_cells`).
pub(crate) struct InitialSyncState {
    pub(crate) peer_state: sync::State,
}

impl InitialSyncState {
    fn new() -> Self {
        Self {
            peer_state: sync::State::new(),
        }
    }
}

/// Generate and send the initial notebook-doc AutomergeSync frame.
///
/// Returns the `peer_state` so the rest of bootstrap (and streaming load)
/// continues from the same baseline and emits correct deltas.
pub(crate) async fn send_initial_notebook_doc_sync<W>(
    writer: &mut W,
    room: &Arc<NotebookRoom>,
) -> anyhow::Result<InitialSyncState>
where
    W: AsyncWrite + Unpin,
{
    let mut sync_state = InitialSyncState::new();

    // Encode the sync message inside the lock, then send outside it to avoid
    // holding the write lock across async I/O.
    let initial_encoded = {
        let mut doc = room.doc.write().await;
        match catch_automerge_panic("initial-doc-sync", || {
            doc.generate_sync_message(&mut sync_state.peer_state)
                .map(|msg| msg.encode())
        }) {
            Ok(encoded) => encoded,
            Err(e) => {
                warn!("{}", e);
                sync_state.peer_state = sync::State::new();
                if doc.rebuild_from_save() {
                    doc.generate_sync_message(&mut sync_state.peer_state)
                        .map(|msg| msg.encode())
                } else {
                    // Cell-count guard prevented rebuild — skip sync message,
                    // fresh peer_state will trigger full re-sync on next exchange
                    None
                }
            }
        }
    };
    if let Some(encoded) = initial_encoded {
        connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &encoded).await?;
    }

    Ok(sync_state)
}

/// Generate and send the initial RuntimeStateDoc sync frame.
///
/// The caller owns `state_peer_state` because the steady-state peer loop uses
/// the same sync state to compute later RuntimeStateDoc deltas.
pub(crate) async fn send_initial_runtime_state_sync<W>(
    writer: &mut W,
    room: &Arc<NotebookRoom>,
    state_peer_state: &mut sync::State,
) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    // Encode inside the RuntimeStateDoc lock, then send outside it to avoid
    // holding state while awaiting socket I/O.
    let initial_state_encoded = room
        .state
        .with_doc(|state_doc| {
            // Safety net: compact before initial sync if the doc grew too large.
            // 80 MiB leaves headroom under the 100 MiB frame limit.
            const COMPACTION_THRESHOLD: usize = 80 * 1024 * 1024;
            if state_doc.compact_if_oversized(COMPACTION_THRESHOLD) {
                info!("[notebook-sync] Compacted oversized RuntimeStateDoc before initial sync");
            }
            match catch_automerge_panic("initial-state-sync", || {
                state_doc.generate_sync_message_bounded_encoded(
                    state_peer_state,
                    STATE_SYNC_COMPACT_THRESHOLD,
                )
            }) {
                Ok(encoded) => Ok(encoded),
                Err(e) => {
                    warn!("{}", e);
                    state_doc.rebuild_from_save();
                    *state_peer_state = sync::State::new();
                    Ok(state_doc
                        .generate_sync_message(state_peer_state)
                        .map(|msg| msg.encode()))
                }
            }
        })
        .ok()
        .flatten();
    if let Some(encoded) = initial_state_encoded {
        connection::send_typed_frame(writer, NotebookFrameType::RuntimeStateSync, &encoded).await?;
    }

    Ok(())
}

/// Stream initial notebook file contents into the room before steady-state sync.
///
/// The caller passes `peer_state` from the initial notebook-doc sync so each
/// streamed batch can produce deltas from the same baseline.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn stream_initial_load<R, W>(
    reader: &mut R,
    writer: &mut W,
    room: &Arc<NotebookRoom>,
    needs_load: Option<&Path>,
    execution_store_dir: &Path,
    peer_state: &mut sync::State,
    notebook_doc_phase: notebook_protocol::protocol::NotebookDocPhaseWire,
    runtime_state_phase: notebook_protocol::protocol::RuntimeStatePhaseWire,
    initial_load_phase: notebook_protocol::protocol::InitialLoadPhaseWire,
    client_protocol_version: u8,
) -> anyhow::Result<notebook_protocol::protocol::InitialLoadPhaseWire>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let Some(load_path) = needs_load else {
        return Ok(initial_load_phase);
    };
    if !room.try_start_loading() {
        return Ok(initial_load_phase);
    }

    // Streaming load: add cells in batches and sync after each batch so the
    // frontend can observe progressive notebook-doc updates.
    let execution_store =
        runtimed_client::execution_store::ExecutionStore::new(execution_store_dir.to_path_buf());
    match streaming_load_cells(
        reader,
        writer,
        room,
        load_path,
        Some(&execution_store),
        peer_state,
    )
    .await
    {
        Ok(count) => {
            room.finish_loading();
            info!(
                "[notebook-sync] Streaming load complete: {} cells from {}",
                count,
                load_path.display()
            );
            let initial_load_phase = notebook_protocol::protocol::InitialLoadPhaseWire::Ready;
            if client_protocol_version >= 3 {
                send_session_status(
                    writer,
                    notebook_doc_phase,
                    runtime_state_phase,
                    initial_load_phase.clone(),
                )
                .await?;
            }
            Ok(initial_load_phase)
        }
        Err(e) => {
            let cell_ids = {
                let mut doc = room.doc.write().await;
                let cell_ids = doc
                    .get_cells()
                    .into_iter()
                    .map(|cell| cell.id)
                    .collect::<Vec<_>>();
                if let Err(err) = doc.clear_all_cells() {
                    warn!(
                        "[notebook-sync] Failed to clear partial load cells for {}: {}",
                        load_path.display(),
                        err
                    );
                }
                cell_ids
            };
            if !cell_ids.is_empty() {
                if let Err(err) = room
                    .state
                    .with_doc(|state_doc| state_doc.remove_executions_for_cells(&cell_ids))
                {
                    warn!(
                        "[notebook-sync] Failed to remove partial load executions for {}: {}",
                        load_path.display(),
                        err
                    );
                }
            }
            room.finish_loading();
            let _ = room.broadcasts.changed_tx.send(());
            warn!(
                "[notebook-sync] Streaming load failed for {}: {}",
                load_path.display(),
                e
            );
            if client_protocol_version >= 3 {
                send_session_status(
                    writer,
                    notebook_doc_phase,
                    runtime_state_phase,
                    notebook_protocol::protocol::InitialLoadPhaseWire::Failed { reason: e.clone() },
                )
                .await?;
            }
            Err(anyhow::anyhow!("Streaming load failed: {}", e))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::Pin;
    use std::sync::Arc;
    use std::task::{Context, Poll};

    use notebook_protocol::protocol::{
        InitialLoadPhaseWire, NotebookDocPhaseWire, RuntimeStatePhaseWire, SessionControlMessage,
    };
    use tokio::io::AsyncWrite;
    use uuid::Uuid;

    use super::*;
    use crate::blob_store::BlobStore;

    #[derive(Default)]
    struct CaptureWriter {
        bytes: Vec<u8>,
    }

    impl AsyncWrite for CaptureWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.bytes.extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[derive(Default)]
    struct FailFirstWrite {
        bytes: Vec<u8>,
        failed: bool,
        partial_room: Option<Arc<NotebookRoom>>,
        observed_partial_doc: bool,
        observed_partial_executions: bool,
    }

    impl FailFirstWrite {
        fn expecting_partial_state(room: &Arc<NotebookRoom>) -> Self {
            Self {
                partial_room: Some(Arc::clone(room)),
                ..Self::default()
            }
        }
    }

    impl AsyncWrite for FailFirstWrite {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            if !self.failed {
                self.failed = true;
                if let Some(room) = self.partial_room.clone() {
                    let doc = room
                        .doc
                        .try_read()
                        .expect("notebook doc lock should be released before socket write");
                    assert!(
                        doc.cell_count() > 0,
                        "injected write failure should happen after partial cells are loaded"
                    );
                    drop(doc);
                    self.observed_partial_doc = true;

                    let observed_partial_executions = room
                        .state
                        .with_doc(|state_doc| Ok(!state_doc.read_state().executions.is_empty()))
                        .expect("runtime state doc should be readable before injected failure");
                    assert!(
                        observed_partial_executions,
                        "injected write failure should happen after partial executions are loaded"
                    );
                    self.observed_partial_executions = true;
                }
                return Poll::Ready(Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "injected first write failure",
                )));
            }
            self.bytes.extend_from_slice(buf);
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn test_room(tmp: &tempfile::TempDir) -> Arc<NotebookRoom> {
        let blob_store = Arc::new(BlobStore::new(tmp.path().join("blobs")));
        Arc::new(NotebookRoom::new_fresh(
            Uuid::new_v4(),
            None,
            tmp.path(),
            blob_store,
            true,
        ))
    }

    async fn write_one_cell_notebook(path: &Path) {
        tokio::fs::write(
            path,
            r##"{
                "nbformat": 4,
                "nbformat_minor": 5,
                "metadata": {},
                "cells": [
                    {
                        "id": "loaded-cell",
                        "cell_type": "code",
                        "metadata": {},
                        "source": "x = 1",
                        "execution_count": 7,
                        "outputs": [
                            {
                                "output_type": "stream",
                                "name": "stdout",
                                "text": "hello\n"
                            }
                        ]
                    }
                ]
            }"##,
        )
        .await
        .unwrap();
    }

    fn frame_types(bytes: &[u8]) -> Vec<NotebookFrameType> {
        let mut types = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let len = u32::from_be_bytes(
                bytes[offset..offset + 4]
                    .try_into()
                    .expect("frame length prefix"),
            ) as usize;
            offset += 4;
            let frame = &bytes[offset..offset + len];
            offset += len;
            types.push(NotebookFrameType::try_from(frame[0]).expect("known frame type"));
        }
        types
    }

    fn decode_session_statuses(bytes: &[u8]) -> Vec<SessionControlMessage> {
        let mut statuses = Vec::new();
        let mut offset = 0;
        while offset < bytes.len() {
            let len = u32::from_be_bytes(
                bytes[offset..offset + 4]
                    .try_into()
                    .expect("frame length prefix"),
            ) as usize;
            offset += 4;
            let frame = &bytes[offset..offset + len];
            offset += len;

            if frame[0] == NotebookFrameType::SessionControl as u8 {
                statuses.push(serde_json::from_slice(&frame[1..]).expect("session control json"));
            }
        }
        statuses
    }

    #[tokio::test]
    async fn stream_initial_load_contention_leaves_owner_loading_state_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let room = test_room(&tmp);
        let load_path = tmp.path().join("missing-but-not-read.ipynb");
        let mut reader = tokio::io::empty();
        let mut writer = CaptureWriter::default();
        let mut peer_state = sync::State::new();

        assert!(room.try_start_loading(), "first peer owns streaming load");

        let phase = stream_initial_load(
            &mut reader,
            &mut writer,
            &room,
            Some(&load_path),
            tmp.path(),
            &mut peer_state,
            NotebookDocPhaseWire::Syncing,
            RuntimeStatePhaseWire::Syncing,
            InitialLoadPhaseWire::Streaming,
            4,
        )
        .await
        .expect("contending peer should not fail");

        assert_eq!(phase, InitialLoadPhaseWire::Streaming);
        assert!(
            writer.bytes.is_empty(),
            "contending peer must not emit Ready/Failed before the owner finishes loading"
        );
        assert!(
            room.is_loading(),
            "contending peer must leave the owner loading marker intact"
        );

        room.finish_loading();
        assert!(
            !room.is_loading(),
            "steady-state changed_rx readiness check can now promote Streaming to Ready"
        );
    }

    #[tokio::test]
    async fn stream_initial_load_failure_emits_failed_status_and_clears_partial_doc() {
        let tmp = tempfile::tempdir().unwrap();
        let room = test_room(&tmp);
        let load_path = tmp.path().join("source.ipynb");
        write_one_cell_notebook(&load_path).await;
        let mut changed_rx = room.broadcasts.changed_tx.subscribe();
        let mut reader = tokio::io::empty();
        let mut writer = FailFirstWrite::expecting_partial_state(&room);
        let mut peer_state = sync::State::new();

        let err = stream_initial_load(
            &mut reader,
            &mut writer,
            &room,
            Some(&load_path),
            tmp.path(),
            &mut peer_state,
            NotebookDocPhaseWire::Syncing,
            RuntimeStatePhaseWire::Syncing,
            InitialLoadPhaseWire::Streaming,
            4,
        )
        .await
        .expect_err("missing file should fail streaming load");

        assert!(
            err.to_string()
                .contains("Failed to send sync message: injected first write failure"),
            "error should preserve the peer-loop failure prefix"
        );
        assert!(writer.observed_partial_doc);
        assert!(writer.observed_partial_executions);
        assert!(
            !room.is_loading(),
            "failure cleanup must release the loading marker"
        );
        assert_eq!(
            room.doc.read().await.cell_count(),
            0,
            "failure cleanup must clear partially loaded cells"
        );
        room.state
            .with_doc(|state_doc| {
                assert!(
                    state_doc.read_state().executions.is_empty(),
                    "failure cleanup must remove executions for rolled-back cells"
                );
                Ok(())
            })
            .unwrap();
        changed_rx
            .try_recv()
            .expect("failure cleanup should broadcast document change");
        assert!(
            changed_rx.try_recv().is_err(),
            "failure cleanup should emit exactly one cleanup broadcast"
        );

        let statuses = decode_session_statuses(&writer.bytes);
        assert_eq!(statuses.len(), 1);
        let SessionControlMessage::SyncStatus(status) = &statuses[0];
        assert_eq!(status.notebook_doc, NotebookDocPhaseWire::Syncing);
        assert_eq!(status.runtime_state, RuntimeStatePhaseWire::Syncing);
        match &status.initial_load {
            InitialLoadPhaseWire::Failed { reason } => {
                assert!(
                    reason.contains("Failed to send sync message"),
                    "failed status should include the load failure reason: {reason}"
                );
            }
            other => panic!("expected failed initial load status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_initial_load_v2_client_suppresses_session_status_frames() {
        let tmp = tempfile::tempdir().unwrap();
        let room = test_room(&tmp);
        let load_path = tmp.path().join("source.ipynb");
        write_one_cell_notebook(&load_path).await;
        let mut reader = tokio::io::empty();
        let mut writer = CaptureWriter::default();
        let mut peer_state = sync::State::new();

        let phase = stream_initial_load(
            &mut reader,
            &mut writer,
            &room,
            Some(&load_path),
            tmp.path(),
            &mut peer_state,
            NotebookDocPhaseWire::Syncing,
            RuntimeStatePhaseWire::Syncing,
            InitialLoadPhaseWire::Streaming,
            2,
        )
        .await
        .expect("valid notebook should stream successfully");

        assert_eq!(phase, InitialLoadPhaseWire::Ready);
        let emitted_frame_types = frame_types(&writer.bytes);
        assert!(
            !emitted_frame_types.is_empty(),
            "pre-v3 success must still emit document sync frames"
        );
        assert!(
            decode_session_statuses(&writer.bytes).is_empty(),
            "pre-v3 clients must not receive SessionControl frames"
        );
        assert!(
            emitted_frame_types
                .iter()
                .all(|frame_type| *frame_type == NotebookFrameType::AutomergeSync),
            "pre-v3 success should only emit Automerge sync frames"
        );
    }

    #[tokio::test]
    async fn stream_initial_load_v2_failure_suppresses_failed_status_frame() {
        let tmp = tempfile::tempdir().unwrap();
        let room = test_room(&tmp);
        let load_path = tmp.path().join("source.ipynb");
        write_one_cell_notebook(&load_path).await;
        let mut reader = tokio::io::empty();
        let mut writer = FailFirstWrite::expecting_partial_state(&room);
        let mut peer_state = sync::State::new();

        stream_initial_load(
            &mut reader,
            &mut writer,
            &room,
            Some(&load_path),
            tmp.path(),
            &mut peer_state,
            NotebookDocPhaseWire::Syncing,
            RuntimeStatePhaseWire::Syncing,
            InitialLoadPhaseWire::Streaming,
            2,
        )
        .await
        .expect_err("injected write failure should fail streaming load");

        assert!(writer.observed_partial_doc);
        assert!(writer.observed_partial_executions);
        assert!(
            writer.bytes.is_empty(),
            "pre-v3 failure must not emit SessionControl frames"
        );
        assert!(!room.is_loading());
        assert_eq!(room.doc.read().await.cell_count(), 0);
        room.state
            .with_doc(|state_doc| {
                assert!(
                    state_doc.read_state().executions.is_empty(),
                    "pre-v3 failure cleanup must remove rolled-back executions"
                );
                Ok(())
            })
            .unwrap();
    }

    #[tokio::test]
    async fn stream_initial_load_missing_file_emits_failed_status() {
        let tmp = tempfile::tempdir().unwrap();
        let room = test_room(&tmp);
        let load_path = tmp.path().join("does-not-exist.ipynb");
        let mut changed_rx = room.broadcasts.changed_tx.subscribe();
        let mut reader = tokio::io::empty();
        let mut writer = CaptureWriter::default();
        let mut peer_state = sync::State::new();

        let err = stream_initial_load(
            &mut reader,
            &mut writer,
            &room,
            Some(&load_path),
            tmp.path(),
            &mut peer_state,
            NotebookDocPhaseWire::Syncing,
            RuntimeStatePhaseWire::Syncing,
            InitialLoadPhaseWire::Streaming,
            4,
        )
        .await
        .expect_err("missing file should fail streaming load");

        assert!(
            err.to_string()
                .contains("Streaming load failed: Failed to read notebook"),
            "missing file should preserve the load failure reason"
        );
        changed_rx
            .try_recv()
            .expect("missing-file cleanup should broadcast document change");
        assert!(changed_rx.try_recv().is_err());

        let statuses = decode_session_statuses(&writer.bytes);
        assert_eq!(statuses.len(), 1);
        let SessionControlMessage::SyncStatus(status) = &statuses[0];
        match &status.initial_load {
            InitialLoadPhaseWire::Failed { reason } => {
                assert!(
                    reason.contains("Failed to read notebook"),
                    "failed status should include the missing-file reason: {reason}"
                );
            }
            other => panic!("expected failed initial load status, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn stream_initial_load_success_orders_doc_sync_before_ready_status() {
        let tmp = tempfile::tempdir().unwrap();
        let room = test_room(&tmp);
        let load_path = tmp.path().join("source.ipynb");
        write_one_cell_notebook(&load_path).await;
        let mut reader = tokio::io::empty();
        let mut writer = CaptureWriter::default();
        let mut peer_state = sync::State::new();

        let phase = stream_initial_load(
            &mut reader,
            &mut writer,
            &room,
            Some(&load_path),
            tmp.path(),
            &mut peer_state,
            NotebookDocPhaseWire::Syncing,
            RuntimeStatePhaseWire::Syncing,
            InitialLoadPhaseWire::Streaming,
            4,
        )
        .await
        .expect("valid notebook should stream successfully");

        assert_eq!(phase, InitialLoadPhaseWire::Ready);
        assert!(!room.is_loading(), "successful load should finish loading");
        let cells = room.doc.read().await.get_cells();
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].id, "loaded-cell");
        assert_eq!(cells[0].source, "x = 1");

        let frame_types = frame_types(&writer.bytes);
        assert!(
            frame_types.len() >= 2,
            "success should emit at least one doc sync followed by Ready"
        );
        assert_eq!(
            frame_types.last(),
            Some(&NotebookFrameType::SessionControl),
            "Ready must be the final bootstrap frame"
        );
        assert!(
            frame_types[..frame_types.len() - 1]
                .iter()
                .all(|frame_type| *frame_type == NotebookFrameType::AutomergeSync),
            "all frames before Ready must be document sync frames"
        );

        let statuses = decode_session_statuses(&writer.bytes);
        assert_eq!(statuses.len(), 1);
        let SessionControlMessage::SyncStatus(status) = &statuses[0];
        assert_eq!(status.notebook_doc, NotebookDocPhaseWire::Syncing);
        assert_eq!(status.runtime_state, RuntimeStatePhaseWire::Syncing);
        assert_eq!(status.initial_load, InitialLoadPhaseWire::Ready);
    }

    #[tokio::test]
    async fn stream_initial_load_success_records_non_null_execution_count() {
        let tmp = tempfile::tempdir().unwrap();
        let room = test_room(&tmp);
        let load_path = tmp.path().join("source.ipynb");
        write_one_cell_notebook(&load_path).await;
        let mut reader = tokio::io::empty();
        let mut writer = CaptureWriter::default();
        let mut peer_state = sync::State::new();

        stream_initial_load(
            &mut reader,
            &mut writer,
            &room,
            Some(&load_path),
            tmp.path(),
            &mut peer_state,
            NotebookDocPhaseWire::Syncing,
            RuntimeStatePhaseWire::Syncing,
            InitialLoadPhaseWire::Streaming,
            4,
        )
        .await
        .expect("valid notebook should stream successfully");

        room.state
            .with_doc(|state_doc| {
                let state = state_doc.read_state();
                let execution = state
                    .executions
                    .values()
                    .find(|execution| execution.cell_id == "loaded-cell")
                    .expect("loaded code cell should have an execution record");
                assert_eq!(execution.execution_count, Some(7));
                Ok(())
            })
            .unwrap();
    }
}
