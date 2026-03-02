//! Unified connection framing and handshake for the runtimed socket.
//!
//! All connections to the daemon use length-prefixed binary framing:
//!
//! ```text
//! [4 bytes: payload length (big-endian u32)] [payload bytes]
//! ```
//!
//! The first frame on every connection is a JSON handshake declaring the
//! channel. After the handshake, the daemon routes the connection to the
//! appropriate handler.
//!
//! ## Notebook Sync Frame Types (Phase 8)
//!
//! Notebook sync connections use a typed frame format where the first byte
//! of the payload indicates the frame type:
//!
//! - `0x00`: Automerge sync message (binary)
//! - `0x01`: NotebookRequest (JSON)
//! - `0x02`: NotebookResponse (JSON)
//! - `0x03`: NotebookBroadcast (JSON)

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum frame size for data frames: 100 MiB (matches blob size limit).
const MAX_FRAME_SIZE: usize = 100 * 1024 * 1024;

/// Maximum frame size for control/handshake frames: 64 KiB.
/// Applied to the initial handshake and JSON request/response traffic
/// so that oversized frames can't force large allocations before channel
/// routing has occurred.
const MAX_CONTROL_FRAME_SIZE: usize = 64 * 1024;

/// Channel handshake — the first frame on every connection.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "channel", rename_all = "snake_case")]
pub enum Handshake {
    /// Pool IPC: environment take/return/status/ping.
    Pool,
    /// Automerge settings sync.
    SettingsSync,
    /// Automerge notebook sync (per-notebook room).
    ///
    /// The optional `protocol` field enables version negotiation:
    /// - Absent or "v1": Raw Automerge frames (legacy protocol)
    /// - "v2": Typed frames with first-byte type indicator
    ///
    /// After handshake, new servers send a `ProtocolCapabilities` response
    /// before starting sync. Old servers skip this and send raw Automerge frames.
    NotebookSync {
        notebook_id: String,
        /// Protocol version requested by client. Default is "v1" (raw frames).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<String>,
        /// Working directory for untitled notebooks (used for project file detection).
        /// When a notebook_id is a UUID (untitled), this provides the directory context
        /// for finding pyproject.toml, pixi.toml, or environment.yaml.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
    },
    /// Blob store: write blobs, query port.
    Blob,
    /// Pool state subscription: receive broadcasts when pool errors occur/clear.
    /// Read-only channel - server pushes DaemonBroadcast messages to client.
    PoolStateSubscribe,
}

/// Protocol version constants.
pub const PROTOCOL_V1: &str = "v1";
pub const PROTOCOL_V2: &str = "v2";

/// Server response indicating negotiated protocol capabilities.
///
/// Sent by new servers immediately after handshake, before starting sync.
/// Old servers don't send this (they start sync immediately).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolCapabilities {
    /// Negotiated protocol version: "v1" or "v2"
    pub protocol: String,
}

/// Frame types for notebook sync connections.
///
/// The first byte of each frame payload indicates the type of message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NotebookFrameType {
    /// Automerge sync message (binary).
    AutomergeSync = 0x00,
    /// NotebookRequest (JSON).
    Request = 0x01,
    /// NotebookResponse (JSON).
    Response = 0x02,
    /// NotebookBroadcast (JSON).
    Broadcast = 0x03,
}

impl TryFrom<u8> for NotebookFrameType {
    type Error = std::io::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x00 => Ok(Self::AutomergeSync),
            0x01 => Ok(Self::Request),
            0x02 => Ok(Self::Response),
            0x03 => Ok(Self::Broadcast),
            _ => Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown notebook frame type: 0x{:02x}", value),
            )),
        }
    }
}

/// A typed notebook frame with its type and payload.
#[derive(Debug)]
pub struct TypedNotebookFrame {
    pub frame_type: NotebookFrameType,
    pub payload: Vec<u8>,
}

/// Send a typed notebook frame.
pub async fn send_typed_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame_type: NotebookFrameType,
    payload: &[u8],
) -> std::io::Result<()> {
    let mut data = Vec::with_capacity(1 + payload.len());
    data.push(frame_type as u8);
    data.extend_from_slice(payload);
    send_frame(writer, &data).await
}

