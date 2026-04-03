//! Coordinator-side management of a runtime agent subprocess.
//!
//! `AgentHandle` spawns a `runtimed agent` child process, pipes the framed
//! protocol on its stdin/stdout, and provides typed request methods.
//!
//! RuntimeStateDoc sync happens inline: after each request/response exchange,
//! the handle drains any pending sync frames from the agent before returning.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use log::{debug, info, warn};
use notebook_doc::runtime_state::RuntimeStateDoc;
use notebook_protocol::connection::{
    recv_frame, recv_json_frame, recv_preamble, send_json_frame, send_preamble, send_typed_frame,
    Handshake, NotebookFrameType,
};
use notebook_protocol::protocol::{AgentNotification, AgentRequest, AgentResponse, LaunchedEnvConfig};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::sync::{broadcast, RwLock};

/// Handle to a running agent subprocess.
///
/// Owns the child process and its stdin/stdout. Request methods write to
/// stdin and read from stdout synchronously (no background IO task).
pub struct AgentHandle {
    reader: ChildStdout,
    writer: ChildStdin,
    child: Child,
    alive: Arc<AtomicBool>,
    state_doc: Arc<RwLock<RuntimeStateDoc>>,
    state_changed_tx: broadcast::Sender<()>,
    /// Automerge sync state for the agent peer
    agent_sync_state: automerge::sync::State,
}

impl AgentHandle {
    /// Spawn a new agent subprocess.
    pub async fn spawn(
        notebook_id: String,
        agent_id: String,
        blob_root: PathBuf,
        state_doc: Arc<RwLock<RuntimeStateDoc>>,
        state_changed_tx: broadcast::Sender<()>,
        _broadcast_tx: broadcast::Sender<notebook_protocol::protocol::NotebookBroadcast>,
    ) -> Result<Self> {
        let exe = std::env::current_exe()?;
        info!(
            "[agent-handle] Spawning agent: {} agent (notebook_id={})",
            exe.display(),
            notebook_id
        );

        let mut child = tokio::process::Command::new(&exe)
            .arg("agent")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;

        let mut writer = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture agent stdin"))?;
        let mut reader = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture agent stdout"))?;

        // Preamble + handshake exchange
        send_preamble(&mut writer).await?;
        send_json_frame(
            &mut writer,
            &Handshake::RuntimeAgent {
                notebook_id,
                agent_id,
                blob_root: blob_root.to_string_lossy().to_string(),
            },
        )
        .await?;
        recv_preamble(&mut reader).await?;

        // Read the agent's initial RuntimeStateDoc sync frame.
        // The agent sends this immediately after preamble — it contains the
        // full scaffolded schema. We apply it to the coordinator's state_doc.
        info!("[agent-handle] Waiting for agent's initial sync...");
        match recv_frame(&mut reader).await? {
            Some(data) if !data.is_empty() && data[0] == 0x05 => {
                let payload = &data[1..];
                let msg = automerge::sync::Message::decode(payload)?;
                let mut sd = state_doc.write().await;
                let mut sync_state = automerge::sync::State::new();
                sd.receive_sync_message_with_changes(&mut sync_state, msg)?;
                let _ = state_changed_tx.send(());
                info!(
                    "[agent-handle] Applied agent's initial RuntimeStateDoc sync ({} bytes)",
                    data.len()
                );
            }
            Some(data) => {
                warn!(
                    "[agent-handle] Expected RuntimeStateSync (0x05), got 0x{:02x}",
                    data.first().copied().unwrap_or(0)
                );
            }
            None => {
                anyhow::bail!("Agent EOF before initial sync");
            }
        }

        info!("[agent-handle] Agent spawned — RuntimeStateDoc bootstrapped");

        Ok(Self {
            reader,
            writer,
            child,
            alive: Arc::new(AtomicBool::new(true)),
            state_doc,
            state_changed_tx,
            agent_sync_state: automerge::sync::State::new(),
        })
    }

