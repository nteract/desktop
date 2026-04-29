//! Automerge sync protocol handler for settings synchronization.
//!
//! Handles a single client connection that has already been routed by the
//! daemon's unified socket. Exchanges Automerge sync messages to keep a
//! shared settings document in sync across all notebook windows.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use automerge::sync;
use notebook_protocol::connection::{SettingsRpcClientMessage, SettingsRpcServerMessage};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, info, warn};

use crate::connection;
use crate::settings_doc::{
    SettingsDoc, SyncedSettings, MAX_KEEP_ALIVE_SECS, MAX_POOL_SIZE, MIN_KEEP_ALIVE_SECS,
};

const SETTINGS_RPC_KEYS: &[&str] = &[
    "theme",
    "color_theme",
    "default_runtime",
    "default_python_env",
    "uv.default_packages",
    "conda.default_packages",
    "pixi.default_packages",
    "keep_alive_secs",
    "onboarding_completed",
    "uv_pool_size",
    "conda_pool_size",
    "pixi_pool_size",
    "bootstrap_dx",
    "install_id",
    "telemetry_enabled",
    "telemetry_consent_recorded",
    "telemetry_last_daemon_ping_at",
    "telemetry_last_app_ping_at",
    "telemetry_last_mcp_ping_at",
];

/// Check if an error is just a normal connection close.
pub(crate) fn is_connection_closed(e: &anyhow::Error) -> bool {
    if let Some(io_err) = e.downcast_ref::<std::io::Error>() {
        matches!(
            io_err.kind(),
            std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::NotConnected
        )
    } else {
        false
    }
}

/// Handle a single settings sync client connection.
///
/// The caller has already consumed the handshake frame. This function
/// runs the Automerge sync protocol:
/// 1. Initial sync: exchange messages until both sides converge
/// 2. Watch loop: wait for changes (from other peers or from this client),
///    exchange sync messages to propagate
pub async fn handle_settings_sync_connection<R, W>(
    mut reader: R,
    mut writer: W,
    settings: Arc<RwLock<SettingsDoc>>,
    changed_tx: broadcast::Sender<()>,
    mut changed_rx: broadcast::Receiver<()>,
    automerge_path: PathBuf,
    json_path: PathBuf,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut peer_state = sync::State::new();
    info!("[sync] New client connected, starting initial sync");

    // Phase 1: Initial sync -- server sends first
    {
        let encoded = {
            let mut doc = settings.write().await;
            doc.generate_sync_message(&mut peer_state)
                .map(|msg| msg.encode())
        };
        if let Some(data) = encoded {
            connection::send_frame(&mut writer, &data).await?;
        }
    }

    // Phase 2: Exchange messages until sync is complete, then watch for changes
    loop {
        tokio::select! {
            // Incoming message from this client
            result = connection::recv_frame(&mut reader) => {
                match result? {
                    Some(data) => {
                        let message = sync::Message::decode(&data)
                            .map_err(|e| anyhow::anyhow!("decode error: {}", e))?;

                        let mut doc = settings.write().await;
                        // Compare heads before/after so pure acks or duplicate
                        // messages don't fire `settings_changed`. Without this
                        // the pool warming loops wake up on every sync-protocol
                        // round-trip, which thrashes the pools when several
                        // per-`invoke` clients land back-to-back (#2120).
                        let before = doc.heads();
                        doc.receive_sync_message(&mut peer_state, message)?;
                        let after = doc.heads();
                        let doc_changed = before != after;

                        if doc_changed {
                            persist_settings(&mut doc, &automerge_path, &json_path);
                            let _ = changed_tx.send(());
                        }

                        // Send our response
                        if let Some(reply) = doc.generate_sync_message(&mut peer_state) {
                            connection::send_frame(&mut writer, &reply.encode()).await?;
                        }
                    }
                    None => {
                        // Client disconnected
                        return Ok(());
                    }
                }
            }

            // Another peer changed settings -- push update to this client
            _ = changed_rx.recv() => {
                let mut doc = settings.write().await;
                if let Some(msg) = doc.generate_sync_message(&mut peer_state) {
                    connection::send_frame(&mut writer, &msg.encode()).await?;
                }
            }
        }
    }
}

