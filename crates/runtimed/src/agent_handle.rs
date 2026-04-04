//! Coordinator-side management of a runtime agent subprocess.
//!
//! `AgentHandle` spawns a `runtimed agent` child process that connects back
//! to the daemon's Unix socket as a regular peer. The handle only monitors
//! the child process lifecycle — all communication happens via the socket.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use log::{info, warn};

/// Handle to a running agent subprocess.
///
/// The agent connects back to the daemon's Unix socket as a peer.
/// This handle only spawns and monitors the child process — it does
/// not manage any communication channels.
pub struct AgentHandle {
    alive: Arc<AtomicBool>,
}

impl AgentHandle {
    /// Spawn an agent subprocess that will connect back to the daemon socket.
    ///
    /// The agent is given the socket path, notebook ID, agent ID, and blob root
    /// as CLI arguments. It connects to the daemon socket and joins the notebook
    /// room as a `RuntimeAgent` peer.
    pub async fn spawn(
        notebook_id: String,
        agent_id: String,
        blob_root: PathBuf,
        socket_path: PathBuf,
    ) -> Result<Self> {
        let exe = std::env::current_exe()?;
        info!(
            "[agent-handle] Spawning agent: {} agent --notebook-id {} (socket: {})",
            exe.display(),
            notebook_id,
            socket_path.display(),
        );

        let mut child = tokio::process::Command::new(&exe)
            .arg("agent")
            .arg("--notebook-id")
            .arg(&notebook_id)
            .arg("--agent-id")
            .arg(&agent_id)
            .arg("--blob-root")
            .arg(blob_root.as_os_str())
            .arg("--socket")
            .arg(socket_path.as_os_str())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;

        info!("[agent-handle] Agent spawned (pid={:?})", child.id());

        let alive = Arc::new(AtomicBool::new(true));

        // Monitor child process exit
        let alive_clone = alive.clone();
        let agent_id_clone = agent_id.clone();
        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => {
                    info!(
                        "[agent-handle] Agent {} exited with status: {}",
                        agent_id_clone, status
                    );
                }
                Err(e) => {
                    warn!("[agent-handle] Agent {} wait error: {}", agent_id_clone, e);
                }
            }
            alive_clone.store(false, Ordering::Relaxed);
        });

        Ok(Self { alive })
    }

    /// Check if the agent process is still running.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}
