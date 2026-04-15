//! Output store: manifests, ContentRef, and blob storage for notebook outputs.
//!
//! ## Design
//!
//! Output manifests are inlined directly into the RuntimeStateDoc CRDT as
//! structured Automerge Maps. Content within those manifests is referenced
//! via [`ContentRef`] — small text content (< 1KB) is inlined directly in
//! the manifest, while larger content and all binary data is stored in the
//! blob store.
//!
//! ## Text vs Binary Content
//!
//! Content is classified by MIME type via [`is_binary_mime()`]:
//!
//! **Text** (`text/*`, `application/json`, `image/svg+xml`, `+json`, `+xml`):
//! - Inlined if < 1KB, otherwise stored in the blob store
//! - Resolved via [`ContentRef::resolve()`] → `String`
//!
//! **Binary** (`image/png`, `image/jpeg`, `audio/*`, `video/*`, most `application/*`):
//! - Jupyter sends these as base64 on the wire; we **decode before storing**
//! - Always stored as blobs (never inlined, regardless of size)
//! - The blob store holds actual binary bytes (real PNG, JPEG, etc.)
//! - Resolved via [`ContentRef::resolve_binary_as_base64()`] for the .ipynb
//!   save path, or as `http://` blob URLs on the frontend
//!
//! **Important:** `image/svg+xml` is TEXT, not binary. Jupyter sends SVG as
//! plain XML strings, not base64.
//!
//! ## The `is_binary_mime` Contract
//!
//! Three implementations must stay in sync:
//! - Rust: `is_binary_mime()` in this file
//! - Rust: `is_binary_mime()` in `crates/runtimed-py/src/output_resolver.rs`
//! - TypeScript: `isBinaryMime()` in `apps/notebook/src/lib/manifest-resolution.ts`
//!
//! If you change the classification, update all three.
//!
//! ## Key Types
//!
//! - [`ContentRef`]: inline string or blob hash — the MIME type determines
//!   whether to read as text or binary
//! - [`OutputManifest`]: Jupyter output with `ContentRef` fields
//! - [`create_manifest()`]: nbformat JSON → `OutputManifest` (decodes binary, stores blobs)
//! - [`resolve_manifest()`]: `&OutputManifest` → nbformat JSON (re-encodes binary to base64)

use std::collections::HashMap;
use std::io;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use notebook_doc::mime::{is_binary_mime, BLOB_REF_MIME};

use crate::blob_store::BlobStore;

/// MIME types whose `ContentRef::Blob` outputs are externalized as
/// [`BLOB_REF_MIME`] entries in saved `.ipynb` files instead of being
/// re-inlined as base64.
///
/// Tightly scoped on purpose. Images / PDFs / HTML keep their existing
/// base64-inline behavior regardless of size — those are well-understood
/// in `.ipynb` files and have no vanilla-Jupyter fallback path if we
/// replaced them. We only externalize MIMEs that:
///
/// 1. Are nteract-specific and have a reasonable fallback elsewhere in
///    the bundle (parquet ships alongside pandas `text/html` + `text/plain`).
/// 2. Would otherwise blow up `.ipynb` size catastrophically (parquet
///    exports can hit tens or hundreds of MiB).
///
/// Because this whitelist holds at most one entry per output bundle in
/// practice (dx emits exactly one parquet ref per display), we can write
/// the ref as a single `{hash, content_type, size}` object under the
/// [`BLOB_REF_MIME`] key. nbformat's schema wouldn't accept an array
/// there, so a whitelist-of-one is the right shape.
///
/// See `docs/superpowers/specs/2026-04-14-ipynb-save-blob-refs-design.md`.
const REF_MIME_SAVE_WHITELIST: &[&str] = &["application/vnd.apache.parquet"];

/// Returns true if a binary MIME type should be externalized as a
/// [`BLOB_REF_MIME`] entry on save instead of base64-inlined.
fn should_externalize_mime_on_save(mime_type: &str) -> bool {
    REF_MIME_SAVE_WHITELIST.contains(&mime_type)
}

/// Default inlining threshold: 1 KB.
///
/// Text content smaller than this is inlined in the manifest (and thus in the
/// CRDT). Content equal to or larger than this is stored in the blob store.
/// Binary content always goes to the blob store regardless of size.
pub const DEFAULT_INLINE_THRESHOLD: usize = 1024;

/// A reference to content that may be inlined or stored in the blob store.
///
/// Serializes as an untagged enum:
/// - `{"inline": "..."}` — content is inlined
/// - `{"blob": "hash...", "size": 12345}` — content is in blob store
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentRef {
    /// Content is inlined in the manifest.
    Inline { inline: String },
    /// Content is stored in the blob store.
    Blob { blob: String, size: u64 },
}

impl ContentRef {
    /// Create a ContentRef from data, applying the inlining threshold.
    ///
    /// If the data is smaller than the threshold, it's inlined.
    /// Otherwise, it's stored in the blob store.
    pub async fn from_data(
        data: &str,
        media_type: &str,
        blob_store: &BlobStore,
        threshold: usize,
    ) -> io::Result<Self> {
        if data.len() < threshold {
            Ok(ContentRef::Inline {
                inline: data.to_string(),
            })
        } else {
            let hash = blob_store.put(data.as_bytes(), media_type).await?;
            Ok(ContentRef::Blob {
                blob: hash,
                size: data.len() as u64,
            })
        }
    }

    /// Resolve a ContentRef to its string content.
    ///
    /// For inline content, returns the content directly.
    /// For blob content, fetches from the blob store.
    pub async fn resolve(&self, blob_store: &BlobStore) -> io::Result<String> {
        match self {
            ContentRef::Inline { inline } => Ok(inline.clone()),
            ContentRef::Blob { blob, .. } => {
                let data = blob_store.get(blob).await?.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, format!("blob not found: {}", blob))
                })?;
                String::from_utf8(data).map_err(|e| {
                    io::Error::new(io::ErrorKind::InvalidData, format!("invalid UTF-8: {}", e))
                })
            }
        }
    }

    /// Returns true if the content is inlined.
    pub fn is_inline(&self) -> bool {
        matches!(self, ContentRef::Inline { .. })
    }

    /// Create a ContentRef from raw binary data, always using the blob store.
    ///
    /// Binary content (images, Arrow IPC, etc.) skips the inline threshold
    /// and is always stored as a blob. The raw bytes are stored directly —
    /// no base64 encoding — so the blob store holds the actual binary content
    /// and the HTTP server can serve it with the correct Content-Type.
    pub async fn from_binary(
        data: &[u8],
        media_type: &str,
        blob_store: &BlobStore,
    ) -> io::Result<Self> {
        let hash = blob_store.put(data, media_type).await?;
        Ok(ContentRef::Blob {
            blob: hash,
            size: data.len() as u64,
        })
    }

    /// Build a [`ContentRef::Blob`] from a hash already present in the
    /// blob store (e.g. just written by [`preflight_ref_buffers`]).
    pub fn from_hash(hash: String, size: u64) -> Self {
        ContentRef::Blob { blob: hash, size }
    }

    /// Resolve a ContentRef that holds binary content, returning base64.
    ///
    /// For inline content, returns the string as-is (it's already base64
    /// from the Jupyter wire protocol, kept inline for small images).
    /// For blob content, reads the raw bytes and base64-encodes them.
    ///
    /// Used by `resolve_data_bundle` for binary MIME types to reconstruct
    /// the Jupyter nbformat representation (base64 strings for images).
    pub async fn resolve_binary_as_base64(&self, blob_store: &BlobStore) -> io::Result<String> {
        match self {
            ContentRef::Inline { inline } => Ok(inline.clone()),
            ContentRef::Blob { blob, .. } => {
                let data = blob_store.get(blob).await?.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, format!("blob not found: {}", blob))
                })?;
                Ok(base64::engine::general_purpose::STANDARD.encode(&data))
            }
        }
    }
}

