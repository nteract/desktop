//! Process-isolated runtime agent.
//!
//! The agent is a subprocess spawned by the coordinator (daemon) that owns
//! the kernel lifecycle, IOPub processing, execution queue, and RuntimeStateDoc
//! writes. It communicates with the coordinator over stdin/stdout using the
//! existing framed protocol.
//!
//! ## Sync-only architecture
//!
//! The agent does **not** forward `NotebookBroadcast` messages. All kernel
//! state changes (outputs, execution lifecycle, queue, comms, kernel status)
//! flow through RuntimeStateDoc Automerge sync (frame 0x05). The only
//! explicit messages are `AgentNotification` for things the coordinator must
//! act on outside of RuntimeStateDoc (writing to NotebookDoc, cleaning up
//! the agent handle).
//!
//! ## Protocol
//!
//! - Frame 0x01: `AgentRequest` (coordinator → agent)
//! - Frame 0x02: `AgentResponse` (agent → coordinator)
//! - Frame 0x03: `AgentNotification` (agent → coordinator, rare)
//! - Frame 0x05: `RuntimeStateSync` (bidirectional Automerge sync)
//!
//! ## Lifecycle
//!
//! 1. Coordinator spawns agent, sends preamble + `Handshake::RuntimeAgent`
//! 2. Agent bootstraps RuntimeStateDoc via initial sync (frame 0x05)
//! 3. Agent creates RoomKernel with local channels
//! 4. Main select loop: stdin frames, RuntimeStateDoc changes, QueueCommands
//! 5. On shutdown or coordinator disconnect, agent exits

use std::sync::Arc;

use log::{debug, info, warn};
use notebook_doc::presence::PresenceState;
use notebook_doc::runtime_state::RuntimeStateDoc;
use notebook_protocol::connection::{
    recv_frame, recv_json_frame, recv_preamble, send_preamble, send_typed_frame, Handshake,
    NotebookFrameType,
};
use notebook_protocol::protocol::{AgentRequest, AgentResponse};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, RwLock};

use crate::blob_store::BlobStore;
use crate::kernel_manager::{QueueCommand, RoomKernel};

/// Shared agent state passed to request/command handlers.
struct AgentContext {
    kernel: Arc<tokio::sync::Mutex<Option<RoomKernel>>>,
    state_doc: Arc<RwLock<RuntimeStateDoc>>,
    state_changed_tx: broadcast::Sender<()>,
    blob_store: Arc<BlobStore>,
    /// RoomKernel requires a broadcast_tx at construction. The agent creates
    /// a channel but never reads from it — all state flows via CRDT sync.
    broadcast_tx: broadcast::Sender<notebook_protocol::protocol::NotebookBroadcast>,
    presence: Arc<RwLock<PresenceState>>,
    presence_tx: broadcast::Sender<(String, Vec<u8>)>,
}

