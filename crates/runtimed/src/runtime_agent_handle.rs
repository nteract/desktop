//! Coordinator-side management of a runtime agent subprocess.
//!
//! `RuntimeAgentHandle` spawns a `runtimed runtime-agent` child process that
//! connects back to the daemon's Unix socket as a regular peer. The handle
//! only monitors the child process lifecycle — all communication happens via
//! the socket.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use log::{info, warn};

/// Handle to a running runtime agent subprocess.
///
/// The runtime agent connects back to the daemon's Unix socket as a peer.
/// This handle only spawns and monitors the child process — it does
/// not manage any communication channels.
pub struct RuntimeAgentHandle {
    alive: Arc<AtomicBool>,
}

impl RuntimeAgentHandle {
    /// Spawn a runtime agent subprocess that will connect back to the daemon socket.
    ///
    /// The runtime agent is given the socket path, notebook ID, runtime agent ID,
    /// and blob root as CLI arguments. It connects to the daemon socket and joins
    /// the notebook room as a `RuntimeAgent` peer.
    pub async fn spawn(
        notebook_id: String,
        runtime_agent_id: String,
        blob_root: PathBuf,
        socket_path: PathBuf,
    ) -> Result<Self> {
        let exe = std::env::current_exe()?;
        info!(
            "[runtime-agent-handle] Spawning runtime agent: {} runtime-agent --notebook-id {} (socket: {})",
            exe.display(),
            notebook_id,
            socket_path.display(),
        );

        let mut child = tokio::process::Command::new(&exe)
            .arg("runtime-agent")
            .arg("--notebook-id")
            .arg(&notebook_id)
            .arg("--runtime-agent-id")
            .arg(&runtime_agent_id)
            .arg("--blob-root")
            .arg(blob_root.as_os_str())
            .arg("--socket")
            .arg(socket_path.as_os_str())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;

        info!(
            "[runtime-agent-handle] Runtime agent spawned (pid={:?})",
            child.id()
        );

        let alive = Arc::new(AtomicBool::new(true));

        // Monitor child process exit
        let alive_clone = alive.clone();
        let runtime_agent_id_clone = runtime_agent_id.clone();
        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => {
                    info!(
                        "[runtime-agent-handle] Runtime agent {} exited with status: {}",
                        runtime_agent_id_clone, status
                    );
                }
                Err(e) => {
                    warn!(
                        "[runtime-agent-handle] Runtime agent {} wait error: {}",
                        runtime_agent_id_clone, e
                    );
                }
            }
            alive_clone.store(false, Ordering::Relaxed);
        });

        Ok(Self { alive })
    }

    /// Check if the runtime agent process is still running.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}
