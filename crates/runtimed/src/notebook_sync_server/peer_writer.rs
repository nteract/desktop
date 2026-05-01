use std::sync::Arc;

use automerge::ChangeHash;
use tokio::io::AsyncWrite;
use tokio::sync::mpsc;
use tracing::{debug, warn};

use crate::connection::{self, NotebookFrameType};

use super::NotebookRoom;
use crate::requests::{handle_notebook_request, request_label};

pub(super) const PEER_OUTBOUND_QUEUE_CAPACITY: usize = 1024;
const PEER_REQUEST_QUEUE_CAPACITY: usize = 64;
const SLOW_PEER_REQUEST: std::time::Duration = std::time::Duration::from_secs(30);
const REQUIRED_HEADS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

struct OutboundFrame {
    frame_type: NotebookFrameType,
    payload: Vec<u8>,
}

#[derive(Clone)]
pub(super) struct PeerWriter {
    tx: mpsc::Sender<OutboundFrame>,
}

pub(super) struct PeerWriterTask {
    pub(super) handle: tokio::task::JoinHandle<anyhow::Result<()>>,
}

pub(super) struct PeerRequestWorker {
    tx: mpsc::Sender<notebook_protocol::protocol::NotebookRequestEnvelope>,
    pub(super) handle: tokio::task::JoinHandle<anyhow::Result<()>>,
}

#[derive(Debug)]
pub(super) enum RequestEnqueueError {
    Full(notebook_protocol::protocol::NotebookRequestEnvelope),
    Closed(notebook_protocol::protocol::NotebookRequestEnvelope),
}

impl Drop for PeerWriterTask {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl Drop for PeerRequestWorker {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl PeerWriter {
    pub(super) fn send_frame(
        &self,
        frame_type: NotebookFrameType,
        payload: Vec<u8>,
    ) -> anyhow::Result<()> {
        self.tx
            .try_send(OutboundFrame {
                frame_type,
                payload,
            })
            .map_err(|e| match e {
                mpsc::error::TrySendError::Full(frame) => anyhow::anyhow!(
                    "peer outbound queue full while sending {:?} frame",
                    frame.frame_type
                ),
                mpsc::error::TrySendError::Closed(frame) => anyhow::anyhow!(
                    "peer writer stopped before sending {:?} frame",
                    frame.frame_type
                ),
            })
    }

    pub(super) fn send_json<T>(
        &self,
        frame_type: NotebookFrameType,
        value: &T,
    ) -> anyhow::Result<()>
    where
        T: serde::Serialize,
    {
        let payload = serde_json::to_vec(value)?;
        self.send_frame(frame_type, payload)
    }

    /// Number of free slots in the outbound channel.
    ///
    /// `PEER_OUTBOUND_QUEUE_CAPACITY - capacity()` gives the number of
    /// in-flight frames waiting to be flushed to the socket — useful as a
    /// backpressure signal in telemetry.
    pub(super) fn capacity(&self) -> usize {
        self.tx.capacity()
    }
}

impl PeerRequestWorker {
    pub(super) fn enqueue(
        &self,
        envelope: notebook_protocol::protocol::NotebookRequestEnvelope,
    ) -> Result<(), RequestEnqueueError> {
        self.tx.try_send(envelope).map_err(|e| match e {
            mpsc::error::TrySendError::Full(envelope) => RequestEnqueueError::Full(envelope),
            mpsc::error::TrySendError::Closed(envelope) => RequestEnqueueError::Closed(envelope),
        })
    }
}

pub(super) fn spawn_peer_writer<W>(
    mut writer: W,
    notebook_id: String,
    peer_id: String,
) -> (PeerWriter, PeerWriterTask)
where
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (tx, mut rx) = mpsc::channel::<OutboundFrame>(PEER_OUTBOUND_QUEUE_CAPACITY);
    let handle = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            connection::send_typed_frame(&mut writer, frame.frame_type, &frame.payload)
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "failed to write {:?} frame to peer {} for {}: {}",
                        frame.frame_type,
                        peer_id,
                        notebook_id,
                        e
                    )
                })?;
        }
        Ok(())
    });
    (PeerWriter { tx }, PeerWriterTask { handle })
}

