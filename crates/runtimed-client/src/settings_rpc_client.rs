//! Client for the `Handshake::SettingsRpc` channel.
//!
//! Prototype for the design described in nteract/desktop#1598. The daemon
//! owns `settings.json` as canonical state; this client receives a full
//! snapshot whenever it changes and sends scalar `SetSetting` writes
//! against the daemon's current view.
//!
//! This is a separate code path from `SyncClient` (Automerge). Both speak
//! over the same Unix-domain socket using different `Handshake` variants.
//! Until the new shape is the default, callers wire it up explicitly.

use std::path::PathBuf;
use std::time::Duration;

use log::info;
use notebook_protocol::connection::{
    self, Handshake, SettingsRpcClientMessage, SettingsRpcServerMessage,
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::settings_doc::SyncedSettings;

/// Errors from the settings RPC client.
#[derive(Debug, thiserror::Error)]
pub enum SettingsRpcError {
    #[error("Failed to connect: {0}")]
    ConnectionFailed(#[from] std::io::Error),

    #[error("Connection timeout")]
    Timeout,

    #[error("Disconnected")]
    Disconnected,

    #[error("Protocol error: {0}")]
    Protocol(String),

    #[error("Daemon rejected SetSetting: {0}")]
    Rejected(String),
}

impl From<anyhow::Error> for SettingsRpcError {
    fn from(err: anyhow::Error) -> Self {
        // The framing helpers return io errors verbatim through `anyhow`.
        // Surface them as `ConnectionFailed` so disconnects come through
        // with the right kind; everything else is a protocol decode error.
        if let Some(io_err) = err.downcast_ref::<std::io::Error>() {
            return SettingsRpcError::ConnectionFailed(std::io::Error::new(
                io_err.kind(),
                io_err.to_string(),
            ));
        }
        SettingsRpcError::Protocol(err.to_string())
    }
}

/// Client for the RPC settings channel.
///
/// Holds the most recently received `SyncedSettings` snapshot. Use
/// `recv_snapshot` to wait for the next update from the daemon, or
/// `set_setting` to request a write.
pub struct SettingsRpcClient<S> {
    snapshot: SyncedSettings,
    stream: S,
}

#[cfg(unix)]
impl SettingsRpcClient<tokio::net::UnixStream> {
    /// Connect to the daemon's unified socket.
    pub async fn connect(socket_path: PathBuf) -> Result<Self, SettingsRpcError> {
        Self::connect_with_timeout(socket_path, Duration::from_secs(2)).await
    }

    /// Connect with a custom timeout.
    pub async fn connect_with_timeout(
        socket_path: PathBuf,
        timeout: Duration,
    ) -> Result<Self, SettingsRpcError> {
        let stream = tokio::time::timeout(timeout, tokio::net::UnixStream::connect(&socket_path))
            .await
            .map_err(|_| SettingsRpcError::Timeout)?
            .map_err(SettingsRpcError::ConnectionFailed)?;

        info!("[settings-rpc-client] Connected to {:?}", socket_path);

        Self::init(stream).await
    }
}

#[cfg(windows)]
impl SettingsRpcClient<tokio::net::windows::named_pipe::NamedPipeClient> {
    /// Connect to the daemon's unified socket.
    pub async fn connect(socket_path: PathBuf) -> Result<Self, SettingsRpcError> {
        Self::connect_with_timeout(socket_path, Duration::from_secs(2)).await
    }

    pub async fn connect_with_timeout(
        socket_path: PathBuf,
        timeout: Duration,
    ) -> Result<Self, SettingsRpcError> {
        let pipe_name = socket_path.to_string_lossy().to_string();
        const ERROR_PIPE_BUSY: i32 = 231;
        let client = tokio::time::timeout(timeout, async {
            let mut attempts = 0;
            loop {
                match tokio::net::windows::named_pipe::ClientOptions::new().open(&pipe_name) {
                    Ok(client) => return Ok(client),
                    Err(e) if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && attempts < 5 => {
                        attempts += 1;
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                    Err(e) => return Err(e),
                }
            }
        })
        .await
        .map_err(|_| SettingsRpcError::Timeout)?
        .map_err(SettingsRpcError::ConnectionFailed)?;

        info!("[settings-rpc-client] Connected to {}", pipe_name);

        Self::init(client).await
    }
}

impl<S> SettingsRpcClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Send the preamble + `SettingsRpc` handshake and consume the initial
    /// snapshot frame.
    async fn init(mut stream: S) -> Result<Self, SettingsRpcError> {
        connection::send_preamble(&mut stream).await?;
        connection::send_json_frame(&mut stream, &Handshake::SettingsRpc).await?;

        // The daemon sends the initial snapshot first.
        let snapshot = expect_snapshot(&mut stream).await?;
        info!(
            "[settings-rpc-client] Initial snapshot received: theme={:?}",
            snapshot.theme
        );
        Ok(Self { snapshot, stream })
    }

    /// Cached most-recent snapshot.
    pub fn get_snapshot(&self) -> &SyncedSettings {
        &self.snapshot
    }

    /// Wait for the next snapshot push from the daemon.
    ///
    /// Stray `SetSettingAck` frames here would only happen if the daemon
    /// is misbehaving, so surface them as a protocol error rather than
    /// silently swallowing.
    pub async fn recv_snapshot(&mut self) -> Result<SyncedSettings, SettingsRpcError> {
        match recv_server_message(&mut self.stream).await? {
            SettingsRpcServerMessage::Snapshot { settings } => {
                let parsed: SyncedSettings = serde_json::from_value(settings)
                    .map_err(|e| SettingsRpcError::Protocol(format!("snapshot decode: {e}")))?;
                self.snapshot = parsed.clone();
                Ok(parsed)
            }
            SettingsRpcServerMessage::SetSettingAck { ok, error } => {
                Err(SettingsRpcError::Protocol(format!(
                    "unexpected ack while waiting for snapshot: ok={ok} error={error:?}"
                )))
            }
        }
    }

    /// Send `SetSetting` and wait for the daemon's ack.
    ///
    /// Snapshot pushes that arrive before the ack are applied to the
    /// cached snapshot in-line so callers don't lose updates that happen
    /// to land on the same socket between request and reply.
    pub async fn set_setting(
        &mut self,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<(), SettingsRpcError> {
        let req = SettingsRpcClientMessage::SetSetting {
            key: key.to_string(),
            value: value.clone(),
        };
        connection::send_json_frame(&mut self.stream, &req).await?;

        loop {
            match recv_server_message(&mut self.stream).await? {
                SettingsRpcServerMessage::Snapshot { settings } => {
                    let parsed: SyncedSettings = serde_json::from_value(settings)
                        .map_err(|e| SettingsRpcError::Protocol(format!("snapshot decode: {e}")))?;
                    self.snapshot = parsed;
                    // Keep waiting for the ack — the daemon broadcasts the
                    // snapshot on `settings_changed`, which fires before
                    // the ack on the writer's own connection.
                }
                SettingsRpcServerMessage::SetSettingAck { ok, error } => {
                    return if ok {
                        Ok(())
                    } else {
                        Err(SettingsRpcError::Rejected(
                            error.unwrap_or_else(|| "unknown".into()),
                        ))
                    };
                }
            }
        }
    }
}

async fn expect_snapshot<S>(stream: &mut S) -> Result<SyncedSettings, SettingsRpcError>
where
    S: AsyncRead + Unpin,
{
    match recv_server_message(stream).await? {
        SettingsRpcServerMessage::Snapshot { settings } => serde_json::from_value(settings)
            .map_err(|e| SettingsRpcError::Protocol(format!("snapshot decode: {e}"))),
        SettingsRpcServerMessage::SetSettingAck { ok, error } => Err(SettingsRpcError::Protocol(
            format!("expected initial snapshot, got ack ok={ok} error={error:?}"),
        )),
    }
}

async fn recv_server_message<S>(
    stream: &mut S,
) -> Result<SettingsRpcServerMessage, SettingsRpcError>
where
    S: AsyncRead + Unpin,
{
    match connection::recv_json_frame::<_, SettingsRpcServerMessage>(stream).await? {
        Some(msg) => Ok(msg),
        None => Err(SettingsRpcError::Disconnected),
    }
}
