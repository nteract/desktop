//! Client for the Automerge settings sync service.
//!
//! Each notebook window creates a `SyncClient` that maintains a local
//! Automerge document replica. Changes made locally are sent to the daemon,
//! and changes from other peers arrive as sync messages.

use std::path::PathBuf;
use std::time::Duration;

use automerge::sync::{self, SyncDoc};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ObjType, ReadDoc};
use log::info;
use tokio::io::{AsyncRead, AsyncWrite};

use crate::connection::{self, Handshake};
use crate::settings_doc::{
    read_nested_list, split_comma_list, CondaDefaults, SyncedSettings, ThemeMode, UvDefaults,
};

/// Error type for sync client operations.
#[derive(Debug, thiserror::Error)]
pub enum SyncClientError {
    #[error("Failed to connect: {0}")]
    ConnectionFailed(#[from] std::io::Error),

    #[error("Sync protocol error: {0}")]
    SyncError(String),

    #[error("Connection timeout")]
    Timeout,

    #[error("Disconnected")]
    Disconnected,
}

/// Client for the Automerge settings sync service.
///
/// Holds a local Automerge document replica that stays in sync with the
/// daemon's canonical copy via the Automerge sync protocol.
pub struct SyncClient<S> {
    doc: AutoCommit,
    peer_state: sync::State,
    stream: S,
}

#[cfg(unix)]
impl SyncClient<tokio::net::UnixStream> {
    /// Connect to the daemon's unified socket and perform initial sync.
    pub async fn connect(socket_path: PathBuf) -> Result<Self, SyncClientError> {
        Self::connect_with_timeout(socket_path, Duration::from_secs(2)).await
    }

    /// Connect with a custom timeout.
    pub async fn connect_with_timeout(
        socket_path: PathBuf,
        timeout: Duration,
    ) -> Result<Self, SyncClientError> {
        let stream = tokio::time::timeout(timeout, tokio::net::UnixStream::connect(&socket_path))
            .await
            .map_err(|_| SyncClientError::Timeout)?
            .map_err(SyncClientError::ConnectionFailed)?;

        info!("[sync-client] Connected to {:?}", socket_path);

        Self::init(stream).await
    }
}

#[cfg(windows)]
impl SyncClient<tokio::net::windows::named_pipe::NamedPipeClient> {
    /// Connect to the daemon's unified socket and perform initial sync.
    pub async fn connect(socket_path: PathBuf) -> Result<Self, SyncClientError> {
        Self::connect_with_timeout(socket_path, Duration::from_secs(2)).await
    }

    /// Connect with a custom timeout, retrying on transient pipe-busy errors.
    pub async fn connect_with_timeout(
        socket_path: PathBuf,
        timeout: Duration,
    ) -> Result<Self, SyncClientError> {
        let pipe_name = socket_path.to_string_lossy().to_string();
        // ERROR_PIPE_BUSY (231): all pipe instances are in use between server rotations
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
        .map_err(|_| SyncClientError::Timeout)?
        .map_err(SyncClientError::ConnectionFailed)?;

        info!("[sync-client] Connected to {}", pipe_name);

        Self::init(client).await
    }
}

impl<S> SyncClient<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Initialize the client by sending the handshake and performing
    /// the initial sync exchange.
    async fn init(mut stream: S) -> Result<Self, SyncClientError> {
        // Send preamble (magic bytes + protocol version)
        connection::send_preamble(&mut stream)
            .await
            .map_err(|e| SyncClientError::SyncError(format!("preamble: {}", e)))?;

        // Send the channel handshake so the daemon routes us to settings sync
        connection::send_json_frame(&mut stream, &Handshake::SettingsSync)
            .await
            .map_err(|e| SyncClientError::SyncError(format!("handshake: {}", e)))?;

        let mut doc = AutoCommit::new();
        let mut peer_state = sync::State::new();

        // The server sends first -- receive and apply
        match connection::recv_frame(&mut stream).await? {
            Some(data) => {
                let message = sync::Message::decode(&data)
                    .map_err(|e| SyncClientError::SyncError(format!("decode: {}", e)))?;
                doc.sync()
                    .receive_sync_message(&mut peer_state, message)
                    .map_err(|e| SyncClientError::SyncError(format!("receive: {}", e)))?;
            }
            None => return Err(SyncClientError::Disconnected),
        }

        // Send our sync message back (to complete the handshake)
        if let Some(msg) = doc.sync().generate_sync_message(&mut peer_state) {
            connection::send_frame(&mut stream, &msg.encode()).await?;
        }

        // There might be more rounds needed -- keep going until no more messages
        loop {
            // Try to receive with a short timeout (the server may not have more to say)
            match tokio::time::timeout(
                Duration::from_millis(100),
                connection::recv_frame(&mut stream),
            )
            .await
            {
                Ok(Ok(Some(data))) => {
                    let message = sync::Message::decode(&data)
                        .map_err(|e| SyncClientError::SyncError(format!("decode: {}", e)))?;
                    doc.sync()
                        .receive_sync_message(&mut peer_state, message)
                        .map_err(|e| SyncClientError::SyncError(format!("receive: {}", e)))?;

                    if let Some(msg) = doc.sync().generate_sync_message(&mut peer_state) {
                        connection::send_frame(&mut stream, &msg.encode()).await?;
                    }
                }
                Ok(Ok(None)) => return Err(SyncClientError::Disconnected),
                Ok(Err(e)) => return Err(SyncClientError::ConnectionFailed(e)),
                Err(_) => break, // Timeout -- initial sync is done
            }
        }

        let settings = get_all_from_doc(&doc);
        info!("[sync-client] Initial sync complete: {:?}", settings);

        Ok(Self {
            doc,
            peer_state,
            stream,
        })
    }