pub(super) fn spawn_peer_request_worker(
    room: Arc<NotebookRoom>,
    daemon: Arc<crate::daemon::Daemon>,
    writer: PeerWriter,
    notebook_id: String,
    peer_id: String,
) -> PeerRequestWorker {
    let (tx, mut rx) = mpsc::channel::<notebook_protocol::protocol::NotebookRequestEnvelope>(
        PEER_REQUEST_QUEUE_CAPACITY,
    );
    let handle = tokio::spawn(async move {
        while let Some(envelope) = rx.recv().await {
            let label = request_label(&envelope.request);
            let req_id = envelope.id.as_deref().unwrap_or("-");
            let writer_queue_depth = PEER_OUTBOUND_QUEUE_CAPACITY - writer.capacity();
            debug!(
                "[notebook-sync] Request {} id={} peer={} notebook={} writer_queue={}",
                label, req_id, peer_id, notebook_id, writer_queue_depth,
            );

            let start = std::time::Instant::now();
            let response = match wait_for_required_heads(&room, &envelope.required_heads).await {
                Ok(()) => handle_notebook_request(&room, envelope.request, daemon.clone()).await,
                Err(error) => notebook_protocol::protocol::NotebookResponse::Error { error },
            };
            let elapsed = start.elapsed();
            if elapsed >= SLOW_PEER_REQUEST {
                let response_kind = std::mem::discriminant(&response);
                warn!(
                    request = label,
                    id = req_id,
                    peer = %peer_id,
                    notebook = %notebook_id,
                    elapsed_ms = elapsed.as_millis(),
                    writer_queue_depth,
                    ?response_kind,
                    "Slow notebook peer request"
                );
            }
            debug!(
                "[notebook-sync] Request {} id={} completed in {:?}",
                label, req_id, elapsed,
            );

            let reply = notebook_protocol::protocol::NotebookResponseEnvelope {
                id: envelope.id,
                response,
            };
            writer
                .send_json(NotebookFrameType::Response, &reply)
                .map_err(|e| {
                    anyhow::anyhow!(
                        "failed to queue response to peer {} for {}: {}",
                        peer_id,
                        notebook_id,
                        e
                    )
                })?;
        }
        Ok(())
    });
    PeerRequestWorker { tx, handle }
}

async fn wait_for_required_heads(
    room: &NotebookRoom,
    required_heads: &[String],
) -> Result<(), String> {
    if required_heads.is_empty() {
        return Ok(());
    }

    let heads = parse_required_heads(required_heads)?;
    let mut changed_rx = room.broadcasts.changed_tx.subscribe();

    tokio::time::timeout(REQUIRED_HEADS_TIMEOUT, async {
        loop {
            {
                let mut doc = room.doc.write().await;
                let has_all_heads = heads
                    .iter()
                    .all(|head| doc.doc_mut().get_change_by_hash(head).is_some());
                if has_all_heads {
                    return Ok(());
                }
            }

            match changed_rx.recv().await {
                Ok(()) => {}
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                    return Err("Required notebook heads could not be observed".to_string());
                }
            }
        }
    })
    .await
    .map_err(|_| "Timed out waiting for required notebook heads".to_string())?
}

fn parse_required_heads(required_heads: &[String]) -> Result<Vec<ChangeHash>, String> {
    required_heads
        .iter()
        .map(|head| {
            head.parse::<ChangeHash>()
                .map_err(|_| "Request contained an invalid required notebook head".to_string())
        })
        .collect()
}

pub(super) fn enqueue_notebook_request(
    request_worker: &PeerRequestWorker,
    writer: &PeerWriter,
    payload: &[u8],
    notebook_id: &str,
    peer_id: &str,
) -> anyhow::Result<()> {
    let envelope: notebook_protocol::protocol::NotebookRequestEnvelope =
        serde_json::from_slice(payload)?;
    debug!(
        "[notebook-sync] Enqueuing {} id={} peer={} notebook={}",
        request_label(&envelope.request),
        envelope.id.as_deref().unwrap_or("-"),
        peer_id,
        notebook_id,
    );

    if let Err(e) = request_worker.enqueue(envelope) {
        match e {
            RequestEnqueueError::Full(envelope) => {
                warn!(
                    "[notebook-sync] Peer request queue full for {} (peer_id={})",
                    notebook_id, peer_id
                );
                queue_request_error(writer, envelope.id, "Peer request queue full")?;
            }
            RequestEnqueueError::Closed(envelope) => {
                warn!(
                    "[notebook-sync] Peer request worker stopped for {} (peer_id={})",
                    notebook_id, peer_id
                );
                queue_request_error(writer, envelope.id, "Peer request worker stopped")?;
                anyhow::bail!("peer request worker stopped for {}", notebook_id);
            }
        }
    }
    Ok(())
}

pub(super) fn queue_request_error(
    writer: &PeerWriter,
    id: Option<String>,
    error: impl Into<String>,
) -> anyhow::Result<()> {
    writer.send_json(
        NotebookFrameType::Response,
        &notebook_protocol::protocol::NotebookResponseEnvelope {
            id,
            response: notebook_protocol::protocol::NotebookResponse::Error {
                error: error.into(),
            },
        },
    )
}