/// Persist the settings document to disk (both Automerge binary and JSON mirror).
///
/// The Automerge sync path treats persist failures as best-effort warnings
/// (the in-memory CRDT is still authoritative; the next change retries).
/// The RPC path needs the failure surfaced to the writing client, so it
/// uses `try_persist_settings` instead.
fn persist_settings(doc: &mut SettingsDoc, automerge_path: &Path, json_path: &Path) {
    if let Err(e) = doc.save_to_file(automerge_path) {
        warn!("[sync] Failed to save Automerge doc: {}", e);
    }
    if let Err(e) = doc.save_json_mirror(json_path) {
        warn!("[sync] Failed to write JSON mirror: {}", e);
    }
}

/// Persist the settings document, surfacing the first failure as `Err`.
///
/// Used by the RPC `SetSetting` path so the ack can carry `ok: false`
/// when the on-disk write fails. The in-memory doc is left as the
/// caller wrote it; rollback is the caller's call. We still attempt
/// both writes so a partially recoverable state (Automerge ok, JSON
/// mirror failed) doesn't silently leave only one side persisted.
fn try_persist_settings(
    doc: &mut SettingsDoc,
    automerge_path: &Path,
    json_path: &Path,
) -> Result<(), String> {
    let mut first_error: Option<String> = None;
    if let Err(e) = doc.save_to_file(automerge_path) {
        let msg = format!("save Automerge doc: {e}");
        warn!("[settings-rpc] {msg}");
        first_error.get_or_insert(msg);
    }
    if let Err(e) = doc.save_json_mirror(json_path) {
        let msg = format!("write JSON mirror: {e}");
        warn!("[settings-rpc] {msg}");
        first_error.get_or_insert(msg);
    }
    match first_error {
        None => Ok(()),
        Some(msg) => Err(msg),
    }
}

/// Build a `Snapshot` server message from the current `SettingsDoc`.
fn build_snapshot_message(doc: &SettingsDoc) -> anyhow::Result<SettingsRpcServerMessage> {
    let snapshot = doc.get_all();
    let value = serde_json::to_value(&snapshot)?;
    Ok(SettingsRpcServerMessage::Snapshot { settings: value })
}

fn validate_settings_rpc_write(
    doc: &SettingsDoc,
    key: &str,
    value: &serde_json::Value,
) -> Result<bool, String> {
    if !SETTINGS_RPC_KEYS.contains(&key) {
        return Err(format!(
            "unknown setting '{key}'. Valid keys: {}",
            SETTINGS_RPC_KEYS.join(", ")
        ));
    }

    validate_field_constraints(key, value)?;

    let current = serde_json::to_value(doc.get_all()).map_err(|e| e.to_string())?;
    let mut candidate = current.clone();
    set_json_setting(&mut candidate, key, value.clone())?;
    serde_json::from_value::<SyncedSettings>(candidate.clone())
        .map_err(|e| format!("invalid value for setting '{key}': {e}"))?;

    Ok(candidate != current)
}