/// Run the runtime agent on the given stdin/stdout streams.
///
/// This is the agent's main entry point. It reads the handshake from stdin,
/// bootstraps its RuntimeStateDoc, and enters the main event loop.
pub async fn run_agent<R, W>(mut reader: R, mut writer: W) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    // ── 1. Read preamble + handshake ────────────────────────────────────────

    recv_preamble(&mut reader).await?;
    let handshake: Handshake = recv_json_frame(&mut reader)
        .await?
        .ok_or_else(|| anyhow::anyhow!("EOF before handshake"))?;

    let (notebook_id, agent_id, blob_root) = match handshake {
        Handshake::RuntimeAgent {
            notebook_id,
            agent_id,
            blob_root,
        } => (notebook_id, agent_id, blob_root),
        other => {
            anyhow::bail!(
                "Expected RuntimeAgent handshake, got {:?}",
                std::mem::discriminant(&other)
            );
        }
    };

    info!(
        "[agent] Starting agent_id={} notebook_id={} blob_root={}",
        agent_id, notebook_id, blob_root
    );

    // Send preamble back to coordinator (confirms protocol version)
    send_preamble(&mut writer).await?;

    // ── 2. Bootstrap RuntimeStateDoc ────────────────────────────────────────

    // Start with an empty doc (no scaffolding) to avoid DuplicateSeqNumber
    // conflicts when syncing with the coordinator. All state arrives via sync.
    let mut state_doc = RuntimeStateDoc::new_empty();
    state_doc.set_actor(&agent_id);

    let state_doc = Arc::new(RwLock::new(state_doc));
    let (state_changed_tx, mut state_changed_rx) = broadcast::channel::<()>(64);

    // Automerge sync state for the coordinator peer. Persistent across the
    // agent's lifetime so incremental sync works correctly.
    let mut coordinator_sync_state = automerge::sync::State::new();

    // ── 3. Create local infrastructure ──────────────────────────────────────

    let blob_store = Arc::new(BlobStore::new(std::path::PathBuf::from(&blob_root)));

    // RoomKernel requires a broadcast channel even though we don't forward
    // broadcasts. The kernel's IOPub task sends on it; we just don't read.
    let (broadcast_tx, _broadcast_rx) =
        broadcast::channel::<notebook_protocol::protocol::NotebookBroadcast>(16);

    // Presence — kernel writes status here but we rely on RuntimeStateDoc
    // sync for propagation, not presence frames.
    let presence = Arc::new(RwLock::new(PresenceState::new()));
    let (presence_tx, _presence_rx) = broadcast::channel::<(String, Vec<u8>)>(16);

    let ctx = AgentContext {
        kernel: Arc::new(tokio::sync::Mutex::new(None)),
        state_doc,
        state_changed_tx,
        blob_store,
        broadcast_tx,
        presence,
        presence_tx,
    };

    // QueueCommand receiver — starts as None. After LaunchKernel, populated
    // with the kernel's actual cmd_rx (taken via kernel.take_cmd_rx()).
    let mut cmd_rx: Option<tokio::sync::mpsc::Receiver<QueueCommand>> = None;

    info!("[agent] Infrastructure ready, entering main loop");

    // ── 4. Main event loop ──────────────────────────────────────────────────
    //
    // Three input sources:
    //   - stdin: AgentRequest frames (0x01), RuntimeStateSync frames (0x05)
    //   - cmd_rx: QueueCommands from kernel IOPub/shell tasks
    //   - state_changed_rx: RuntimeStateDoc mutations to sync to coordinator
    //
    // Output (stdout): AgentResponse (0x02), AgentNotification (0x03),
    //   RuntimeStateSync (0x05)

    loop {
        tokio::select! {
            // Read next frame from coordinator (stdin)
            frame = recv_frame(&mut reader) => {
                match frame {
                    Ok(Some(data)) => {
                        if let Err(e) = handle_stdin_frame(
                            &data,
                            &mut writer,
                            &ctx,
                            &mut coordinator_sync_state,
                            &mut cmd_rx,
                        ).await {
                            warn!("[agent] Error handling stdin frame: {}", e);
                        }
                    }
                    Ok(None) => {
                        info!("[agent] EOF on stdin, shutting down");
                        break;
                    }
                    Err(e) => {
                        info!("[agent] stdin read error (coordinator disconnected?): {}", e);
                        break;
                    }
                }
            }

            // Process QueueCommands from kernel tasks
            Some(command) = async {
                match cmd_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                if let Err(e) = handle_queue_command(command, &ctx).await {
                    warn!("[agent] Error handling queue command: {}", e);
                }
            }

            // Sync RuntimeStateDoc changes to coordinator
            _ = state_changed_rx.recv() => {
                // Drain any buffered notifications
                while state_changed_rx.try_recv().is_ok() {}

                // Generate sync message and send as frame 0x05
                let mut sd = ctx.state_doc.write().await;
                if let Some(msg) = sd.generate_sync_message(&mut coordinator_sync_state) {
                    let encoded = msg.encode();
                    if let Err(e) = send_typed_frame(
                        &mut writer,
                        NotebookFrameType::RuntimeStateSync,
                        &encoded,
                    ).await {
                        warn!("[agent] Failed to send RuntimeStateSync: {}", e);
                        break;
                    }
                }
            }
        }
    }

    // ── 5. Cleanup ──────────────────────────────────────────────────────────

    info!("[agent] Shutting down");
    let mut guard = ctx.kernel.lock().await;
    if let Some(ref mut k) = *guard {
        k.shutdown().await.ok();
    }

    Ok(())
}

