//! Child process spawning and MCP client identity forwarding.

use std::collections::HashMap;
use std::path::Path;

use rmcp::model::Implementation;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
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

    let cmd_path = command.to_path_buf();
    let args_owned: Vec<String> = args.to_vec();
    let env_owned: HashMap<String, String> = env.clone();

    let transport = TokioChildProcess::new(Command::new(&cmd_path).configure(move |cmd| {
        for arg in &args_owned {
            cmd.arg(arg);
        }
        for (key, val) in &env_owned {
            cmd.env(key, val);
        }
    }))
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
