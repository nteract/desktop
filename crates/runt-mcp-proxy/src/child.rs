//! Child process spawning and MCP client identity forwarding.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::process::Stdio;

use rmcp::model::Implementation;
use rmcp::service::{RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::{async_rw::AsyncRwTransport, Transport};
use rmcp::{ClientHandler, ServiceExt};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio::sync::oneshot;
use tracing::{info, warn};

/// rmcp client role type alias.
pub type RoleChild = rmcp::service::RoleClient;

/// Client handler that presents the upstream MCP client's identity to the
/// child process, so the child sees the real client name (e.g. "Claude Code")
/// instead of the proxy's name.
#[derive(Clone)]
pub struct ChildClientHandler {
    pub upstream_name: String,
    pub upstream_title: Option<String>,
}

impl ClientHandler for ChildClientHandler {
    fn get_info(&self) -> rmcp::model::ClientInfo {
        let mut impl_info = Implementation::new(&self.upstream_name, env!("CARGO_PKG_VERSION"));
        impl_info.title = self.upstream_title.clone();
        rmcp::model::ClientInfo::new(Default::default(), impl_info)
    }
}

/// A running child service.
pub type RunningChild = rmcp::service::RunningService<RoleChild, ChildClientHandler>;

/// Transport for a child MCP process whose lifecycle is owned by this proxy.
struct ManagedChildTransport {
    inner: AsyncRwTransport<RoleChild, ChildStdout, ChildStdin>,
    shutdown_tx: Option<oneshot::Sender<()>>,
}

impl Drop for ManagedChildTransport {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
    }
}

impl Transport<RoleChild> for ManagedChildTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleChild>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        self.inner.send(item)
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleChild>>> + Send {
        self.inner.receive()
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        let close = self.inner.close();
        let shutdown_tx = self.shutdown_tx.take();
        async move {
            let result = close.await;
            if let Some(tx) = shutdown_tx {
                let _ = tx.send(());
            }
            result
        }
    }
}

async fn wait_for_child(mut child: Child, mut shutdown_rx: oneshot::Receiver<()>) {
    let child_id = child.id();

    tokio::select! {
        status = child.wait() => {
            match status {
                Ok(status) => info!(pid = ?child_id, %status, "Child process reaped"),
                Err(e) => warn!(pid = ?child_id, error = %e, "Failed to wait for child process"),
            }
        }
        _ = &mut shutdown_rx => {
            if let Err(e) = child.start_kill() {
                warn!(pid = ?child_id, error = %e, "Failed to request child process shutdown");
            }

            match child.wait().await {
                Ok(status) => info!(pid = ?child_id, %status, "Child process stopped and reaped"),
                Err(e) => warn!(pid = ?child_id, error = %e, "Failed to reap child process after shutdown"),
            }
        }
    }
}

fn spawn_managed_transport(
    command: &Path,
    args: &[String],
    env: &HashMap<String, String>,
) -> std::io::Result<ManagedChildTransport> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());

    let mut child = cmd.spawn()?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("child stdin was not piped"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("child stdout was not piped"))?;

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    tokio::spawn(wait_for_child(child, shutdown_rx));

    Ok(ManagedChildTransport {
        inner: AsyncRwTransport::new(stdout, stdin),
        shutdown_tx: Some(shutdown_tx),
    })
}

/// Spawn `runt mcp` (or similar) as a child process and return an rmcp client.
///
/// The upstream client's `name` and `title` are forwarded through the MCP
/// initialize handshake so the child's peer label sniffing picks up the
/// real client identity.
pub async fn spawn_child(
    command: &Path,
    args: &[String],
    env: &HashMap<String, String>,
    upstream_name: &str,
    upstream_title: Option<&str>,
) -> Result<RunningChild, String> {
    if !command.exists() {
        return Err(format!("Child binary not found at {}", command.display()));
    }

    info!(
        "Spawning child: {} {} (client={upstream_name:?})",
        command.display(),
        args.join(" ")
    );

    let transport = spawn_managed_transport(command, args, env)
        .map_err(|e| format!("Failed to spawn child process: {e}"))?;

    let handler = ChildClientHandler {
        upstream_name: upstream_name.to_string(),
        upstream_title: upstream_title.map(|s| s.to_string()),
    };

    let client = handler
        .serve(transport)
        .await
        .map_err(|e| format!("Failed to initialize child MCP client: {e}"))?;

    Ok(client)
}

