//! Coordinator-side management of a runtime agent subprocess.
//!
//! `AgentHandle` spawns a `runtimed agent` child process, pipes the framed
//! protocol on its stdin/stdout, syncs RuntimeStateDoc bidirectionally, and
//! detects agent exit.
//!
//! ## Data flow
//!
//! ```text
//! Coordinator                          Agent subprocess
//! ──────────                          ─────────────────
//! AgentRequest (0x01) ──stdin──►      handle_agent_request()
//! ◄──stdout── AgentResponse (0x02)
//! ◄──stdout── RuntimeStateSync (0x05)  kernel writes state_doc
//! RuntimeStateSync (0x05) ──stdin──►  comm state from frontend
//! ◄──stdout── Broadcast (0x03)         KernelDied notification
//! ```

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use log::{debug, info, warn};
use notebook_doc::runtime_state::RuntimeStateDoc;
use notebook_protocol::connection::{
    recv_frame, send_json_frame, send_preamble, send_typed_frame, Handshake, NotebookFrameType,
};
use notebook_protocol::protocol::{
    AgentNotification, AgentRequest, AgentResponse, LaunchedEnvConfig,
};
use tokio::io::{AsyncRead, AsyncWrite, BufReader, BufWriter};
use tokio::process::{Child, Command};
use tokio::sync::{broadcast, mpsc, oneshot, RwLock};

/// Handle to a running agent subprocess.
///
/// Provides typed methods for sending requests and automatically manages
/// RuntimeStateDoc sync between coordinator and agent.
pub struct AgentHandle {
    /// Channel for sending requests to the IO task (which writes to stdin).
    request_tx: mpsc::Sender<(AgentRequest, oneshot::Sender<AgentResponse>)>,
    /// Whether the agent process is still alive.
    alive: Arc<AtomicBool>,
    /// Handle to the IO task for cleanup.
    io_task: tokio::task::JoinHandle<()>,
    /// Handle to the child process.
    child: Child,
}

impl AgentHandle {
    /// Spawn a new agent subprocess and set up bidirectional communication.
    ///
    /// The coordinator sends the preamble + `RuntimeAgent` handshake, then
    /// starts the IO task for frame multiplexing and RuntimeStateDoc sync.
    pub async fn spawn(
        notebook_id: String,
        agent_id: String,
        blob_root: PathBuf,
        state_doc: Arc<RwLock<RuntimeStateDoc>>,
        state_changed_tx: broadcast::Sender<()>,
        _broadcast_tx: broadcast::Sender<notebook_protocol::protocol::NotebookBroadcast>,
    ) -> Result<Self> {
        // Find the current binary — agent runs as `runtimed agent`
        let exe = std::env::current_exe()?;
        info!(
            "[agent-handle] Spawning agent: {} agent (notebook_id={})",
            exe.display(),
            notebook_id
        );

        let mut child = Command::new(&exe)
            .arg("agent")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;

        let child_stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture agent stdin"))?;
        let child_stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture agent stdout"))?;

        let mut writer = child_stdin;
        let mut reader = child_stdout;

        // Send preamble + handshake
        send_preamble(&mut writer).await?;
        send_json_frame(
            &mut writer,
            &Handshake::RuntimeAgent {
                notebook_id: notebook_id.clone(),
                agent_id: agent_id.clone(),
                blob_root: blob_root.to_string_lossy().to_string(),
            },
        )
        .await?;

        // Wait for agent's preamble response
        notebook_protocol::connection::recv_preamble(&mut reader).await?;

        // The agent owns the RuntimeStateDoc — it creates a fully scaffolded doc
        // and syncs it to the coordinator via the IO loop. No bootstrap sync needed
        // here. The IO task handles bidirectional sync naturally.
        info!("[agent-handle] Agent spawned — RuntimeStateDoc owned by agent");

        // Set up channels
        let (request_tx, request_rx) = mpsc::channel(32);
        let alive = Arc::new(AtomicBool::new(true));

        // Spawn the IO task
        let io_task = tokio::spawn(io_loop(
            reader,
            writer,
            request_rx,
            state_doc,
            state_changed_tx,
            alive.clone(),
        ));

        Ok(Self {
            request_tx,
            alive,
            io_task,
            child,
        })
    }

