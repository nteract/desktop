//! Unified connection framing and handshake for the runtimed socket.
//!
//! Every connection begins with a 5-byte preamble:
//!
//! ```text
//! [4 bytes: magic 0xC0DE01AC] [1 byte: protocol version]
//! ```
//!
//! Followed by length-prefixed binary framing:
//!
//! ```text
//! [4 bytes: payload length (big-endian u32)] [payload bytes]
//! ```
//!
//! The first frame after the preamble is a JSON handshake declaring the
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

/// Maximum frame body size for `Presence` frames: 1 MiB.
/// Presence payloads are cursor positions, selection ranges, and small
/// peer-state snapshots encoded as CBOR. Per-peer they're hundreds of
/// bytes; a snapshot with many peers is still well under 1 MiB. A cap
/// here catches a desync that happened to land on the Presence channel
/// (e.g. a garbage length of tens of MiB) far below the 100 MiB outer
/// ceiling.
///
/// We deliberately do *not* apply a tight cap to Request/Response/
/// Broadcast: those carry legitimately-large payloads —
/// `SendComm` / `Broadcast::Comm` widget envelopes can include binary
/// buffers; `Response::DocBytes` returns serialized Automerge doc
/// state — and they already share the 100 MiB outer ceiling with data
/// frames.
const MAX_PRESENCE_FRAME_SIZE: usize = 1024 * 1024;

/// Maximum body size allowed for a given `NotebookFrameType` byte.
///
/// Enforced in `recv_typed_frame` so a corrupted stream with an
/// oversized length prefix for a narrow-purpose channel (Presence)
/// trips this check before the allocator honors the bogus length. The
/// outer 100 MiB ceiling still gates everything else.
fn max_payload_size_for_frame_type(type_byte: u8) -> usize {
    use notebook_doc::frame_types;
    match type_byte {
        frame_types::PRESENCE => MAX_PRESENCE_FRAME_SIZE,
        // Every other type — Request/Response/Broadcast (may carry
        // widget buffers or DocBytes), AutomergeSync/RuntimeStateSync/
        // PoolStateSync (Automerge sync), and unknown future types —
        // shares the 100 MiB outer ceiling.
        _ => MAX_FRAME_SIZE,
    }
}

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
    /// The optional `protocol` field is accepted for future version negotiation.
    /// Currently only v2 (typed frames) is supported. After handshake, the server
    /// sends a `ProtocolCapabilities` response before starting sync.
    NotebookSync {
        notebook_id: String,
        /// Protocol version requested by client. Currently only v2 is supported.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        protocol: Option<String>,
        /// Working directory for untitled notebooks (used for project file detection).
        /// When a notebook_id is a UUID (untitled), this provides the directory context
        /// for finding pyproject.toml, pixi.toml, or environment.yaml.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        /// Serialized NotebookMetadataSnapshot JSON, sent with the initial handshake
        /// so the daemon can read kernelspec before auto-launching a kernel.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        initial_metadata: Option<String>,
    },
    /// Blob store: write blobs, query port.
    Blob,

    /// Open an existing notebook file. Daemon loads from disk, derives notebook_id.
    ///
    /// The daemon returns `NotebookConnectionInfo` before starting sync.
    /// After that, the connection becomes a normal notebook sync connection.
    OpenNotebook {
        /// Path to the .ipynb file.
        path: String,
    },

    /// Runtime agent handshake. Sent by the coordinator to a spawned runtime
    /// agent subprocess on its stdin. The runtime agent reads this, bootstraps
    /// its RuntimeStateDoc, and begins processing kernel requests.
    RuntimeAgent {
        /// Notebook room to attach to.
        notebook_id: String,
        /// Unique runtime agent identifier (e.g., "runtime-agent:a1b2c3d4").
        runtime_agent_id: String,
        /// Filesystem path to the shared blob store root
        /// (e.g., "~/.cache/runt/blobs/").
        blob_root: String,
    },

    /// Create a new untitled notebook. Daemon creates empty room, generates env_id.
    ///
    /// The daemon returns `NotebookConnectionInfo` before starting sync.
    /// After that, the connection becomes a normal notebook sync connection.
    CreateNotebook {
        /// Runtime type: "python" or "deno".
        runtime: String,
        /// Working directory for project file detection (pyproject.toml, pixi.toml, environment.yml).
        /// Used since untitled notebooks have no path to derive working_dir from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        /// Optional notebook_id hint for restoring an untitled notebook from a previous session.
        /// If provided and the daemon has a persisted Automerge doc for this ID, the room is
        /// reused instead of creating a fresh empty notebook. If the persisted doc doesn't exist,
        /// a new notebook is created and this ID is used as the notebook_id/env_id.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notebook_id: Option<String>,
        /// When true, the notebook exists only in memory — no .automerge persisted to disk.
        /// Defaults to false (backward compat). MCP agents use true for scratch compute.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ephemeral: Option<bool>,
    },
}