#[cfg(test)]
mod tests {
    //! Regression test for the post-`exit(75)` respawn gap.
    //!
    //! `RunningService::is_closed()` in rmcp 1.4 returns true only when the
    //! handle has been consumed by `.waiting()` OR the cancellation token
    //! has been cancelled — neither happens when the child process exits on
    //! its own. `is_transport_closed()` (via `Peer::is_transport_closed`)
    //! *does* flip when the service loop breaks because the transport
    //! reader returned None.
    //!
    //! This matters for `proxy.rs`: the monitor loop and the
    //! `forward_tool_call` fallback use this check to decide whether the
    //! child is gone and a restart is needed. Using `is_closed()` wedges
    //! the proxy after a daemon-upgrade exit(75) — the child is dead but
    //! the proxy never respawns it. See the lab finding on PR #2004.
    use super::*;
    use rmcp::{ServerHandler, ServiceExt};
    use std::time::{Duration, Instant};

    /// Minimal `ServerHandler` — all methods use rmcp's defaults.
    struct NoopServer;
    impl ServerHandler for NoopServer {}

    /// Bring up a `RunningChild` over an in-memory duplex pipe, then drop
    /// the server side. The client's service loop should observe EOF on
    /// read and break out.
    async fn dead_transport_child() -> RunningChild {
        let (server_side, client_side) = tokio::io::duplex(1024);

        // Server handshake + hold the transport alive until we drop it.
        let server_handle = tokio::spawn(async move {
            let server = NoopServer.serve(server_side).await.expect("serve server");
            // Park until the client disconnects.
            let _ = server.waiting().await;
        });

        let handler = ChildClientHandler {
            upstream_name: "test".to_string(),
            upstream_title: None,
        };
        let client = handler
            .serve(client_side)
            .await
            .expect("client handshake completes");

        // Kill the server. Its service task ends, stdout half of the duplex
        // closes, the client's `transport.receive()` returns None, and the
        // client service loop breaks with `QuitReason::Closed`.
        server_handle.abort();

        client
    }

    /// Wait until `cond` returns true or `deadline` elapses.
    async fn wait_for(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if cond() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        cond()
    }