/// Send a typed notebook frame with JSON payload.
pub async fn send_typed_json_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    frame_type: NotebookFrameType,
    value: &T,
) -> anyhow::Result<()> {
    let json_bytes = serde_json::to_vec(value)?;
    send_typed_frame(writer, frame_type, &json_bytes).await?;
    Ok(())
}

/// Receive a typed notebook frame.
/// Returns `None` on clean disconnect (EOF).
pub async fn recv_typed_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<TypedNotebookFrame>> {
    let Some(data) = recv_frame(reader).await? else {
        return Ok(None);
    };

    if data.is_empty() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "empty frame",
        ));
    }

    let frame_type = NotebookFrameType::try_from(data[0])?;
    let payload = data[1..].to_vec();

    Ok(Some(TypedNotebookFrame {
        frame_type,
        payload,
    }))
}

/// Send a length-prefixed frame.
pub async fn send_frame<W: AsyncWrite + Unpin>(writer: &mut W, data: &[u8]) -> std::io::Result<()> {
    let len = (data.len() as u32).to_be_bytes();
    writer.write_all(&len).await?;
    writer.write_all(data).await?;
    writer.flush().await?;
    Ok(())
}

/// Receive a length-prefixed frame with a caller-specified size limit.
/// Returns `None` on clean disconnect (EOF).
async fn recv_frame_with_limit<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_size: usize,
) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes(len_buf) as usize;

    if len > max_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("frame too large: {} bytes (max {})", len, max_size),
        ));
    }

    let mut buf = vec![0u8; len];
    reader.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

/// Receive a length-prefixed frame (up to 100 MiB for data payloads).
/// Returns `None` on clean disconnect (EOF).
pub async fn recv_frame<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    recv_frame_with_limit(reader, MAX_FRAME_SIZE).await
}

/// Receive a length-prefixed frame with the control/handshake size limit
/// (64 KiB). Use this for handshake and JSON request/response traffic to
/// prevent oversized frames from forcing large allocations.
pub async fn recv_control_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<Vec<u8>>> {
    recv_frame_with_limit(reader, MAX_CONTROL_FRAME_SIZE).await
}

/// Send a value as a JSON-encoded length-prefixed frame.
pub async fn send_json_frame<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> anyhow::Result<()> {
    let data = serde_json::to_vec(value)?;
    send_frame(writer, &data).await?;
    Ok(())
}