    /// Send a request to the agent and wait for the response.
    async fn send_request(&self, request: AgentRequest) -> Result<AgentResponse> {
        if !self.alive.load(Ordering::Relaxed) {
            return Ok(AgentResponse::Error {
                error: "Agent is not running".to_string(),
            });
        }
        let (reply_tx, reply_rx) = oneshot::channel();
        self.request_tx
            .send((request, reply_tx))
            .await
            .map_err(|_| anyhow::anyhow!("Agent IO task closed"))?;
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Agent IO task dropped response"))
    }

    /// Launch a kernel in the agent subprocess.
    pub async fn launch_kernel(
        &self,
        kernel_type: &str,
        env_source: &str,
        notebook_path: Option<&str>,
        launched_config: LaunchedEnvConfig,
    ) -> Result<AgentResponse> {
        self.send_request(AgentRequest::LaunchKernel {
            kernel_type: kernel_type.to_string(),
            env_source: env_source.to_string(),
            notebook_path: notebook_path.map(String::from),
            launched_config,
            env_vars: Default::default(),
        })
        .await
    }

    /// Execute a cell in the agent's kernel.
    pub async fn execute_cell(
        &self,
        cell_id: &str,
        code: &str,
        execution_id: &str,
    ) -> Result<AgentResponse> {
        self.send_request(AgentRequest::ExecuteCell {
            cell_id: cell_id.to_string(),
            code: code.to_string(),
            execution_id: execution_id.to_string(),
        })
        .await
    }

    /// Interrupt the currently executing cell.
    pub async fn interrupt(&self) -> Result<AgentResponse> {
        self.send_request(AgentRequest::InterruptExecution).await
    }

    /// Shutdown the agent's kernel.
    pub async fn shutdown_kernel(&self) -> Result<AgentResponse> {
        self.send_request(AgentRequest::ShutdownKernel).await
    }

    /// Send a comm message to the agent's kernel.
    pub async fn send_comm(&self, message: serde_json::Value) -> Result<AgentResponse> {
        self.send_request(AgentRequest::SendComm { message }).await
    }

    /// Request code completions.
    pub async fn complete(&self, code: &str, cursor_pos: usize) -> Result<AgentResponse> {
        self.send_request(AgentRequest::Complete {
            code: code.to_string(),
            cursor_pos,
        })
        .await
    }

    /// Search kernel input history.
    pub async fn get_history(
        &self,
        pattern: Option<&str>,
        n: i32,
        unique: bool,
    ) -> Result<AgentResponse> {
        self.send_request(AgentRequest::GetHistory {
            pattern: pattern.map(String::from),
            n,
            unique,
        })
        .await
    }

    /// Check if the agent process is still alive.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Shutdown the agent process.
    pub async fn shutdown(&mut self) {
        // Try graceful shutdown first
        if self.is_alive() {
            let _ = self.shutdown_kernel().await;
        }
        // Kill the child process
        let _ = self.child.kill().await;
        self.alive.store(false, Ordering::Relaxed);
        // Abort the IO task
        self.io_task.abort();
    }
}