    /// Get a snapshot of all settings from the local replica.
    pub fn get_all(&self) -> SyncedSettings {
        get_all_from_doc(&self.doc)
    }

    /// Consume the client and return the local Automerge document.
    ///
    /// This is useful when you want to keep the synced settings doc
    /// without maintaining the network connection.
    pub fn into_doc(self) -> AutoCommit {
        self.doc
    }

    /// Get a single scalar setting value.
    pub fn get(&self, key: &str) -> Option<String> {
        self.doc
            .get(automerge::ROOT, key)
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                automerge::Value::Scalar(s) => match s.as_ref() {
                    automerge::ScalarValue::Str(s) => Some(s.to_string()),
                    _ => None,
                },
                _ => None,
            })
    }

    /// Update a scalar setting and sync the change to the daemon.
    pub async fn put(&mut self, key: &str, value: &str) -> Result<(), SyncClientError> {
        if let Some((map_key, sub_key)) = key.split_once('.') {
            let map_id = self.ensure_map(map_key)?;
            self.doc
                .put(&map_id, sub_key, value)
                .map_err(|e| SyncClientError::SyncError(format!("put nested: {}", e)))?;
        } else {
            self.doc
                .put(automerge::ROOT, key, value)
                .map_err(|e| SyncClientError::SyncError(format!("put: {}", e)))?;
        }

        self.sync_to_daemon().await
    }

    /// Update a setting from a `serde_json::Value` and sync the change.
    ///
    /// Dispatches to scalar `put` for strings or list replacement for arrays.
    pub async fn put_value(
        &mut self,
        key: &str,
        value: &serde_json::Value,
    ) -> Result<(), SyncClientError> {
        match value {
            serde_json::Value::String(s) => {
                // Scalar write -- delegate to put which handles dotted paths
                if let Some((map_key, sub_key)) = key.split_once('.') {
                    let map_id = self.ensure_map(map_key)?;
                    self.doc
                        .put(&map_id, sub_key, s.as_str())
                        .map_err(|e| SyncClientError::SyncError(format!("put nested: {}", e)))?;
                } else {
                    self.doc
                        .put(automerge::ROOT, key, s.as_str())
                        .map_err(|e| SyncClientError::SyncError(format!("put: {}", e)))?;
                }
            }
            serde_json::Value::Array(arr) => {
                let items: Vec<String> = arr
                    .iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect();
                self.put_list(key, &items)?;
            }
            serde_json::Value::Number(n) => {
                if let Some(u) = n.as_u64() {
                    // Store as i64 since Automerge's Int is more widely supported
                    self.doc
                        .put(automerge::ROOT, key, u as i64)
                        .map_err(|e| SyncClientError::SyncError(format!("put u64: {}", e)))?;
                }
            }
            serde_json::Value::Bool(b) => {
                if let Some((map_key, sub_key)) = key.split_once('.') {
                    let map_id = self.ensure_map(map_key)?;
                    self.doc.put(&map_id, sub_key, *b).map_err(|e| {
                        SyncClientError::SyncError(format!("put nested bool: {}", e))
                    })?;
                } else {
                    self.doc
                        .put(automerge::ROOT, key, *b)
                        .map_err(|e| SyncClientError::SyncError(format!("put bool: {}", e)))?;
                }
            }
            _ => {}
        }

        self.sync_to_daemon().await
    }

    /// Replace a list at a dotted path in the local Automerge doc.
    fn put_list(&mut self, key: &str, values: &[String]) -> Result<(), SyncClientError> {
        let (map_key, sub_key) = key
            .split_once('.')
            .ok_or_else(|| SyncClientError::SyncError("list key must be dotted".into()))?;

        let map_id = self.ensure_map(map_key)?;

        // Delete existing value
        let _ = self.doc.delete(&map_id, sub_key);

        // Create new list
        let list_id = self
            .doc
            .put_object(&map_id, sub_key, ObjType::List)
            .map_err(|e| SyncClientError::SyncError(format!("put_object list: {}", e)))?;

        for (i, item) in values.iter().enumerate() {
            self.doc
                .insert(&list_id, i, item.as_str())
                .map_err(|e| SyncClientError::SyncError(format!("insert: {}", e)))?;
        }

        Ok(())
    }

    /// Get or create a nested Map at ROOT.
    fn ensure_map(&mut self, map_key: &str) -> Result<automerge::ObjId, SyncClientError> {
        // Check if map already exists
        if let Some((automerge::Value::Object(ObjType::Map), id)) =
            self.doc.get(automerge::ROOT, map_key).ok().flatten()
        {
            return Ok(id);
        }

        // Create it
        self.doc
            .put_object(automerge::ROOT, map_key, ObjType::Map)
            .map_err(|e| SyncClientError::SyncError(format!("put_object map: {}", e)))
    }

    /// Generate and send sync message to daemon.
    async fn sync_to_daemon(&mut self) -> Result<(), SyncClientError> {
        if let Some(msg) = self.doc.sync().generate_sync_message(&mut self.peer_state) {
            connection::send_frame(&mut self.stream, &msg.encode()).await?;
        }
        Ok(())
    }

    /// Wait for the next settings change from the daemon.
    ///
    /// Blocks until a sync message arrives, applies it, and returns the
    /// updated settings snapshot.
    pub async fn recv_changes(&mut self) -> Result<SyncedSettings, SyncClientError> {
        match connection::recv_frame(&mut self.stream).await? {
            Some(data) => {
                let message = sync::Message::decode(&data)
                    .map_err(|e| SyncClientError::SyncError(format!("decode: {}", e)))?;
                self.doc
                    .sync()
                    .receive_sync_message(&mut self.peer_state, message)
                    .map_err(|e| SyncClientError::SyncError(format!("receive: {}", e)))?;

                // Send ack if needed
                if let Some(msg) = self.doc.sync().generate_sync_message(&mut self.peer_state) {
                    connection::send_frame(&mut self.stream, &msg.encode()).await?;
                }

                Ok(self.get_all())
            }
            None => Err(SyncClientError::Disconnected),
        }
    }
}

