//! Core Jupyter message types for WebView communication.

use bytes::Bytes;
use jupyter_protocol::{Channel, Header, JupyterMessage, JupyterMessageContent};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::base64::{deserialize_buffers, serialize_buffers};

/// Error type for message conversion failures.
#[derive(Debug, thiserror::Error)]
pub enum ConversionError {
    #[error("Failed to parse message content: {0}")]
    ContentParseError(#[from] anyhow::Error),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),
}

/// Intermediate struct for deserializing incoming Jupyter messages from WebView.
///
/// This struct handles the JSON format used by frontend applications where:
/// - `content` is passed as raw JSON (parsed later based on `msg_type`)
/// - `buffers` are base64-encoded strings
/// - `parent_header` may be empty `{}`, `null`, or a valid header
#[derive(Debug, Clone, Deserialize)]
pub struct RawJupyterMessage {
    pub header: Header,

    #[serde(
        default,
        deserialize_with = "jupyter_protocol::deserialize_parent_header"
    )]
    pub parent_header: Option<Header>,

    #[serde(default)]
    pub metadata: Value,

    /// Raw JSON content, to be parsed based on header.msg_type
    pub content: Value,

    #[serde(default, deserialize_with = "deserialize_buffers")]
    pub buffers: Vec<Bytes>,

    #[serde(default)]
    pub channel: Option<Channel>,
}

impl TryFrom<RawJupyterMessage> for JupyterMessage {
    type Error = ConversionError;

    fn try_from(raw: RawJupyterMessage) -> Result<Self, Self::Error> {
        let content =
            JupyterMessageContent::from_type_and_content(&raw.header.msg_type, raw.content)?;

        Ok(JupyterMessage {
            zmq_identities: Vec::new(),
            header: raw.header,
            parent_header: raw.parent_header,
            metadata: raw.metadata,
            content,
            buffers: raw.buffers,
            channel: raw.channel,
        })
    }
}

/// Serializable Jupyter message for sending to WebView frontends.
///
/// This struct omits `zmq_identities` (not needed for WebView) and
/// serializes buffers as base64-encoded strings.
#[derive(Debug, Clone, Serialize)]
pub struct WebViewJupyterMessage {
    pub header: Header,
    pub parent_header: Option<Header>,
    pub metadata: Value,
    pub content: JupyterMessageContent,

    #[serde(serialize_with = "serialize_buffers")]
    pub buffers: Vec<Bytes>,

    pub channel: Option<Channel>,
}

impl From<JupyterMessage> for WebViewJupyterMessage {
    fn from(msg: JupyterMessage) -> Self {
        WebViewJupyterMessage {
            header: msg.header,
            parent_header: msg.parent_header,
            metadata: msg.metadata,
            content: msg.content,
            buffers: msg.buffers,
            channel: msg.channel,
        }
    }
}

impl From<WebViewJupyterMessage> for JupyterMessage {
    fn from(msg: WebViewJupyterMessage) -> Self {
        JupyterMessage {
            zmq_identities: Vec::new(),
            header: msg.header,
            parent_header: msg.parent_header,
            metadata: msg.metadata,
            content: msg.content,
            buffers: msg.buffers,
            channel: msg.channel,
        }
    }
}

/// Custom Deserialize implementation that uses RawJupyterMessage internally.
///
/// This allows direct deserialization: `serde_json::from_str::<WebViewJupyterMessage>(...)`
impl<'de> Deserialize<'de> for WebViewJupyterMessage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawJupyterMessage::deserialize(deserializer)?;
        let content =
            JupyterMessageContent::from_type_and_content(&raw.header.msg_type, raw.content)
                .map_err(serde::de::Error::custom)?;

        Ok(WebViewJupyterMessage {
            header: raw.header,
            parent_header: raw.parent_header,
            metadata: raw.metadata,
            content,
            buffers: raw.buffers,
            channel: raw.channel,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn test_deserialize_empty_parent_header() {
        let msg = r#"
        {
            "header": {
                "date": "2025-05-14T14:32:23.490Z",
                "msg_id": "test-id",
                "msg_type": "kernel_info_request",
                "session": "test-session",
                "username": "",
                "version": "5.2"
            },
            "parent_header": {},
            "metadata": {},
            "content": {},
            "buffers": [],
            "channel": "shell"
        }
        "#;

        let raw: RawJupyterMessage = serde_json::from_str(msg).unwrap();
        assert!(raw.parent_header.is_none());
    }

    #[test]
    fn test_deserialize_null_parent_header() {
        let msg = r#"
        {
            "header": {
                "date": "2025-05-14T14:32:23.490Z",
                "msg_id": "test-id",
                "msg_type": "kernel_info_request",
                "session": "test-session",
                "username": "",
                "version": "5.2"
            },
            "parent_header": null,
            "metadata": {},
            "content": {},
            "buffers": [],
            "channel": "shell"
        }
        "#;

        let raw: RawJupyterMessage = serde_json::from_str(msg).unwrap();
        assert!(raw.parent_header.is_none());
    }

    #[test]
    fn test_convert_to_jupyter_message() {
        let msg = r#"
        {
            "header": {
                "date": "2025-05-14T14:32:23.490Z",
                "msg_id": "test-id",
                "msg_type": "kernel_info_request",
                "session": "test-session",
                "username": "",
                "version": "5.2"
            },
            "parent_header": {},
            "metadata": {},
            "content": {},
            "buffers": [],
            "channel": "shell"
        }
        "#;

        let raw: RawJupyterMessage = serde_json::from_str(msg).unwrap();
        let jupyter_msg: JupyterMessage = raw.try_into().unwrap();

        assert_eq!(jupyter_msg.header.msg_type, "kernel_info_request");
        assert!(jupyter_msg.zmq_identities.is_empty());
    }

    #[test]
    fn test_webview_message_roundtrip() {
        let msg = r#"
        {
            "header": {
                "date": "2025-05-14T14:32:23.490Z",
                "msg_id": "test-id",
                "msg_type": "kernel_info_request",
                "session": "test-session",
                "username": "",
                "version": "5.2"
            },
            "parent_header": {},
            "metadata": {},
            "content": {},
            "buffers": [],
            "channel": "shell"
        }
        "#;

        let wv_msg: WebViewJupyterMessage = serde_json::from_str(msg).unwrap();
        assert_eq!(wv_msg.header.msg_type, "kernel_info_request");

        // Convert to JupyterMessage and back
        let jupyter_msg: JupyterMessage = wv_msg.into();
        let wv_msg2 = WebViewJupyterMessage::from(jupyter_msg);
        assert_eq!(wv_msg2.header.msg_type, "kernel_info_request");
    }
}
