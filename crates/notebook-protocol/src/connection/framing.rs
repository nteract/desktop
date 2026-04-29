//! Length-prefixed framing, preamble validation, and typed notebook frames.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use serde::{de::DeserializeOwned, Serialize};

use super::handshake::PROTOCOL_VERSION;

// ── Frame size limits ───────────────────────────────────────────────
//
// Every typed frame has a hard cap and a soft warn threshold. The cap
// rejects the frame outright (drops the connection); the warn threshold
// just logs so we see growth before a cap rejection is triggered in
// production.
//
// Caps are sized for *legitimate worst case*, not the typical case.
// They exist as defense-in-depth against:
//   1. parser/serde bugs that might let a giant payload land on the
//      wrong channel (e.g. kernel output bytes confused for a Request),
//   2. runaway senders OOMing the receiver,
//   3. corrupted length prefixes on the wire.
//
// They do not replace per-content offloading — large outputs and widget
// buffers still belong in the blob store.

const KIB: usize = 1024;
const MIB: usize = 1024 * 1024;

/// Outer ceiling for any frame. Matches the blob-store size limit and
/// the u32 length-prefix domain. Per-type caps below are tighter for
/// every channel.
pub(super) const MAX_FRAME_SIZE: usize = 100 * MIB;

/// Maximum frame size for control/handshake frames: 64 KiB.
/// Applied to the initial handshake (before per-type routing exists).
pub(super) const MAX_CONTROL_FRAME_SIZE: usize = 64 * KIB;

/// Per-type body cap and warn threshold for a typed frame.
#[derive(Debug, Clone, Copy)]
pub(super) struct FrameSizeLimits {
    /// Hard cap. Frames exceeding this are rejected.
    pub(super) cap: usize,
    /// Soft threshold. Frames exceeding this log a warning but proceed.
    pub(super) warn: usize,
}

/// Per-type body limits.
///
/// Sized for legitimate worst case per channel:
/// - **AutomergeSync** (NotebookDoc) and **RuntimeStateSync**: 64 MiB.
///   Initial sync of a notebook with many cells / lots of accumulated
///   ephemeral state can be many MB; outputs are blob-offloaded but
///   the doc itself can still be large.
/// - **NotebookRequest**: 16 MiB. Most variants are tiny (cell IDs,
///   metadata snapshots, completion requests) — they sit well under the
///   256 KiB warn so growth on them still surfaces. The cap exists for
///   one outlier: `SendComm` carries the same comm envelope shape as
///   `NotebookBroadcast::Comm`, including custom-message buffers from
///   `model.send(content, callbacks, buffers)`. JSON-encoding `Vec<Vec<u8>>`
///   inflates binary by ~4×, so a 256 KiB widget buffer becomes ~1 MiB
///   on the wire. Tightens once `SendComm.buffers` and
///   `NotebookBroadcast::Comm.buffers` both move through the blob store.
/// - **NotebookResponse**: 64 MiB. `Response::DocBytes` returns a full
///   Automerge doc dump; `Response::NotebookState` carries
///   inspect-notebook output. Both can legitimately be large.
/// - **NotebookBroadcast**: 16 MiB. `Comm.buffers` carries kernel-sourced
///   widget buffer bytes and is the only path where kernel-controlled
///   bytes still ride raw on a non-CRDT frame. Tightens further once
///   that path moves through the blob store.
/// - **Presence**: 1 MiB. CBOR cursor + selection per peer.
/// - **PoolStateSync**: 1 MiB. The pool doc is bounded.
/// - **SessionControl**: 1 MiB. JSON readiness frames are tiny.
pub(super) fn frame_size_limits(type_byte: u8) -> FrameSizeLimits {
    use notebook_doc::frame_types;
    match type_byte {
        frame_types::AUTOMERGE_SYNC => FrameSizeLimits {
            cap: 64 * MIB,
            warn: 16 * MIB,
        },
        frame_types::REQUEST => FrameSizeLimits {
            cap: 16 * MIB,
            warn: 256 * KIB,
        },
        frame_types::RESPONSE => FrameSizeLimits {
            cap: 64 * MIB,
            warn: 16 * MIB,
        },
        frame_types::BROADCAST => FrameSizeLimits {
            cap: 16 * MIB,
            warn: 4 * MIB,
        },
        frame_types::PRESENCE => FrameSizeLimits {
            cap: MIB,
            warn: 256 * KIB,
        },
        frame_types::RUNTIME_STATE_SYNC => FrameSizeLimits {
            cap: 64 * MIB,
            warn: 16 * MIB,
        },
        frame_types::POOL_STATE_SYNC => FrameSizeLimits {
            cap: MIB,
            warn: 256 * KIB,
        },
        frame_types::SESSION_CONTROL => FrameSizeLimits {
            cap: MIB,
            warn: 256 * KIB,
        },
        // Unknown types fall back to the outer ceiling. The receive
        // path skips them anyway after the body is consumed.
        _ => FrameSizeLimits {
            cap: MAX_FRAME_SIZE,
            warn: MAX_FRAME_SIZE / 2,
        },
    }
}