/// Protocol version constant (string for backwards compatibility).
pub const PROTOCOL_V2: &str = "v2";

/// Numeric protocol version for version negotiation.
/// Increment this when making breaking protocol changes.
///
/// The request/response envelope change (`NotebookRequestEnvelope` /
/// `NotebookResponseEnvelope` with an optional `id` correlation field)
/// is JSON-wire-compatible — the id is `Option<String>` and serde
/// ignores unknown fields by default — so mixed v2/new deployments
/// interoperate. No version bump is needed for that change.
pub const PROTOCOL_VERSION: u32 = 2;

/// Magic bytes identifying the runtimed protocol.
/// Sent as the first 4 bytes of every connection, before the handshake frame.
pub const MAGIC: [u8; 4] = [0xC0, 0xDE, 0x01, 0xAC];

/// Total preamble size: 4-byte magic + 1-byte protocol version.
pub const PREAMBLE_LEN: usize = 5;

/// Send the connection preamble (magic bytes + protocol version).
///
/// Must be called once at the start of every connection, before
/// the handshake frame.
pub async fn send_preamble<W: AsyncWrite + Unpin>(writer: &mut W) -> std::io::Result<()> {
    const _: () = assert!(
        PROTOCOL_VERSION <= u8::MAX as u32,
        "PROTOCOL_VERSION must fit in a single byte for the wire preamble"
    );
    let mut buf = [0u8; PREAMBLE_LEN];
    buf[..4].copy_from_slice(&MAGIC);
    buf[4] = PROTOCOL_VERSION as u8;
    writer.write_all(&buf).await?;
    writer.flush().await?;
    Ok(())
}

/// Receive and validate the connection preamble.
///
/// Returns the protocol version byte. Returns an error if the magic bytes
/// don't match or the protocol version is incompatible.
pub async fn recv_preamble<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<u8> {
    let mut buf = [0u8; PREAMBLE_LEN];
    match reader.read_exact(&mut buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "connection closed before preamble",
            ));
        }
        Err(e) => return Err(e),
    }

    if buf[..4] != MAGIC {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "invalid magic bytes: expected {:02X?}, got {:02X?}",
                MAGIC,
                &buf[..4]
            ),
        ));
    }

    let version = buf[4];
    if version != PROTOCOL_VERSION as u8 {
        let direction = if (version as u32) > PROTOCOL_VERSION {
            "The daemon is newer than this client. Please update the CLI (or reinstall the app)."
        } else {
            "The daemon is older than this client. Please update the daemon: runt daemon doctor --fix"
        };
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "protocol version mismatch: daemon has v{}, client expects v{}. {}",
                version, PROTOCOL_VERSION, direction
            ),
        ));
    }

    Ok(version)
}

/// Server response indicating protocol capabilities.
///
/// Sent immediately after handshake, before starting sync.
/// Used by the `NotebookSync` handshake variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolCapabilities {
    /// Protocol version string (currently always "v2").
    pub protocol: String,
    /// Numeric protocol version for explicit version checking.
    /// Clients can compare this against their expected version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
    /// Daemon version string (e.g., "2.0.0+abc123").
    /// Useful for debugging version mismatches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<String>,
}

/// Server response for `OpenNotebook` and `CreateNotebook` handshakes.
///
/// Sent immediately after handshake, before starting sync.
/// Contains notebook_id derived by the daemon (from path or generated env_id).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotebookConnectionInfo {
    /// Protocol version string (currently always "v2").
    pub protocol: String,
    /// Numeric protocol version for explicit version checking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol_version: Option<u32>,
    /// Daemon version string (e.g., "2.0.0+abc123").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub daemon_version: Option<String>,
    /// Notebook identifier derived by the daemon.
    /// For existing files: canonical path.
    /// For new notebooks: generated UUID (env_id).
    pub notebook_id: String,
    /// Number of cells in the notebook (for progress indication).
    pub cell_count: usize,
    /// True if the notebook has untrusted dependencies requiring user approval.
    pub needs_trust_approval: bool,
    /// Error message if the notebook could not be opened/created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Whether this notebook is ephemeral (in-memory only, no persistence).
    #[serde(default)]
    pub ephemeral: bool,
}

