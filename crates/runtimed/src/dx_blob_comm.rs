//! Handler for the `nteract.dx.blob` comm target.
//!
//! The comm carries byte uploads from the Python kernel's `dx` library. The
//! kernel opens a comm with target `nteract.dx.blob`, then sends `op: put`
//! messages whose raw bytes travel in the ZMQ `buffers` frames (not base64
//! JSON). We hash + write to the [`BlobStore`] and reply with an ack or an
//! error on the same comm.
//!
//! **Critical invariant:** comm traffic on `nteract.dx.*` target names must
//! **not** be written to [`RuntimeStateDoc::comms`] — that persistence path
//! is reserved for ipywidgets/anywidget state. Callers short-circuit based
//! on [`is_dx_target`] before any `put_comm` / `merge_comm_state_delta`
//! call.
//!
//! See `docs/superpowers/specs/2026-04-13-nteract-dx-design.md`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::warn;

use crate::blob_store::{BlobStore, MAX_BLOB_SIZE};

/// Reserved comm-target namespace prefix. All targets starting with this
/// prefix are handled by dx subsystems and excluded from [`RuntimeStateDoc`]
/// persistence.
pub const DX_NAMESPACE_PREFIX: &str = "nteract.dx.";

/// Comm target for blob uploads (kernel → runtime agent → blob store).
pub const DX_BLOB_TARGET: &str = "nteract.dx.blob";

/// Returns true if `target_name` is part of the reserved dx namespace.
///
/// Requires at least one character after the prefix so the literal strings
/// `"nteract.dx"` and `"nteract.dx."` do not match (reserving the ability
/// to define an explicit namespace root without accidentally matching it).
pub fn is_dx_target(target_name: &str) -> bool {
    target_name.starts_with(DX_NAMESPACE_PREFIX) && target_name.len() > DX_NAMESPACE_PREFIX.len()
}

/// Request envelope received on the `nteract.dx.blob` comm.
#[derive(Debug, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DxBlobRequest {
    /// Upload a single buffer. The raw bytes arrive in the ZMQ `buffers`
    /// frames alongside this JSON envelope; the request carries just the
    /// metadata.
    Put {
        req_id: String,
        content_type: String,
    },
}

/// Response envelope sent back on the same comm.
#[derive(Debug, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DxBlobResponse {
    Ack {
        req_id: String,
        hash: String,
        size: u64,
    },
    Err {
        req_id: String,
        code: String,
        message: String,
    },
}

/// Handle a single `op: put` request. Hashes the buffer, writes to the
/// blob store, and returns an ack (or a structured error).
///
/// The caller is responsible for (a) deserializing the request from the
/// comm_msg envelope, (b) extracting the single buffer from the ZMQ frame
/// list, and (c) forwarding the returned response back to the kernel on
/// the shell socket.
pub async fn handle_blob_msg(
    blob_store: &Arc<BlobStore>,
    request: DxBlobRequest,
    buffer: &[u8],
) -> DxBlobResponse {
    match request {
        DxBlobRequest::Put {
            req_id,
            content_type,
        } => {
            if buffer.len() > MAX_BLOB_SIZE {
                return DxBlobResponse::Err {
                    req_id,
                    code: "too_large".to_string(),
                    message: format!(
                        "payload {} bytes exceeds MAX_BLOB_SIZE {}",
                        buffer.len(),
                        MAX_BLOB_SIZE
                    ),
                };
            }
            match blob_store.put(buffer, &content_type).await {
                Ok(hash) => DxBlobResponse::Ack {
                    req_id,
                    hash,
                    size: buffer.len() as u64,
                },
                Err(err) => {
                    warn!("[dx] blob store put failed: {}", err);
                    DxBlobResponse::Err {
                        req_id,
                        code: "blob_store_error".to_string(),
                        message: err.to_string(),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn namespace_prefix_check() {
        assert!(is_dx_target("nteract.dx.blob"));
        assert!(is_dx_target("nteract.dx.query"));
        assert!(is_dx_target("nteract.dx.stream"));
        assert!(is_dx_target("nteract.dx.x"));
        // Literal namespace root and trailing-dot form must not match.
        assert!(!is_dx_target("nteract.dx"));
        assert!(!is_dx_target("nteract.dx."));
        // Unrelated targets never match.
        assert!(!is_dx_target("jupyter.widget"));
        assert!(!is_dx_target(""));
        assert!(!is_dx_target("dx.blob"));
    }

    #[test]
    fn request_deserialize_put() {
        let req: DxBlobRequest = serde_json::from_value(serde_json::json!({
            "op": "put",
            "req_id": "r1",
            "content_type": "image/png",
        }))
        .unwrap();
        match req {
            DxBlobRequest::Put {
                req_id,
                content_type,
            } => {
                assert_eq!(req_id, "r1");
                assert_eq!(content_type, "image/png");
            }
        }
    }

    #[test]
    fn response_serialize_ack() {
        let resp = DxBlobResponse::Ack {
            req_id: "r1".into(),
            hash: "abc123".into(),
            size: 3,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["op"], "ack");
        assert_eq!(v["hash"], "abc123");
        assert_eq!(v["size"], 3);
    }

    #[test]
    fn response_serialize_err_too_large() {
        let resp = DxBlobResponse::Err {
            req_id: "r2".into(),
            code: "too_large".into(),
            message: "m".into(),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["op"], "err");
        assert_eq!(v["code"], "too_large");
    }

    #[tokio::test]
    async fn handle_put_writes_to_blob_store_and_acks() {
        let dir = tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path().to_path_buf()));

        let request = DxBlobRequest::Put {
            req_id: "r1".into(),
            content_type: "image/png".into(),
        };
        let payload = b"hello world";
        let resp = handle_blob_msg(&store, request, payload).await;

        match resp {
            DxBlobResponse::Ack { req_id, hash, size } => {
                assert_eq!(req_id, "r1");
                assert_eq!(size, payload.len() as u64);
                // Blob should now exist in the store at the acked hash.
                assert!(store.exists(&hash));
            }
            other => panic!("expected ack, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn handle_put_rejects_oversized_payload() {
        let dir = tempdir().unwrap();
        let store = Arc::new(BlobStore::new(dir.path().to_path_buf()));

        // Construct a payload one byte over the limit (skipping allocation of
        // 100 MiB by using a sliced view — we only need the length for the
        // guard, but the handler reads the buffer to hash it, so we do need
        // actual bytes if it ever reached `put`. The guard short-circuits
        // first, so a large stub buffer is never materialized here. Use a
        // modest buffer and override via a sentinel: this test asserts the
        // length check fires *before* `put` is called.
        //
        // To keep this fast: use a zero-filled Vec just over the limit.
        let oversize = vec![0u8; MAX_BLOB_SIZE + 1];
        let request = DxBlobRequest::Put {
            req_id: "r2".into(),
            content_type: "application/octet-stream".into(),
        };
        let resp = handle_blob_msg(&store, request, &oversize).await;

        match resp {
            DxBlobResponse::Err { code, .. } => assert_eq!(code, "too_large"),
            other => panic!("expected too_large err, got {other:?}"),
        }
    }
}