/// Handle a typed frame received from the coordinator on stdin.
async fn handle_stdin_frame<W: AsyncWrite + Unpin>(
    data: &[u8],
    writer: &mut W,
    ctx: &AgentContext,
    coordinator_sync_state: &mut automerge::sync::State,
    cmd_rx: &mut Option<tokio::sync::mpsc::Receiver<QueueCommand>>,
) -> anyhow::Result<()> {
    if data.is_empty() {
        return Ok(());
    }

    let frame_type = data[0];
    let payload = &data[1..];

    match frame_type {
        // AgentRequest (0x01)
        0x01 => {
            let request: AgentRequest = serde_json::from_slice(payload)?;
            let response = handle_agent_request(request, ctx).await;

            // After successful launch, take the kernel's cmd_rx so the
            // main loop can start processing execution lifecycle events.
            if matches!(response, AgentResponse::KernelLaunched { .. }) {
                let mut guard = ctx.kernel.lock().await;
                if let Some(ref mut k) = *guard {
                    *cmd_rx = k.take_cmd_rx();
                }
            }

            let json = serde_json::to_vec(&response)?;
            send_typed_frame(writer, NotebookFrameType::Response, &json).await?;
        }

        // RuntimeStateSync (0x05)
        0x05 => {
            // Coordinator is sending RuntimeStateDoc changes (e.g., comm state
            // from frontend, trust state). Apply to our local replica.
            let mut sd = ctx.state_doc.write().await;
            let sync_message = automerge::sync::Message::decode(payload)?;
            if sd.receive_sync_message_with_changes(coordinator_sync_state, sync_message)? {
                let _ = ctx.state_changed_tx.send(());
            }
        }

        other => {
            debug!("[agent] Ignoring unknown frame type 0x{:02x}", other);
        }
    }

    Ok(())
}

