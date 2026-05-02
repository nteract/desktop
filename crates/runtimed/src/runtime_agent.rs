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
use notebook_protocol::connection::{
    send_json_frame, send_preamble, send_typed_frame, FramedReader, Handshake, NotebookFrameType,
    PackageManager,
};
use notebook_protocol::protocol::{RuntimeAgentRequest, RuntimeAgentResponse};
use runtime_doc::RuntimeStateHandle;
use runtime_doc::{CommDocEntry, RuntimeLifecycle, RuntimeStateDoc};
use tokio::sync::{broadcast, mpsc, RwLock};
use tracing::{debug, error, info, warn};

use crate::blob_store::BlobStore;
use crate::jupyter_kernel::JupyterKernel;
use crate::kernel_connection::{KernelConnection, KernelLaunchConfig, KernelSharedRefs};
use crate::kernel_state::KernelState;
use crate::output_prep::QueueCommand;

/// Shared context for the runtime agent (no kernel -- kernel is owned locally).
struct RuntimeAgentContext {
    state: RuntimeStateHandle,
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

    let (reader, mut writer) =
        connect_and_handshake(&socket_path, &notebook_id, &runtime_agent_id, &blob_root).await?;

    info!("[runtime-agent] Connected to daemon, handshake sent");

    // Hand the read half to a dedicated FramedReader actor so the busy
    // `select!` below stays cancel-safe — `recv_typed_frame`'s
    // `read_exact` calls would otherwise drop bytes mid-read whenever
    // another arm wins, producing the runtime-agent ↔ daemon desync
    // captured in production logs as `frame too large: 538976288 bytes`.
    let mut framed_reader = FramedReader::spawn(reader, 16);

    // -- 2. Bootstrap RuntimeStateDoc ---------------------------------------

    let state_doc = RuntimeStateDoc::try_new_with_actor(&runtime_agent_id)
        .map_err(|e| anyhow::anyhow!("create runtime-agent state doc: {e}"))?;
    let mut coordinator_sync_state = automerge::sync::State::new();
    let (state_changed_tx, mut state_changed_rx) = broadcast::channel::<()>(64);
    // Keep a clone of the sender for the reconnect "kick" path that needs
    // to force a sync round even when heads haven't changed.
    let state_kick_tx = state_changed_tx.clone();
    let state = RuntimeStateHandle::new(state_doc, state_changed_tx);

    // -- 3. Create local infrastructure -------------------------------------

    let blob_store = Arc::new(BlobStore::new(blob_root.clone()));
    let (broadcast_tx, _broadcast_rx) =
        broadcast::channel::<notebook_protocol::protocol::NotebookBroadcast>(16);
    let presence = Arc::new(RwLock::new(PresenceState::new()));
    let (presence_tx, _presence_rx) = broadcast::channel::<(String, Vec<u8>)>(16);

    let ctx = RuntimeAgentContext {
        state: state.clone(),
        blob_store,
        broadcast_tx: broadcast_tx.clone(),
        presence,
        presence_tx,
    };

    // -- Local variables owned by the select! loop (no mutex) ---------------

    let mut kernel: Option<JupyterKernel> = None;
    let mut interrupt_handle: Option<crate::jupyter_kernel::InterruptHandle> = None;
    let mut kernel_state = KernelState::new(state.clone());
    let mut seen_execution_ids = HashSet::new();
    let mut cmd_rx: Option<mpsc::Receiver<QueueCommand>> = None;

    // Async responses from spawned tasks (currently: SyncEnvironment).
    // Keeping these off the request handler's await frees the main loop to
    // forward state_changed_rx frames while `sync_dependencies` runs, so the
    // progress banner updates live instead of collapsing to one final write.
    let (async_response_tx, mut async_response_rx) =
        mpsc::channel::<notebook_protocol::protocol::RuntimeAgentResponseEnvelope>(4);

    // In-flight env-sync task. Only one SyncEnvironment may run at a time;
    // overlapping env mutations race on the same prefix. Launch/restart is
    // rejected while a sync is active for the same reason. Shutdown is allowed,
    // but it invalidates the sync generation so terminal progress does not
    // claim the now-shutdown kernel is ready.
    //
    // The atomic is shared between the main loop (which bumps it) and each
    // spawned task (which compares its captured generation before emitting
    // terminal events). u64 wraparound is a non-issue in practice.
    let sync_generation = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut inflight_sync: Option<tokio::task::JoinHandle<()>> = None;