/// Receive and deserialize a JSON-encoded length-prefixed frame.
/// Returns `None` on clean disconnect (EOF).
pub async fn recv_json_frame<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> anyhow::Result<Option<T>> {
    match recv_frame(reader).await? {
        Some(data) => {
            let value = serde_json::from_slice(&data)?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_frame_roundtrip() {
        let data = b"hello world";

        let mut buf = Vec::new();
        send_frame(&mut buf, data).await.unwrap();
        assert_eq!(buf.len(), 4 + data.len());

        let mut cursor = std::io::Cursor::new(buf);
        let received = recv_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(received, data);
    }

    #[tokio::test]
    async fn test_frame_eof() {
        let buf: &[u8] = &[];
        let mut cursor = std::io::Cursor::new(buf);
        let result = recv_frame(&mut cursor).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_frame_too_large() {
        let len_bytes = (MAX_FRAME_SIZE as u32 + 1).to_be_bytes();
        let mut cursor = std::io::Cursor::new(len_bytes.to_vec());
        let result = recv_frame(&mut cursor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_control_frame_rejects_oversized() {
        // A frame larger than 64 KiB should be rejected by recv_control_frame
        let oversized_len = (MAX_CONTROL_FRAME_SIZE as u32 + 1).to_be_bytes();
        let mut cursor = std::io::Cursor::new(oversized_len.to_vec());
        let result = recv_control_frame(&mut cursor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_control_frame_accepts_small() {
        let data = b"small control payload";
        let mut buf = Vec::new();
        send_frame(&mut buf, data).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let received = recv_control_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(received, data);
    }

    #[tokio::test]
    async fn test_json_frame_roundtrip() {
        let handshake = Handshake::Pool;

        let mut buf = Vec::new();
        send_json_frame(&mut buf, &handshake).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let received: Handshake = recv_json_frame(&mut cursor).await.unwrap().unwrap();
        assert!(matches!(received, Handshake::Pool));
    }

    #[tokio::test]
    async fn test_handshake_serialization() {
        // Pool
        let json = serde_json::to_string(&Handshake::Pool).unwrap();
        assert_eq!(json, r#"{"channel":"pool"}"#);

        // SettingsSync
        let json = serde_json::to_string(&Handshake::SettingsSync).unwrap();
        assert_eq!(json, r#"{"channel":"settings_sync"}"#);

        // NotebookSync (without protocol - should omit the field)
        let json = serde_json::to_string(&Handshake::NotebookSync {
            notebook_id: "abc".into(),
            protocol: None,
            working_dir: None,
        })
        .unwrap();
        assert_eq!(json, r#"{"channel":"notebook_sync","notebook_id":"abc"}"#);

        // NotebookSync with v2 protocol
        let json = serde_json::to_string(&Handshake::NotebookSync {
            notebook_id: "abc".into(),
            protocol: Some("v2".into()),
            working_dir: None,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"notebook_sync","notebook_id":"abc","protocol":"v2"}"#
        );

        // NotebookSync with working_dir for untitled notebook
        let json = serde_json::to_string(&Handshake::NotebookSync {
            notebook_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            protocol: Some("v2".into()),
            working_dir: Some("/home/user/project".into()),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"notebook_sync","notebook_id":"550e8400-e29b-41d4-a716-446655440000","protocol":"v2","working_dir":"/home/user/project"}"#
        );

        // Blob
        let json = serde_json::to_string(&Handshake::Blob).unwrap();
        assert_eq!(json, r#"{"channel":"blob"}"#);
    }

    #[tokio::test]
    async fn test_multiple_frames_on_same_stream() {
        let mut buf = Vec::new();
        send_frame(&mut buf, b"first").await.unwrap();
        send_frame(&mut buf, b"second").await.unwrap();
        send_frame(&mut buf, b"third").await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        assert_eq!(recv_frame(&mut cursor).await.unwrap().unwrap(), b"first");
        assert_eq!(recv_frame(&mut cursor).await.unwrap().unwrap(), b"second");
        assert_eq!(recv_frame(&mut cursor).await.unwrap().unwrap(), b"third");
        // EOF
        assert!(recv_frame(&mut cursor).await.unwrap().is_none());
    }

    #[test]
    fn test_notebook_frame_type_conversion() {
        assert_eq!(
            NotebookFrameType::try_from(0x00).unwrap(),
            NotebookFrameType::AutomergeSync
        );
        assert_eq!(
            NotebookFrameType::try_from(0x01).unwrap(),
            NotebookFrameType::Request
        );
        assert_eq!(
            NotebookFrameType::try_from(0x02).unwrap(),
            NotebookFrameType::Response
        );
        assert_eq!(
            NotebookFrameType::try_from(0x03).unwrap(),
            NotebookFrameType::Broadcast
        );
        assert!(NotebookFrameType::try_from(0xFF).is_err());
    }

    #[tokio::test]
    async fn test_typed_frame_roundtrip() {
        let payload = b"test payload";

        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::Request, payload)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::Request);
        assert_eq!(frame.payload, payload);
    }

    #[tokio::test]
    async fn test_typed_frame_automerge_sync() {
        let sync_data = b"\x00binary automerge data";

        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::AutomergeSync, sync_data)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::AutomergeSync);
        assert_eq!(frame.payload, sync_data);
    }

    #[tokio::test]
    async fn test_typed_json_frame() {
        #[derive(Debug, PartialEq, Serialize, Deserialize)]
        struct TestMsg {
            value: i32,
        }

        let msg = TestMsg { value: 42 };

        let mut buf = Vec::new();
        send_typed_json_frame(&mut buf, NotebookFrameType::Request, &msg)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::Request);

        let parsed: TestMsg = serde_json::from_slice(&frame.payload).unwrap();
        assert_eq!(parsed, msg);
    }
}