// =============================================================================
// Output manifest types
// =============================================================================

/// Transient data for display outputs (e.g., display_id for UpdateDisplayData).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransientData {
    /// Display ID for UpdateDisplayData support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_id: Option<String>,
}

impl TransientData {
    /// Returns true if transient data is empty (no display_id).
    pub fn is_empty(&self) -> bool {
        self.display_id.is_none()
    }
}

/// Manifest for display_data and execute_result outputs.
///
/// These are the most common output types, containing MIME-typed data bundles.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DisplayDataManifest {
    /// Output type: "display_data" or "execute_result"
    pub output_type: String,
    /// MIME type -> content reference
    pub data: HashMap<String, ContentRef>,
    /// MIME type -> metadata (unchanged from Jupyter)
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, Value>,
    /// Execution count (only for execute_result)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_count: Option<i32>,
}

/// Maximum head/tail size per side in bytes.
const PREVIEW_BYTE_CAP: usize = 1024;
/// Maximum head/tail size per side in lines.
const PREVIEW_LINE_CAP: usize = 40;

/// LLM-friendly summary of a spilled stream text blob. Populated at
/// manifest-creation time so readers never need to fetch the blob just
/// to describe it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamPreview {
    pub head: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tail: String,
    pub total_bytes: u64,
    pub total_lines: u64,
}

impl StreamPreview {
    pub fn from_text(text: &str) -> Self {
        let total_bytes = text.len() as u64;
        let total_lines = text.lines().count() as u64;
        let head = take_head(text, PREVIEW_LINE_CAP, PREVIEW_BYTE_CAP);
        let tail = if head.len() as u64 >= total_bytes {
            String::new()
        } else {
            take_tail(text, PREVIEW_LINE_CAP, PREVIEW_BYTE_CAP)
        };
        Self {
            head,
            tail,
            total_bytes,
            total_lines,
        }
    }
}

/// LLM-friendly summary of a spilled traceback blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorPreview {
    pub last_frame: String,
    pub total_bytes: u64,
    pub frames: u32,
}

impl ErrorPreview {
    pub fn from_traceback_value(tb: &Value) -> Self {
        let frames_arr: Vec<&str> = tb
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        let frames = frames_arr.len() as u32;
        let total_bytes = serde_json::to_string(tb)
            .map(|s| s.len() as u64)
            .unwrap_or(0);
        let raw_last = frames_arr
            .iter()
            .rev()
            .find(|s| !s.trim().is_empty())
            .copied()
            .unwrap_or("");
        let stripped = strip_ansi(raw_last);
        let last_frame = truncate_bytes(&stripped, PREVIEW_BYTE_CAP);
        Self {
            last_frame,
            total_bytes,
            frames,
        }
    }
}

fn take_head(text: &str, line_cap: usize, byte_cap: usize) -> String {
    let mut out = String::new();
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i >= line_cap {
            break;
        }
        if out.len() + line.len() > byte_cap {
            let remaining = byte_cap.saturating_sub(out.len());
            if remaining > 0 {
                out.push_str(&safe_byte_slice(line, 0, remaining));
            }
            break;
        }
        out.push_str(line);
    }
    if out.is_empty() && !text.is_empty() {
        out.push_str(&safe_byte_slice(text, 0, byte_cap));
    }
    out
}

fn take_tail(text: &str, line_cap: usize, byte_cap: usize) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = lines.len().saturating_sub(line_cap);
    let mut out = String::new();
    for line in &lines[start..] {
        if out.len() + line.len() > byte_cap {
            let remaining = byte_cap.saturating_sub(out.len());
            if remaining > 0 {
                let start_byte = line.len() - remaining;
                out.push_str(&safe_byte_slice(line, start_byte, line.len()));
            }
            break;
        }
        out.push_str(line);
    }
    out
}

fn safe_byte_slice(s: &str, start: usize, end: usize) -> String {
    let mut lo = start.min(s.len());
    while lo > 0 && !s.is_char_boundary(lo) {
        lo -= 1;
    }
    let mut hi = end.min(s.len());
    while hi < s.len() && !s.is_char_boundary(hi) {
        hi += 1;
    }
    s[lo..hi].to_string()
}

fn truncate_bytes(s: &str, cap: usize) -> String {
    if s.len() <= cap {
        return s.to_string();
    }
    safe_byte_slice(s, 0, cap)
}

/// ANSI escape code stripper. Mirrors `runt-mcp::formatting::strip_ansi`.
fn strip_ansi(text: &str) -> String {
    use std::sync::LazyLock;
    static ANSI_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
        #[allow(clippy::expect_used)]
        regex::Regex::new(r"\x1b\[[0-9;]*[A-Za-z]|\x1b\].*?\x07|\x1b\(B").expect("valid ANSI regex")
    });
    ANSI_RE.replace_all(text, "").to_string()
}

/// Manifest for stream outputs (stdout/stderr).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamManifest {
    /// Output type: always "stream"
    pub output_type: String,
    /// Stream name: "stdout" or "stderr"
    pub name: String,
    /// Stream text content
    pub text: ContentRef,
}

/// Manifest for error outputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorManifest {
    /// Output type: always "error"
    pub output_type: String,
    /// Exception class name
    pub ename: String,
    /// Exception value/message
    pub evalue: String,
    /// Traceback lines (JSON array as string)
    pub traceback: ContentRef,
}

/// A unified output manifest enum for serialization.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "output_type")]
pub enum OutputManifest {
    #[serde(rename = "display_data")]
    DisplayData {
        data: HashMap<String, ContentRef>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        metadata: HashMap<String, Value>,
        #[serde(default, skip_serializing_if = "TransientData::is_empty")]
        transient: TransientData,
    },
    #[serde(rename = "execute_result")]
    ExecuteResult {
        data: HashMap<String, ContentRef>,
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        metadata: HashMap<String, Value>,
        execution_count: Option<i32>,
        #[serde(default, skip_serializing_if = "TransientData::is_empty")]
        transient: TransientData,
    },
    #[serde(rename = "stream")]
    Stream {
        name: String,
        text: ContentRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        llm_preview: Option<StreamPreview>,
    },
    #[serde(rename = "error")]
    Error {
        ename: String,
        evalue: String,
        traceback: ContentRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        llm_preview: Option<ErrorPreview>,
    },
}

impl OutputManifest {
    /// Serialize the manifest to a JSON Value (for writing into the CRDT).
    #[allow(clippy::expect_used)]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("OutputManifest should always serialize to JSON")
    }
}

// =============================================================================
// Manifest creation and resolution
// =============================================================================

