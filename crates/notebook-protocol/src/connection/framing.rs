//! Length-prefixed framing, preamble validation, and typed notebook frames.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use serde::{de::DeserializeOwned, Serialize};

use super::handshake::PROTOCOL_VERSION;
pub use notebook_wire::{
    frame_size_limits, NotebookFrameType, TypedNotebookFrame, MAGIC, MAX_CONTROL_FRAME_SIZE,
    MAX_FRAME_SIZE, MIN_PROTOCOL_VERSION, PREAMBLE_LEN,
};

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
