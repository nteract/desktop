use std::path::{Path, PathBuf};

use std::future::Future;

use jupyter_protocol::{
    ConnectionInfo, ExecuteReply, ExecuteRequest, InputReply, InputRequest, InterruptRequest,
    JupyterMessage, JupyterMessageContent, ReplyStatus, ShutdownRequest,
};
use petname::petname;
use uuid::Uuid;

use runtimelib::{
    create_client_control_connection, create_client_iopub_connection,
    create_client_shell_connection_with_identity, create_client_stdin_connection_with_identity,
    peek_ports_with_listeners, peer_identity_for_session, runtime_dir, KernelspecDir, Result,
    RuntimeError,
};

/// Get the default working directory for kernel processes.
/// - If running from CLI (cwd is not `/`), uses the current working directory
/// - Otherwise falls back to ~/notebooks (creating it if needed)
/// - If ~/notebooks creation fails, falls back to home directory, then temp directory
fn default_kernel_cwd() -> PathBuf {
    // Check if we're running from CLI (cwd is something other than `/`)
    // App bundles on macOS run with `/` as cwd, but CLI usage preserves shell cwd
    if let Ok(cwd) = std::env::current_dir() {
        if cwd != Path::new("/") {
            return cwd;
        }
    }

    // Fall back to ~/notebooks, creating it if needed
    if let Some(home) = dirs::home_dir() {
        let notebooks_dir = home.join("notebooks");
        match std::fs::create_dir(&notebooks_dir) {
            Ok(()) => return notebooks_dir,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return notebooks_dir,
            Err(_) => return home,
        }
    }

    std::env::temp_dir()
}

pub struct KernelClient {
    kernel_id: String,
    session_id: String,
    connection_info: ConnectionInfo,
    connection_file: PathBuf,
    child: Option<tokio::process::Child>,
}