/// Create an output manifest from a raw Jupyter output JSON value.
///
/// Applies the inlining threshold to text data fields:
/// - Text data smaller than the threshold is inlined
/// - Text data larger than the threshold is stored in the blob store
/// - Binary data is always stored in the blob store
///
/// Returns the manifest struct directly. Use `OutputManifest::to_json()` to
/// serialize it for writing into the CRDT.
pub async fn create_manifest(
    output: &Value,
    blob_store: &BlobStore,
    threshold: usize,
) -> io::Result<OutputManifest> {
    let output_type = output
        .get("output_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing output_type"))?;

    let manifest = match output_type {
        "display_data" => {
            let data = convert_data_bundle(output.get("data"), blob_store, threshold).await?;
            let metadata = extract_metadata(output.get("metadata"));
            let transient = extract_transient(output.get("transient"));
            OutputManifest::DisplayData {
                data,
                metadata,
                transient,
            }
        }
        "execute_result" => {
            let data = convert_data_bundle(output.get("data"), blob_store, threshold).await?;
            let metadata = extract_metadata(output.get("metadata"));
            let transient = extract_transient(output.get("transient"));
            let execution_count = output
                .get("execution_count")
                .and_then(|v| v.as_i64())
                .map(|n| n as i32);
            OutputManifest::ExecuteResult {
                data,
                metadata,
                execution_count,
                transient,
            }
        }
        "stream" => {
            let name = output
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("stdout")
                .to_string();
            let text_value = output
                .get("text")
                .cloned()
                .unwrap_or(Value::String(String::new()));
            let text_str = normalize_text(&text_value);
            let text =
                ContentRef::from_data(&text_str, "text/plain", blob_store, threshold).await?;
            let llm_preview = match &text {
                ContentRef::Blob { .. } => Some(StreamPreview::from_text(&text_str)),
                ContentRef::Inline { .. } => None,
            };
            OutputManifest::Stream {
                name,
                text,
                llm_preview,
            }
        }
        "error" => {
            let ename = output
                .get("ename")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let evalue = output
                .get("evalue")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let traceback_value = output
                .get("traceback")
                .cloned()
                .unwrap_or(Value::Array(vec![]));
            let traceback_json = serde_json::to_string(&traceback_value)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            let traceback =
                ContentRef::from_data(&traceback_json, "application/json", blob_store, threshold)
                    .await?;
            let llm_preview = match &traceback {
                ContentRef::Blob { .. } => {
                    Some(ErrorPreview::from_traceback_value(&traceback_value))
                }
                ContentRef::Inline { .. } => None,
            };
            OutputManifest::Error {
                ename,
                evalue,
                traceback,
                llm_preview,
            }
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown output_type: {}", output_type),
            ))
        }
    };

    Ok(manifest)
}

/// Write ref-MIME buffers to the blob store before the manifest is built.
///
/// When a `display_data` / `execute_result` carries
/// [`BLOB_REF_MIME`](notebook_doc::mime::BLOB_REF_MIME) + trailing ZMQ
/// `buffers` frames, each blob-ref entry's `buffer_index` points into
/// the `buffers` list. We hash + store those bytes so the subsequent
/// [`create_manifest`] call resolves the ref against an existing blob.
///
/// Missing `buffer_index` defaults to 0. Out-of-range indices, missing
/// `hash` / `content_type`, computed-vs-declared hash mismatches, and
/// blob-store errors all log a `warn!` and skip the entry —
/// [`create_manifest`] then drops the ref because [`BlobStore::exists`]
/// fails on the declared hash.
///
/// Call from the IOPub task before [`create_manifest`].
pub async fn preflight_ref_buffers(
    nbformat: &serde_json::Value,
    buffers: &[Vec<u8>],
    blob_store: &BlobStore,
) {
    if buffers.is_empty() {
        return;
    }
    let Some(data) = nbformat.get("data").and_then(|v| v.as_object()) else {
        return;
    };
    for (mime, body) in data {
        if mime != notebook_doc::mime::BLOB_REF_MIME {
            continue;
        }
        let declared_hash = body.get("hash").and_then(|v| v.as_str()).unwrap_or("");
        let target_ct = body
            .get("content_type")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let buf_idx = body
            .get("buffer_index")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        if target_ct.is_empty() || declared_hash.is_empty() {
            tracing::warn!(
                "[dx] blob-ref MIME missing hash or content_type (skipping buffer preflight)"
            );
            continue;
        }
        let Some(buf) = buffers.get(buf_idx) else {
            tracing::warn!(
                "[dx] blob-ref buffer_index {} out of range ({} buffers); skipping",
                buf_idx,
                buffers.len()
            );
            continue;
        };
        match blob_store.put(buf, target_ct).await {
            Ok(computed) => {
                if computed != declared_hash {
                    tracing::warn!(
                        "[dx] blob-ref hash mismatch: declared={} computed={} — ContentRef will drop",
                        declared_hash,
                        computed
                    );
                }
            }
            Err(err) => {
                tracing::warn!("[dx] blob-ref buffer put failed: {}", err);
            }
        }
    }
}

/// Get the display_id from an OutputManifest, if present.
///
/// Used by UpdateDisplayData to find the output to update.
pub fn get_display_id(manifest: &OutputManifest) -> Option<String> {
    match manifest {
        OutputManifest::DisplayData { transient, .. }
        | OutputManifest::ExecuteResult { transient, .. } => transient.display_id.clone(),
        _ => None,
    }
}

/// Update display data in a manifest with new data and metadata.
///
/// Returns the updated OutputManifest if the manifest is a display_data or execute_result
/// with matching display_id, otherwise returns None.
pub async fn update_manifest_display_data(
    manifest: &OutputManifest,
    display_id: &str,
    new_data: &serde_json::Value,
    new_metadata: &serde_json::Map<String, serde_json::Value>,
    blob_store: &BlobStore,
    threshold: usize,
) -> io::Result<Option<OutputManifest>> {
    // Check if this manifest has the matching display_id
    let matches = match manifest {
        OutputManifest::DisplayData { transient, .. }
        | OutputManifest::ExecuteResult { transient, .. } => {
            transient.display_id.as_deref() == Some(display_id)
        }
        _ => false,
    };

    if !matches {
        return Ok(None);
    }

    // Create updated manifest with new data
    match manifest {
        OutputManifest::DisplayData { transient, .. } => {
            let data = convert_value_to_content_refs(new_data, blob_store, threshold).await?;
            let metadata = new_metadata.clone().into_iter().collect();
            let updated = OutputManifest::DisplayData {
                data,
                metadata,
                transient: transient.clone(),
            };
            Ok(Some(updated))
        }
        OutputManifest::ExecuteResult {
            execution_count,
            transient,
            ..
        } => {
            let data = convert_value_to_content_refs(new_data, blob_store, threshold).await?;
            let metadata = new_metadata.clone().into_iter().collect();
            let updated = OutputManifest::ExecuteResult {
                data,
                metadata,
                execution_count: *execution_count,
                transient: transient.clone(),
            };
            Ok(Some(updated))
        }
        _ => Ok(None),
    }
}

/// Convert a data Value (MIME bundle) to ContentRef map.
async fn convert_value_to_content_refs(
    data: &Value,
    blob_store: &BlobStore,
    threshold: usize,
) -> io::Result<HashMap<String, ContentRef>> {
    let mut result = HashMap::new();
    if let Value::Object(map) = data {
        for (mime_type, value) in map {
            let content_ref = if is_binary_mime(mime_type) {
                // Binary MIME type: base64-decode → store raw bytes in blob.
                let base64_str = value_to_string(value);
                let raw_bytes = base64::engine::general_purpose::STANDARD
                    .decode(&base64_str)
                    .map_err(|e| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("base64 decode failed for {}: {}", mime_type, e),
                        )
                    })?;
                ContentRef::from_binary(&raw_bytes, mime_type, blob_store).await?
            } else {
                let content_str = value_to_string(value);
                ContentRef::from_data(&content_str, mime_type, blob_store, threshold).await?
            };
            result.insert(mime_type.clone(), content_ref);
        }
    }
    Ok(result)
}