/// Extract all settings from an Automerge document.
///
/// Reads nested maps/lists first, falling back to old flat keys for
/// backward compatibility during upgrades.
pub fn get_all_from_doc(doc: &AutoCommit) -> SyncedSettings {
    let defaults = SyncedSettings::default();

    let get_str = |key: &str| -> Option<String> {
        doc.get(automerge::ROOT, key)
            .ok()
            .flatten()
            .and_then(|(value, _)| match value {
                automerge::Value::Scalar(s) => match s.as_ref() {
                    automerge::ScalarValue::Str(s) => Some(s.to_string()),
                    _ => None,
                },
                _ => None,
            })
    };

    // Get a u64 value from the doc
    let get_u64 = |key: &str| -> Option<u64> {
        match doc.get(automerge::ROOT, key).ok().flatten() {
            Some((automerge::Value::Scalar(s), _)) => match s.as_ref() {
                automerge::ScalarValue::Int(i) => u64::try_from(*i).ok(),
                automerge::ScalarValue::Uint(u) => Some(*u),
                automerge::ScalarValue::Str(s) => s.parse().ok(),
                _ => None,
            },
            _ => None,
        }
    };

    // Get a bool value from the doc
    let get_bool = |key: &str| -> Option<bool> {
        match doc.get(automerge::ROOT, key).ok().flatten() {
            Some((automerge::Value::Scalar(s), _)) => match s.as_ref() {
                automerge::ScalarValue::Boolean(b) => Some(*b),
                _ => None,
            },
            _ => None,
        }
    };

    // Read uv packages: try nested list, fall back to flat comma string
    let uv_packages = {
        let nested = read_nested_list(doc, "uv", "default_packages");
        if !nested.is_empty() {
            nested
        } else if let Some(flat) = get_str("default_uv_packages") {
            split_comma_list(&flat)
        } else {
            defaults.uv.default_packages.clone()
        }
    };

    // Read conda packages: try nested list, fall back to flat comma string
    let conda_packages = {
        let nested = read_nested_list(doc, "conda", "default_packages");
        if !nested.is_empty() {
            nested
        } else if let Some(flat) = get_str("default_conda_packages") {
            split_comma_list(&flat)
        } else {
            defaults.conda.default_packages.clone()
        }
    };

    SyncedSettings {
        theme: get_str("theme")
            .and_then(|s| serde_json::from_str::<ThemeMode>(&format!("\"{s}\"")).ok())
            .unwrap_or(defaults.theme),
        default_runtime: get_str("default_runtime")
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
        default_python_env: get_str("default_python_env")
            .and_then(|s| s.parse().ok())
            .unwrap_or_default(),
        uv: UvDefaults {
            default_packages: uv_packages,
        },
        conda: CondaDefaults {
            default_packages: conda_packages,
        },
        keep_alive_secs: get_u64("keep_alive_secs").unwrap_or(defaults.keep_alive_secs),
        // For existing users: if onboarding_completed is missing but other settings exist,
        // assume they're upgrading from before onboarding was added → treat as completed
        onboarding_completed: get_bool("onboarding_completed")
            .unwrap_or_else(|| get_str("theme").is_some() || get_str("default_runtime").is_some()),
        agent_mode: get_bool("agent_mode").unwrap_or(false),
    }
}

