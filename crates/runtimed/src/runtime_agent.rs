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
//! - Frame 0x01: RuntimeAgentRequest (coordinator -> runtime agent)
//! - Frame 0x02: RuntimeAgentResponse (runtime agent -> coordinator)
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

use notebook_doc::presence::PresenceState;
use notebook_doc::runtime_state::{CommDocEntry, RuntimeStateDoc};
use notebook_protocol::connection::{
    recv_typed_frame, send_json_frame, send_preamble, send_typed_frame, Handshake,
    NotebookFrameType,
};
use notebook_protocol::protocol::{RuntimeAgentRequest, RuntimeAgentResponse};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, info, warn};

use crate::blob_store::BlobStore;
use crate::jupyter_kernel::JupyterKernel;
use crate::kernel_connection::{KernelConnection, KernelLaunchConfig, KernelSharedRefs};
use crate::kernel_manager::QueueCommand;
use crate::kernel_state::KernelState;

/// Shared context for the runtime agent (no kernel -- kernel is owned locally).
struct RuntimeAgentContext {
    state_doc: Arc<RwLock<RuntimeStateDoc>>,
    state_changed_tx: broadcast::Sender<()>,
    blob_store: Arc<BlobStore>,
    broadcast_tx: broadcast::Sender<notebook_protocol::protocol::NotebookBroadcast>,
    presence: Arc<RwLock<PresenceState>>,
    presence_tx: broadcast::Sender<(String, Vec<u8>)>,
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

    // -- 1. Connect to daemon socket ----------------------------------------

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

    // -- 2. Bootstrap RuntimeStateDoc ---------------------------------------

    let state_doc = RuntimeStateDoc::new_with_actor(&runtime_agent_id);
    let mut coordinator_sync_state = automerge::sync::State::new();
    let state_doc = Arc::new(RwLock::new(state_doc));
    let (state_changed_tx, mut state_changed_rx) = broadcast::channel::<()>(64);

    // -- 3. Create local infrastructure -------------------------------------

    let blob_store = Arc::new(BlobStore::new(blob_root));
    let (broadcast_tx, _broadcast_rx) =
        broadcast::channel::<notebook_protocol::protocol::NotebookBroadcast>(16);
    let presence = Arc::new(RwLock::new(PresenceState::new()));
    let (presence_tx, _presence_rx) = broadcast::channel::<(String, Vec<u8>)>(16);

    let ctx = RuntimeAgentContext {
        state_doc: state_doc.clone(),
        state_changed_tx: state_changed_tx.clone(),
        blob_store,
        broadcast_tx: broadcast_tx.clone(),
        presence,
        presence_tx,
    };

    // -- Local variables owned by the select! loop (no mutex) ---------------

    let mut kernel: Option<JupyterKernel> = None;
    let mut kernel_state = KernelState::new(
        state_doc.clone(),
        state_changed_tx.clone(),
        broadcast_tx.clone(),
    );
    let mut seen_execution_ids = HashSet::new();
    let mut cmd_rx: Option<mpsc::Receiver<QueueCommand>> = None;

    info!("[runtime-agent] Infrastructure ready, entering main loop");

    // -- 4. Main event loop -------------------------------------------------

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
                                    let (response, new_cmd_rx) = handle_runtime_agent_request(
                                        request,
                                        &ctx,
                                        &mut kernel,
                                        &mut kernel_state,
                                        &mut seen_execution_ids,
                                    ).await;

                                    // After launch/restart, take cmd_rx from response
                                    if let Some(rx) = new_cmd_rx {
                                        cmd_rx = Some(rx);
                                    }