impl KernelClient {
    #[allow(clippy::expect_used)] // petname only returns None when word count is 0
    pub async fn start_from_kernelspec(kernelspec: KernelspecDir) -> Result<Self> {
        let kernel_id = petname(2, "-").expect("failed to generate petname");
        let session_id = Uuid::new_v4().to_string();
        let key = Uuid::new_v4().to_string();

        let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1));
        let (ports, listeners) = peek_ports_with_listeners(ip, 5).await?;
        let connection_info = ConnectionInfo {
            transport: jupyter_protocol::connection_info::Transport::TCP,
            ip: ip.to_string(),
            stdin_port: ports[0],
            control_port: ports[1],
            hb_port: ports[2],
            shell_port: ports[3],
            iopub_port: ports[4],
            signature_scheme: "hmac-sha256".to_string(),
            key,
            kernel_name: Some(kernelspec.kernel_name.clone()),
        };

        let runtime_dir = runtime_dir();
        tokio::fs::create_dir_all(&runtime_dir).await?;

        let connection_file = runtime_dir.join(format!("runt-kernel-{}.json", kernel_id));

        let mut command = kernelspec.clone().command(&connection_file, None, None)?;
        command.current_dir(default_kernel_cwd());

        let child = command.spawn()?;
        drop(listeners);

        let content = serde_json::to_string(&connection_info)?;
        tokio::fs::write(&connection_file, &content).await?;

        Ok(Self {
            kernel_id,
            session_id,
            connection_info,
            connection_file,
            child: Some(child),
        })
    }

    /// Start a kernel from a raw command string.
    ///
    /// The command is split on whitespace and `{connection_file}` is replaced
    /// with the path to the generated connection file.
    #[allow(clippy::expect_used)] // petname only returns None when word count is 0
    pub async fn start_from_command(cmd: &str) -> Result<Self> {
        let kernel_id = petname(2, "-").expect("failed to generate petname");
        let session_id = Uuid::new_v4().to_string();
        let key = Uuid::new_v4().to_string();

        let ip = std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1));
        let (ports, listeners) = peek_ports_with_listeners(ip, 5).await?;
        let connection_info = ConnectionInfo {
            transport: jupyter_protocol::connection_info::Transport::TCP,
            ip: ip.to_string(),
            stdin_port: ports[0],
            control_port: ports[1],
            hb_port: ports[2],
            shell_port: ports[3],
            iopub_port: ports[4],
            signature_scheme: "hmac-sha256".to_string(),
            key,
            kernel_name: None,
        };

        let runtime_dir = runtime_dir();
        tokio::fs::create_dir_all(&runtime_dir).await?;

        let connection_file = runtime_dir.join(format!("runt-kernel-{}.json", kernel_id));
        let content = serde_json::to_string(&connection_info)?;
        tokio::fs::write(&connection_file, &content).await?;

        let cf_str = connection_file.to_string_lossy();
        let args: Vec<String> = cmd
            .split_whitespace()
            .map(|arg| arg.replace("{connection_file}", &cf_str))
            .collect();

        let mut command = tokio::process::Command::new(&args[0]);
        command.args(&args[1..]);
        command.current_dir(default_kernel_cwd());
        let child = command.spawn().map_err(|e| RuntimeError::CommandFailed {
            command: "kernel",
            source: e,
        })?;
        drop(listeners);

        Ok(Self {
            kernel_id,
            session_id,
            connection_info,
            connection_file,
            child: Some(child),
        })
    }

    pub async fn from_connection_file(path: impl AsRef<Path>) -> Result<Self> {
        let connection_file = path.as_ref().to_path_buf();
        let content = tokio::fs::read_to_string(&connection_file).await?;
        let connection_info: ConnectionInfo = serde_json::from_str(&content)?;

        let kernel_id =
            extract_kernel_id(&connection_file).ok_or_else(|| RuntimeError::KernelIdMissing {
                path: connection_file.display().to_string(),
            })?;
        let session_id = Uuid::new_v4().to_string();

        Ok(Self {
            kernel_id,
            session_id,
            connection_info,
            connection_file,
            child: None,
        })
    }

    pub fn kernel_id(&self) -> &str {
        &self.kernel_id
    }

    pub fn connection_file(&self) -> &Path {
        &self.connection_file
    }

    pub fn connection_info(&self) -> &ConnectionInfo {
        &self.connection_info
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub async fn interrupt(&mut self) -> Result<()> {
        let mut control =
            create_client_control_connection(&self.connection_info, &self.session_id).await?;
        let message: JupyterMessage = InterruptRequest::default().into();
        control.send(message).await?;
        Ok(())
    }

    pub async fn shutdown(&mut self, restart: bool) -> Result<()> {
        // Try a graceful shutdown with a timeout
        let graceful = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.send_shutdown(restart),
        )
        .await;

        match graceful {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("Shutdown request failed: {e}"),
            Err(_) => eprintln!("Kernel did not respond to shutdown, killing process"),
        }

        // Kill the child process if it's still running
        if let Some(ref mut child) = self.child {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }

        let _ = tokio::fs::remove_file(&self.connection_file).await;
        Ok(())
    }

    async fn send_shutdown(&self, restart: bool) -> Result<()> {
        let mut control =
            create_client_control_connection(&self.connection_info, &self.session_id).await?;
        let message: JupyterMessage = ShutdownRequest { restart }.into();
        let message_id = message.header.msg_id.clone();
        control.send(message).await?;
        loop {
            let reply = control.read().await?;
            let is_parent = reply
                .parent_header
                .as_ref()
                .map(|parent| parent.msg_id.as_str())
                == Some(message_id.as_str());
            if !is_parent {
                continue;
            }
            match reply.content {
                JupyterMessageContent::ShutdownReply(reply) => {
                    if reply.status != ReplyStatus::Ok {
                        let mut details = format!("{:?}", reply.status);
                        if let Some(error) = reply.error {
                            details = format!("{}: {:?}", details, error);
                        }
                        return Err(RuntimeError::KernelShutdownFailed { details });
                    }
                    break;
                }
                _ => continue,
            }
        }
        Ok(())
    }

    pub async fn execute<F>(&self, code: &str, mut on_iopub: F) -> Result<ExecuteReply>
    where
        F: FnMut(JupyterMessageContent),
    {
        let identity = peer_identity_for_session(&self.session_id)?;
        let mut shell = create_client_shell_connection_with_identity(
            &self.connection_info,
            &self.session_id,
            identity,
        )
        .await?;
        let mut iopub =
            create_client_iopub_connection(&self.connection_info, "", &self.session_id).await?;

        let message: JupyterMessage = ExecuteRequest::new(code.to_string()).into();
        let message_id = message.header.msg_id.clone();
        shell.send(message).await?;

        loop {
            tokio::select! {
                shell_msg = shell.read() => {
                    let msg = shell_msg?;
                    let is_parent = msg
                        .parent_header
                        .as_ref()
                        .map(|parent| parent.msg_id.as_str())
                        == Some(message_id.as_str());
                    if !is_parent {
                        continue;
                    }
                    if let JupyterMessageContent::ExecuteReply(reply) = msg.content {
                        return Ok(reply);
                    }
                }
                iopub_msg = iopub.read() => {
                    let msg = iopub_msg?;
                    let is_parent = msg
                        .parent_header
                        .as_ref()
                        .map(|parent| parent.msg_id.as_str())
                        == Some(message_id.as_str());
                    if !is_parent {
                        continue;
                    }
                    on_iopub(msg.content);
                }
            }
        }
    }
    /// Execute code with stdin support, allowing the kernel to request user input.
    ///
    /// Creates shell and stdin connections that share a ZMQ identity (required
    /// by the Jupyter protocol for stdin routing). When the kernel sends an
    /// `input_request`, the `on_stdin` callback is invoked to get the user's response.
    #[allow(dead_code)]
    pub async fn execute_with_stdin<F, G, Fut>(
        &self,
        code: &str,
        mut on_iopub: F,
        mut on_stdin: G,
    ) -> Result<ExecuteReply>
    where
        F: FnMut(JupyterMessageContent),
        G: FnMut(InputRequest) -> Fut,
        Fut: Future<Output = InputReply>,
    {
        let identity = peer_identity_for_session(&self.session_id)?;
        let shell = create_client_shell_connection_with_identity(
            &self.connection_info,
            &self.session_id,
            identity.clone(),
        )
        .await?;
        let mut stdin = create_client_stdin_connection_with_identity(
            &self.connection_info,
            &self.session_id,
            identity,
        )
        .await?;
        let (mut shell_send, mut shell_recv) = shell.split();
        let mut iopub =
            create_client_iopub_connection(&self.connection_info, "", &self.session_id).await?;

        let mut execute_request = ExecuteRequest::new(code.to_string());
        execute_request.allow_stdin = true;
        let message: JupyterMessage = execute_request.into();
        let message_id = message.header.msg_id.clone();
        shell_send.send(message).await?;

        loop {
            tokio::select! {
                shell_msg = shell_recv.read() => {
                    let msg = shell_msg?;
                    let is_parent = msg
                        .parent_header
                        .as_ref()
                        .map(|parent| parent.msg_id.as_str())
                        == Some(message_id.as_str());
                    if !is_parent {
                        continue;
                    }
                    if let JupyterMessageContent::ExecuteReply(reply) = msg.content {
                        return Ok(reply);
                    }
                }
                iopub_msg = iopub.read() => {
                    let msg = iopub_msg?;
                    let is_parent = msg
                        .parent_header
                        .as_ref()
                        .map(|parent| parent.msg_id.as_str())
                        == Some(message_id.as_str());
                    if !is_parent {
                        continue;
                    }
                    on_iopub(msg.content);
                }
                stdin_msg = stdin.read() => {
                    let msg = stdin_msg?;
                    if let JupyterMessageContent::InputRequest(ref request) = msg.content {
                        let reply = on_stdin(request.clone()).await;
                        let reply_message = reply.as_child_of(&msg);
                        stdin.send(reply_message).await?;
                    }
                }
            }
        }
    }
}

fn extract_kernel_id(path: &Path) -> Option<String> {
    let file_stem = path.file_stem()?.to_string_lossy();
    let id_str = file_stem.strip_prefix("runt-kernel-")?;
    Some(id_str.to_string())
}