/// Minimum protocol version accepted by v4 daemons.
pub const MIN_PROTOCOL_VERSION: u32 = 4;

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
    /// Session-control message (JSON, server-originated connection status).
    SessionControl = notebook_doc::frame_types::SESSION_CONTROL,
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
            frame_types::SESSION_CONTROL => Ok(Self::SessionControl),
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
///
/// Enforces the same per-type cap the receiver applies, so an outbound
/// oversize is caught with a clear local error rather than a generic
/// `frame too large` from the peer. A soft warn fires between the warn
/// threshold and the cap so we see growth before it ever rejects.
pub async fn send_typed_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame_type: NotebookFrameType,
    payload: &[u8],
) -> std::io::Result<()> {
    let type_byte = frame_type as u8;
    let limits = frame_size_limits(type_byte);
    if payload.len() > limits.cap {
        log::error!(
            "[notebook-protocol] outbound frame type 0x{:02x} exceeds cap: {} bytes (cap {})",
            type_byte,
            payload.len(),
            limits.cap,
        );
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "outbound frame too large for type 0x{:02x}: {} bytes (max {})",
                type_byte,
                payload.len(),
                limits.cap
            ),
        ));
    }
    if payload.len() > limits.warn {
        log::warn!(
            "[notebook-protocol] outbound frame type 0x{:02x} over warn threshold: {} bytes (warn {}, cap {})",
            type_byte,
            payload.len(),
            limits.warn,
            limits.cap,
        );
    }
    let mut data = Vec::with_capacity(1 + payload.len());
    data.push(type_byte);
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

        // Per-type ceiling. The hard cap rejects oversized payloads —
        // a corrupted length prefix on a narrow-purpose channel trips
        // this check before the allocator honors the bogus length. The
        // soft warn threshold logs growth so we see drift in production
        // before it ever rejects.
        let limits = frame_size_limits(type_byte);
        if body_len > limits.cap {
            log::error!(
                "[notebook-protocol] frame type 0x{:02x} exceeds cap: {} bytes (cap {}); dropping connection",
                type_byte,
                body_len,
                limits.cap,
            );
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "frame too large for type 0x{:02x}: {} bytes (max {})",
                    type_byte, body_len, limits.cap
                ),
            ));
        }
        if body_len > limits.warn {
            log::warn!(
                "[notebook-protocol] frame type 0x{:02x} over warn threshold: {} bytes (warn {}, cap {})",
                type_byte,
                body_len,
                limits.warn,
                limits.cap,
            );
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

/// Cancel-safe wrapper around `recv_typed_frame`.
///
/// `recv_typed_frame` is built on `read_exact`, which is *not*
/// cancel-safe — dropping the future mid-read silently consumes bytes
/// from the underlying reader and the partial-read state is discarded.
/// Putting `recv_typed_frame` directly inside a busy `tokio::select!`
/// arm therefore desyncs the stream the moment another arm wins while
/// a body read is in flight; the next iteration's length prefix is
/// read from the middle of the previous payload.
///
/// `FramedReader` runs the read loop on a dedicated tokio task that
/// owns the read half exclusively and publishes frames through a
/// bounded mpsc channel. `FramedReader::recv()` is just an mpsc
/// `recv()` — fully cancel-safe — so callers can place it in any
/// `select!` without losing bytes.
///
/// On clean EOF the channel closes and `recv()` returns `None`.
/// On a stream error the reader sends `Err(e)` once and then closes.
pub struct FramedReader {
    rx: tokio::sync::mpsc::Receiver<std::io::Result<TypedNotebookFrame>>,
    handle: tokio::task::JoinHandle<()>,
}

impl FramedReader {
    /// Spawn a reader task that owns `reader`. `capacity` bounds the
    /// in-flight frame queue so a slow consumer applies backpressure
    /// to the source.
    pub fn spawn<R>(mut reader: R, capacity: usize) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
    {
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        let handle = tokio::spawn(async move {
            loop {
                match recv_typed_frame(&mut reader).await {
                    Ok(Some(frame)) => {
                        if tx.send(Ok(frame)).await.is_err() {
                            // Receiver dropped — caller is gone, stop reading.
                            break;
                        }
                    }
                    Ok(None) => break, // clean EOF, drop tx, channel closes
                    Err(e) => {
                        let _ = tx.send(Err(e)).await;
                        break;
                    }
                }
            }
        });
        Self { rx, handle }
    }

    /// Cancel-safe receive of the next frame.
    ///
    /// Returns `Some(Ok(frame))` for each successful frame,
    /// `Some(Err(_))` once on a stream error, and `None` once the
    /// reader task has finished (clean EOF or post-error close).
    pub async fn recv(&mut self) -> Option<std::io::Result<TypedNotebookFrame>> {
        self.rx.recv().await
    }
}

impl Drop for FramedReader {
    fn drop(&mut self) {
        // Closing the receiver lets the task observe a closed channel
        // on its next send and exit cleanly. Abort is a backstop for
        // the case where the task is parked on `read_exact` with no
        // bytes ever arriving (e.g. half-closed socket).
        self.handle.abort();
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
    match recv_control_frame(reader).await? {
        Some(data) => {
            let value = serde_json::from_slice(&data)?;
            Ok(Some(value))
        }
        None => Ok(None),
    }
}
