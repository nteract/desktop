//! Coordinator-side management of a runtime agent subprocess.
//!
//! `AgentHandle` spawns a `runtimed agent` child process and communicates
//! via framed protocol on stdin/stdout. A reader task reads all frames
//! from the agent's stdout and routes them via channels.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use log::{debug, info, warn};
use notebook_doc::runtime_state::RuntimeStateDoc;
use notebook_protocol::connection::{
    recv_frame, recv_preamble, send_json_frame, send_preamble, send_typed_frame, Handshake,
    NotebookFrameType,
};
use notebook_protocol::protocol::{AgentNotification, AgentRequest, AgentResponse, LaunchedEnvConfig};
use tokio::io::AsyncWriteExt;
use tokio::process::ChildStdin;
use tokio::sync::{broadcast, mpsc, oneshot, Mutex, RwLock};

/// Handle to a running agent subprocess.
pub struct AgentHandle {
    /// Writer to agent's stdin (protected by mutex for concurrent access)
    writer: Arc<Mutex<ChildStdin>>,
    /// Channel for receiving responses from the reader task
    response_rx: mpsc::Receiver<AgentResponse>,
    /// Channel for sending response requests to the reader task
    response_request_tx: mpsc::Sender<oneshot::Sender<AgentResponse>>,
    alive: Arc<AtomicBool>,
}

impl AgentHandle {
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

        let writer = child
            .stdin
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture agent stdin"))?;
        let mut reader = child
            .stdout
            .take()
            .ok_or_else(|| anyhow::anyhow!("Failed to capture agent stdout"))?;

        let writer = Arc::new(Mutex::new(writer));

        // Preamble + handshake exchange
        {
            let mut w = writer.lock().await;
            send_preamble(&mut *w).await?;
            send_json_frame(
                &mut *w,
                &Handshake::RuntimeAgent {
                    notebook_id,
                    agent_id,
                    blob_root: blob_root.to_string_lossy().to_string(),
                },
            )
            .await?;
        }
        recv_preamble(&mut reader).await?;

        // Read the agent's initial RuntimeStateDoc sync
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
                    "[agent-handle] Applied initial RuntimeStateDoc sync ({} bytes)",
                    data.len()
                );
            }
            _ => {
                anyhow::bail!("Expected RuntimeStateSync from agent");
            }
        }

        let alive = Arc::new(AtomicBool::new(true));
        let (response_request_tx, mut response_request_rx) =
            mpsc::channel::<oneshot::Sender<AgentResponse>>(8);
        let (response_tx, response_rx) = mpsc::channel::<AgentResponse>(8);

        // Spawn reader task — reads ALL frames from agent stdout.
        // This task owns `reader` and never gives it up. Frames are routed
        // via channels. This works because the task is spawned from the same
        // async context where `reader` was created.
        let alive_clone = alive.clone();
        let state_doc_clone = state_doc.clone();
        let state_changed_clone = state_changed_tx.clone();
        let writer_clone = writer.clone();
        tokio::spawn(async move {
            let mut agent_sync_state = automerge::sync::State::new();
            let mut pending_reply: Option<oneshot::Sender<AgentResponse>> = None;

            // Also wait on the child process
            let mut child_wait = Box::pin(child.wait());

            loop {
                tokio::select! {
                    biased;

                    // Read frames from agent stdout
                    frame = recv_frame(&mut reader) => {
                        match frame {
                            Ok(Some(data)) if !data.is_empty() => {
                                let frame_type = data[0];
                                let payload = &data[1..];

                                match frame_type {
                                    // AgentResponse
                                    0x02 => {
                                        if let Ok(response) = serde_json::from_slice::<AgentResponse>(payload) {
                                            if let Some(reply) = pending_reply.take() {
                                                let _ = reply.send(response);
                                            } else {
                                                let _ = response_tx.send(response).await;
                                            }
                                        }
                                    }
                                    // RuntimeStateSync
                                    0x05 => {
                                        if let Ok(msg) = automerge::sync::Message::decode(payload) {
                                            let mut sd = state_doc_clone.write().await;
                                            if let Ok(changed) = sd.receive_sync_message_with_changes(
                                                &mut agent_sync_state, msg,
                                            ) {
                                                if changed {
                                                    let _ = state_changed_clone.send(());
                                                }
                                            }
                                            // Send sync reply
                                            if let Some(reply_msg) = sd.generate_sync_message(&mut agent_sync_state) {
                                                let encoded = reply_msg.encode();
                                                let mut w = writer_clone.lock().await;
                                                let _ = send_typed_frame(&mut *w, NotebookFrameType::RuntimeStateSync, &encoded).await;
                                            }
                                        }
                                    }
                                    // AgentNotification
                                    0x03 => {
                                        if let Ok(AgentNotification::KernelDied) = serde_json::from_slice(payload) {
                                            warn!("[agent-handle] Agent kernel died");
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            _ => {
                                info!("[agent-handle] Agent stdout closed");
                                break;
                            }
                        }
                    }

                    // Accept response request from send_request callers
                    req = response_request_rx.recv() => {
                        match req {
                            Some(reply_tx) => {
                                pending_reply = Some(reply_tx);
                            }
                            None => break,
                        }
                    }

                    // Wait for child exit
                    _ = &mut child_wait => {
                        info!("[agent-handle] Agent process exited");
                        break;
                    }
                }
            }

            alive_clone.store(false, Ordering::Relaxed);
        });

        info!("[agent-handle] Agent spawned — reader task started");

        Ok(Self {
            writer,
            response_rx,
            response_request_tx,
            alive,
        })
    }

    async fn send_request(&mut self, request: AgentRequest) -> Result<AgentResponse> {
        if !self.alive.load(Ordering::Relaxed) {
            return Ok(AgentResponse::Error {
                error: "Agent is not running".to_string(),
            });
        }

        // Register for the response BEFORE sending the request
        let (reply_tx, reply_rx) = oneshot::channel();
        self.response_request_tx
            .send(reply_tx)
            .await
            .map_err(|_| anyhow::anyhow!("Reader task closed"))?;

        // Send request
        let json = serde_json::to_vec(&request)?;
        {
            let mut w = self.writer.lock().await;
            send_typed_frame(&mut *w, NotebookFrameType::Request, &json).await?;
        }

        // Wait for response
        reply_rx
            .await
            .map_err(|_| anyhow::anyhow!("Reader task dropped response"))
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
        self.alive.store(false, Ordering::Relaxed);
    }
}