/// Try to connect to the sync daemon and get current settings.
///
/// Returns an error if the daemon is unavailable. Callers should
/// fall back to their own local state (e.g. localStorage) on error
/// rather than silently adopting defaults.
pub async fn try_get_synced_settings() -> Result<SyncedSettings, SyncClientError> {
    #[cfg(unix)]
    {
        let client = SyncClient::connect(crate::default_socket_path()).await?;
        let settings = client.get_all();
        info!("[sync-client] Got settings from daemon: {:?}", settings);
        Ok(settings)
    }

    #[cfg(windows)]
    {
        let client = SyncClient::connect(crate::default_socket_path()).await?;
        let settings = client.get_all();
        info!("[sync-client] Got settings from daemon: {:?}", settings);
        Ok(settings)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::runtime::Runtime;
    use crate::settings_doc::{PythonEnvType, ThemeMode};
    use automerge::transaction::Transactable;

    #[test]
    fn test_get_all_from_empty_doc() {
        let doc = AutoCommit::new();
        let settings = get_all_from_doc(&doc);
        assert_eq!(settings, SyncedSettings::default());
    }

    #[test]
    fn test_get_all_from_populated_doc() {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "theme", "dark").unwrap();
        doc.put(automerge::ROOT, "default_runtime", "deno").unwrap();
        doc.put(automerge::ROOT, "default_python_env", "conda")
            .unwrap();

        let settings = get_all_from_doc(&doc);
        assert_eq!(settings.theme, ThemeMode::Dark);
        assert_eq!(settings.default_runtime, Runtime::Deno);
        assert_eq!(settings.default_python_env, PythonEnvType::Conda);
    }

    #[test]
    fn test_get_all_reads_nested_lists() {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "theme", "system").unwrap();
        doc.put(automerge::ROOT, "default_runtime", "python")
            .unwrap();
        doc.put(automerge::ROOT, "default_python_env", "uv")
            .unwrap();

        // Create nested uv map with package list
        let uv_id = doc.put_object(automerge::ROOT, "uv", ObjType::Map).unwrap();
        let uv_pkgs_id = doc
            .put_object(&uv_id, "default_packages", ObjType::List)
            .unwrap();
        doc.insert(&uv_pkgs_id, 0, "numpy").unwrap();
        doc.insert(&uv_pkgs_id, 1, "pandas").unwrap();

        let settings = get_all_from_doc(&doc);
        assert_eq!(settings.uv.default_packages, vec!["numpy", "pandas"]);
    }

    #[test]
    fn test_get_all_falls_back_to_flat_comma_string() {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "theme", "system").unwrap();
        doc.put(automerge::ROOT, "default_runtime", "python")
            .unwrap();
        doc.put(automerge::ROOT, "default_python_env", "uv")
            .unwrap();
        // Old flat format
        doc.put(automerge::ROOT, "default_uv_packages", "numpy, scipy")
            .unwrap();

        let settings = get_all_from_doc(&doc);
        assert_eq!(settings.uv.default_packages, vec!["numpy", "scipy"]);
    }

    #[test]
    fn test_get_all_reads_onboarding_completed_bool() {
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "onboarding_completed", true)
            .unwrap();

        let settings = get_all_from_doc(&doc);
        assert!(settings.onboarding_completed);
    }

    #[test]
    fn test_get_all_onboarding_defaults_false_for_fresh_install() {
        // Fresh install: no settings at all
        let doc = AutoCommit::new();
        let settings = get_all_from_doc(&doc);
        // Should be false because no other settings exist
        assert!(!settings.onboarding_completed);
    }

    #[test]
    fn test_get_all_onboarding_defaults_true_for_existing_user() {
        // Existing user: has theme but no onboarding_completed
        let mut doc = AutoCommit::new();
        doc.put(automerge::ROOT, "theme", "dark").unwrap();

        let settings = get_all_from_doc(&doc);
        // Should be true because theme exists (migration scenario)
        assert!(settings.onboarding_completed);
    }
}
