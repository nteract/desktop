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
use tracing::{debug, error, info, warn};

use crate::blob_store::BlobStore;
use crate::jupyter_kernel::JupyterKernel;
use crate::kernel_connection::{KernelConnection, KernelLaunchConfig, KernelSharedRefs};
use crate::kernel_state::KernelState;
use crate::output_prep::QueueCommand;

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

    let (mut reader, mut writer) =
        connect_and_handshake(&socket_path, &notebook_id, &runtime_agent_id, &blob_root).await?;

    info!("[runtime-agent] Connected to daemon, handshake sent");

    // -- 2. Bootstrap RuntimeStateDoc ---------------------------------------

    let state_doc = RuntimeStateDoc::new_with_actor(&runtime_agent_id);
    let mut coordinator_sync_state = automerge::sync::State::new();
    let state_doc = Arc::new(RwLock::new(state_doc));
    let (state_changed_tx, mut state_changed_rx) = broadcast::channel::<()>(64);

    // -- 3. Create local infrastructure -------------------------------------

    let blob_store = Arc::new(BlobStore::new(blob_root.clone()));
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
    let mut interrupt_handle: Option<crate::jupyter_kernel::InterruptHandle> = None;
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
                            // RuntimeAgentRequest: envelope with correlation ID.
                            // Commands (fire-and-forget) get no response.
                            // Queries (Complete, GetHistory) echo the ID back.
                            NotebookFrameType::Request => {
                                if let Ok(envelope) = serde_json::from_slice::<
                                    notebook_protocol::protocol::RuntimeAgentRequestEnvelope,
                                >(&typed_frame.payload) {
                                    // Interrupt bypasses &mut kernel via InterruptHandle
                                    if matches!(envelope.request, RuntimeAgentRequest::InterruptExecution) {
                                        if let Some(ref handle) = interrupt_handle {
                                            let handle = handle.clone();
                                            let cleared = kernel_state.clear_queue();
                                            // Write cleared entries to state doc
                                            {
                                                let mut sd = state_doc.write().await;
                                                for entry in &cleared {
                                                    sd.set_execution_done(&entry.execution_id, false);
                                                }
                                            }
                                            kernel_state.write_queue_to_state_doc().await;
                                            // Interrupt kernel in background — don't block the loop
                                            tokio::spawn(async move {
                                                if let Err(e) = handle.interrupt().await {
                                                    warn!("[runtime-agent] Interrupt failed: {}", e);
                                                }
                                            });
                                        } else {
                                            warn!("[runtime-agent] Interrupt requested but no kernel running");
                                        }
                                        continue;
                                    }

                                    let is_command = envelope.request.is_command();
                                    let id = envelope.id.clone();

                                    let (response, new_cmd_rx) = handle_runtime_agent_request(
                                        envelope.request,
                                        &ctx,
                                        &mut kernel,
                                        &mut kernel_state,
                                        &mut seen_execution_ids,
                                    ).await;

                                    if let Some(rx) = new_cmd_rx {
                                        cmd_rx = Some(rx);
                                    }
                                    // Update interrupt handle after any request that may change kernel state
                                    interrupt_handle = kernel.as_ref().and_then(|k| k.interrupt_handle());

                                    // Only send response for queries (not commands)
                                    if !is_command {
                                        let resp_envelope = notebook_protocol::protocol::RuntimeAgentResponseEnvelope {
                                            id,
                                            response,
                                        };
                                        let json = serde_json::to_vec(&resp_envelope)?;
                                        send_typed_frame(&mut writer, NotebookFrameType::Response, &json).await?;
                                    }
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

                                    // Per-change actor filter: diff comm state against a
                                    // foreign-only view of the post-sync doc. If the
                                    // coordinator coalesces a kernel-authored echo with a
                                    // frontend widget write into one RuntimeStateSync frame,
                                    // the foreign view omits the echo so we don't re-forward
                                    // it back to the kernel and trigger amplification.
                                    // Actors are opaque byte strings; the kernel-side writer
                                    // uses a UTF-8 `rt:kernel:<session>` prefix (see
                                    // `jupyter_kernel.rs`), so `starts_with` on the raw
                                    // bytes is sufficient.
                                    match sd.receive_sync_and_foreign_comms(
                                        &mut coordinator_sync_state,
                                        msg,
                                        |actor| !actor.to_bytes().starts_with(b"rt:kernel:"),
                                    ) {
                                        Ok(view) if !view.applied_actors.is_empty() => {
                                            let _ = state_changed_tx.send(());

                                            let queued = sd.get_queued_executions();
                                            drop(sd); // release write lock before kernel interaction

                                            let comm_updates = match view.foreign_comms {
                                                Some(foreign_comms) => {
                                                    diff_comm_state(&comms_before, &foreign_comms)
                                                }
                                                None => {
                                                    debug!(
                                                        "[runtime-agent] Skipping comm forward: {} applied change(s) were all self-kernel echoes",
                                                        view.applied_actors.len()
                                                    );
                                                    Vec::new()
                                                }
                                            };
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
                                        }
                                        Ok(_) => {
                                            // No changes applied (handshake/ack).
                                            drop(sd);
                                        }
                                        Err(e) => {
                                            warn!(
                                                "[runtime-agent] Failed to apply RuntimeStateSync: {}",
                                                e
                                            );
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
                        // A framing error here means one of two things:
                        //   - the daemon half-closed the sync stream (clean),
                        //     which we treat as a disconnect,
                        //   - or a byte-level desync corrupted the stream
                        //     (e.g. a stray writer, a massive length field
                        //     tripping the MAX_FRAME_SIZE cap).
                        // Either way the kernel state is still ours to own.
                        // Try to reconnect before tearing down the kernel —
                        // a brief network-side blip should not cost the user
                        // their session. Reconnection does a fresh handshake
                        // and a fresh Automerge sync state; the kernel,
                        // queue, seen_execution_ids, and local doc stay.
                        warn!(
                            "[runtime-agent] Socket read error: {} — reconnecting \
                             (kernel stays running)",
                            e
                        );
                        match reconnect_with_backoff(
                            &socket_path,
                            &notebook_id,
                            &runtime_agent_id,
                            &blob_root,
                        )
                        .await
                        {
                            Ok((new_reader, new_writer)) => {
                                reader = new_reader;
                                writer = new_writer;
                                // The daemon creates a fresh sync state for
                                // each connection; match that or the doc
                                // won't converge.
                                coordinator_sync_state = automerge::sync::State::new();
                                // Kick off a full resync so the daemon gets
                                // everything the kernel produced while we
                                // were disconnected.
                                let _ = state_changed_tx.send(());
                                info!("[runtime-agent] Reconnected to daemon");
                                continue;
                            }
                            Err(reconnect_err) => {
                                error!(
                                    "[runtime-agent] Reconnect failed after retries: {}",
                                    reconnect_err
                                );
                                break;
                            }
                        }
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

/// Concrete reader/writer halves returned by connect helpers.
#[cfg(unix)]
type AgentReader = tokio::io::ReadHalf<tokio::net::UnixStream>;
#[cfg(unix)]
type AgentWriter = tokio::io::WriteHalf<tokio::net::UnixStream>;
#[cfg(windows)]
type AgentReader = tokio::io::ReadHalf<tokio::net::windows::named_pipe::NamedPipeClient>;
#[cfg(windows)]
type AgentWriter = tokio::io::WriteHalf<tokio::net::windows::named_pipe::NamedPipeClient>;

/// Open a stream to the daemon socket and perform the RuntimeAgent
/// handshake. Extracted from the main startup path so the reconnect
/// path on framing errors can reuse it without duplicating handshake
/// logic.
async fn connect_and_handshake(
    socket_path: &std::path::Path,
    notebook_id: &str,
    runtime_agent_id: &str,
    blob_root: &std::path::Path,
) -> anyhow::Result<(AgentReader, AgentWriter)> {
    #[cfg(unix)]
    let stream = tokio::net::UnixStream::connect(socket_path).await?;

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

    let (reader, mut writer) = tokio::io::split(stream);

    send_preamble(&mut writer).await?;
    send_json_frame(
        &mut writer,
        &Handshake::RuntimeAgent {
            notebook_id: notebook_id.to_string(),
            runtime_agent_id: runtime_agent_id.to_string(),
            blob_root: blob_root.display().to_string(),
        },
    )
    .await?;

    Ok((reader, writer))
}

/// Attempt to reconnect to the daemon with exponential backoff.
///
/// Called after a framing error on the existing sync stream. Preserves
/// the kernel state by staying alive across transient socket trouble
/// (rogue client writing to the socket, daemon restart, etc.). Gives up
/// after a bounded number of attempts so a genuinely-gone daemon still
/// lets the agent exit rather than spin forever.
async fn reconnect_with_backoff(
    socket_path: &std::path::Path,
    notebook_id: &str,
    runtime_agent_id: &str,
    blob_root: &std::path::Path,
) -> anyhow::Result<(AgentReader, AgentWriter)> {
    const MAX_ATTEMPTS: u32 = 10;
    const BASE_DELAY_MS: u64 = 100;

    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=MAX_ATTEMPTS {
        match connect_and_handshake(socket_path, notebook_id, runtime_agent_id, blob_root).await {
            Ok(pair) => return Ok(pair),
            Err(e) => {
                let delay = BASE_DELAY_MS.saturating_mul(1 << (attempt - 1).min(6));
                warn!(
                    "[runtime-agent] Reconnect attempt {}/{} failed: {} (retrying in {}ms)",
                    attempt, MAX_ATTEMPTS, e, delay
                );
                last_err = Some(e);
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
        }
    }
    Err(last_err.unwrap_or_else(|| anyhow::anyhow!("reconnect gave up with no last error")))
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
                // Defensive sweep: mark any execution entries stuck in
                // "running" or "queued" that the local KernelState missed
                // (e.g., entries from CRDT sync not yet processed locally).
                let orphans = sd.mark_inflight_executions_failed();
                if orphans > 0 {
                    info!(
                        "[runtime-agent] Marked {orphans} orphaned execution(s) as failed on restart"
                    );
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

        RuntimeAgentRequest::SyncEnvironment(env_kind) => {
            info!(
                "[runtime-agent] SyncEnvironment: installing {:?}",
                env_kind.packages()
            );

            if let Some(ref kernel_ref) = kernel {
                let es = kernel_ref.env_source().to_string();

                // Deno doesn't support hot-sync — requires kernel restart
                if es == "deno" {
                    return (
                        RuntimeAgentResponse::Error {
                            error: "Hot-sync not supported for Deno environments. Kernel restart required.".to_string(),
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

                match env_kind {
                    notebook_protocol::protocol::EnvKind::Uv { packages } => {
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
                            Err(e) => {
                                error!(
                                    "[runtime-agent] Failed to sync UV packages {:?}: {}",
                                    packages, e
                                );
                                (
                                    RuntimeAgentResponse::Error {
                                        error: format!("Failed to install packages: {}", e),
                                    },
                                    None,
                                )
                            }
                        }
                    }
                    notebook_protocol::protocol::EnvKind::Conda { packages, channels } => {
                        let conda_env = kernel_env::conda::CondaEnvironment {
                            env_path: venv_path,
                            python_path,
                        };

                        let conda_deps = kernel_env::conda::CondaDependencies {
                            dependencies: packages.clone(),
                            channels: if channels.is_empty() {
                                vec!["conda-forge".to_string()]
                            } else {
                                channels
                            },
                            python: None,
                            env_id: None,
                        };

                        match kernel_env::conda::sync_dependencies(&conda_env, &conda_deps).await {
                            Ok(()) => (
                                RuntimeAgentResponse::EnvironmentSynced {
                                    synced_packages: packages,
                                },
                                None,
                            ),
                            Err(e) => {
                                error!(
                                    "[runtime-agent] Failed to sync Conda packages {:?} with channels {:?}: {}",
                                    packages, conda_deps.channels, e
                                );
                                (
                                    RuntimeAgentResponse::Error {
                                        error: format!("Failed to install packages: {}", e),
                                    },
                                    None,
                                )
                            }
                        }
                    }
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
            if let Some(ref mut k) = kernel {
                k.shutdown().await.ok();
            }
            *kernel = None;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel_connection::{KernelConnection, KernelLaunchConfig, KernelSharedRefs};
    use crate::protocol::CompletionItem;
    use anyhow::Result;
    use notebook_protocol::protocol::LaunchedEnvConfig;
    use std::path::PathBuf;

    /// Minimal mock kernel for testing queue/state logic without ZeroMQ.
    struct MockKernel;

    impl KernelConnection for MockKernel {
        async fn launch(
            _config: KernelLaunchConfig,
            _shared: KernelSharedRefs,
        ) -> Result<(Self, mpsc::Receiver<QueueCommand>)> {
            unimplemented!()
        }
        async fn execute(&mut self, _: &str, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        async fn interrupt(&mut self) -> Result<()> {
            Ok(())
        }
        async fn shutdown(&mut self) -> Result<()> {
            Ok(())
        }
        async fn send_comm_message(&mut self, _: serde_json::Value) -> Result<()> {
            Ok(())
        }
        async fn send_comm_update(&mut self, _: &str, _: serde_json::Value) -> Result<()> {
            Ok(())
        }
        async fn complete(
            &mut self,
            _: &str,
            _: usize,
        ) -> Result<(Vec<CompletionItem>, usize, usize)> {
            Ok((vec![], 0, 0))
        }
        async fn get_history(
            &mut self,
            _: Option<&str>,
            _: i32,
            _: bool,
        ) -> Result<Vec<crate::protocol::HistoryEntry>> {
            Ok(vec![])
        }
        fn kernel_type(&self) -> &str {
            "python"
        }
        fn env_source(&self) -> &str {
            "test"
        }
        fn launched_config(&self) -> &LaunchedEnvConfig {
            Box::leak(Box::new(LaunchedEnvConfig::default()))
        }
        fn env_path(&self) -> Option<&PathBuf> {
            None
        }
        fn is_connected(&self) -> bool {
            true
        }
        fn update_launched_uv_deps(&mut self, _: Vec<String>) {}
    }

    /// Build test fixtures: RuntimeAgentContext + KernelState wired to the same doc.
    fn test_fixtures() -> (
        RuntimeAgentContext,
        KernelState,
        Arc<RwLock<RuntimeStateDoc>>,
    ) {
        let state_doc = Arc::new(RwLock::new(RuntimeStateDoc::new()));
        let (state_changed_tx, _) = broadcast::channel(64);
        let (broadcast_tx, _) = broadcast::channel(64);
        let (presence_tx, _) = broadcast::channel(16);
        let blob_store = Arc::new(BlobStore::new(std::env::temp_dir().join("test-blobs")));
        let presence = Arc::new(RwLock::new(PresenceState::new()));

        let ctx = RuntimeAgentContext {
            state_doc: state_doc.clone(),
            state_changed_tx: state_changed_tx.clone(),
            blob_store,
            broadcast_tx: broadcast_tx.clone(),
            presence,
            presence_tx,
        };
        let state = KernelState::new(state_doc.clone(), state_changed_tx, broadcast_tx);
        (ctx, state, state_doc)
    }

    #[tokio::test]
    async fn kernel_died_marks_inflight_executions_as_failed_in_state_doc() {
        let (ctx, mut state, state_doc) = test_fixtures();
        let mut mock = MockKernel;
        state.set_idle();

        // Queue two cells: c1 starts executing, c2 stays queued
        state
            .queue_cell("c1".into(), "e1".into(), "x=1".into(), &mut mock)
            .await
            .unwrap();
        state
            .queue_cell("c2".into(), "e2".into(), "x=2".into(), &mut mock)
            .await
            .unwrap();

        // Verify initial state in doc
        {
            let sd = state_doc.read().await;
            let e1 = sd.get_execution("e1").unwrap();
            assert_eq!(e1.status, "running");
            let e2 = sd.get_execution("e2").unwrap();
            assert_eq!(e2.status, "queued");
        }

        // Simulate kernel death
        handle_queue_command(
            QueueCommand::KernelDied,
            &ctx,
            &mut None::<JupyterKernel>,
            &mut state,
        )
        .await
        .unwrap();

        // Both executions should now be marked as error in RuntimeStateDoc
        let sd = state_doc.read().await;
        let e1 = sd.get_execution("e1").unwrap();
        assert_eq!(e1.status, "error");
        assert_eq!(e1.success, Some(false));

        let e2 = sd.get_execution("e2").unwrap();
        assert_eq!(e2.status, "error");
        assert_eq!(e2.success, Some(false));

        // Queue should be cleared
        let queue = sd.read_state();
        assert!(queue.queue.executing.is_none());
        assert!(queue.queue.queued.is_empty());

        // Kernel status should be error
        assert_eq!(queue.kernel.status, "error");
    }

    #[tokio::test]
    async fn kernel_died_with_no_inflight_executions_clears_state() {
        let (ctx, mut state, state_doc) = test_fixtures();
        state.set_idle();

        // No cells queued — just fire KernelDied
        handle_queue_command(
            QueueCommand::KernelDied,
            &ctx,
            &mut None::<JupyterKernel>,
            &mut state,
        )
        .await
        .unwrap();

        let sd = state_doc.read().await;
        let rs = sd.read_state();
        assert_eq!(rs.kernel.status, "error");
        assert!(rs.queue.executing.is_none());
        assert!(rs.queue.queued.is_empty());
    }
}
