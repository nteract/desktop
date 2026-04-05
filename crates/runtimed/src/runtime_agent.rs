//! Process-isolated runtime agent.
//!
//! The runtime agent is a subprocess spawned by the coordinator (daemon) that
//! owns the kernel lifecycle, IOPub processing, execution queue, and
//! RuntimeStateDoc writes. It connects back to the daemon's Unix socket as a
//! regular peer.
//!
//! ## CRDT-driven execution
//!
//! The runtime agent does NOT receive execution requests via RPC. Instead, the
//! coordinator writes execution entries (with source code and sequence numbers)
//! to RuntimeStateDoc. The runtime agent discovers new entries via Automerge
//! sync and executes them in seq order.
//!
//! ## Protocol
//!
//! Standard peer protocol over Unix socket:
//! - Frame 0x00: AutomergeSync (NotebookDoc sync, for completions context)
//! - Frame 0x01: RuntimeAgentRequest (coordinator → runtime agent)
//! - Frame 0x02: RuntimeAgentResponse (runtime agent → coordinator)
//! - Frame 0x05: RuntimeStateSync (bidirectional, carries execution queue + outputs)
//!
//! ## Lifecycle
//!
//! 1. Runtime agent connects to daemon socket, sends `Handshake::RuntimeAgent`
//! 2. Initial sync for NotebookDoc and RuntimeStateDoc
//! 3. Runtime agent waits for `LaunchKernel` RPC
//! 4. Main select loop: socket frames, QueueCommands, RuntimeStateDoc changes
//! 5. Watches for new `status=queued` execution entries after each sync
//! 6. On shutdown or daemon disconnect, runtime agent exits

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use log::{debug, info, warn};
use notebook_doc::presence::PresenceState;
use notebook_doc::runtime_state::{CommDocEntry, RuntimeStateDoc};
use notebook_protocol::connection::{
    recv_typed_frame, send_json_frame, send_preamble, send_typed_frame, Handshake,
    NotebookFrameType,
};
use notebook_protocol::protocol::{RuntimeAgentRequest, RuntimeAgentResponse};
use tokio::sync::{broadcast, RwLock};

use crate::blob_store::BlobStore;
use crate::kernel_manager::{QueueCommand, RoomKernel};

/// Shared runtime agent state passed to request/command handlers.
struct RuntimeAgentContext {
    kernel: Arc<tokio::sync::Mutex<Option<RoomKernel>>>,
    state_doc: Arc<RwLock<RuntimeStateDoc>>,
    state_changed_tx: broadcast::Sender<()>,
    blob_store: Arc<BlobStore>,
    broadcast_tx: broadcast::Sender<notebook_protocol::protocol::NotebookBroadcast>,
    presence: Arc<RwLock<PresenceState>>,
    presence_tx: broadcast::Sender<(String, Vec<u8>)>,
    seen_execution_ids: Arc<tokio::sync::Mutex<HashSet<String>>>,
}