/// Frame types for notebook sync connections.
///
/// The first byte of each frame payload indicates the type of message.
/// The byte values are defined in `notebook_doc::frame_types` so all
/// consumers (daemon, WASM, Python) share one source of truth.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NotebookFrameType {
    /// Automerge sync message (binary).
    AutomergeSync = notebook_doc::frame_types::AUTOMERGE_SYNC,
    /// NotebookRequest (JSON).
    Request = notebook_doc::frame_types::REQUEST,
    /// NotebookResponse (JSON).
    Response = notebook_doc::frame_types::RESPONSE,
    /// NotebookBroadcast (JSON).
    Broadcast = notebook_doc::frame_types::BROADCAST,
    /// Presence (CBOR, see notebook_doc::presence).
    Presence = notebook_doc::frame_types::PRESENCE,
    /// RuntimeStateDoc sync message (binary Automerge sync).
    RuntimeStateSync = notebook_doc::frame_types::RUNTIME_STATE_SYNC,
    /// PoolDoc sync message (binary Automerge sync, global).
    PoolStateSync = notebook_doc::frame_types::POOL_STATE_SYNC,
}

impl TryFrom<u8> for NotebookFrameType {
    type Error = std::io::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        use notebook_doc::frame_types;
        match value {
            frame_types::AUTOMERGE_SYNC => Ok(Self::AutomergeSync),
            frame_types::REQUEST => Ok(Self::Request),
            frame_types::RESPONSE => Ok(Self::Response),
            frame_types::BROADCAST => Ok(Self::Broadcast),
            frame_types::PRESENCE => Ok(Self::Presence),
            frame_types::RUNTIME_STATE_SYNC => Ok(Self::RuntimeStateSync),
            frame_types::POOL_STATE_SYNC => Ok(Self::PoolStateSync),
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
/// Unknown frame types are logged and skipped for forward compatibility.
///
/// Length is read first, then the 1-byte type discriminator, then the
/// per-type cap is applied before the body is read. This means a
/// garbage length prefix aimed at, say, the `Request` channel (e.g.
/// 1.8 GB) is rejected before the allocator tries to honor it.
pub async fn recv_typed_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<TypedNotebookFrame>> {
    loop {
        // Read the 4-byte length prefix.
        let mut len_buf = [0u8; 4];
        match reader.read_exact(&mut len_buf).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(e),
        }
        let len = u32::from_be_bytes(len_buf) as usize;

        if len == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "empty frame",
            ));
        }
        // Outer ceiling before we even look at the type byte — 100 MiB.
        if len > MAX_FRAME_SIZE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("frame too large: {} bytes (max {})", len, MAX_FRAME_SIZE),
            ));
        }

        // Read the 1-byte type discriminator.
        let mut type_buf = [0u8; 1];
        reader.read_exact(&mut type_buf).await?;
        let type_byte = type_buf[0];
        let body_len = len - 1;

        // Per-type ceiling. Control frames (Request/Response/Broadcast/
        // Presence) cap at 1 MiB / 64 KiB; data frames keep 100 MiB.
        let max_body = max_payload_size_for_frame_type(type_byte);
        if body_len > max_body {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "frame too large for type 0x{:02x}: {} bytes (max {})",
                    type_byte, body_len, max_body
                ),
            ));
        }

        // Now it's safe to allocate and read the body.
        let mut payload = vec![0u8; body_len];
        reader.read_exact(&mut payload).await?;

        match NotebookFrameType::try_from(type_byte) {
            Ok(frame_type) => {
                return Ok(Some(TypedNotebookFrame {
                    frame_type,
                    payload,
                }));
            }
            Err(_) => {
                log::warn!(
                    "Skipping unknown notebook frame type 0x{:02x} ({} bytes payload)",
                    type_byte,
                    body_len,
                );
                continue;
            }
        }
    }
}