    info!("[runtime-agent] Infrastructure ready, entering main loop");

    // -- 4. Main event loop -------------------------------------------------

    loop {
        tokio::select! {
            // Read frames from daemon socket (cancel-safe via FramedReader actor)
            maybe_frame = framed_reader.recv() => {
                match maybe_frame {
                    Some(Ok(typed_frame)) => {
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
                                            // Mark interrupted BEFORE clearing the queue so
                                            // the subsequent CellError (KeyboardInterrupt on
                                            // IOPub) doesn't cascade into cells queued after
                                            // this point.
                                            kernel_state.mark_interrupted();
                                            let cleared = kernel_state.clear_queue();
                                            // Write cleared entries to state doc
                                            if let Err(e) = state.with_doc(|sd| {
                                                for entry in &cleared {
                                                    sd.set_execution_done(&entry.execution_id, false)?;
                                                }
                                                Ok(())
                                            }) {
                                                warn!("[runtime-state] {}", e);
                                            }
                                            kernel_state.write_queue_to_state_doc();
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

                                    // SyncEnvironment runs `uv`/`conda` install, which can take
                                    // seconds. Handling it inline would starve the
                                    // state_changed_rx arm, so the coordinator wouldn't see
                                    // Installing/Solving/Download events until the install
                                    // finished. Snapshot the launched config and spawn.
                                    //
                                    // Reject overlapping syncs rather than aborting the task:
                                    // tokio::process children keep running when their waiting
                                    // future is dropped, so aborting would not stop the uv
                                    // install that is mutating the environment prefix.
                                    if let RuntimeAgentRequest::SyncEnvironment(env_kind) =
                                        &envelope.request
                                    {
                                        if inflight_sync.is_some() {
                                            if let Err(e) = send_runtime_agent_response(
                                                &mut writer,
                                                envelope.id.clone(),
                                                RuntimeAgentResponse::Error {
                                                    error: "Environment sync already in progress"
                                                        .to_string(),
                                                },
                                            )
                                            .await
                                            {
                                                warn!(
                                                    "[runtime-agent] Failed to send busy SyncEnvironment response: {}",
                                                    e
                                                );
                                                break;
                                            }
                                            continue;
                                        }
                                        let generation = sync_generation
                                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                                            .wrapping_add(1);

                                        let snapshot = kernel.as_ref().map(|k| {
                                            (k.env_source().to_string(), k.launched_config().clone())
                                        });
                                        let env_kind = env_kind.clone();
                                        let id = envelope.id.clone();
                                        let state_for_task = ctx.state.clone();
                                        let tx = async_response_tx.clone();
                                        let gen_ref = sync_generation.clone();

                                        let handle = tokio::spawn(async move {
                                            let response = run_sync_environment(
                                                env_kind,
                                                snapshot,
                                                state_for_task,
                                                generation,
                                                gen_ref,
                                            )
                                            .await;
                                            let response = response.unwrap_or_else(|| {
                                                RuntimeAgentResponse::Error {
                                                    error: "Environment sync was superseded"
                                                        .to_string(),
                                                }
                                            });
                                            let envelope = notebook_protocol::protocol::RuntimeAgentResponseEnvelope {
                                                id,
                                                response,
                                            };
                                            if tx.send(envelope).await.is_err() {
                                                warn!(
                                                    "[runtime-agent] SyncEnvironment response channel closed before send",
                                                );
                                            }
                                        });
                                        inflight_sync = Some(handle);
                                        continue;
                                    }

                                    // Launch/restart would race against the active prefix
                                    // mutation, so reject it until the sync task sends its
                                    // response. Shutdown is allowed because the daemon-side
                                    // handler treats it as best-effort, but it still bumps
                                    // the generation so the sync task will skip terminal
                                    // Ready/Error writes after the kernel is gone.
                                    let is_launch_or_restart = matches!(
                                        &envelope.request,
                                        RuntimeAgentRequest::LaunchKernel { .. }
                                            | RuntimeAgentRequest::RestartKernel { .. }
                                    );
                                    if is_launch_or_restart && inflight_sync.is_some() {
                                        if let Err(e) = send_runtime_agent_response(
                                            &mut writer,
                                            envelope.id.clone(),
                                            RuntimeAgentResponse::Error {
                                                error: "Environment sync in progress; retry after it completes"
                                                    .to_string(),
                                            },
                                        )
                                        .await
                                        {
                                            warn!(
                                                "[runtime-agent] Failed to send lifecycle busy response: {}",
                                                e
                                            );
                                            break;
                                        }
                                        continue;
                                    }

                                    if is_launch_or_restart
                                        || matches!(
                                            &envelope.request,
                                            RuntimeAgentRequest::ShutdownKernel
                                        )
                                    {
                                        sync_generation
                                            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
                                        send_runtime_agent_response(&mut writer, id, response).await?;
                                    }
                                }
                            }

                            // RuntimeStateSync -- apply coordinator's changes, check for new queue entries
                            // and forward frontend-originated comm state changes to kernel
                            NotebookFrameType::RuntimeStateSync => {
                                if let Ok(msg) = automerge::sync::Message::decode(&typed_frame.payload) {
                                    // Apply sync and extract data we need for async
                                    // work, all in one lock acquisition.
                                    let sync_result = ctx.state.with_doc(|sd| {
                                        // Snapshot comm state before applying sync so we can
                                        // detect frontend-originated widget state changes.
                                        let comms_before = sd.read_state().comms;

                                        // Per-change actor filter: diff comm state against a
                                        // foreign-only view of the post-sync doc.
                                        match sd.receive_sync_and_foreign_comms(
                                            &mut coordinator_sync_state,
                                            msg,
                                            |actor| !actor.to_bytes().starts_with(b"rt:kernel:"),
                                        ) {
                                            Ok(view) if !view.applied_actors.is_empty() => {
                                                let queued = sd.get_queued_executions();
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
                                                Ok(Some((queued, comm_updates)))
                                            }
                                            Ok(_) => Ok(None),
                                            Err(e) => {
                                                warn!(
                                                    "[runtime-agent] Failed to apply RuntimeStateSync: {}",
                                                    e
                                                );
                                                Ok(None)
                                            }
                                        }
                                    });

                                    // Async work outside the lock
                                    if let Ok(Some((queued, comm_updates))) = sync_result {
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

                                    // Send sync reply
                                    let reply_encoded = ctx.state.with_doc(|sd| {
                                        Ok(sd.generate_sync_message(&mut coordinator_sync_state)
                                            .map(|reply| reply.encode()))
                                    }).ok().flatten();
                                    if let Some(encoded) = reply_encoded {
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
                    Some(Err(e)) => {
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
                        // Drop the old framed reader before reconnecting so
                        // its background task exits cleanly.
                        drop(framed_reader);
                        match reconnect_with_backoff(
                            &socket_path,
                            &notebook_id,
                            &runtime_agent_id,
                            &blob_root,
                        )
                        .await
                        {
                            Ok((new_reader, new_writer)) => {
                                framed_reader = FramedReader::spawn(new_reader, 16);
                                writer = new_writer;
                                // The daemon creates a fresh sync state for
                                // each connection; match that or the doc
                                // won't converge.
                                coordinator_sync_state = automerge::sync::State::new();
                                // Kick off a full resync so the daemon gets
                                // everything the kernel produced while we
                                // were disconnected.
                                let _ = state_kick_tx.send(());
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
                    None => {
                        info!("[runtime-agent] Daemon disconnected (EOF)");
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

                let encoded = ctx.state.with_doc(|sd| {
                    Ok(sd.generate_sync_message(&mut coordinator_sync_state)
                        .map(|msg| msg.encode()))
                }).ok().flatten();
                if let Some(encoded) = encoded {
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

            // Responses from long-running spawned tasks (SyncEnvironment).
            Some(envelope) = async_response_rx.recv() => {
                inflight_sync = None;
                if let Err(e) = send_runtime_agent_response_envelope(&mut writer, envelope).await {
                    warn!("[runtime-agent] Failed to send async response: {}", e);
                    break;
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

async fn send_runtime_agent_response(
    writer: &mut AgentWriter,
    id: String,
    response: RuntimeAgentResponse,
) -> anyhow::Result<()> {
    send_runtime_agent_response_envelope(
        writer,
        notebook_protocol::protocol::RuntimeAgentResponseEnvelope { id, response },
    )
    .await
}

async fn send_runtime_agent_response_envelope(
    writer: &mut AgentWriter,
    envelope: notebook_protocol::protocol::RuntimeAgentResponseEnvelope,
) -> anyhow::Result<()> {
    let json = serde_json::to_vec(&envelope)?;
    send_typed_frame(writer, NotebookFrameType::Response, &json).await?;
    Ok(())
}

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
            kernel_ports,
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
                        env_type: if env_source.package_manager() == Some(PackageManager::Conda) {
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
                state: ctx.state.clone(),
                blob_store: ctx.blob_store.clone(),
                broadcast_tx: ctx.broadcast_tx.clone(),
                presence: ctx.presence.clone(),
                presence_tx: ctx.presence_tx.clone(),
            };
            let config = KernelLaunchConfig {
                kernel_type,
                env_source: env_source.as_str().to_string(),
                notebook_path: notebook_path.as_deref().map(PathBuf::from),
                launched_config,
                kernel_ports,
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
                        RuntimeAgentResponse::KernelLaunched {
                            env_source: notebook_protocol::connection::EnvSource::parse(&es),
                        },
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
            kernel_ports,
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
                        env_type: if env_source.package_manager() == Some(PackageManager::Conda) {
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
                state: ctx.state.clone(),
                blob_store: ctx.blob_store.clone(),
                broadcast_tx: ctx.broadcast_tx.clone(),
                presence: ctx.presence.clone(),
                presence_tx: ctx.presence_tx.clone(),
            };
            let config = KernelLaunchConfig {
                kernel_type,
                env_source: env_source.as_str().to_string(),
                notebook_path: notebook_path.as_deref().map(PathBuf::from),
                launched_config,
                kernel_ports,
                env_vars: vec![],
                pooled_env,
            };

            // Mark stale executions as failed in RuntimeStateDoc.
            // The old kernel is gone after shutdown, so these executions
            // can never complete — do this before launching the new kernel.
            if let Err(e) = ctx.state.with_doc(|sd| {
                if let Some(ref eid) = interrupted_eid {
                    sd.set_execution_done(eid, false)?;
                }
                for eid in &stale_queue {
                    sd.set_execution_done(eid, false)?;
                }
                // Defensive sweep: mark any execution entries stuck in
                // "running" or "queued" that the local KernelState missed
                // (e.g., entries from CRDT sync not yet processed locally).
                match sd.mark_inflight_executions_failed() {
                    Ok(orphans) if orphans > 0 => {
                        info!(
                            "[runtime-agent] Marked {orphans} orphaned execution(s) as failed on restart"
                        );
                    }
                    Err(e) => {
                        warn!("[runtime-state] {}", e);
                    }
                    _ => {}
                }
                sd.set_queue(None, &[])?;
                Ok(())
            }) {
                warn!("[runtime-state] {}", e);
            }

            match JupyterKernel::launch(config, shared).await {
                Ok((k, rx)) => {
                    let es = k.env_source().to_string();
                    *kernel = Some(k);
                    state.reset();
                    state.set_idle();
                    (
                        RuntimeAgentResponse::KernelRestarted {
                            env_source: notebook_protocol::connection::EnvSource::parse(&es),
                        },
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
                        state.mark_interrupted();
                        let cleared = state.clear_queue();
                        // Write cleared entries to state doc
                        if let Err(e) = ctx.state.with_doc(|sd| {
                            for entry in &cleared {
                                sd.set_execution_done(&entry.execution_id, false)?;
                            }
                            Ok(())
                        }) {
                            warn!("[runtime-state] {}", e);
                        }
                        state.write_queue_to_state_doc();
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

        RuntimeAgentRequest::SyncEnvironment(_) => {
            // The main select loop intercepts SyncEnvironment and spawns
            // `run_sync_environment` so it doesn't block state sync. This
            // arm is unreachable in practice, but keeps the match exhaustive.
            (
                RuntimeAgentResponse::Error {
                    error: "SyncEnvironment must be dispatched through run_sync_environment"
                        .to_string(),
                },
                None,
            )
        }
    }
}

/// Run a hot-sync install outside the main select loop.
///
/// The caller snapshots the kernel's `(env_source, launched_config)` before
/// spawning so we don't borrow the kernel across the install. Progress phases
/// flow through [`RuntimeDocProgressHandler`] into the runtime-agent's
/// RuntimeStateDoc, which the main loop forwards to the coordinator live.
///
/// `generation` is the sync generation this task owns; `current_generation`
/// is a shared atomic bumped by the main loop on kernel lifecycle transitions
/// and on any newer SyncEnvironment request. Returning `None` signals the task
/// was superseded: the caller drops the response so a stale id doesn't
/// correlate to the coordinator's latest request, and the final Ready/Error
/// is skipped so we don't clobber a fresher `env.progress` write.
async fn run_sync_environment(
    env_kind: notebook_protocol::protocol::EnvKind,
    kernel_snapshot: Option<(String, notebook_protocol::protocol::LaunchedEnvConfig)>,
    state: RuntimeStateHandle,
    generation: u64,
    current_generation: std::sync::Arc<std::sync::atomic::AtomicU64>,
) -> Option<RuntimeAgentResponse> {
    info!(
        "[runtime-agent] SyncEnvironment[gen={}]: installing {:?}",
        generation,
        env_kind.packages()
    );

    let is_current = || current_generation.load(std::sync::atomic::Ordering::SeqCst) == generation;

    let Some((es, launched)) = kernel_snapshot else {
        return Some(RuntimeAgentResponse::Error {
            error: "No kernel running".to_string(),
        });
    };

    // Deno doesn't support hot-sync — requires kernel restart.
    if es == "deno" {
        return Some(RuntimeAgentResponse::Error {
            error: "Hot-sync not supported for Deno environments. Kernel restart required."
                .to_string(),
        });
    }

    let Some(venv_path) = launched.venv_path.clone() else {
        return Some(RuntimeAgentResponse::Error {
            error: "No venv path available".to_string(),
        });
    };
    let Some(python_path) = launched.python_path.clone() else {
        return Some(RuntimeAgentResponse::Error {
            error: "No python path available".to_string(),
        });
    };

    let handler: std::sync::Arc<dyn kernel_env::ProgressHandler> = std::sync::Arc::new(
        crate::inline_env::RuntimeDocProgressHandler::new(state.clone()),
    );

    match env_kind {
        notebook_protocol::protocol::EnvKind::Uv { packages } => {
            let uv_env = kernel_env::uv::UvEnvironment {
                venv_path: venv_path.clone(),
                python_path: python_path.clone(),
            };
            match kernel_env::uv::sync_dependencies(&uv_env, &packages, handler.clone()).await {
                Ok(()) => {
                    if !is_current() {
                        debug!(
                            "[runtime-agent] SyncEnvironment[gen={}] superseded before Ready",
                            generation
                        );
                        return None;
                    }
                    // Terminal Ready phase so the frontend banner clears.
                    // `sync_dependencies` emits InstallComplete, which is not a
                    // terminal state for `projectEnvProgress`; Ready is.
                    handler.on_progress(
                        "uv",
                        kernel_env::EnvProgressPhase::Ready {
                            env_path: venv_path.to_string_lossy().into_owned(),
                            python_path: python_path.to_string_lossy().into_owned(),
                        },
                    );
                    Some(RuntimeAgentResponse::EnvironmentSynced {
                        synced_packages: packages,
                    })
                }
                Err(e) => {
                    error!(
                        "[runtime-agent] Failed to sync UV packages {:?}: {}",
                        packages, e
                    );
                    if !is_current() {
                        return None;
                    }
                    let msg = format!("Failed to install packages: {}", e);
                    handler.on_progress(
                        "uv",
                        kernel_env::EnvProgressPhase::Error {
                            message: msg.clone(),
                        },
                    );
                    Some(RuntimeAgentResponse::Error { error: msg })
                }
            }
        }
        notebook_protocol::protocol::EnvKind::Conda { packages, channels } => {
            let conda_env = kernel_env::conda::CondaEnvironment {
                env_path: venv_path.clone(),
                python_path: python_path.clone(),
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
            match kernel_env::conda::sync_dependencies(&conda_env, &conda_deps, handler.clone())
                .await
            {
                Ok(()) => {
                    if !is_current() {
                        debug!(
                            "[runtime-agent] SyncEnvironment[gen={}] superseded before Ready",
                            generation
                        );
                        return None;
                    }
                    handler.on_progress(
                        "conda",
                        kernel_env::EnvProgressPhase::Ready {
                            env_path: venv_path.to_string_lossy().into_owned(),
                            python_path: python_path.to_string_lossy().into_owned(),
                        },
                    );
                    Some(RuntimeAgentResponse::EnvironmentSynced {
                        synced_packages: packages,
                    })
                }
                Err(e) => {
                    error!(
                        "[runtime-agent] Failed to sync Conda packages {:?} with channels {:?}: {}",
                        packages, conda_deps.channels, e
                    );
                    if !is_current() {
                        return None;
                    }
                    let msg = format!("Failed to install packages: {}", e);
                    handler.on_progress(
                        "conda",
                        kernel_env::EnvProgressPhase::Error {
                            message: msg.clone(),
                        },
                    );
                    Some(RuntimeAgentResponse::Error { error: msg })
                }
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

            // If the current execution was interrupted, the CellError is
            // just the KeyboardInterrupt — don't cascade into the queue.
            // Cells queued after the interrupt should still execute.
            if !state.is_interrupted() {
                let cleared = state.clear_queue();
                if let Err(e) = ctx.state.with_doc(|sd| {
                    for entry in &cleared {
                        sd.set_execution_done(&entry.execution_id, false)?;
                    }
                    sd.set_queue(None, &[])?;
                    Ok(())
                }) {
                    warn!("[runtime-state] {}", e);
                }
            } else {
                debug!(
                    "[runtime-agent] Skipping queue clear for interrupted cell {} ({})",
                    cell_id, execution_id
                );
            }
        }

        QueueCommand::KernelDied => {
            warn!("[runtime-agent] Kernel died");
            if let Some(ref mut k) = kernel {
                k.shutdown().await.ok();
            }
            *kernel = None;
            let (interrupted, cleared) = state.kernel_died();
            if let Err(e) = ctx.state.with_doc(|sd| {
                if let Some((_, ref eid)) = interrupted {
                    sd.set_execution_done(eid, false)?;
                }
                for entry in &cleared {
                    sd.set_execution_done(&entry.execution_id, false)?;
                }
                // Generic kernel-died path — no specific typed reason. Clear
                // any stale error_reason from a prior failure so the frontend
                // doesn't misreport this death as (say) a repeat
                // missing_ipykernel incident.
                sd.set_lifecycle_with_error(&RuntimeLifecycle::Error, None)?;
                sd.set_queue(None, &[])?;
                Ok(())
            }) {
                warn!("[runtime-state] {}", e);
            }
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
    fn test_fixtures() -> (RuntimeAgentContext, KernelState, RuntimeStateHandle) {
        let (state_changed_tx, _) = broadcast::channel(64);
        let handle = RuntimeStateHandle::new(RuntimeStateDoc::new(), state_changed_tx);
        let (broadcast_tx, _) = broadcast::channel(64);
        let (presence_tx, _) = broadcast::channel(16);
        let blob_store = Arc::new(BlobStore::new(std::env::temp_dir().join("test-blobs")));
        let presence = Arc::new(RwLock::new(PresenceState::new()));

        let ctx = RuntimeAgentContext {
            state: handle.clone(),
            blob_store,
            broadcast_tx: broadcast_tx.clone(),
            presence,
            presence_tx,
        };
        let state = KernelState::new(handle.clone());
        (ctx, state, handle)
    }

    #[tokio::test]
    async fn kernel_died_marks_inflight_executions_as_failed_in_state_doc() {
        let (ctx, mut state, handle) = test_fixtures();
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
            let e1 = handle.read(|sd| sd.get_execution("e1").unwrap()).unwrap();
            assert_eq!(e1.status, "running");
            let e2 = handle.read(|sd| sd.get_execution("e2").unwrap()).unwrap();
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
        let e1 = handle.read(|sd| sd.get_execution("e1").unwrap()).unwrap();
        assert_eq!(e1.status, "error");
        assert_eq!(e1.success, Some(false));

        let e2 = handle.read(|sd| sd.get_execution("e2").unwrap()).unwrap();
        assert_eq!(e2.status, "error");
        assert_eq!(e2.success, Some(false));

        // Queue should be cleared
        let queue = handle.read(|sd| sd.read_state()).unwrap();
        assert!(queue.queue.executing.is_none());
        assert!(queue.queue.queued.is_empty());

        // Kernel lifecycle should be Error
        assert_eq!(queue.kernel.lifecycle, RuntimeLifecycle::Error);
    }

    #[tokio::test]
    async fn kernel_died_with_no_inflight_executions_clears_state() {
        let (ctx, mut state, handle) = test_fixtures();
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

        let rs = handle.read(|sd| sd.read_state()).unwrap();
        assert_eq!(rs.kernel.lifecycle, RuntimeLifecycle::Error);
        assert!(rs.queue.executing.is_none());
        assert!(rs.queue.queued.is_empty());
    }

    /// Reproduces the interrupt+execute race: interrupt fires while cell A is
    /// running, then cell B is queued. The IOPub KeyboardInterrupt error for
    /// cell A must NOT clear cell B from the queue.
    #[tokio::test]
    async fn cell_error_after_interrupt_does_not_clear_queue() {
        let (ctx, mut state, _handle) = test_fixtures();
        let mut mock = MockKernel;
        state.set_idle();

        // 1. Cell A starts executing
        state
            .queue_cell(
                "cA".into(),
                "eA".into(),
                "while True: pass".into(),
                &mut mock,
            )
            .await
            .unwrap();
        assert_eq!(state.executing_cell().unwrap().0, "cA");

        // 2. Interrupt fires — clears queue, marks interrupted
        state.mark_interrupted();
        let cleared = state.clear_queue();
        assert!(cleared.is_empty()); // only cA was executing, nothing queued

        // 3. Cell B is queued (arrives via CRDT sync after interrupt)
        state
            .queue_cell("cB".into(), "eB".into(), "1 + 1".into(), &mut mock)
            .await
            .unwrap();
        assert_eq!(state.queued_entries().len(), 1);

        // 4. IOPub delivers KeyboardInterrupt error for cell A
        //    handle_queue_command requires Option<JupyterKernel>, not MockKernel.
        //    Call mark_execution_error + verify is_interrupted directly.
        state.mark_execution_error();

        // The CellError handler checks is_interrupted — simulate the branch
        assert!(state.is_interrupted());

        // Since interrupted, queue should NOT be cleared
        assert_eq!(state.queued_entries().len(), 1);
        assert_eq!(state.queued_entries()[0].cell_id, "cB");
    }

    /// Without the interrupted flag, CellError should still cascade normally
    /// (e.g., a real user-code error in "Run All").
    #[tokio::test]
    async fn cell_error_without_interrupt_clears_queue() {
        let (ctx, mut state, handle) = test_fixtures();
        let mut mock = MockKernel;
        state.set_idle();

        // Queue three cells: cA executes, cB and cC are queued
        state
            .queue_cell("cA".into(), "eA".into(), "1/0".into(), &mut mock)
            .await
            .unwrap();
        state
            .queue_cell("cB".into(), "eB".into(), "x=1".into(), &mut mock)
            .await
            .unwrap();
        state
            .queue_cell("cC".into(), "eC".into(), "x=2".into(), &mut mock)
            .await
            .unwrap();
        assert_eq!(state.queued_entries().len(), 2);

        // Not interrupted — CellError should cascade
        assert!(!state.is_interrupted());
        state.mark_execution_error();
        let cleared = state.clear_queue();

        // Queue should be cleared — both cB and cC drained
        assert_eq!(cleared.len(), 2);
        assert!(state.queued_entries().is_empty());

        // Write cleared entries to state doc (mirrors handle_queue_command)
        if let Err(e) = ctx.state.with_doc(|sd| {
            for entry in &cleared {
                sd.set_execution_done(&entry.execution_id, false)?;
            }
            Ok(())
        }) {
            panic!("state doc write failed: {}", e);
        }

        // cB and cC should be marked as failed in RuntimeStateDoc
        let eb = handle.read(|sd| sd.get_execution("eB").unwrap()).unwrap();
        assert_eq!(eb.status, "error");
        let ec = handle.read(|sd| sd.get_execution("eC").unwrap()).unwrap();
        assert_eq!(ec.status, "error");
    }
}