/// Run the runtime agent, connecting to the daemon socket as a peer.
pub async fn run_runtime_agent(
    socket_path: PathBuf,
    notebook_id: String,
    runtime_agent_id: String,
    blob_root: PathBuf,
) -> anyhow::Result<()> {
    info!(
        "[runtime-agent] Starting runtime_agent_id={} notebook_id={} socket={}",
        runtime_agent_id,
        notebook_id,
        socket_path.display()
    );

    // ── 1. Connect to daemon socket ────────────────────────────────────────

    #[cfg(unix)]
    let stream = tokio::net::UnixStream::connect(&socket_path).await?;

    #[cfg(windows)]
    let stream = {
        let pipe_name = socket_path.to_string_lossy().to_string();
        let mut attempts = 0u32;
        loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe_name) {
                Ok(client) => break client,
                Err(_) if attempts < 10 => {
                    attempts += 1;
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
                Err(e) => return Err(e.into()),
            }
        }
    };

    let (mut reader, mut writer) = tokio::io::split(stream);

    // Send preamble + RuntimeAgent handshake
    send_preamble(&mut writer).await?;
    send_json_frame(
        &mut writer,
        &Handshake::RuntimeAgent {
            notebook_id: notebook_id.clone(),
            runtime_agent_id: runtime_agent_id.clone(),
            blob_root: blob_root.display().to_string(),
        },
    )
    .await?;

    info!("[runtime-agent] Connected to daemon, handshake sent");

    // ── 2. Bootstrap RuntimeStateDoc ───────────────────────────────────────

    let state_doc = RuntimeStateDoc::new_with_actor(&runtime_agent_id);
    let mut coordinator_sync_state = automerge::sync::State::new();
    let state_doc = Arc::new(RwLock::new(state_doc));
    let (state_changed_tx, mut state_changed_rx) = broadcast::channel::<()>(64);

    // ── 3. Create local infrastructure ─────────────────────────────────────

    let blob_store = Arc::new(BlobStore::new(blob_root));
    let (broadcast_tx, _broadcast_rx) =
        broadcast::channel::<notebook_protocol::protocol::NotebookBroadcast>(16);
    let presence = Arc::new(RwLock::new(PresenceState::new()));
    let (presence_tx, _presence_rx) = broadcast::channel::<(String, Vec<u8>)>(16);

    let ctx = RuntimeAgentContext {
        kernel: Arc::new(tokio::sync::Mutex::new(None)),
        state_doc,
        state_changed_tx,
        blob_store,
        broadcast_tx,
        presence,
        presence_tx,
        seen_execution_ids: Arc::new(tokio::sync::Mutex::new(HashSet::new())),
    };

    let mut cmd_rx: Option<tokio::sync::mpsc::Receiver<QueueCommand>> = None;

    info!("[runtime-agent] Infrastructure ready, entering main loop");

    // ── 4. Main event loop ─────────────────────────────────────────────────

    loop {
        tokio::select! {
            // Read frames from daemon socket
            frame = recv_typed_frame(&mut reader) => {
                match frame {
                    Ok(Some(typed_frame)) => {
                        match typed_frame.frame_type {
                            // RuntimeAgentRequest RPC: LaunchKernel, Interrupt, etc.
                            NotebookFrameType::Request => {
                                if let Ok(request) = serde_json::from_slice::<RuntimeAgentRequest>(&typed_frame.payload) {
                                    let response = handle_runtime_agent_request(request, &ctx).await;

                                    // After launch/restart, take cmd_rx from kernel
                                    if matches!(response, RuntimeAgentResponse::KernelLaunched { .. } | RuntimeAgentResponse::KernelRestarted { .. }) {
                                        let mut guard = ctx.kernel.lock().await;
                                        if let Some(ref mut k) = *guard {
                                            cmd_rx = k.take_cmd_rx();
                                        }
                                    }

                                    let json = serde_json::to_vec(&response)?;
                                    send_typed_frame(&mut writer, NotebookFrameType::Response, &json).await?;
                                }
                            }

                            // RuntimeStateSync — apply coordinator's changes, check for new queue entries
                            // and forward frontend-originated comm state changes to kernel
                            NotebookFrameType::RuntimeStateSync => {
                                if let Ok(msg) = automerge::sync::Message::decode(&typed_frame.payload) {
                                    let mut sd = ctx.state_doc.write().await;

                                    // Snapshot comm state before applying sync so we can
                                    // detect frontend-originated widget state changes.
                                    let comms_before = sd.read_state().comms;

                                    if let Ok(changed) = sd.receive_sync_message_with_changes(
                                        &mut coordinator_sync_state,
                                        msg,
                                    ) {
                                        if changed {
                                            let _ = ctx.state_changed_tx.send(());

                                            // Diff comm state — forward changes to kernel
                                            let comms_after = sd.read_state().comms;
                                            let queued = sd.get_queued_executions();
                                            drop(sd); // release write lock before kernel interaction

                                            let comm_updates = diff_comm_state(&comms_before, &comms_after);
                                            if !comm_updates.is_empty() {
                                                let mut guard = ctx.kernel.lock().await;
                                                if let Some(ref mut k) = *guard {
                                                    for (comm_id, delta) in &comm_updates {
                                                        if let Err(e) = k.send_comm_update(comm_id, delta.clone()).await {
                                                            warn!("[runtime-agent] Failed to forward comm state to kernel: {}", e);
                                                        }
                                                    }
                                                }
                                            }

                                            // Check for new queued executions
                                            let mut seen = ctx.seen_execution_ids.lock().await;
                                            for (eid, exec) in queued {
                                                if seen.insert(eid.clone()) {
                                                    if let Some(ref source) = exec.source {
                                                        let mut guard = ctx.kernel.lock().await;
                                                        if let Some(ref mut k) = *guard {
                                                            match k.queue_cell_with_id(
                                                                exec.cell_id.clone(),
                                                                source.clone(),
                                                                eid.clone(),
                                                            ).await {
                                                                Ok(_) => {
                                                                    info!(
                                                                        "[runtime-agent] Queued cell {} (execution {})",
                                                                        exec.cell_id, eid
                                                                    );
                                                                }
                                                                Err(e) => {
                                                                    warn!(
                                                                        "[runtime-agent] Failed to queue cell {}: {}",
                                                                        exec.cell_id, e
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        } else {
                                            drop(sd);
                                        }
                                    }

                                    // Send sync reply
                                    let mut sd = ctx.state_doc.write().await;
                                    if let Some(reply) = sd.generate_sync_message(&mut coordinator_sync_state) {
                                        let encoded = reply.encode();
                                        let _ = send_typed_frame(
                                            &mut writer,
                                            NotebookFrameType::RuntimeStateSync,
                                            &encoded,
                                        ).await;
                                    }
                                }
                            }

                            // AutomergeSync (NotebookDoc — for completions context)
                            NotebookFrameType::AutomergeSync => {
                                // The runtime agent doesn't need NotebookDoc state for execution
                                // (source comes from execution entries), but it may be
                                // useful for completions context in the future.
                                debug!("[runtime-agent] Received NotebookDoc sync frame (ignored for now)");
                            }

                            _ => {
                                debug!("[runtime-agent] Ignoring frame type {:?}", typed_frame.frame_type);
                            }
                        }
                    }
                    Ok(None) => {
                        info!("[runtime-agent] Daemon disconnected (EOF)");
                        break;
                    }
                    Err(e) => {
                        info!("[runtime-agent] Socket read error: {}", e);
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
                    warn!("[runtime-agent] Error handling queue command: {}", e);
                }
            }

            // Sync RuntimeStateDoc changes to coordinator
            _ = state_changed_rx.recv() => {
                while state_changed_rx.try_recv().is_ok() {}

                let mut sd = ctx.state_doc.write().await;
                if let Some(msg) = sd.generate_sync_message(&mut coordinator_sync_state) {
                    let encoded = msg.encode();
                    if let Err(e) = send_typed_frame(
                        &mut writer,
                        NotebookFrameType::RuntimeStateSync,
                        &encoded,
                    ).await {
                        warn!("[runtime-agent] Failed to send RuntimeStateSync: {}", e);
                        break;
                    }
                }
            }
        }
    }

    // ── 5. Cleanup ─────────────────────────────────────────────────────────

    info!("[runtime-agent] Shutting down");
    let mut guard = ctx.kernel.lock().await;
    if let Some(ref mut k) = *guard {
        k.shutdown().await.ok();
    }

    Ok(())
}

/// Handle a `RuntimeAgentRequest` and return a `RuntimeAgentResponse`.
///
/// Note: ExecuteCell is NOT handled here — execution is CRDT-driven.
/// The coordinator writes execution entries to RuntimeStateDoc, and the
/// runtime agent picks them up via sync.
async fn handle_runtime_agent_request(
    request: RuntimeAgentRequest,
    ctx: &RuntimeAgentContext,
) -> RuntimeAgentResponse {
    match request {
        RuntimeAgentRequest::LaunchKernel {
            kernel_type,
            env_source,
            notebook_path,
            launched_config,
            env_vars: _,
        } => {
            info!(
                "[runtime-agent] LaunchKernel: type={} source={}",
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
                    RuntimeAgentResponse::KernelLaunched { env_source: es }
                }
                Err(e) => RuntimeAgentResponse::Error {
                    error: format!("Failed to launch kernel: {}", e),
                },
            }
        }

        RuntimeAgentRequest::RestartKernel {
            kernel_type,
            env_source,
            notebook_path,
            launched_config,
            env_vars: _,
        } => {
            info!(
                "[runtime-agent] RestartKernel: type={} source={}",
                kernel_type, env_source
            );

            // Shut down existing kernel
            {
                let mut guard = ctx.kernel.lock().await;
                if let Some(ref mut k) = *guard {
                    k.shutdown().await.ok();
                }
                *guard = None;
            }

            // Clear seen execution IDs so new RunAllCells entries are picked up
            {
                let mut seen = ctx.seen_execution_ids.lock().await;
                seen.clear();
            }

            let mut k = RoomKernel::new(
                ctx.broadcast_tx.clone(),
                ctx.blob_store.clone(),
                ctx.state_doc.clone(),
                ctx.state_changed_tx.clone(),
                ctx.presence.clone(),
                ctx.presence_tx.clone(),
            );

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
                    RuntimeAgentResponse::KernelRestarted { env_source: es }
                }
                Err(e) => RuntimeAgentResponse::Error {
                    error: format!("Failed to restart kernel: {}", e),
                },
            }
        }

        RuntimeAgentRequest::InterruptExecution => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                match k.interrupt().await {
                    Ok(()) => RuntimeAgentResponse::Ok,
                    Err(e) => RuntimeAgentResponse::Error {
                        error: format!("Failed to interrupt: {}", e),
                    },
                }
            } else {
                RuntimeAgentResponse::Error {
                    error: "No kernel running".to_string(),
                }
            }
        }

        RuntimeAgentRequest::ShutdownKernel => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                k.shutdown().await.ok();
                *guard = None;
                RuntimeAgentResponse::Ok
            } else {
                RuntimeAgentResponse::Ok
            }
        }

        RuntimeAgentRequest::SendComm { message } => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                match k.send_comm_message(message).await {
                    Ok(()) => RuntimeAgentResponse::Ok,
                    Err(e) => RuntimeAgentResponse::Error {
                        error: format!("Failed to send comm: {}", e),
                    },
                }
            } else {
                RuntimeAgentResponse::Error {
                    error: "No kernel running".to_string(),
                }
            }
        }

        RuntimeAgentRequest::Complete { code, cursor_pos } => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                match k.complete(code, cursor_pos).await {
                    Ok((items, cursor_start, cursor_end)) => {
                        RuntimeAgentResponse::CompletionResult {
                            items,
                            cursor_start,
                            cursor_end,
                        }
                    }
                    Err(e) => RuntimeAgentResponse::Error {
                        error: format!("Failed to complete: {}", e),
                    },
                }
            } else {
                RuntimeAgentResponse::Error {
                    error: "No kernel running".to_string(),
                }
            }
        }

        RuntimeAgentRequest::GetHistory { pattern, n, unique } => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                match k.get_history(pattern, n, unique).await {
                    Ok(entries) => RuntimeAgentResponse::HistoryResult { entries },
                    Err(e) => RuntimeAgentResponse::Error {
                        error: format!("Failed to get history: {}", e),
                    },
                }
            } else {
                RuntimeAgentResponse::Error {
                    error: "No kernel running".to_string(),
                }
            }
        }

        RuntimeAgentRequest::SyncEnvironment { packages } => {
            info!("[runtime-agent] SyncEnvironment: installing {:?}", packages);
            let guard = ctx.kernel.lock().await;
            if let Some(ref kernel) = *guard {
                let es = kernel.env_source().to_string();
                if !es.starts_with("uv:") {
                    return RuntimeAgentResponse::Error {
                        error: "Hot-sync only supported for UV environments".to_string(),
                    };
                }

                // Get venv and python paths from the kernel's launched config
                let launched = kernel.launched_config();
                let venv_path = match &launched.venv_path {
                    Some(p) => p.clone(),
                    None => {
                        return RuntimeAgentResponse::Error {
                            error: "No venv path available".to_string(),
                        };
                    }
                };
                let python_path = match &launched.python_path {
                    Some(p) => p.clone(),
                    None => {
                        return RuntimeAgentResponse::Error {
                            error: "No python path available".to_string(),
                        };
                    }
                };
                drop(guard);

                // Run uv pip install
                let uv_env = kernel_env::uv::UvEnvironment {
                    venv_path,
                    python_path,
                };

                match kernel_env::uv::sync_dependencies(&uv_env, &packages).await {
                    Ok(()) => RuntimeAgentResponse::EnvironmentSynced {
                        synced_packages: packages,
                    },
                    Err(e) => RuntimeAgentResponse::Error {
                        error: format!("Failed to install packages: {}", e),
                    },
                }
            } else {
                RuntimeAgentResponse::Error {
                    error: "No kernel running".to_string(),
                }
            }
        }
    }
}