/// Resolve a manifest back to a full Jupyter output JSON value.
///
/// Fetches any blob-referenced content and reconstructs the original format.
pub async fn resolve_manifest(
    manifest: &OutputManifest,
    blob_store: &BlobStore,
) -> io::Result<Value> {
    match manifest {
        OutputManifest::DisplayData {
            data,
            metadata,
            transient,
        } => {
            let resolved_data = resolve_data_bundle(data, blob_store).await?;
            let mut output = serde_json::json!({
                "output_type": "display_data",
                "data": resolved_data,
            });
            if !metadata.is_empty() {
                output["metadata"] = Value::Object(metadata.clone().into_iter().collect());
            } else {
                output["metadata"] = Value::Object(serde_json::Map::new());
            }
            if !transient.is_empty() {
                let mut transient_map = serde_json::Map::new();
                if let Some(display_id) = &transient.display_id {
                    transient_map
                        .insert("display_id".to_string(), Value::String(display_id.clone()));
                }
                output["transient"] = Value::Object(transient_map);
            }
            Ok(output)
        }
        OutputManifest::ExecuteResult {
            data,
            metadata,
            execution_count,
            transient,
        } => {
            let resolved_data = resolve_data_bundle(data, blob_store).await?;
            let mut output = serde_json::json!({
                "output_type": "execute_result",
                "data": resolved_data,
                "execution_count": execution_count,
            });
            if !metadata.is_empty() {
                output["metadata"] = Value::Object(metadata.clone().into_iter().collect());
            } else {
                output["metadata"] = Value::Object(serde_json::Map::new());
            }
            if !transient.is_empty() {
                let mut transient_map = serde_json::Map::new();
                if let Some(display_id) = &transient.display_id {
                    transient_map
                        .insert("display_id".to_string(), Value::String(display_id.clone()));
                }
                output["transient"] = Value::Object(transient_map);
            }
            Ok(output)
        }
        OutputManifest::Stream { name, text, .. } => {
            let resolved_text = text.resolve(blob_store).await?;
            Ok(serde_json::json!({
                "output_type": "stream",
                "name": name,
                "text": resolved_text,
            }))
        }
        OutputManifest::Error {
            ename,
            evalue,
            traceback,
            ..
        } => {
            let traceback_json = traceback.resolve(blob_store).await?;
            let traceback_array: Value = serde_json::from_str(&traceback_json)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(serde_json::json!({
                "output_type": "error",
                "ename": ename,
                "evalue": evalue,
                "traceback": traceback_array,
            }))
        }
    }
}

// =============================================================================
// Helper functions
// =============================================================================

/// Convert a Jupyter data bundle (MIME type -> content) to ContentRefs.
///
/// Binary MIME types (images, Arrow IPC, etc.) are base64-decoded and stored
/// as raw bytes in the blob store. Text MIME types use the existing
/// inline/blob threshold logic.
async fn convert_data_bundle(
    data: Option<&Value>,
    blob_store: &BlobStore,
    threshold: usize,
) -> io::Result<HashMap<String, ContentRef>> {
    let mut result = HashMap::new();

    if let Some(Value::Object(map)) = data {
        for (mime_type, value) in map {
            // dx blob-ref MIME: the kernel already uploaded the bytes via
            // the nteract.dx.blob comm. Compose a ContentRef under the
            // wrapped content_type without a new BlobStore::put call. The
            // blob-ref MIME itself is NOT emitted as a manifest entry — it
            // is a transport detail, not display content.
            if mime_type == notebook_doc::mime::BLOB_REF_MIME {
                let hash = value.get("hash").and_then(|v| v.as_str());
                let target_ct = value.get("content_type").and_then(|v| v.as_str());
                let size = value.get("size").and_then(|v| v.as_u64()).unwrap_or(0);
                match (hash, target_ct) {
                    (Some(h), Some(ct)) => {
                        if blob_store.exists(h) {
                            result
                                .insert(ct.to_string(), ContentRef::from_hash(h.to_string(), size));
                        } else {
                            tracing::warn!(
                                "[dx] blob-ref MIME references missing blob hash={} (dropping)",
                                h
                            );
                        }
                    }
                    _ => {
                        tracing::warn!(
                            "[dx] blob-ref MIME missing hash or content_type (dropping)"
                        );
                    }
                }
                continue;
            }

            let content_ref = if is_binary_mime(mime_type) {
                // Binary MIME type: base64-decode → store raw bytes in blob.
                // Jupyter sends image data as base64 strings on the wire.
                // We decode to actual bytes so the blob store holds real
                // binary content and the HTTP server serves it correctly.
                let base64_str = value_to_string(value);
                let raw_bytes = base64::engine::general_purpose::STANDARD
                    .decode(&base64_str)
                    .map_err(|e| {
                        io::Error::new(
                            io::ErrorKind::InvalidData,
                            format!("base64 decode failed for {}: {}", mime_type, e),
                        )
                    })?;
                ContentRef::from_binary(&raw_bytes, mime_type, blob_store).await?
            } else {
                // Text MIME type: store as-is with inline/blob threshold.
                let content_str = value_to_string(value);
                ContentRef::from_data(&content_str, mime_type, blob_store, threshold).await?
            };
            result.insert(mime_type.clone(), content_ref);
        }
    }

    Ok(result)
}

/// Resolve a data bundle of ContentRefs back to string values.
///
/// Binary MIME types are resolved via `resolve_binary_as_base64` which
/// reads raw bytes from the blob store and base64-encodes them for the
/// Jupyter nbformat representation (used when saving .ipynb to disk).
///
/// Whitelisted MIMEs ([`REF_MIME_SAVE_WHITELIST`] — currently parquet)
/// are externalized as [`BLOB_REF_MIME`] entries instead of being
/// re-inlined as base64. The original binary MIME key is dropped and
/// replaced by a `BLOB_REF_MIME` → `{hash, content_type, size}` entry.
/// See `docs/superpowers/specs/2026-04-14-ipynb-save-blob-refs-design.md`.
/// The reverse transform is handled by `convert_data_bundle`'s
/// existing `BLOB_REF_MIME` branch on load.
async fn resolve_data_bundle(
    data: &HashMap<String, ContentRef>,
    blob_store: &BlobStore,
) -> io::Result<HashMap<String, Value>> {
    let mut result = HashMap::new();

    for (mime_type, content_ref) in data {
        // Spec 2: externalize whitelisted binary blobs as a BLOB_REF_MIME
        // entry instead of re-inlining them as base64 in the .ipynb.
        // Non-whitelisted MIMEs (images, PDFs, HTML, audio, video) keep
        // the legacy path so vanilla Jupyter renders them unchanged.
        if should_externalize_mime_on_save(mime_type) {
            if let ContentRef::Blob { blob: hash, size } = content_ref {
                let ref_body = json!({
                    "hash": hash,
                    "content_type": mime_type,
                    "size": size,
                });
                result.insert(BLOB_REF_MIME.to_string(), ref_body);
                continue;
            }
        }

        let value = if is_binary_mime(mime_type) {
            // Binary: read raw bytes from blob → base64-encode for nbformat
            let base64_str = content_ref.resolve_binary_as_base64(blob_store).await?;
            Value::String(base64_str)
        } else if mime_type.ends_with("+json") || mime_type == "application/json" {
            // JSON: parse into structured Value
            let content = content_ref.resolve(blob_store).await?;
            serde_json::from_str(&content).unwrap_or(Value::String(content))
        } else {
            // Text: return as string
            let content = content_ref.resolve(blob_store).await?;
            Value::String(content)
        };
        result.insert(mime_type.clone(), value);
    }

    Ok(result)
}

/// Extract metadata from a Jupyter output, preserving as Value.
fn extract_metadata(metadata: Option<&Value>) -> HashMap<String, Value> {
    match metadata {
        Some(Value::Object(map)) => map.clone().into_iter().collect(),
        _ => HashMap::new(),
    }
}