/// Handle an AgentRequest and return an AgentResponse.
async fn handle_agent_request(request: AgentRequest, ctx: &AgentContext) -> AgentResponse {
    match request {
        AgentRequest::LaunchKernel {
            kernel_type,
            env_source,
            notebook_path,
            launched_config,
            env_vars: _,
        } => {
            info!(
                "[agent] LaunchKernel: type={} source={}",
                kernel_type, env_source
            );

            let mut k = RoomKernel::new(
                ctx.broadcast_tx.clone(),
                ctx.blob_store.clone(),
                ctx.state_doc.clone(),
                ctx.state_changed_tx.clone(),
                ctx.presence.clone(),
                ctx.presence_tx.clone(),
            );

            // Reconstruct PooledEnv from launched_config paths if available.
            let pooled_env = launched_config.venv_path.as_ref().and_then(|venv| {
                launched_config
                    .python_path
                    .as_ref()
                    .map(|python| runtimed_client::PooledEnv {
                        env_type: if env_source.starts_with("conda") {
                            runtimed_client::EnvType::Conda
                        } else {
                            runtimed_client::EnvType::Uv
                        },
                        venv_path: venv.clone(),
                        python_path: python.clone(),
                        prewarmed_packages: launched_config.prewarmed_packages.clone(),
                    })
            });

            let nb_path = notebook_path.as_deref().map(std::path::Path::new);

            match k
                .launch(
                    &kernel_type,
                    &env_source,
                    nb_path,
                    pooled_env,
                    launched_config,
                )
                .await
            {
                Ok(()) => {
                    let es = k.env_source().to_string();
                    let mut guard = ctx.kernel.lock().await;
                    *guard = Some(k);
                    AgentResponse::KernelLaunched { env_source: es }
                }
                Err(e) => AgentResponse::Error {
                    error: format!("Failed to launch kernel: {}", e),
                },
            }
        }

        AgentRequest::ExecuteCell {
            cell_id,
            code,
            execution_id,
        } => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                // Use the coordinator's execution_id to keep NotebookDoc
                // and RuntimeStateDoc in sync.
                match k
                    .queue_cell_with_id(cell_id.clone(), code, execution_id)
                    .await
                {
                    Ok(eid) => AgentResponse::CellQueued {
                        cell_id,
                        execution_id: eid,
                    },
                    Err(e) => AgentResponse::Error {
                        error: format!("Failed to queue cell: {}", e),
                    },
                }
            } else {
                AgentResponse::Error {
                    error: "No kernel running".to_string(),
                }
            }
        }

        AgentRequest::InterruptExecution => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                match k.interrupt().await {
                    Ok(()) => AgentResponse::Ok,
                    Err(e) => AgentResponse::Error {
                        error: format!("Failed to interrupt: {}", e),
                    },
                }
            } else {
                AgentResponse::Error {
                    error: "No kernel running".to_string(),
                }
            }
        }

        AgentRequest::ShutdownKernel => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                k.shutdown().await.ok();
                *guard = None;
                AgentResponse::Ok
            } else {
                AgentResponse::Ok
            }
        }

        AgentRequest::SendComm { message } => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                match k.send_comm_message(message).await {
                    Ok(()) => AgentResponse::Ok,
                    Err(e) => AgentResponse::Error {
                        error: format!("Failed to send comm: {}", e),
                    },
                }
            } else {
                AgentResponse::Error {
                    error: "No kernel running".to_string(),
                }
            }
        }

        AgentRequest::Complete { code, cursor_pos } => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                match k.complete(code, cursor_pos).await {
                    Ok((items, cursor_start, cursor_end)) => AgentResponse::CompletionResult {
                        items,
                        cursor_start,
                        cursor_end,
                    },
                    Err(e) => AgentResponse::Error {
                        error: format!("Failed to complete: {}", e),
                    },
                }
            } else {
                AgentResponse::Error {
                    error: "No kernel running".to_string(),
                }
            }
        }

        AgentRequest::GetHistory { pattern, n, unique } => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                match k.get_history(pattern, n, unique).await {
                    Ok(entries) => AgentResponse::HistoryResult { entries },
                    Err(e) => AgentResponse::Error {
                        error: format!("Failed to get history: {}", e),
                    },
                }
            } else {
                AgentResponse::Error {
                    error: "No kernel running".to_string(),
                }
            }
        }
    }
}