/// Handle a QueueCommand from the kernel's IOPub/shell/heartbeat tasks.
async fn handle_queue_command(
    command: QueueCommand,
    ctx: &RuntimeAgentContext,
) -> anyhow::Result<()> {
    match command {
        QueueCommand::ExecutionDone {
            cell_id,
            execution_id,
        } => {
            debug!(
                "[runtime-agent] ExecutionDone for {} ({})",
                cell_id, execution_id
            );
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                if let Err(e) = k.execution_done(&cell_id, &execution_id).await {
                    warn!("[runtime-agent] execution_done error: {}", e);
                }
            }
        }

        QueueCommand::CellError {
            cell_id,
            execution_id,
        } => {
            debug!(
                "[runtime-agent] CellError: cell={} execution={}",
                cell_id, execution_id
            );
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                k.mark_execution_error();
                let cleared = k.clear_queue();
                let mut sd = ctx.state_doc.write().await;
                for entry in &cleared {
                    sd.set_execution_done(&entry.execution_id, false);
                }
                sd.set_queue(None, &[]);
                let _ = ctx.state_changed_tx.send(());
            }
        }

        QueueCommand::KernelDied => {
            warn!("[runtime-agent] Kernel died");
            let mut sd = ctx.state_doc.write().await;
            sd.set_kernel_status("error");
            sd.set_queue(None, &[]);
            let _ = ctx.state_changed_tx.send(());
        }

        QueueCommand::SendCommUpdate { comm_id, state } => {
            let mut guard = ctx.kernel.lock().await;
            if let Some(ref mut k) = *guard {
                if let Err(e) = k.send_comm_update(&comm_id, state).await {
                    warn!("[runtime-agent] Failed to send comm update: {}", e);
                }
            }
        }
    }

    Ok(())
}

/// Diff two comm state snapshots, returning `(comm_id, changed_properties)` pairs.
///
/// Only diffs existing comms (new comms originate from kernel `comm_open` and
/// don't need forwarding back). Returns a minimal delta per comm — only
/// properties whose values actually changed.
fn diff_comm_state(
    before: &HashMap<String, CommDocEntry>,
    after: &HashMap<String, CommDocEntry>,
) -> Vec<(String, serde_json::Value)> {
    let mut updates = Vec::new();
    for (comm_id, after_entry) in after {
        if let Some(before_entry) = before.get(comm_id) {
            if let (Some(before_obj), Some(after_obj)) = (
                before_entry.state.as_object(),
                after_entry.state.as_object(),
            ) {
                let mut delta = serde_json::Map::new();
                for (key, after_val) in after_obj {
                    match before_obj.get(key) {
                        Some(before_val) if before_val == after_val => {}
                        _ => {
                            delta.insert(key.clone(), after_val.clone());
                        }
                    }
                }
                if !delta.is_empty() {
                    updates.push((comm_id.clone(), serde_json::Value::Object(delta)));
                }
            }
        }
    }
    updates
}