fn validate_field_constraints(key: &str, value: &serde_json::Value) -> Result<(), String> {
    match key {
        "keep_alive_secs" => {
            let secs = value
                .as_u64()
                .ok_or_else(|| "keep_alive_secs must be an unsigned integer".to_string())?;
            if !(MIN_KEEP_ALIVE_SECS..=MAX_KEEP_ALIVE_SECS).contains(&secs) {
                return Err(format!(
                    "keep_alive_secs must be between {MIN_KEEP_ALIVE_SECS} and {MAX_KEEP_ALIVE_SECS}"
                ));
            }
        }
        "uv_pool_size" | "conda_pool_size" | "pixi_pool_size" => {
            let size = value
                .as_u64()
                .ok_or_else(|| format!("{key} must be an unsigned integer"))?;
            if size > MAX_POOL_SIZE {
                return Err(format!("{key} must be between 0 and {MAX_POOL_SIZE}"));
            }
        }
        "uv.default_packages" | "conda.default_packages" | "pixi.default_packages" => {
            let packages = value
                .as_array()
                .ok_or_else(|| format!("{key} must be an array of package strings"))?;
            for package in packages {
                let package = package
                    .as_str()
                    .ok_or_else(|| format!("{key} must be an array of package strings"))?;
                notebook_doc::metadata::validate_package_specifier(package)
                    .map_err(|e| e.to_string())?;
            }
        }
        "telemetry_last_daemon_ping_at"
        | "telemetry_last_app_ping_at"
        | "telemetry_last_mcp_ping_at" => {
            if !(value.is_null() || value.as_u64().is_some()) {
                return Err(format!(
                    "{key} must be null or an unsigned integer timestamp"
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

fn set_json_setting(
    root: &mut serde_json::Value,
    key: &str,
    value: serde_json::Value,
) -> Result<(), String> {
    let parts: Vec<&str> = key.split('.').collect();
    if parts.iter().any(|part| part.is_empty()) {
        return Err("setting key must not contain empty path segments".to_string());
    }

    let mut current = root;
    for part in &parts[..parts.len().saturating_sub(1)] {
        current = current
            .as_object_mut()
            .ok_or_else(|| format!("expected object while setting '{key}'"))?
            .entry((*part).to_string())
            .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    }

    let leaf = parts
        .last()
        .ok_or_else(|| "setting key must not be empty".to_string())?;
    current
        .as_object_mut()
        .ok_or_else(|| format!("expected object while setting '{key}'"))?
        .insert((*leaf).to_string(), value);
    Ok(())
}

/// Handle a single `Handshake::SettingsRpc` client.
///
/// Prototype channel for nteract/desktop#1598. Runs alongside the existing
/// Automerge `SettingsSync` handler against the same `SettingsDoc` and the
/// same `settings_changed` broadcast. The Automerge path is the source of
/// truth for now; this channel is opt-in and additive.
///
/// Wire shape:
/// 1. On connect: server sends one `Snapshot`.
/// 2. Loop: select between client `SetSetting` requests and `settings_changed`
///    broadcast ticks. Each `SetSetting` is applied via
///    `SettingsDoc::put_value`, persisted, broadcast, and acked. Each
///    broadcast tick causes a fresh `Snapshot` to go out.
pub async fn handle_settings_rpc_connection<R, W>(
    mut reader: R,
    mut writer: W,
    settings: Arc<RwLock<SettingsDoc>>,
    changed_tx: broadcast::Sender<()>,
    mut changed_rx: broadcast::Receiver<()>,
    automerge_path: PathBuf,
    json_path: PathBuf,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    info!("[settings-rpc] New client connected");

    // Initial snapshot. Build with a short read lock so we don't hold the
    // guard across the socket write.
    let initial = {
        let doc = settings.read().await;
        build_snapshot_message(&doc)?
    };
    connection::send_json_frame(&mut writer, &initial).await?;

    loop {
        tokio::select! {
            // Inbound client message.
            result = connection::recv_json_frame::<_, SettingsRpcClientMessage>(&mut reader) => {
                match result? {
                    Some(SettingsRpcClientMessage::SetSetting { key, value }) => {
                        debug!("[settings-rpc] SetSetting key={key} value={value}");

                        // Apply + persist + build the post-write snapshot
                        // under a single write-lock scope. Don't hold the
                        // RwLock guard across `.await`. Compare heads so
                        // no-op writes (same value, unsupported value
                        // shape silently ignored by `put_value`) don't
                        // wake the `settings_changed` subscribers — the
                        // existing Automerge handler enforces the same
                        // invariant against pool-warming churn (#2120).
                        type ApplyOk = (SettingsRpcServerMessage, bool);
                        let apply_result: Result<ApplyOk, String> = {
                            let mut doc = settings.write().await;
                            match validate_settings_rpc_write(&doc, &key, &value) {
                                Err(e) => Err(e),
                                Ok(expected_change) => {
                                    let before = doc.heads();
                                    if expected_change {
                                        doc.put_value(&key, &value);
                                    }
                                    let after = doc.heads();
                                    let doc_changed = before != after;

                                    if expected_change && !doc_changed {
                                        Err(format!(
                                            "setting '{key}' is valid but is not supported by the SettingsDoc writer"
                                        ))
                                    } else {
                                        let persist_result = if doc_changed {
                                            try_persist_settings(&mut doc, &automerge_path, &json_path)
                                        } else {
                                            Ok(())
                                        };
                                        persist_result.and_then(|()| {
                                            build_snapshot_message(&doc)
                                                .map(|snapshot| (snapshot, doc_changed))
                                                .map_err(|e| e.to_string())
                                        })
                                    }
                                }
                            }
                        };

                        match apply_result {
                            Ok((snapshot, doc_changed)) => {
                                // Always echo the post-write snapshot to the
                                // writer so set-and-read patterns see a
                                // consistent view, even on no-op writes.
                                connection::send_json_frame(&mut writer, &snapshot).await?;
                                if doc_changed {
                                    // Fan out to peers; our own broadcast
                                    // tick will fire on the next select
                                    // iteration and resend the same
                                    // snapshot — harmless, the client
                                    // treats a duplicate snapshot as a
                                    // no-op refresh.
                                    let _ = changed_tx.send(());
                                }
                                let ack = SettingsRpcServerMessage::SetSettingAck {
                                    ok: true,
                                    error: None,
                                };
                                connection::send_json_frame(&mut writer, &ack).await?;
                            }
                            Err(e) => {
                                let ack = SettingsRpcServerMessage::SetSettingAck {
                                    ok: false,
                                    error: Some(e),
                                };
                                connection::send_json_frame(&mut writer, &ack).await?;
                            }
                        }
                    }
                    None => {
                        info!("[settings-rpc] Client disconnected");
                        return Ok(());
                    }
                }
            }

            // Settings changed elsewhere (this client's own write, the
            // Automerge sync handler, or the `settings.json` watcher).
            // Push a fresh snapshot.
            tick = changed_rx.recv() => {
                match tick {
                    Ok(()) => {
                        let snapshot = {
                            let doc = settings.read().await;
                            build_snapshot_message(&doc)?
                        };
                        connection::send_json_frame(&mut writer, &snapshot).await?;
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // Broadcast queue overflowed. Resync with current
                        // state instead of giving up.
                        debug!("[settings-rpc] broadcast lagged by {n}, resyncing");
                        let snapshot = {
                            let doc = settings.read().await;
                            build_snapshot_message(&doc)?
                        };
                        connection::send_json_frame(&mut writer, &snapshot).await?;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return Ok(());
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_doc::{ColorTheme, ThemeMode};

    fn validation_error(key: &str, value: serde_json::Value) -> String {
        let doc = SettingsDoc::new();
        validate_settings_rpc_write(&doc, key, &value).expect_err("write should be rejected")
    }

    #[test]
    fn settings_rpc_validation_accepts_supported_changes() {
        let doc = SettingsDoc::new();

        assert!(
            validate_settings_rpc_write(&doc, "theme", &serde_json::json!("dark")).unwrap(),
            "changing a scalar enum should report a document change"
        );
        assert!(
            validate_settings_rpc_write(
                &doc,
                "uv.default_packages",
                &serde_json::json!(["numpy", "pandas"])
            )
            .unwrap(),
            "changing a package list should report a document change"
        );
        assert!(
            validate_settings_rpc_write(&doc, "keep_alive_secs", &serde_json::json!(60)).unwrap(),
            "changing a bounded number should report a document change"
        );
    }

    #[test]
    fn settings_rpc_validation_detects_no_op_writes() {
        let doc = SettingsDoc::new();

        assert!(
            !validate_settings_rpc_write(&doc, "theme", &serde_json::json!(ThemeMode::System))
                .unwrap(),
            "writing the current theme should be a no-op"
        );
        assert!(
            !validate_settings_rpc_write(
                &doc,
                "color_theme",
                &serde_json::json!(ColorTheme::Classic)
            )
            .unwrap(),
            "writing the current color theme should be a no-op"
        );
    }

    #[test]
    fn settings_rpc_validation_rejects_unknown_keys_and_bad_types() {
        let unknown = validation_error("theme.typo", serde_json::json!("dark"));
        assert!(unknown.contains("unknown setting"));

        let bad_theme = validation_error("theme", serde_json::json!("midnight"));
        assert!(bad_theme.contains("invalid value"));

        let bad_bool = validation_error("telemetry_enabled", serde_json::json!("false"));
        assert!(bad_bool.contains("invalid value"));

        let bad_timestamp =
            validation_error("telemetry_last_app_ping_at", serde_json::json!("yesterday"));
        assert!(bad_timestamp.contains("must be null or an unsigned integer timestamp"));
    }

    #[test]
    fn settings_rpc_validation_rejects_out_of_range_numbers() {
        let too_short = validation_error("keep_alive_secs", serde_json::json!(1));
        assert!(too_short.contains("keep_alive_secs must be between"));

        let too_large_pool = validation_error("uv_pool_size", serde_json::json!(MAX_POOL_SIZE + 1));
        assert!(too_large_pool.contains("uv_pool_size must be between"));

        let wrong_pool_type = validation_error("conda_pool_size", serde_json::json!("3"));
        assert!(wrong_pool_type.contains("conda_pool_size must be an unsigned integer"));
    }

    #[test]
    fn settings_rpc_validation_rejects_malformed_package_lists() {
        let not_array = validation_error("uv.default_packages", serde_json::json!("numpy"));
        assert!(not_array.contains("must be an array of package strings"));

        let non_string =
            validation_error("conda.default_packages", serde_json::json!(["numpy", 123]));
        assert!(non_string.contains("must be an array of package strings"));

        let invalid_spec =
            validation_error("pixi.default_packages", serde_json::json!(["[\"numpy\""]));
        assert!(!invalid_spec.is_empty());
    }
}
