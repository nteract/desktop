//! Coordinator-side management of a runtime agent subprocess.
//!
//! `RuntimeAgentHandle` spawns a `runtimed runtime-agent` child process in its
//! own ownership group. The ownership record is persisted as a per-agent
//! manifest for orphan reaping. The handle monitors the child lifecycle and
//! tears down the ownership group on drop.

#[cfg(windows)]
use std::os::windows::io::RawHandle;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use crate::runtime_agent_manifest::RuntimeAgentManifest;
use crate::task_supervisor::spawn_supervised;

/// Handle to a running runtime agent subprocess.
///
/// The runtime agent connects back to the daemon socket as a peer. On drop,
/// tears down the agent's ownership group (agent + kernel).
pub struct RuntimeAgentHandle {
    alive: Arc<AtomicBool>,
    /// Runtime agent ID used as the durable manifest key.
    runtime_agent_id: String,
    /// Notebook ID for logging/context.
    notebook_id: String,
    /// Process group ID (== agent PID on Unix, since we use process_group(0)).
    #[cfg(unix)]
    pgid: Option<i32>,
    /// Windows Job Object with KILL_ON_JOB_CLOSE for this runtime agent.
    #[cfg(windows)]
    job: Option<crate::runtime_agent_manifest::WindowsJob>,
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

        #[cfg(unix)]
        let manifest = match pgid {
            Some(pgid) => {
                RuntimeAgentManifest::unix(runtime_agent_id.clone(), notebook_id.clone(), pgid)
            }
            None => {
                let _ = child.kill().await;
                anyhow::bail!("runtime agent spawned without a process id");
            }
        };

        #[cfg(windows)]
        let (manifest, job) = {
            let pid = match child.id() {
                Some(pid) => pid,
                None => {
                    let _ = child.kill().await;
                    anyhow::bail!("runtime agent spawned without a process id");
                }
            };
            let process_handle = match child.raw_handle() {
                Some(handle) => handle as RawHandle,
                None => {
                    let _ = child.kill().await;
                    anyhow::bail!("runtime agent spawned without a process handle");
                }
            };
            let (job_name, job) = crate::runtime_agent_manifest::create_windows_job_for_process(
                &runtime_agent_id,
                process_handle,
            )
            .inspect_err(|_| {
                let _ = child.start_kill();
            })?;
            (
                RuntimeAgentManifest::windows(
                    runtime_agent_id.clone(),
                    notebook_id.clone(),
                    pid,
                    job_name,
                ),
                job,
            )
        };

        if let Err(e) = crate::runtime_agent_manifest::write_manifest(&manifest) {
            let _ = child.kill().await;
            return Err(e.context("failed to persist runtime-agent ownership manifest"));
        }

        info!(
            "[runtime-agent-handle] Runtime agent spawned (pid={:?}, notebook_id={}, runtime_agent_id={})",
            child.id(),
            notebook_id,
            runtime_agent_id,
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
            runtime_agent_id,
            notebook_id,
            #[cfg(unix)]
            pgid,
            #[cfg(windows)]
            job: Some(job),
        })
    }

    /// Check if the runtime agent process is still running.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }
}

impl Drop for RuntimeAgentHandle {
    fn drop(&mut self) {
        #[cfg(not(unix))]
        let remove_manifest = true;

        // SIGKILL the entire process group (agent + kernel)
        #[cfg(unix)]
        let remove_manifest = {
            let mut remove_manifest = true;
            if let Some(pgid) = self.pgid.take() {
                use crate::runtime_agent_manifest::CleanupDecision;
                use nix::sys::signal::{killpg, Signal};
                use nix::unistd::Pid;

                let decision = if pgid > 0 {
                    crate::runtime_agent_manifest::unix_cleanup_decision(
                        &self.runtime_agent_id,
                        pgid,
                        killpg(Pid::from_raw(pgid), Signal::SIGKILL),
                    )
                } else {
                    CleanupDecision::RetainForRetry
                };
                remove_manifest =
                    matches!(decision, CleanupDecision::Reaped | CleanupDecision::Missing);
            }
            remove_manifest
        };

        #[cfg(windows)]
        {
            self.job.take();
        }

        if remove_manifest {
            crate::runtime_agent_manifest::remove_manifest(&self.runtime_agent_id);
        }

        info!(
            "[runtime-agent-handle] RuntimeAgentHandle dropped for notebook {} runtime_agent {}",
            self.notebook_id, self.runtime_agent_id
        );
    }
}