/// Main IO loop for the agent handle.
///
/// Reads frames from the agent's stdout, routes responses to pending
/// request waiters, applies RuntimeStateDoc sync, and handles notifications.
/// Also writes outgoing requests and RuntimeStateDoc sync to the agent's stdin.
async fn io_loop<R, W>(
    mut reader: R,
    mut writer: W,
    mut request_rx: mpsc::Receiver<(AgentRequest, oneshot::Sender<AgentResponse>)>,
    state_doc: Arc<RwLock<RuntimeStateDoc>>,
    state_changed_tx: broadcast::Sender<()>,
    alive: Arc<AtomicBool>,
) where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Pending response waiter — only one request at a time (sequential dispatch).
    let mut pending_reply: Option<oneshot::Sender<AgentResponse>> = None;

    // Automerge sync state for the agent peer
    let mut agent_sync_state = automerge::sync::State::new();

    // Subscribe to state_doc changes for reverse sync (coordinator → agent)
    let mut state_changed_rx = state_changed_tx.subscribe();

    info!("[agent-handle] IO loop started");
    loop {
        tokio::select! {
            // Read frames from agent stdout
            frame = recv_frame(&mut reader) => {
                match frame {
                    Ok(Some(data)) if !data.is_empty() => {
                        let frame_type = data[0];
                        let payload = &data[1..];

                        match frame_type {
                            // AgentResponse (0x02)
                            0x02 => {
                                if let Some(reply) = pending_reply.take() {
                                    match serde_json::from_slice::<AgentResponse>(payload) {
                                        Ok(response) => { let _ = reply.send(response); }
                                        Err(e) => {
                                            warn!("[agent-handle] Failed to parse response: {}", e);
                                            let _ = reply.send(AgentResponse::Error {
                                                error: format!("Failed to parse agent response: {}", e),
                                            });
                                        }
                                    }
                                } else {
                                    warn!("[agent-handle] Received response with no pending request");
                                }
                            }

                            // RuntimeStateSync (0x05)
                            0x05 => {
                                debug!("[agent-handle] Received RuntimeStateSync ({} bytes)", payload.len());
                                if let Ok(msg) = automerge::sync::Message::decode(payload) {
                                    let mut sd = state_doc.write().await;
                                    if let Ok(changed) = sd.receive_sync_message_with_changes(
                                        &mut agent_sync_state, msg,
                                    ) {
                                        if changed {
                                            debug!("[agent-handle] RuntimeStateDoc changed, notifying peers");
                                            let _ = state_changed_tx.send(());
                                        }
                                    }

                                    // Generate reply sync message if needed
                                    if let Some(reply_msg) = sd.generate_sync_message(&mut agent_sync_state) {
                                        let encoded = reply_msg.encode();
                                        if let Err(e) = send_typed_frame(
                                            &mut writer,
                                            NotebookFrameType::RuntimeStateSync,
                                            &encoded,
                                        ).await {
                                            warn!("[agent-handle] Failed to send sync reply: {}", e);
                                            break;
                                        }
                                    }
                                }
                            }

                            // AgentNotification (0x03)
                            0x03 => {
                                if let Ok(notification) = serde_json::from_slice::<AgentNotification>(payload) {
                                    match notification {
                                        AgentNotification::KernelDied => {
                                            warn!("[agent-handle] Agent reported kernel died");
                                            // State is already in RuntimeStateDoc via sync
                                        }
                                    }
                                }
                            }

                            _ => {
                                debug!("[agent-handle] Ignoring frame type 0x{:02x}", frame_type);
                            }
                        }
                    }
                    Ok(Some(_)) => {} // empty frame
                    Ok(None) => {
                        info!("[agent-handle] Agent stdout EOF — process exited");
                        break;
                    }
                    Err(e) => {
                        warn!("[agent-handle] Agent stdout read error: {}", e);
                        break;
                    }
                }
            }

            // Send requests to agent stdin (only when no pending reply)
            request = async {
                if pending_reply.is_some() {
                    // Block until the current request is answered
                    std::future::pending::<Option<(AgentRequest, oneshot::Sender<AgentResponse>)>>().await
                } else {
                    request_rx.recv().await
                }
            } => {
                match request {
                    Some((req, reply)) => {
                        let json = match serde_json::to_vec(&req) {
                            Ok(j) => j,
                            Err(e) => {
                                let _ = reply.send(AgentResponse::Error {
                                    error: format!("Failed to serialize request: {}", e),
                                });
                                continue;
                            }
                        };
                        if let Err(e) = send_typed_frame(
                            &mut writer,
                            NotebookFrameType::Request,
                            &json,
                        ).await {
                            warn!("[agent-handle] Failed to send request to agent: {}", e);
                            let _ = reply.send(AgentResponse::Error {
                                error: format!("Failed to send to agent: {}", e),
                            });
                            break;
                        }
                        pending_reply = Some(reply);
                    }
                    None => {
                        info!("[agent-handle] Request channel closed, shutting down IO");
                        break;
                    }
                }
            }

            // Forward coordinator RuntimeStateDoc changes to agent
            _ = state_changed_rx.recv() => {
                // Drain buffered notifications
                while state_changed_rx.try_recv().is_ok() {}

                let mut sd = state_doc.write().await;
                if let Some(msg) = sd.generate_sync_message(&mut agent_sync_state) {
                    let encoded = msg.encode();
                    if let Err(e) = send_typed_frame(
                        &mut writer,
                        NotebookFrameType::RuntimeStateSync,
                        &encoded,
                    ).await {
                        warn!("[agent-handle] Failed to send state sync to agent: {}", e);
                        break;
                    }
                }
            }
        }
    }

    alive.store(false, Ordering::Relaxed);
    info!("[agent-handle] IO loop exited");
}