    /// Send a request and wait for the response, draining sync frames.
    async fn send_request(&mut self, request: AgentRequest) -> Result<AgentResponse> {
        if !self.alive.load(Ordering::Relaxed) {
            return Ok(AgentResponse::Error {
                error: "Agent is not running".to_string(),
            });
        }

        // Send request
        let json = serde_json::to_vec(&request)?;
        send_typed_frame(&mut self.writer, NotebookFrameType::Request, &json).await?;

        // Read frames until we get the response, processing sync frames along the way
        loop {
            match recv_frame(&mut self.reader).await {
                Ok(Some(data)) if !data.is_empty() => {
                    let frame_type = data[0];
                    let payload = &data[1..];

                    match frame_type {
                        // AgentResponse
                        0x02 => {
                            let response: AgentResponse = serde_json::from_slice(payload)?;
                            return Ok(response);
                        }
                        // RuntimeStateSync — process inline
                        0x05 => {
                            self.apply_sync_frame(payload).await;
                        }
                        // AgentNotification
                        0x03 => {
                            if let Ok(notification) =
                                serde_json::from_slice::<AgentNotification>(payload)
                            {
                                match notification {
                                    AgentNotification::KernelDied => {
                                        warn!("[agent-handle] Agent kernel died");
                                    }
                                }
                            }
                        }
                        _ => {
                            debug!(
                                "[agent-handle] Ignoring frame type 0x{:02x}",
                                frame_type
                            );
                        }
                    }
                }
                Ok(Some(_)) => {} // empty frame
                Ok(None) => {
                    self.alive.store(false, Ordering::Relaxed);
                    anyhow::bail!("Agent exited while waiting for response");
                }
                Err(e) => {
                    self.alive.store(false, Ordering::Relaxed);
                    anyhow::bail!("Agent read error: {}", e);
                }
            }
        }
    }

    /// Apply a RuntimeStateSync frame from the agent.
    async fn apply_sync_frame(&mut self, payload: &[u8]) {
        if let Ok(msg) = automerge::sync::Message::decode(payload) {
            let mut sd = self.state_doc.write().await;
            if let Ok(changed) =
                sd.receive_sync_message_with_changes(&mut self.agent_sync_state, msg)
            {
                if changed {
                    debug!("[agent-handle] RuntimeStateDoc changed, notifying peers");
                    let _ = self.state_changed_tx.send(());
                }
            }

            // Send sync reply if needed
            if let Some(reply) = sd.generate_sync_message(&mut self.agent_sync_state) {
                let encoded = reply.encode();
                if let Err(e) =
                    send_typed_frame(&mut self.writer, NotebookFrameType::RuntimeStateSync, &encoded)
                        .await
                {
                    warn!("[agent-handle] Failed to send sync reply: {}", e);
                }
            }
        }
    }

    /// Drain any pending sync frames from the agent (non-blocking).
    /// Call after operations that trigger agent-side state changes.
    pub async fn drain_sync(&mut self) {
        // TODO: non-blocking read of any pending frames
        // For now, sync happens inline during send_request
    }

    pub async fn launch_kernel(
        &mut self,
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

    pub async fn execute_cell(
        &mut self,
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

    pub async fn interrupt(&mut self) -> Result<AgentResponse> {
        self.send_request(AgentRequest::InterruptExecution).await
    }

    pub async fn shutdown_kernel(&mut self) -> Result<AgentResponse> {
        self.send_request(AgentRequest::ShutdownKernel).await
    }

    pub async fn send_comm(&mut self, message: serde_json::Value) -> Result<AgentResponse> {
        self.send_request(AgentRequest::SendComm { message }).await
    }

    pub async fn complete(&mut self, code: &str, cursor_pos: usize) -> Result<AgentResponse> {
        self.send_request(AgentRequest::Complete {
            code: code.to_string(),
            cursor_pos,
        })
        .await
    }

    pub async fn get_history(
        &mut self,
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

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    pub async fn shutdown(&mut self) {
        if self.is_alive() {
            let _ = self.shutdown_kernel().await;
        }
        let _ = self.child.kill().await;
        self.alive.store(false, Ordering::Relaxed);
    }
}