/// Extract transient data (display_id) from a Jupyter output.
fn extract_transient(transient: Option<&Value>) -> TransientData {
    match transient {
        Some(Value::Object(map)) => {
            let display_id = map
                .get("display_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            TransientData { display_id }
        }
        _ => TransientData::default(),
    }
}

/// Normalize text that may be a string or array of strings (Jupyter format).
fn normalize_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Convert a JSON value to a string for storage.
///
/// Strings are returned as-is. Other types are JSON-serialized.
fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        _ => serde_json::to_string(value).unwrap_or_default(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_store(dir: &TempDir) -> BlobStore {
        BlobStore::new(dir.path().join("blobs"))
    }

    #[test]
    fn stream_preview_short_text_is_head_only() {
        let text = "line 1\nline 2\nline 3\n";
        let p = StreamPreview::from_text(text);
        assert_eq!(p.head, text);
        assert_eq!(p.tail, "");
        assert_eq!(p.total_bytes, text.len() as u64);
        assert_eq!(p.total_lines, 3);
    }

    #[test]
    fn stream_preview_long_text_has_head_and_tail() {
        let text = (0..200)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let p = StreamPreview::from_text(&text);
        assert!(p.head.starts_with("line 0\n"));
        assert!(p.tail.ends_with("line 199"));
        assert!(p.head.len() <= 1024);
        assert!(p.head.lines().count() <= 40);
        assert!(p.tail.len() <= 1024);
        assert!(p.tail.lines().count() <= 40);
        assert_eq!(p.total_bytes, text.len() as u64);
        assert_eq!(p.total_lines, 200);
    }

    #[test]
    fn stream_preview_caps_head_at_byte_limit_mid_line() {
        let text = "x".repeat(10_000);
        let p = StreamPreview::from_text(&text);
        assert!(p.head.len() <= 1024);
        assert_eq!(p.total_bytes, 10_000);
    }

    #[test]
    fn error_preview_keeps_last_frame() {
        let tb = serde_json::json!([
            "Traceback (most recent call last):",
            "  File \"<stdin>\", line 1",
            "ZeroDivisionError: division by zero",
        ]);
        let p = ErrorPreview::from_traceback_value(&tb);
        assert_eq!(p.last_frame, "ZeroDivisionError: division by zero");
        assert_eq!(p.frames, 3);
        assert!(p.total_bytes > 0);
    }

    #[test]
    fn error_preview_strips_ansi_in_last_frame() {
        let tb = serde_json::json!(["Traceback…", "\x1b[31mValueError: bad input\x1b[0m",]);
        let p = ErrorPreview::from_traceback_value(&tb);
        assert_eq!(p.last_frame, "ValueError: bad input");
    }

    #[test]
    fn error_preview_empty_traceback() {
        let tb = serde_json::json!([]);
        let p = ErrorPreview::from_traceback_value(&tb);
        assert_eq!(p.last_frame, "");
        assert_eq!(p.frames, 0);
    }

    #[test]
    fn stream_preview_caps_tail_on_long_single_line() {
        // A single multi-KB line should cap the tail too. The tail walks
        // forward to the next char boundary, so allow a small overrun on
        // the advertised byte cap (≤ 3 bytes of slack for UTF-8).
        let text = "y".repeat(10_000);
        let p = StreamPreview::from_text(&text);
        assert_eq!(p.total_bytes, 10_000);
        assert!(p.tail.len() <= 1024 + 3);
        // Head plus tail together should still sample both ends of the stream.
        assert!(p.head.starts_with('y'));
    }

    #[test]
    fn stream_preview_respects_utf8_boundaries() {
        // Three-byte code point repeated past the cap — must not panic and
        // must produce valid UTF-8. Allow a few bytes of slack because
        // safe_byte_slice rounds to char boundaries.
        let text = "日".repeat(1_000); // 3 bytes each = 3_000 bytes
        let p = StreamPreview::from_text(&text);
        assert_eq!(p.total_bytes, 3_000);
        assert!(p.head.chars().all(|c| c == '日'));
        assert!(p.tail.chars().all(|c| c == '日'));
        assert!(p.head.len() <= 1024 + 3);
        assert!(p.tail.len() <= 1024 + 3);
    }

    #[tokio::test]
    async fn dx_ref_mime_composes_content_ref_under_target_type() {
        let dir = tempfile::tempdir().unwrap();
        let blob_store = test_store(&dir);

        // Pre-populate: simulate the kernel's dx upload having already stored
        // the blob via the nteract.dx.blob comm handler.
        let raw = b"PAR1-fake-parquet-body";
        let hash = blob_store
            .put(raw, "application/vnd.apache.parquet")
            .await
            .unwrap();

        let output = serde_json::json!({
            "output_type": "display_data",
            "data": {
                notebook_doc::mime::BLOB_REF_MIME: {
                    "hash": hash,
                    "content_type": "application/vnd.apache.parquet",
                    "size": raw.len(),
                    "query": null,
                },
                "text/llm+plain": "DataFrame (pandas): 3 rows × 2 columns"
            },
        });

        let manifest = create_manifest(&output, &blob_store, 1024).await.unwrap();
        let data = match manifest {
            OutputManifest::DisplayData { data, .. } => data,
            other => panic!("expected DisplayData, got {other:?}"),
        };

        assert!(!data.contains_key(notebook_doc::mime::BLOB_REF_MIME));
        assert!(data.contains_key("application/vnd.apache.parquet"));
        assert!(data.contains_key("text/llm+plain"));

        match data.get("application/vnd.apache.parquet").unwrap() {
            ContentRef::Blob { blob, size } => {
                assert_eq!(blob, &hash);
                assert_eq!(*size, raw.len() as u64);
            }
            other => panic!("expected blob ref, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn preflight_ref_buffers_writes_blob_when_present() {
        use sha2::Digest;
        let dir = tempfile::tempdir().unwrap();
        let blob_store = test_store(&dir);

        let raw = b"PAR1-fake-parquet-body";
        let declared_hash = hex::encode(sha2::Sha256::digest(raw));

        let nbformat = serde_json::json!({
            "output_type": "display_data",
            "data": {
                notebook_doc::mime::BLOB_REF_MIME: {
                    "hash": declared_hash.clone(),
                    "content_type": "application/vnd.apache.parquet",
                    "size": raw.len(),
                    "buffer_index": 0,
                },
                "text/llm+plain": "DataFrame (pandas): 3 rows × 2 columns"
            },
        });

        preflight_ref_buffers(&nbformat, &[raw.to_vec()], &blob_store).await;
        assert!(blob_store.exists(&declared_hash));

        // And the subsequent create_manifest composes a ContentRef from it.
        let manifest = create_manifest(&nbformat, &blob_store, 1024).await.unwrap();
        let data = match manifest {
            OutputManifest::DisplayData { data, .. } => data,
            other => panic!("expected DisplayData, got {other:?}"),
        };
        match data.get("application/vnd.apache.parquet").unwrap() {
            ContentRef::Blob { blob, size } => {
                assert_eq!(blob, &declared_hash);
                assert_eq!(*size, raw.len() as u64);
            }
            other => panic!("expected blob ref, got {other:?}"),
        }
        assert!(!data.contains_key(notebook_doc::mime::BLOB_REF_MIME));
    }

    #[tokio::test]
    async fn preflight_ref_buffers_with_no_buffers_is_noop() {
        let dir = tempfile::tempdir().unwrap();
        let blob_store = test_store(&dir);

        let nbformat = serde_json::json!({
            "output_type": "display_data",
            "data": {
                notebook_doc::mime::BLOB_REF_MIME: {
                    "hash": "abc",
                    "content_type": "image/png",
                    "size": 0,
                    "buffer_index": 0,
                },
            },
        });
        preflight_ref_buffers(&nbformat, &[], &blob_store).await;
        assert!(!blob_store.exists("abc"));
    }

    #[tokio::test]
    async fn dx_ref_mime_with_missing_blob_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let blob_store = test_store(&dir);

        let output = serde_json::json!({
            "output_type": "display_data",
            "data": {
                notebook_doc::mime::BLOB_REF_MIME: {
                    "hash": "0000000000000000000000000000000000000000000000000000000000000000",
                    "content_type": "image/png",
                    "size": 0,
                },
            },
        });

        let manifest = create_manifest(&output, &blob_store, 1024).await.unwrap();
        let data = match manifest {
            OutputManifest::DisplayData { data, .. } => data,
            other => panic!("expected DisplayData, got {other:?}"),
        };

        assert!(data.is_empty());
    }

    #[test]
    fn test_content_ref_serialization() {
        // Inline variant
        let inline = ContentRef::Inline {
            inline: "hello".to_string(),
        };
        let json = serde_json::to_string(&inline).unwrap();
        assert_eq!(json, r#"{"inline":"hello"}"#);

        // Blob variant
        let blob = ContentRef::Blob {
            blob: "abc123".to_string(),
            size: 1000,
        };
        let json = serde_json::to_string(&blob).unwrap();
        assert_eq!(json, r#"{"blob":"abc123","size":1000}"#);
    }

    #[test]
    fn test_content_ref_deserialization() {
        let inline: ContentRef = serde_json::from_str(r#"{"inline":"hello"}"#).unwrap();
        assert!(matches!(inline, ContentRef::Inline { inline } if inline == "hello"));

        let blob: ContentRef = serde_json::from_str(r#"{"blob":"abc123","size":1000}"#).unwrap();
        assert!(
            matches!(blob, ContentRef::Blob { blob, size } if blob == "abc123" && size == 1000)
        );
    }

    #[tokio::test]
    async fn test_content_ref_from_data_inlines_small() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let small_data = "hello world";
        let content_ref = ContentRef::from_data(small_data, "text/plain", &store, 100)
            .await
            .unwrap();

        assert!(content_ref.is_inline());
        assert!(matches!(content_ref, ContentRef::Inline { inline } if inline == small_data));
    }

    #[tokio::test]
    async fn test_content_ref_from_data_blobs_large() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let large_data = "x".repeat(200);
        let content_ref = ContentRef::from_data(&large_data, "text/plain", &store, 100)
            .await
            .unwrap();

        assert!(!content_ref.is_inline());
        assert!(matches!(content_ref, ContentRef::Blob { size, .. } if size == 200));
    }

    #[tokio::test]
    async fn test_content_ref_resolve_inline() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let content_ref = ContentRef::Inline {
            inline: "hello".to_string(),
        };
        let resolved = content_ref.resolve(&store).await.unwrap();
        assert_eq!(resolved, "hello");
    }

    #[tokio::test]
    async fn test_content_ref_resolve_blob() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let data = "blob content";
        let hash = store.put(data.as_bytes(), "text/plain").await.unwrap();

        let content_ref = ContentRef::Blob {
            blob: hash,
            size: data.len() as u64,
        };
        let resolved = content_ref.resolve(&store).await.unwrap();
        assert_eq!(resolved, data);
    }

    #[tokio::test]
    async fn test_create_manifest_display_data() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let output = serde_json::json!({
            "output_type": "display_data",
            "data": {
                "text/plain": "hello",
                "text/html": "<b>hello</b>"
            },
            "metadata": {}
        });

        let manifest = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        assert!(matches!(manifest, OutputManifest::DisplayData { .. }));
    }

    #[tokio::test]
    async fn test_create_manifest_stream() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let output = serde_json::json!({
            "output_type": "stream",
            "name": "stdout",
            "text": "hello world\n"
        });

        let manifest = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        assert!(matches!(manifest, OutputManifest::Stream { name, .. } if name == "stdout"));
    }

    #[tokio::test]
    async fn test_create_manifest_error() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let output = serde_json::json!({
            "output_type": "error",
            "ename": "ValueError",
            "evalue": "invalid value",
            "traceback": ["line 1", "line 2"]
        });

        let manifest = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        assert!(matches!(manifest, OutputManifest::Error { ename, .. } if ename == "ValueError"));
    }

    #[tokio::test]
    async fn test_round_trip_display_data() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let original = serde_json::json!({
            "output_type": "display_data",
            "data": {
                "text/plain": "hello",
                "text/html": "<b>hello</b>"
            },
            "metadata": {}
        });

        let manifest = create_manifest(&original, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let resolved = resolve_manifest(&manifest, &store).await.unwrap();

        assert_eq!(resolved["output_type"], "display_data");
        assert_eq!(resolved["data"]["text/plain"], "hello");
        assert_eq!(resolved["data"]["text/html"], "<b>hello</b>");
    }

    #[tokio::test]
    async fn test_round_trip_execute_result() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let original = serde_json::json!({
            "output_type": "execute_result",
            "data": {
                "text/plain": "42"
            },
            "metadata": {},
            "execution_count": 5
        });

        let manifest = create_manifest(&original, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let resolved = resolve_manifest(&manifest, &store).await.unwrap();

        assert_eq!(resolved["output_type"], "execute_result");
        assert_eq!(resolved["data"]["text/plain"], "42");
        assert_eq!(resolved["execution_count"], 5);
    }

    #[tokio::test]
    async fn test_round_trip_stream() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let original = serde_json::json!({
            "output_type": "stream",
            "name": "stderr",
            "text": "error message\n"
        });

        let manifest = create_manifest(&original, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let resolved = resolve_manifest(&manifest, &store).await.unwrap();

        assert_eq!(resolved["output_type"], "stream");
        assert_eq!(resolved["name"], "stderr");
        assert_eq!(resolved["text"], "error message\n");
    }

    #[tokio::test]
    async fn test_round_trip_error() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let original = serde_json::json!({
            "output_type": "error",
            "ename": "ZeroDivisionError",
            "evalue": "division by zero",
            "traceback": ["Traceback:", "  File \"test.py\"", "ZeroDivisionError"]
        });

        let manifest = create_manifest(&original, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let resolved = resolve_manifest(&manifest, &store).await.unwrap();

        assert_eq!(resolved["output_type"], "error");
        assert_eq!(resolved["ename"], "ZeroDivisionError");
        assert_eq!(resolved["evalue"], "division by zero");
        assert!(resolved["traceback"].is_array());
        assert_eq!(resolved["traceback"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_small_data_inlines_large_blobs() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        // Create output with data both above and below the threshold
        let large_html = format!("<html>{}</html>", "x".repeat(2000));
        let output = serde_json::json!({
            "output_type": "display_data",
            "data": {
                "text/plain": "small",
                "text/html": large_html
            },
            "metadata": {}
        });

        let manifest = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();

        if let OutputManifest::DisplayData { data, .. } = manifest {
            // text/plain should be inlined (< 1KB)
            assert!(data.get("text/plain").unwrap().is_inline());
            // text/html should be a blob (> 1KB)
            assert!(!data.get("text/html").unwrap().is_inline());
        } else {
            panic!("Expected DisplayData manifest");
        }
    }

    #[tokio::test]
    async fn test_stream_text_array_normalization() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        // Jupyter sometimes sends text as array of strings
        let output = serde_json::json!({
            "output_type": "stream",
            "name": "stdout",
            "text": ["line 1\n", "line 2\n"]
        });

        let manifest = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let resolved = resolve_manifest(&manifest, &store).await.unwrap();

        assert_eq!(resolved["text"], "line 1\nline 2\n");
    }

    // ── Binary blob tests ───────────────────────────────────────────

    #[tokio::test]
    async fn test_from_binary_always_uses_blob() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        // Even tiny binary data should go to blob (no inline threshold)
        let tiny_png = b"\x89PNG\r\n\x1a\n";
        let content_ref = ContentRef::from_binary(tiny_png, "image/png", &store)
            .await
            .unwrap();

        assert!(
            !content_ref.is_inline(),
            "Binary content should always use blob, never inline"
        );
        if let ContentRef::Blob { size, .. } = &content_ref {
            assert_eq!(*size, tiny_png.len() as u64);
        }
    }

    #[tokio::test]
    async fn test_binary_round_trip_base64() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        // Known bytes → store as blob → resolve back as base64
        let raw_bytes: Vec<u8> = (0..=255).collect();
        let content_ref = ContentRef::from_binary(&raw_bytes, "image/png", &store)
            .await
            .unwrap();

        let base64_result = content_ref.resolve_binary_as_base64(&store).await.unwrap();

        // Decode the base64 and verify it matches the original bytes
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&base64_result)
            .unwrap();
        assert_eq!(decoded, raw_bytes);
    }

    #[tokio::test]
    async fn test_binary_display_data_round_trip() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        // Simulate what the kernel sends: base64-encoded PNG in a display_data
        let raw_pixels = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        let base64_from_kernel = base64::engine::general_purpose::STANDARD.encode(&raw_pixels);

        let output = serde_json::json!({
            "output_type": "display_data",
            "data": {
                "text/plain": "<Figure>",
                "image/png": base64_from_kernel
            },
            "metadata": {}
        });

        // Create manifest (should base64-decode the PNG and store raw bytes)
        let manifest = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();

        // Resolve manifest (should base64-encode raw bytes back for nbformat)
        let resolved = resolve_manifest(&manifest, &store).await.unwrap();

        assert_eq!(resolved["output_type"], "display_data");
        assert_eq!(resolved["data"]["text/plain"], "<Figure>");
        // The resolved base64 should match what the kernel originally sent
        assert_eq!(resolved["data"]["image/png"], base64_from_kernel);
    }

    // ── Manifest JSON serialization tests ───────────────────────────

    #[tokio::test]
    async fn test_manifest_to_json() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let output = serde_json::json!({
            "output_type": "stream",
            "name": "stdout",
            "text": "hello\n"
        });

        let manifest = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let json_value = manifest.to_json();

        assert_eq!(json_value["output_type"], "stream");
        assert_eq!(json_value["name"], "stdout");
        // Small text should be inlined
        assert_eq!(json_value["text"]["inline"], "hello\n");
    }

    #[tokio::test]
    async fn test_manifest_to_json_blob_ref() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        // Use threshold of 0 to force everything to blob
        let output = serde_json::json!({
            "output_type": "stream",
            "name": "stdout",
            "text": "hello\n"
        });

        let manifest = create_manifest(&output, &store, 0).await.unwrap();
        let json_value = manifest.to_json();

        assert_eq!(json_value["output_type"], "stream");
        assert_eq!(json_value["name"], "stdout");
        // With threshold=0, text should be a blob ref
        assert!(json_value["text"]["blob"].is_string());
        assert!(json_value["text"]["size"].is_number());
    }

    #[tokio::test]
    async fn test_manifest_to_json_round_trip() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let output = serde_json::json!({
            "output_type": "display_data",
            "data": {
                "text/plain": "test"
            },
            "metadata": {}
        });

        let manifest = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let json_value = manifest.to_json();

        // Should be deserializable back to OutputManifest
        let roundtripped: OutputManifest = serde_json::from_value(json_value).unwrap();
        let resolved = resolve_manifest(&roundtripped, &store).await.unwrap();

        assert_eq!(resolved["output_type"], "display_data");
        assert_eq!(resolved["data"]["text/plain"], "test");
    }

    // ── get_display_id / update tests ───────────────────────────────

    #[tokio::test]
    async fn test_get_display_id() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let output = serde_json::json!({
            "output_type": "display_data",
            "data": {"text/plain": "hi"},
            "metadata": {},
            "transient": {"display_id": "my-display"}
        });

        let manifest = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        assert_eq!(get_display_id(&manifest), Some("my-display".to_string()));

        // Stream outputs have no display_id
        let stream_output = serde_json::json!({
            "output_type": "stream",
            "name": "stdout",
            "text": "hi"
        });
        let stream_manifest = create_manifest(&stream_output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        assert_eq!(get_display_id(&stream_manifest), None);
    }

    #[tokio::test]
    async fn test_update_manifest_display_data() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let output = serde_json::json!({
            "output_type": "display_data",
            "data": {"text/plain": "old"},
            "metadata": {},
            "transient": {"display_id": "my-display"}
        });

        let manifest = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();

        let new_data = serde_json::json!({"text/plain": "new"});
        let new_metadata = serde_json::Map::new();

        let updated = update_manifest_display_data(
            &manifest,
            "my-display",
            &new_data,
            &new_metadata,
            &store,
            DEFAULT_INLINE_THRESHOLD,
        )
        .await
        .unwrap();

        assert!(updated.is_some());
        let updated = updated.unwrap();
        let resolved = resolve_manifest(&updated, &store).await.unwrap();
        assert_eq!(resolved["data"]["text/plain"], "new");

        // Non-matching display_id returns None
        let not_updated = update_manifest_display_data(
            &manifest,
            "wrong-id",
            &new_data,
            &new_metadata,
            &store,
            DEFAULT_INLINE_THRESHOLD,
        )
        .await
        .unwrap();
        assert!(not_updated.is_none());
    }

    #[test]
    fn test_is_binary_mime() {
        // Binary image types
        assert!(is_binary_mime("image/png"));
        assert!(is_binary_mime("image/jpeg"));
        assert!(is_binary_mime("image/gif"));
        assert!(is_binary_mime("image/webp"));

        // SVG is text (plain XML in Jupyter)
        assert!(!is_binary_mime("image/svg+xml"));

        // Audio/video
        assert!(is_binary_mime("audio/mpeg"));
        assert!(is_binary_mime("video/mp4"));

        // Binary application types
        assert!(is_binary_mime("application/pdf"));
        assert!(is_binary_mime("application/octet-stream"));
        assert!(is_binary_mime("application/vnd.apache.arrow.stream"));
        assert!(is_binary_mime("application/wasm"));

        // Text-like application types
        assert!(!is_binary_mime("application/json"));
        assert!(!is_binary_mime("application/javascript"));
        assert!(!is_binary_mime("application/xml"));
        assert!(!is_binary_mime("application/vnd.vegalite.v5+json"));
        assert!(!is_binary_mime("application/xhtml+xml"));

        // Text types
        assert!(!is_binary_mime("text/plain"));
        assert!(!is_binary_mime("text/html"));
        assert!(!is_binary_mime("text/latex"));
    }

    // ── Ref-MIME save whitelist (Spec 2) ────────────────────────────

    #[tokio::test]
    async fn test_resolve_data_bundle_emits_blob_ref_for_whitelisted_mime() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let raw = b"PAR1-parquet-payload-bytes";
        let hash = store
            .put(raw, "application/vnd.apache.parquet")
            .await
            .unwrap();

        let mut data = HashMap::new();
        data.insert(
            "application/vnd.apache.parquet".to_string(),
            ContentRef::Blob {
                blob: hash.clone(),
                size: raw.len() as u64,
            },
        );
        data.insert(
            "text/plain".to_string(),
            ContentRef::Inline {
                inline: "DataFrame (pandas): 3 rows × 2 columns".to_string(),
            },
        );

        let resolved = resolve_data_bundle(&data, &store).await.unwrap();

        // Original whitelisted MIME key is absent; BLOB_REF_MIME took its place.
        assert!(
            !resolved.contains_key("application/vnd.apache.parquet"),
            "whitelisted MIME should be rewritten, not kept: {:?}",
            resolved.keys().collect::<Vec<_>>()
        );
        let ref_entry = resolved
            .get(BLOB_REF_MIME)
            .expect("BLOB_REF_MIME entry present");
        assert_eq!(ref_entry["hash"], hash);
        assert_eq!(ref_entry["content_type"], "application/vnd.apache.parquet");
        assert_eq!(ref_entry["size"], raw.len());

        // Non-binary siblings are untouched.
        assert_eq!(
            resolved.get("text/plain").and_then(|v| v.as_str()),
            Some("DataFrame (pandas): 3 rows × 2 columns")
        );
    }

    #[tokio::test]
    async fn test_resolve_data_bundle_non_whitelisted_binary_stays_base64() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        // A "large" image blob — whitelist-based externalization only
        // applies to parquet, so images keep the classic base64 path
        // regardless of size.
        let raw = vec![0xAAu8; 64 * 1024];
        let content_ref = ContentRef::from_binary(&raw, "image/png", &store)
            .await
            .unwrap();

        let mut data = HashMap::new();
        data.insert("image/png".to_string(), content_ref);

        let resolved = resolve_data_bundle(&data, &store).await.unwrap();

        assert!(
            !resolved.contains_key(BLOB_REF_MIME),
            "non-whitelisted binary must NOT emit BLOB_REF_MIME"
        );
        let b64 = resolved
            .get("image/png")
            .and_then(|v| v.as_str())
            .expect("image/png should be present as base64 string");
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .unwrap();
        assert_eq!(decoded, raw);
    }

    #[tokio::test]
    async fn test_resolve_data_bundle_round_trip_blob_ref() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        // Pre-populate a blob that will become a blob-ref on save.
        let raw = b"PAR1-this-payload-is-larger-than-sixteen-bytes-for-sure";
        let hash = store
            .put(raw, "application/vnd.apache.parquet")
            .await
            .unwrap();

        // Initial manifest: a display_data holding a parquet ContentRef::Blob
        // directly (mimics dx.display having already uploaded the payload).
        let mut data = HashMap::new();
        data.insert(
            "application/vnd.apache.parquet".to_string(),
            ContentRef::Blob {
                blob: hash.clone(),
                size: raw.len() as u64,
            },
        );
        data.insert(
            "text/html".to_string(),
            ContentRef::Inline {
                inline: "<table/>".to_string(),
            },
        );
        let manifest_a = OutputManifest::DisplayData {
            data,
            metadata: HashMap::new(),
            transient: TransientData::default(),
        };

        // Resolve → save-shape JSON (ref-MIME appears).
        let saved = resolve_manifest(&manifest_a, &store).await.unwrap();
        assert!(saved["data"].get(BLOB_REF_MIME).is_some());
        assert!(saved["data"]
            .get("application/vnd.apache.parquet")
            .is_none());

        // Load that JSON back via create_manifest → ContentRef::Blob composed
        // under the original content_type. Round-trip should land on an
        // equivalent manifest shape.
        let reloaded = create_manifest(&saved, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        match reloaded {
            OutputManifest::DisplayData { data, .. } => {
                assert!(!data.contains_key(BLOB_REF_MIME));
                let parquet = data
                    .get("application/vnd.apache.parquet")
                    .expect("parquet key restored on load");
                match parquet {
                    ContentRef::Blob { blob, size } => {
                        assert_eq!(blob, &hash);
                        assert_eq!(*size, raw.len() as u64);
                    }
                    other => panic!("expected ContentRef::Blob, got {other:?}"),
                }
                // Sibling HTML survived as well.
                assert!(data.contains_key("text/html"));
            }
            other => panic!("expected DisplayData, got {other:?}"),
        }

        // And saving the reloaded manifest again gives the same shape
        // (idempotent under repeated save/load cycles).
        let saved_again = resolve_manifest(
            &create_manifest(&saved, &store, DEFAULT_INLINE_THRESHOLD)
                .await
                .unwrap(),
            &store,
        )
        .await
        .unwrap();
        assert_eq!(
            saved_again["data"][BLOB_REF_MIME],
            saved["data"][BLOB_REF_MIME]
        );
    }

    #[tokio::test]
    async fn small_stream_has_no_preview() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);
        let out = serde_json::json!({
            "output_type": "stream",
            "name": "stdout",
            "text": "hello\n",
        });
        let m = create_manifest(&out, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let OutputManifest::Stream {
            text, llm_preview, ..
        } = m
        else {
            panic!("expected Stream");
        };
        assert!(matches!(text, ContentRef::Inline { .. }));
        assert!(llm_preview.is_none());
    }

    #[tokio::test]
    async fn large_stream_has_preview() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);
        let big = (0..500).map(|i| format!("line {i}\n")).collect::<String>();
        let out = serde_json::json!({
            "output_type": "stream",
            "name": "stdout",
            "text": big.clone(),
        });
        let m = create_manifest(&out, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let OutputManifest::Stream {
            text, llm_preview, ..
        } = m
        else {
            panic!("expected Stream");
        };
        assert!(matches!(text, ContentRef::Blob { .. }));
        let p = llm_preview.expect("preview when blob-stored");
        assert_eq!(p.total_lines, 500);
        assert_eq!(p.total_bytes, big.len() as u64);
        assert!(p.head.starts_with("line 0\n"));
        assert!(p.tail.trim_end().ends_with("line 499"));
    }

    #[tokio::test]
    async fn small_error_has_no_preview() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);
        let out = serde_json::json!({
            "output_type": "error",
            "ename": "NameError",
            "evalue": "x",
            "traceback": ["frame 1", "frame 2"],
        });
        let m = create_manifest(&out, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let OutputManifest::Error {
            traceback,
            llm_preview,
            ..
        } = m
        else {
            panic!("expected Error");
        };
        assert!(matches!(traceback, ContentRef::Inline { .. }));
        assert!(llm_preview.is_none());
    }

    #[tokio::test]
    async fn large_error_has_preview_with_last_frame() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);
        let frames: Vec<String> = (0..200).map(|i| format!("frame line {i}")).collect();
        let out = serde_json::json!({
            "output_type": "error",
            "ename": "RecursionError",
            "evalue": "maximum recursion depth",
            "traceback": frames,
        });
        let m = create_manifest(&out, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let OutputManifest::Error {
            traceback,
            llm_preview,
            ..
        } = m
        else {
            panic!("expected Error");
        };
        assert!(matches!(traceback, ContentRef::Blob { .. }));
        let p = llm_preview.expect("preview when blob-stored");
        assert_eq!(p.frames, 200);
        assert_eq!(p.last_frame, "frame line 199");
    }

    #[test]
    fn manifest_without_preview_field_deserializes_to_none() {
        let legacy = serde_json::json!({
            "output_type": "stream",
            "name": "stdout",
            "text": {"inline": "hello"},
        });
        let m: OutputManifest = serde_json::from_value(legacy).unwrap();
        let OutputManifest::Stream { llm_preview, .. } = m else {
            panic!("expected Stream");
        };
        assert!(llm_preview.is_none());
    }
}