/// Handle a QueueCommand from the kernel's IOPub/shell/heartbeat tasks.
///
/// Most kernel state is already in RuntimeStateDoc and syncs automatically.
/// This handler only deals with events that need explicit coordinator action.
async fn handle_queue_command(command: QueueCommand, ctx: &AgentContext) -> anyhow::Result<()> {
    match command {
        QueueCommand::ExecutionDone {
            cell_id,
            execution_id,
        } => {
            debug!("[agent] ExecutionDone for {} ({})", cell_id, execution_id);
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                if let Err(e) = k.execution_done(&cell_id, &execution_id).await {
                    warn!("[agent] execution_done error: {}", e);
                }
            }
            // RuntimeStateDoc changes sync automatically via frame 0x05
        }

        QueueCommand::CellError {
            cell_id,
            execution_id,
        } => {
            debug!(
                "[agent] CellError: cell={} execution={}",
                cell_id, execution_id
            );
            // Stop-on-error: clear remaining queue and mark cleared executions as failed
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                let cleared = k.clear_queue();
                let mut sd = ctx.state_doc.write().await;
                // Mark each cleared execution as error so callers aren't stuck waiting
                for entry in &cleared {
                    sd.set_execution_done(&entry.execution_id, false);
                }
                sd.set_queue(None, &[]);
                let _ = ctx.state_changed_tx.send(());
            }
        }

        QueueCommand::KernelDied => {
            warn!("[agent] Kernel died");
            // Write error state to RuntimeStateDoc — syncs to coordinator
            // automatically. The coordinator also detects child exit.
            let mut sd = ctx.state_doc.write().await;
            sd.set_kernel_status("error");
            sd.set_queue(None, &[]);
            let _ = ctx.state_changed_tx.send(());
        }

        QueueCommand::SendCommUpdate { comm_id, state } => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                if let Err(e) = k.send_comm_update(&comm_id, state).await {
                    warn!("[agent] Failed to send comm update: {}", e);
                }
            }
        }
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use notebook_protocol::connection::{
        recv_frame, recv_preamble, send_json_frame, send_preamble, send_typed_frame,
        NotebookFrameType,
    };
    use tokio::io::duplex;

    /// Test that the agent reads the handshake and responds with preamble.
    #[tokio::test]
    async fn test_agent_handshake() {
        let (coordinator_stream, agent_stream) = duplex(8192);
        let (agent_reader, agent_writer) = tokio::io::split(agent_stream);
        let (mut coord_reader, mut coord_writer) = tokio::io::split(coordinator_stream);

        let agent_task = tokio::spawn(async move { run_agent(agent_reader, agent_writer).await });

        // Coordinator sends preamble + handshake
        send_preamble(&mut coord_writer).await.unwrap();
        send_json_frame(
            &mut coord_writer,
            &Handshake::RuntimeAgent {
                notebook_id: "test-notebook".to_string(),
                agent_id: "rt:agent:test".to_string(),
                blob_root: "/tmp/test-blobs".to_string(),
            },
        )
        .await
        .unwrap();

        // Agent should respond with preamble
        recv_preamble(&mut coord_reader).await.unwrap();

        // Agent is now in its main loop — abort it
        agent_task.abort();
    }

    /// Test that the agent rejects non-RuntimeAgent handshakes.
    #[tokio::test]
    async fn test_agent_rejects_wrong_handshake() {
        let (coordinator_stream, agent_stream) = duplex(8192);
        let (agent_reader, agent_writer) = tokio::io::split(agent_stream);
        let (mut _coord_reader, mut coord_writer) = tokio::io::split(coordinator_stream);

        let agent_task = tokio::spawn(async move { run_agent(agent_reader, agent_writer).await });

        // Send wrong handshake type
        send_preamble(&mut coord_writer).await.unwrap();
        send_json_frame(&mut coord_writer, &Handshake::Pool)
            .await
            .unwrap();

        let result = tokio::time::timeout(std::time::Duration::from_secs(5), agent_task)
            .await
            .unwrap()
            .unwrap();

        assert!(
            result.is_err(),
            "Agent should reject non-RuntimeAgent handshake"
        );
    }

    /// Test that the agent responds to a ShutdownKernel request (no kernel running → Ok).
    #[tokio::test]
    async fn test_agent_shutdown_request() {
        let (coordinator_stream, agent_stream) = duplex(8192);
        let (agent_reader, agent_writer) = tokio::io::split(agent_stream);
        let (mut coord_reader, mut coord_writer) = tokio::io::split(coordinator_stream);

        let agent_task = tokio::spawn(async move { run_agent(agent_reader, agent_writer).await });

        // Handshake
        send_preamble(&mut coord_writer).await.unwrap();
        send_json_frame(
            &mut coord_writer,
            &Handshake::RuntimeAgent {
                notebook_id: "test-notebook".to_string(),
                agent_id: "rt:agent:test".to_string(),
                blob_root: "/tmp/test-blobs".to_string(),
            },
        )
        .await
        .unwrap();
        recv_preamble(&mut coord_reader).await.unwrap();

        // Send ShutdownKernel request
        let request = AgentRequest::ShutdownKernel;
        let json = serde_json::to_vec(&request).unwrap();
        send_typed_frame(&mut coord_writer, NotebookFrameType::Request, &json)
            .await
            .unwrap();

        // Read response
        let response_frame = recv_frame(&mut coord_reader).await.unwrap().unwrap();
        assert_eq!(response_frame[0], 0x02, "Should be a Response frame");
        let response: AgentResponse = serde_json::from_slice(&response_frame[1..]).unwrap();
        assert!(
            matches!(response, AgentResponse::Ok),
            "ShutdownKernel with no kernel should return Ok"
        );

        // Clean up
        agent_task.abort();
    }
}
