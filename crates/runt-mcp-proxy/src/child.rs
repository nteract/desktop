//! Child process spawning and MCP client identity forwarding.

use std::collections::HashMap;
use std::path::Path;

use rmcp::model::Implementation;
use rmcp::transport::async_rw::AsyncRwTransport;
use rmcp::{ClientHandler, ServiceExt};
use tokio::process::Command;
use tracing::info;

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

/// Spawn `runt mcp` (or similar) as a child process and return an rmcp client.
///
/// The upstream client's `name` and `title` are forwarded through the MCP
/// initialize handshake so the child's peer label sniffing picks up the
/// real client identity.
///
/// We spawn the process ourselves (not via rmcp's `TokioChildProcess`) so
/// we own the `Child` handle directly.  A background task calls
/// `child.wait()` to reap the process when it exits — this prevents zombie
/// accumulation that would otherwise occur because rmcp's
/// `ChildWithCleanup::drop` calls `start_kill()` on an already-exited
/// child (which fails with ESRCH), then skips the `wait()` that would
/// reap it.
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

    let mut child = Command::new(command)
        .args(args)
        .envs(env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .map_err(|e| format!("Failed to spawn child process: {e}"))?;

    let child_stdin = child.stdin.take().ok_or("Child stdin was not piped")?;
    let child_stdout = child.stdout.take().ok_or("Child stdout was not piped")?;

    // Background reaper: waits for the child to exit so the kernel releases
    // the process table entry.  Without this, exited children become zombies
    // because nobody calls waitpid() on them.
    tokio::spawn(async move {
        match child.wait().await {
            Ok(status) => {
                tracing::debug!("Child process exited: {status}");
            }
            Err(e) => {
                tracing::warn!("Error waiting for child process: {e}");
            }
        }
    });

    let transport = AsyncRwTransport::new_client(child_stdout, child_stdin);

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