/// Send a length-prefixed frame.
///
/// Returns an error if the payload exceeds `MAX_FRAME_SIZE` (100 MiB).
/// This prevents silent truncation of the 4-byte length field at the u32
/// boundary and keeps send/receive limits symmetric.
pub async fn send_frame<W: AsyncWrite + Unpin>(writer: &mut W, data: &[u8]) -> std::io::Result<()> {
    if data.len() > MAX_FRAME_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "frame too large to send: {} bytes (max {})",
                data.len(),
                MAX_FRAME_SIZE
            ),
        ));
    }
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
#[allow(clippy::unwrap_used, clippy::expect_used)]
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
    async fn test_frame_too_large_recv() {
        let len_bytes = (MAX_FRAME_SIZE as u32 + 1).to_be_bytes();
        let mut cursor = std::io::Cursor::new(len_bytes.to_vec());
        let result = recv_frame(&mut cursor).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_frame_too_large_send() {
        let data = vec![0u8; MAX_FRAME_SIZE + 1];
        let mut buf = Vec::new();
        let result = send_frame(&mut buf, &data).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
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
    async fn test_preamble_roundtrip() {
        let mut buf = Vec::new();
        send_preamble(&mut buf).await.unwrap();
        assert_eq!(buf.len(), PREAMBLE_LEN);
        assert_eq!(&buf[..4], &MAGIC);
        assert_eq!(buf[4], PROTOCOL_VERSION as u8);

        let mut cursor = std::io::Cursor::new(buf);
        let version = recv_preamble(&mut cursor).await.unwrap();
        assert_eq!(version, PROTOCOL_VERSION as u8);
    }

    #[tokio::test]
    async fn test_preamble_bad_magic() {
        let buf = [0xFF, 0xFF, 0xFF, 0xFF, PROTOCOL_VERSION as u8];
        let mut cursor = std::io::Cursor::new(buf.to_vec());
        let result = recv_preamble(&mut cursor).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_preamble_version_mismatch() {
        let mut buf = [0u8; PREAMBLE_LEN];
        buf[..4].copy_from_slice(&MAGIC);
        buf[4] = 99; // wrong version
        let mut cursor = std::io::Cursor::new(buf.to_vec());
        let result = recv_preamble(&mut cursor).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_preamble_eof() {
        let buf: &[u8] = &[0xC0, 0xDE]; // incomplete
        let mut cursor = std::io::Cursor::new(buf);
        let result = recv_preamble(&mut cursor).await;
        assert!(result.is_err());
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::UnexpectedEof
        );
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
            initial_metadata: None,
        })
        .unwrap();
        assert_eq!(json, r#"{"channel":"notebook_sync","notebook_id":"abc"}"#);

        // NotebookSync with v2 protocol
        let json = serde_json::to_string(&Handshake::NotebookSync {
            notebook_id: "abc".into(),
            protocol: Some("v2".into()),
            working_dir: None,
            initial_metadata: None,
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
            initial_metadata: None,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"notebook_sync","notebook_id":"550e8400-e29b-41d4-a716-446655440000","protocol":"v2","working_dir":"/home/user/project"}"#
        );

        // Blob
        let json = serde_json::to_string(&Handshake::Blob).unwrap();
        assert_eq!(json, r#"{"channel":"blob"}"#);

        // OpenNotebook
        let json = serde_json::to_string(&Handshake::OpenNotebook {
            path: "/home/user/notebook.ipynb".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"open_notebook","path":"/home/user/notebook.ipynb"}"#
        );

        // CreateNotebook without working_dir
        let json = serde_json::to_string(&Handshake::CreateNotebook {
            runtime: "python".into(),
            working_dir: None,
            notebook_id: None,
            ephemeral: None,
        })
        .unwrap();
        assert_eq!(json, r#"{"channel":"create_notebook","runtime":"python"}"#);

        // CreateNotebook with working_dir
        let json = serde_json::to_string(&Handshake::CreateNotebook {
            runtime: "deno".into(),
            working_dir: Some("/home/user/project".into()),
            notebook_id: None,
            ephemeral: None,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"create_notebook","runtime":"deno","working_dir":"/home/user/project"}"#
        );

        // CreateNotebook with notebook_id hint (session restore)
        let json = serde_json::to_string(&Handshake::CreateNotebook {
            runtime: "python".into(),
            working_dir: None,
            notebook_id: Some("550e8400-e29b-41d4-a716-446655440000".into()),
            ephemeral: None,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"channel":"create_notebook","runtime":"python","notebook_id":"550e8400-e29b-41d4-a716-446655440000"}"#
        );
    }

    #[test]
    fn test_notebook_connection_info_serialization() {
        // Success case (minimal - no optional fields)
        let info = NotebookConnectionInfo {
            protocol: "v2".into(),
            protocol_version: None,
            daemon_version: None,
            notebook_id: "/home/user/notebook.ipynb".into(),
            cell_count: 5,
            needs_trust_approval: false,
            error: None,
            ephemeral: false,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert_eq!(
            json,
            r#"{"protocol":"v2","notebook_id":"/home/user/notebook.ipynb","cell_count":5,"needs_trust_approval":false,"ephemeral":false}"#
        );

        // With version info
        let info = NotebookConnectionInfo {
            protocol: PROTOCOL_V2.into(),
            protocol_version: Some(PROTOCOL_VERSION),
            daemon_version: Some("0.1.0+abc123".into()),
            notebook_id: "/home/user/notebook.ipynb".into(),
            cell_count: 5,
            needs_trust_approval: false,
            error: None,
            ephemeral: false,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(&format!(r#""protocol_version":{}"#, PROTOCOL_VERSION)));
        assert!(json.contains(r#""daemon_version":"0.1.0+abc123""#));

        // With trust approval needed
        let info = NotebookConnectionInfo {
            protocol: "v2".into(),
            protocol_version: None,
            daemon_version: None,
            notebook_id: "550e8400-e29b-41d4-a716-446655440000".into(),
            cell_count: 1,
            needs_trust_approval: true,
            error: None,
            ephemeral: false,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(r#""needs_trust_approval":true"#));

        // Error case
        let info = NotebookConnectionInfo {
            protocol: "v2".into(),
            protocol_version: None,
            daemon_version: None,
            notebook_id: String::new(),
            cell_count: 0,
            needs_trust_approval: false,
            error: Some("File not found".into()),
            ephemeral: false,
        };
        let json = serde_json::to_string(&info).unwrap();
        assert!(json.contains(r#""error":"File not found""#));
    }

    #[tokio::test]
    async fn typed_frame_rejects_oversized_presence() {
        // Presence frames have a 1 MiB cap because cursor positions /
        // CBOR peer snapshots never legitimately approach that. A
        // desync that happens to land on the Presence channel with a
        // multi-MiB length header is caught here instead of trying to
        // allocate it.
        let body_len: u32 = (MAX_PRESENCE_FRAME_SIZE as u32) + 1;
        let total_len: u32 = body_len + 1;
        let mut buf = Vec::new();
        buf.extend_from_slice(&total_len.to_be_bytes());
        buf.push(notebook_doc::frame_types::PRESENCE);
        let mut cursor = std::io::Cursor::new(buf);
        let err = recv_typed_frame(&mut cursor).await.unwrap_err();
        assert!(err.to_string().contains("too large for type 0x04"));
    }

    #[tokio::test]
    async fn typed_frame_allows_big_broadcast_for_widget_comm() {
        // NotebookBroadcast::Comm and NotebookRequest::SendComm carry
        // widget envelopes with inline binary buffers that can exceed
        // 1 MiB. Response::DocBytes similarly carries a serialized
        // Automerge doc. These must NOT be capped tightly — they share
        // the 100 MiB outer ceiling with data frames.
        let big_payload = vec![0x42u8; 2 * 1024 * 1024]; // 2 MiB
        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::Broadcast, &big_payload)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::Broadcast);
        assert_eq!(frame.payload.len(), big_payload.len());
    }

    #[tokio::test]
    async fn typed_frame_allows_big_automerge_sync() {
        let big_payload = vec![0x42u8; 2 * 1024 * 1024]; // 2 MiB
        let mut buf = Vec::new();
        send_typed_frame(&mut buf, NotebookFrameType::AutomergeSync, &big_payload)
            .await
            .unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let frame = recv_typed_frame(&mut cursor).await.unwrap().unwrap();
        assert_eq!(frame.frame_type, NotebookFrameType::AutomergeSync);
        assert_eq!(frame.payload.len(), big_payload.len());
    }

    #[tokio::test]
    async fn typed_frame_rejects_1819243560_byte_length() {
        // The specific desync value observed in the field: 0x6C6F6761
        // ("loga"). Interpreted as a u32 big-endian length this is
        // 1,819,243,560 bytes — well above the 100 MiB outer cap.
        // This must be rejected at the outer check before we ever read
        // the type byte.
        let loga_bytes: [u8; 4] = [0x6C, 0x6F, 0x67, 0x61];
        let mut cursor = std::io::Cursor::new(loga_bytes.to_vec());
        let err = recv_typed_frame(&mut cursor).await.unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("frame too large"), "unexpected error: {msg}");
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