pub(super) fn queue_session_status(
    writer: &PeerWriter,
    notebook_doc: notebook_protocol::protocol::NotebookDocPhaseWire,
    runtime_state: notebook_protocol::protocol::RuntimeStatePhaseWire,
    initial_load: notebook_protocol::protocol::InitialLoadPhaseWire,
) -> anyhow::Result<()> {
    writer.send_json(
        NotebookFrameType::SessionControl,
        &notebook_protocol::protocol::SessionControlMessage::SyncStatus(
            notebook_protocol::protocol::SessionSyncStatusWire {
                notebook_doc,
                runtime_state,
                initial_load,
            },
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::blob_store::BlobStore;
    use crate::protocol::NotebookRequest;
    use tokio::sync::oneshot;
    use uuid::Uuid;

    fn test_room() -> Arc<NotebookRoom> {
        let tmp = tempfile::tempdir().expect("tempdir");
        let blob_store = Arc::new(BlobStore::new(tmp.path().join("blobs")));
        Arc::new(NotebookRoom::new_fresh(
            Uuid::new_v4(),
            None,
            tmp.path(),
            blob_store,
            true,
        ))
    }

    #[tokio::test]
    async fn required_heads_are_satisfied_by_present_change_history() {
        let room = test_room();
        let heads = {
            let mut doc = room.doc.write().await;
            doc.add_cell(0, "cell-1", "code").expect("add cell");
            doc.get_heads_hex()
        };

        wait_for_required_heads(&room, &heads)
            .await
            .expect("present heads should satisfy the causal gate");
    }

    #[tokio::test]
    async fn required_heads_wait_until_change_history_arrives() {
        let room = test_room();
        let mut incoming = {
            let mut doc = room.doc.write().await;
            doc.fork_with_actor("test:incoming")
        };
        incoming
            .add_cell(0, "cell-1", "code")
            .expect("add incoming cell");
        let heads = incoming.get_heads_hex();

        let wait_room = room.clone();
        let wait_heads = heads.clone();
        let waiter =
            tokio::spawn(async move { wait_for_required_heads(&wait_room, &wait_heads).await });

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(
            !waiter.is_finished(),
            "missing heads should keep the causal gate pending"
        );

        {
            let mut doc = room.doc.write().await;
            doc.merge(&mut incoming).expect("merge incoming change");
        }
        let _ = room.broadcasts.changed_tx.send(());

        waiter
            .await
            .expect("wait task should not panic")
            .expect("merged heads should satisfy the causal gate");
    }

    #[tokio::test]
    async fn peer_writer_enqueue_does_not_wait_for_socket_drain() {
        let (server, client) = tokio::io::duplex(16);
        let (writer, mut task) =
            spawn_peer_writer(server, "notebook".to_string(), "peer".to_string());

        writer
            .send_frame(NotebookFrameType::AutomergeSync, vec![1; 4096])
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        assert!(
            !task.handle.is_finished(),
            "writer task should be backpressured by the socket"
        );

        for index in 0..32 {
            writer
                .send_frame(NotebookFrameType::Presence, vec![index])
                .unwrap();
        }

        drop(writer);
        drop(client);

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), &mut task.handle)
            .await
            .expect("writer task should observe the closed socket")
            .expect("writer task should not panic");
        assert!(
            result.is_err(),
            "closed socket should surface as a writer task error"
        );
    }

    #[tokio::test]
    async fn peer_request_enqueue_reports_full_without_waiting_for_worker() {
        let (tx, mut rx) = mpsc::channel::<notebook_protocol::protocol::NotebookRequestEnvelope>(1);
        let (started_tx, started_rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            let _first = rx.recv().await;
            let _ = started_tx.send(());
            std::future::pending::<anyhow::Result<()>>().await
        });
        let worker = PeerRequestWorker { tx, handle };

        worker
            .enqueue(notebook_protocol::protocol::NotebookRequestEnvelope {
                id: Some("first".to_string()),
                required_heads: Vec::new(),
                request: NotebookRequest::GetDocBytes {},
            })
            .expect("first request should enqueue");
        started_rx.await.expect("worker should start first request");
        worker
            .enqueue(notebook_protocol::protocol::NotebookRequestEnvelope {
                id: Some("second".to_string()),
                required_heads: Vec::new(),
                request: NotebookRequest::GetDocBytes {},
            })
            .expect("second request should fill the queue");

        let start = std::time::Instant::now();
        let err = worker
            .enqueue(notebook_protocol::protocol::NotebookRequestEnvelope {
                id: Some("third".to_string()),
                required_heads: Vec::new(),
                request: NotebookRequest::GetDocBytes {},
            })
            .expect_err("full queue should reject immediately");
        assert!(
            start.elapsed() < std::time::Duration::from_millis(50),
            "request enqueue should not wait for the busy worker"
        );
        assert!(matches!(err, RequestEnqueueError::Full(_)));
    }

    #[tokio::test]
    async fn enqueue_notebook_request_rejects_malformed_payload_without_reply() {
        let (request_tx, _request_rx) =
            mpsc::channel::<notebook_protocol::protocol::NotebookRequestEnvelope>(1);
        let request_worker = PeerRequestWorker {
            tx: request_tx,
            handle: tokio::spawn(std::future::pending::<anyhow::Result<()>>()),
        };
        let (writer_tx, mut writer_rx) = mpsc::channel::<OutboundFrame>(1);
        let writer = PeerWriter { tx: writer_tx };

        let err =
            enqueue_notebook_request(&request_worker, &writer, b"not json", "notebook", "peer")
                .expect_err("malformed request payload should fail");
        assert!(err.to_string().contains("expected ident"));
        assert!(
            writer_rx.try_recv().is_err(),
            "malformed envelopes have no request id to echo"
        );
    }

    #[tokio::test]
    async fn enqueue_notebook_request_reports_full_queue_to_peer() {
        let (request_tx, _request_rx) =
            mpsc::channel::<notebook_protocol::protocol::NotebookRequestEnvelope>(1);
        request_tx
            .try_send(notebook_protocol::protocol::NotebookRequestEnvelope {
                id: Some("first".to_string()),
                required_heads: Vec::new(),
                request: NotebookRequest::GetDocBytes {},
            })
            .expect("queue should accept first request");
        let request_worker = PeerRequestWorker {
            tx: request_tx,
            handle: tokio::spawn(std::future::pending::<anyhow::Result<()>>()),
        };
        let (writer_tx, mut writer_rx) = mpsc::channel::<OutboundFrame>(1);
        let writer = PeerWriter { tx: writer_tx };

        let payload = serde_json::to_vec(&notebook_protocol::protocol::NotebookRequestEnvelope {
            id: Some("second".to_string()),
            required_heads: Vec::new(),
            request: NotebookRequest::GetDocBytes {},
        })
        .unwrap();
        enqueue_notebook_request(&request_worker, &writer, &payload, "notebook", "peer")
            .expect("full queue should be reported to the peer without failing the loop");

        let frame = writer_rx
            .try_recv()
            .expect("full queue should enqueue an error response");
        assert_eq!(frame.frame_type, NotebookFrameType::Response);
        let reply: notebook_protocol::protocol::NotebookResponseEnvelope =
            serde_json::from_slice(&frame.payload).unwrap();
        assert_eq!(reply.id.as_deref(), Some("second"));
        assert!(matches!(
            reply.response,
            notebook_protocol::protocol::NotebookResponse::Error { ref error }
                if error == "Peer request queue full"
        ));
    }

    #[tokio::test]
    async fn enqueue_notebook_request_reports_closed_worker_then_fails() {
        let (request_tx, request_rx) =
            mpsc::channel::<notebook_protocol::protocol::NotebookRequestEnvelope>(1);
        drop(request_rx);
        let request_worker = PeerRequestWorker {
            tx: request_tx,
            handle: tokio::spawn(std::future::pending::<anyhow::Result<()>>()),
        };
        let (writer_tx, mut writer_rx) = mpsc::channel::<OutboundFrame>(1);
        let writer = PeerWriter { tx: writer_tx };

        let payload = serde_json::to_vec(&notebook_protocol::protocol::NotebookRequestEnvelope {
            id: Some("closed".to_string()),
            required_heads: Vec::new(),
            request: NotebookRequest::GetDocBytes {},
        })
        .unwrap();
        let err = enqueue_notebook_request(&request_worker, &writer, &payload, "notebook", "peer")
            .expect_err("closed worker should stop the peer loop");
        assert_eq!(err.to_string(), "peer request worker stopped for notebook");

        let frame = writer_rx
            .try_recv()
            .expect("closed worker should enqueue an error response before failing");
        assert_eq!(frame.frame_type, NotebookFrameType::Response);
        let reply: notebook_protocol::protocol::NotebookResponseEnvelope =
            serde_json::from_slice(&frame.payload).unwrap();
        assert_eq!(reply.id.as_deref(), Some("closed"));
        assert!(matches!(
            reply.response,
            notebook_protocol::protocol::NotebookResponse::Error { ref error }
                if error == "Peer request worker stopped"
        ));
    }
}
