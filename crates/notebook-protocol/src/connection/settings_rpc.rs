//! Wire types for the `Handshake::SettingsRpc` channel.
//!
//! Prototype for the design described in nteract/desktop#1598. The daemon
//! owns `settings.json` as canonical state; clients receive a full snapshot
//! whenever it changes and send scalar `SetSetting` writes against the
//! daemon's current view.
//!
//! Only the fields that matter on the wire live here. The `SyncedSettings`
//! shape itself is defined in `runtimed-client::settings_doc` and rides this
//! channel as opaque JSON to avoid pulling that crate into the protocol
//! crate.

use serde::{Deserialize, Serialize};

/// Server -> Client. Sent on connect and on every settings change.
///
/// `settings` carries a serialized `SyncedSettings` snapshot. The shape is
/// defined by the daemon; clients deserialize against their own
/// `SyncedSettings` definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettingsRpcServerMessage {
    /// Full settings snapshot.
    Snapshot {
        /// Serialized `SyncedSettings` JSON object.
        settings: serde_json::Value,
    },
    /// Acknowledgement of a `SetSetting` request.
    SetSettingAck {
        /// True if the write was applied. False on validation/persist error.
        ok: bool,
        /// Human-readable error description on failure.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

/// Client -> Server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SettingsRpcClientMessage {
    /// Set a single scalar setting against the daemon's current snapshot.
    ///
    /// `key` may be a top-level field (`"theme"`) or a dotted path into a
    /// nested map (`"uv.default_packages"`), matching the existing
    /// `SettingsDoc::put_value` accepted shapes.
    SetSetting {
        key: String,
        value: serde_json::Value,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_snapshot_serializes_with_type_tag() {
        let msg = SettingsRpcServerMessage::Snapshot {
            settings: serde_json::json!({"theme": "dark"}),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains(r#""type":"snapshot""#));
        assert!(json.contains(r#""theme":"dark""#));
    }

    #[test]
    fn server_ack_omits_none_error() {
        let msg = SettingsRpcServerMessage::SetSettingAck {
            ok: true,
            error: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(json, r#"{"type":"set_setting_ack","ok":true}"#);
    }

    #[test]
    fn server_ack_round_trips_error() {
        let msg = SettingsRpcServerMessage::SetSettingAck {
            ok: false,
            error: Some("invalid value".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: SettingsRpcServerMessage = serde_json::from_str(&json).unwrap();
        match back {
            SettingsRpcServerMessage::SetSettingAck { ok, error } => {
                assert!(!ok);
                assert_eq!(error.as_deref(), Some("invalid value"));
            }
            _ => panic!("expected ack"),
        }
    }

    #[test]
    fn client_set_setting_round_trip() {
        let msg = SettingsRpcClientMessage::SetSetting {
            key: "theme".into(),
            value: serde_json::Value::String("dark".into()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        let back: SettingsRpcClientMessage = serde_json::from_str(&json).unwrap();
        match back {
            SettingsRpcClientMessage::SetSetting { key, value } => {
                assert_eq!(key, "theme");
                assert_eq!(value, serde_json::Value::String("dark".into()));
            }
        }
    }
}