                                    let json = serde_json::to_vec(&response)?;
                                    send_typed_frame(&mut writer, NotebookFrameType::Response, &json).await?;
                                }
                            }

                            // RuntimeStateSync -- apply coordinator's changes, check for new queue entries
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
                                            let _ = state_changed_tx.send(());

                                            // Diff comm state -- forward changes to kernel
                                            let comms_after = sd.read_state().comms;
                                            let queued = sd.get_queued_executions();
                                            drop(sd); // release write lock before kernel interaction

                                            let comm_updates = diff_comm_state(&comms_before, &comms_after);
                                            if !comm_updates.is_empty() {
                                                if let Some(ref mut k) = kernel {
                                                    for (comm_id, delta) in &comm_updates {
                                                        if let Err(e) = k.send_comm_update(comm_id, delta.clone()).await {
                                                            warn!("[runtime-agent] Failed to forward comm state to kernel: {}", e);
                                                        }
                                                    }
                                                }
                                            }

                                            // Check for new queued executions
                                            for (eid, exec) in queued {
                                                if seen_execution_ids.insert(eid.clone()) {
                                                    if let Some(ref source) = exec.source {
                                                        if let Some(ref mut k) = kernel {
                                                            match kernel_state.queue_cell(
                                                                exec.cell_id.clone(),
                                                                eid.clone(),
                                                                source.clone(),
                                                                k,
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

                            // AutomergeSync (NotebookDoc -- for completions context)
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
                if let Err(e) = handle_queue_command(
                    command,
                    &ctx,
                    &mut kernel,
                    &mut kernel_state,
                ).await {
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

    // -- 5. Cleanup ---------------------------------------------------------

    info!("[runtime-agent] Shutting down");
    if let Some(ref mut k) = kernel {
        k.shutdown().await.ok();
    }

    Ok(())
}

/// Handle a `RuntimeAgentRequest` and return a `RuntimeAgentResponse`.
///
/// Also returns an optional `cmd_rx` when a kernel is launched/restarted
/// (the caller needs to install it in the select! loop).
///
/// Note: ExecuteCell is NOT handled here -- execution is CRDT-driven.
/// The coordinator writes execution entries to RuntimeStateDoc, and the
/// runtime agent picks them up via sync.
async fn handle_runtime_agent_request(
    request: RuntimeAgentRequest,
    ctx: &RuntimeAgentContext,
    kernel: &mut Option<JupyterKernel>,
    state: &mut KernelState,
    seen_execution_ids: &mut HashSet<String>,
) -> (RuntimeAgentResponse, Option<mpsc::Receiver<QueueCommand>>) {
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

            let shared = KernelSharedRefs {
                state_doc: ctx.state_doc.clone(),
                state_changed_tx: ctx.state_changed_tx.clone(),
                blob_store: ctx.blob_store.clone(),
                broadcast_tx: ctx.broadcast_tx.clone(),
                presence: ctx.presence.clone(),
                presence_tx: ctx.presence_tx.clone(),
            };
            let config = KernelLaunchConfig {
                kernel_type,
                env_source: env_source.clone(),
                notebook_path: notebook_path.as_deref().map(PathBuf::from),
                launched_config,
                env_vars: vec![],
                pooled_env,
            };

            match JupyterKernel::launch(config, shared).await {
                Ok((k, rx)) => {
                    let es = k.env_source().to_string();
                    *kernel = Some(k);
                    state.reset();
                    state.set_idle();
                    (
                        RuntimeAgentResponse::KernelLaunched { env_source: es },
                        Some(rx),
                    )
                }
                Err(e) => (
                    RuntimeAgentResponse::Error {
                        error: format!("Failed to launch kernel: {}", e),
                    },
                    None,
                ),
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

            // Capture in-flight executions before shutdown so we can mark them
            // as failed in RuntimeStateDoc (the old kernel can't finish them).
            let interrupted_eid = state.executing_cell().map(|(_, eid)| eid.clone());
            let stale_queue: Vec<_> = state
                .queued_entries()
                .iter()
                .map(|e| e.execution_id.clone())
                .collect();

            // Shut down existing kernel
            if let Some(ref mut k) = kernel {
                k.shutdown().await.ok();
            }
            *kernel = None;

            // Clear seen execution IDs so new RunAllCells entries are picked up
            seen_execution_ids.clear();

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

            let shared = KernelSharedRefs {
                state_doc: ctx.state_doc.clone(),
                state_changed_tx: ctx.state_changed_tx.clone(),
                blob_store: ctx.blob_store.clone(),
                broadcast_tx: ctx.broadcast_tx.clone(),
                presence: ctx.presence.clone(),
                presence_tx: ctx.presence_tx.clone(),
            };
            let config = KernelLaunchConfig {
                kernel_type,
                env_source: env_source.clone(),
                notebook_path: notebook_path.as_deref().map(PathBuf::from),
                launched_config,
                env_vars: vec![],
                pooled_env,
            };

            // Mark stale executions as failed in RuntimeStateDoc.
            // The old kernel is gone after shutdown, so these executions
            // can never complete — do this before launching the new kernel.
            {
                let mut sd = ctx.state_doc.write().await;
                if let Some(ref eid) = interrupted_eid {
                    sd.set_execution_done(eid, false);
                }
                for eid in &stale_queue {
                    sd.set_execution_done(eid, false);
                }
                sd.set_queue(None, &[]);
                let _ = ctx.state_changed_tx.send(());
            }

            match JupyterKernel::launch(config, shared).await {
                Ok((k, rx)) => {
                    let es = k.env_source().to_string();
                    *kernel = Some(k);
                    state.reset();
                    state.set_idle();
                    (
                        RuntimeAgentResponse::KernelRestarted { env_source: es },
                        Some(rx),
                    )
                }
                Err(e) => (
                    RuntimeAgentResponse::Error {
                        error: format!("Failed to restart kernel: {}", e),
                    },
                    None,
                ),
            }
        }

        RuntimeAgentRequest::InterruptExecution => {
            if let Some(ref mut k) = kernel {
                match k.interrupt().await {
                    Ok(()) => {
                        let cleared = state.clear_queue();
                        // Write cleared entries to state doc
                        {
                            let mut sd = ctx.state_doc.write().await;
                            for entry in &cleared {
                                sd.set_execution_done(&entry.execution_id, false);
                            }
                        }
                        state.write_queue_to_state_doc().await;
                        (
                            RuntimeAgentResponse::InterruptAcknowledged { cleared },
                            None,
                        )
                    }
                    Err(e) => (
                        RuntimeAgentResponse::Error {
                            error: format!("Failed to interrupt: {}", e),
                        },
                        None,
                    ),
                }
            } else {
                (
                    RuntimeAgentResponse::Error {
                        error: "No kernel running".to_string(),
                    },
                    None,
                )
            }
        }

        RuntimeAgentRequest::ShutdownKernel => {
            if let Some(ref mut k) = kernel {
                k.shutdown().await.ok();
            }
            *kernel = None;
            (RuntimeAgentResponse::Ok, None)
        }

        RuntimeAgentRequest::SendComm { message } => {
            if let Some(ref mut k) = kernel {
                match k.send_comm_message(message).await {
                    Ok(()) => (RuntimeAgentResponse::Ok, None),
                    Err(e) => (
                        RuntimeAgentResponse::Error {
                            error: format!("Failed to send comm: {}", e),
                        },
                        None,
                    ),
                }
            } else {
                (
                    RuntimeAgentResponse::Error {
                        error: "No kernel running".to_string(),
                    },
                    None,
                )
            }
        }

        RuntimeAgentRequest::Complete { code, cursor_pos } => {
            if let Some(ref mut k) = kernel {
                match k.complete(&code, cursor_pos).await {
                    Ok((items, cursor_start, cursor_end)) => (
                        RuntimeAgentResponse::CompletionResult {
                            items,
                            cursor_start,
                            cursor_end,
                        },
                        None,
                    ),
                    Err(e) => (
                        RuntimeAgentResponse::Error {
                            error: format!("Failed to complete: {}", e),
                        },
                        None,
                    ),
                }
            } else {
                (
                    RuntimeAgentResponse::Error {
                        error: "No kernel running".to_string(),
                    },
                    None,
                )
            }
        }

        RuntimeAgentRequest::GetHistory { pattern, n, unique } => {
            if let Some(ref mut k) = kernel {
                match k.get_history(pattern.as_deref(), n, unique).await {
                    Ok(entries) => (RuntimeAgentResponse::HistoryResult { entries }, None),
                    Err(e) => (
                        RuntimeAgentResponse::Error {
                            error: format!("Failed to get history: {}", e),
                        },
                        None,
                    ),
                }
            } else {
                (
                    RuntimeAgentResponse::Error {
                        error: "No kernel running".to_string(),
                    },
                    None,
                )
            }
        }

        RuntimeAgentRequest::SyncEnvironment { packages } => {
            info!("[runtime-agent] SyncEnvironment: installing {:?}", packages);
            if let Some(ref kernel_ref) = kernel {
                let es = kernel_ref.env_source().to_string();
                if !es.starts_with("uv:") {
                    return (
                        RuntimeAgentResponse::Error {
                            error: "Hot-sync only supported for UV environments".to_string(),
                        },
                        None,
                    );
                }

                // Get venv and python paths from the kernel's launched config
                let launched = kernel_ref.launched_config();
                let venv_path = match &launched.venv_path {
                    Some(p) => p.clone(),
                    None => {
                        return (
                            RuntimeAgentResponse::Error {
                                error: "No venv path available".to_string(),
                            },
                            None,
                        );
                    }
                };
                let python_path = match &launched.python_path {
                    Some(p) => p.clone(),
                    None => {
                        return (
                            RuntimeAgentResponse::Error {
                                error: "No python path available".to_string(),
                            },
                            None,
                        );
                    }
                };

                // Run uv pip install (no kernel access needed during sync)
                let uv_env = kernel_env::uv::UvEnvironment {
                    venv_path,
                    python_path,
                };

                match kernel_env::uv::sync_dependencies(&uv_env, &packages).await {
                    Ok(()) => (
                        RuntimeAgentResponse::EnvironmentSynced {
                            synced_packages: packages,
                        },
                        None,
                    ),
                    Err(e) => (
                        RuntimeAgentResponse::Error {
                            error: format!("Failed to install packages: {}", e),
                        },
                        None,
                    ),
                }
            } else {
                (
                    RuntimeAgentResponse::Error {
                        error: "No kernel running".to_string(),
                    },
                    None,
                )
            }
        }
    }
}

/// Handle a QueueCommand from the kernel's IOPub/shell/heartbeat tasks.
async fn handle_queue_command(
    command: QueueCommand,
    ctx: &RuntimeAgentContext,
    kernel: &mut Option<JupyterKernel>,
    state: &mut KernelState,
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
            if let Some(ref mut k) = kernel {
                if let Err(e) = state.execution_done(&cell_id, &execution_id, k).await {
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
            state.mark_execution_error();
            let cleared = state.clear_queue();
            let mut sd = ctx.state_doc.write().await;
            for entry in &cleared {
                sd.set_execution_done(&entry.execution_id, false);
            }
            sd.set_queue(None, &[]);
            let _ = ctx.state_changed_tx.send(());
        }

        QueueCommand::KernelDied => {
            warn!("[runtime-agent] Kernel died");
            let (interrupted, cleared) = state.kernel_died();
            let mut sd = ctx.state_doc.write().await;
            if let Some((_, ref eid)) = interrupted {
                sd.set_execution_done(eid, false);
            }
            for entry in &cleared {
                sd.set_execution_done(&entry.execution_id, false);
            }
            sd.set_kernel_status("error");
            sd.set_queue(None, &[]);
            let _ = ctx.state_changed_tx.send(());
        }

        QueueCommand::SendCommUpdate {
            comm_id,
            state: comm_state,
        } => {
            if let Some(ref mut k) = kernel {
                if let Err(e) = k.send_comm_update(&comm_id, comm_state).await {
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
/// don't need forwarding back). Returns a minimal delta per comm -- only
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