    fn instant_exit_command() -> Command {
        if cfg!(target_os = "windows") {
            let mut cmd = Command::new("cmd");
            cmd.args(["/C", "exit 0"]);
            cmd
        } else {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "true"]);
            cmd
        }
    }

    fn long_running_command() -> Command {
        if cfg!(target_os = "windows") {
            let mut cmd = Command::new("cmd");
            cmd.args(["/C", "ping -n 30 127.0.0.1 >NUL"]);
            cmd
        } else {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "sleep 30"]);
            cmd
        }
    }

    #[tokio::test]
    async fn lifecycle_task_reaps_natural_child_exit() {
        let mut cmd = instant_exit_command();
        let child = cmd.spawn().expect("spawn child");
        let (_shutdown_tx, shutdown_rx) = oneshot::channel();

        tokio::time::timeout(Duration::from_secs(5), wait_for_child(child, shutdown_rx))
            .await
            .expect("child wait should complete");
    }

    #[tokio::test]
    async fn lifecycle_task_kills_and_reaps_on_shutdown() {
        let mut cmd = long_running_command();
        let child = cmd.spawn().expect("spawn child");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let task = tokio::spawn(wait_for_child(child, shutdown_rx));
        shutdown_tx.send(()).expect("send shutdown");

        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("shutdown should complete")
            .expect("lifecycle task should not panic");
    }

    /// Regression test for the original zombie scenario: the child has
    /// already exited when the shutdown signal arrives. `start_kill()`
    /// fails with ESRCH (no such process) on Unix, but `wait()` must
    /// still succeed and reap the zombie.
    #[tokio::test]
    async fn shutdown_after_child_already_exited() {
        let mut cmd = instant_exit_command();
        let child = cmd.spawn().expect("spawn child");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        // Wait for the child to exit naturally first.
        let pid = child.id();
        let task = tokio::spawn(wait_for_child(child, shutdown_rx));

        // Give the instant-exit child time to terminate before we signal.
        tokio::time::sleep(Duration::from_millis(200)).await;

        // Now signal shutdown. The child is already dead — `start_kill()`
        // will hit ESRCH (or equivalent), but `wait()` should still reap.
        // If the shutdown signal arrives after wait_for_child already
        // reaped the child via the `child.wait()` branch, that's fine too —
        // the oneshot is simply dropped.
        let _ = shutdown_tx.send(());

        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("lifecycle task should complete even with late shutdown signal")
            .expect("lifecycle task should not panic");

        // Verify no zombie remains for this PID. On Linux we can check
        // /proc/<pid>/stat; on other platforms we just trust the wait().
        #[cfg(target_os = "linux")]
        if let Some(pid) = pid {
            let stat_path = format!("/proc/{pid}/stat");
            assert!(
                !std::path::Path::new(&stat_path).exists(),
                "PID {pid} should have been reaped (no /proc entry)"
            );
        }
        let _ = pid; // suppress unused warning on non-linux
    }

    /// Child exits with a non-zero status (like exit code 75 from a daemon
    /// upgrade). The lifecycle task must still reap it without panicking.
    #[tokio::test]
    async fn lifecycle_task_reaps_nonzero_exit() {
        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", "exit 75"]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "exit 75"]);
            c
        };
        let child = cmd.spawn().expect("spawn child");
        let (_shutdown_tx, shutdown_rx) = oneshot::channel();

        tokio::time::timeout(Duration::from_secs(5), wait_for_child(child, shutdown_rx))
            .await
            .expect("child with exit(75) should be reaped");
    }

    /// Rapid spawn/kill cycles must not leak tasks or panic. This exercises
    /// the transport's Drop impl (which sends the oneshot) under churn.
    #[tokio::test]
    async fn rapid_spawn_kill_cycles_do_not_leak() {
        for _ in 0..10 {
            let mut cmd = long_running_command();
            let child = cmd.spawn().expect("spawn child");
            let (shutdown_tx, shutdown_rx) = oneshot::channel();

            let task = tokio::spawn(wait_for_child(child, shutdown_rx));
            // Immediately kill
            shutdown_tx.send(()).expect("send shutdown");

            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("rapid cycle should not hang")
                .expect("rapid cycle should not panic");
        }
    }

    /// When the oneshot sender is dropped (simulating transport Drop without
    /// explicit close), the receiver sees a closed channel. The lifecycle
    /// task must handle this the same as an explicit shutdown signal.
    #[tokio::test]
    async fn dropped_sender_triggers_shutdown_path() {
        let mut cmd = long_running_command();
        let child = cmd.spawn().expect("spawn child");
        let (shutdown_tx, shutdown_rx) = oneshot::channel();

        let task = tokio::spawn(wait_for_child(child, shutdown_rx));

        // Drop the sender instead of calling send() — this is what happens
        // when ManagedChildTransport::drop fires.
        drop(shutdown_tx);

        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("dropped sender should still clean up the child")
            .expect("lifecycle task should not panic on dropped sender");
    }

    #[tokio::test]
    async fn is_transport_closed_flips_when_child_exits() {
        let client = dead_transport_child().await;
        let closed = wait_for(Duration::from_secs(2), || client.is_transport_closed()).await;
        assert!(
            closed,
            "is_transport_closed() must flip to true when the child's stdio closes"
        );
    }

    #[tokio::test]
    async fn is_closed_does_not_flip_on_child_exit() {
        // Documents the rmcp 1.4 behavior that motivated the swap: even
        // after the transport dies, `is_closed()` remains false because
        // the `handle` Option still holds the (now-finished) JoinHandle
        // and nobody has cancelled the token. If this ever changes
        // upstream we can drop the workaround — and this test will fail
        // loudly to tell us.
        let client = dead_transport_child().await;
        // Give the loop ample time to tear down.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            client.is_transport_closed(),
            "precondition: transport should be closed by now"
        );
        assert!(
            !client.is_closed(),
            "rmcp 1.4's is_closed() is expected to stay false; the proxy must not rely on it"
        );
    }
}
