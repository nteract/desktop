//! Coordinator-side management of a runtime agent subprocess.
//!
//! `RuntimeAgentHandle` spawns a `runtimed runtime-agent` child process in its
//! own process group. The PGID is registered in `agents.json` for orphan
//! reaping. The handle monitors the child lifecycle and sends SIGKILL to the
//! entire process group on drop.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use crate::task_supervisor::spawn_supervised;

/// Handle to a running runtime agent subprocess.
///
/// The runtime agent connects back to the daemon's Unix socket as a peer.
/// On drop, sends SIGKILL to the agent's process group (agent + kernel).
pub struct RuntimeAgentHandle {
    alive: Arc<AtomicBool>,
    /// Notebook ID used as the registry key for orphan tracking.
    notebook_id: String,
    /// Process group ID (== agent PID on Unix, since we use process_group(0)).
    #[cfg(unix)]
    pgid: Option<i32>,
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

        let mut cmd = tokio::process::Command::new(&exe);
        cmd.arg("runtime-agent")
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
            .kill_on_drop(true);

        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd.spawn()?;

        #[cfg(unix)]
        let pgid = child.id().map(|pid| pid as i32);

        // Register PGID for orphan reaping on unclean daemon exit
        #[cfg(unix)]
        if let Some(pgid) = pgid {
            crate::process_groups::register_agent(&notebook_id, pgid);
        }

        info!(
            "[runtime-agent-handle] Runtime agent spawned (pid={:?}, notebook_id={})",
            child.id(),
            notebook_id,
        );

        let alive = Arc::new(AtomicBool::new(true));

        // Monitor child process exit
        let alive_clone = alive.clone();
        let panic_alive = alive.clone();
        let runtime_agent_id_clone = runtime_agent_id.clone();
        spawn_supervised(
            "runtime-agent-watcher",
            async move {
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
            },
            move |_| {
                panic_alive.store(false, Ordering::Relaxed);
            },
        );

        Ok(Self {
            alive,
            notebook_id,
            #[cfg(unix)]
            pgid,
        })
    }

    /// Check if the runtime agent process is still running.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Unregister this agent from the process group registry.
    ///
    /// Called during graceful shutdown and room eviction, before
    /// the handle is dropped.
    pub fn unregister(&self) {
        crate::process_groups::unregister_agent(&self.notebook_id);
    }
}

impl Drop for RuntimeAgentHandle {
    fn drop(&mut self) {
        // SIGKILL the entire process group (agent + kernel)
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            if pgid > 0 {
                use nix::sys::signal::{killpg, Signal};
                use nix::unistd::Pid;
                let _ = killpg(Pid::from_raw(pgid), Signal::SIGKILL);
            }
        }

        info!(
            "[runtime-agent-handle] RuntimeAgentHandle dropped for notebook {}",
            self.notebook_id
        );
    }
}
