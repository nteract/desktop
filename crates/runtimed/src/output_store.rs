//! Output store: manifests, ContentRef, and inlining for notebook outputs.
//!
//! This module provides the foundation for Phase 6 output handling:
//! - `ContentRef`: a reference to content that may be inlined or stored in the blob store
//! - Output manifests: Jupyter output types with `ContentRef` for data fields
//! - Inlining threshold: small data is inlined, large data goes to blob store
//!
//! ## Design
//!
//! Instead of storing full Jupyter outputs as JSON strings in the CRDT (which
//! causes bloat for images and large outputs), we store output manifests that
//! reference content via `ContentRef`. Small content (< 8KB by default) is
//! inlined in the manifest. Large content is stored in the blob store.
//!
//! The manifest is itself stored in the blob store with media type
//! `application/x-jupyter-output+json`, and its hash is stored in the CRDT.

use std::collections::HashMap;
use std::io;

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::blob_store::BlobStore;

/// Default inlining threshold: 8 KB.
///
/// Content smaller than this is inlined in the manifest.
/// Content equal to or larger than this is stored in the blob store.
pub const DEFAULT_INLINE_THRESHOLD: usize = 8 * 1024;

/// Media type for output manifests stored in the blob store.
pub const MANIFEST_MEDIA_TYPE: &str = "application/x-jupyter-output+json";

/// A reference to content that may be inlined or stored in the blob store.
///
/// Serializes as an untagged enum:
/// - `{"inline": "..."}`  — content is inlined
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
    Stream { name: String, text: ContentRef },
    #[serde(rename = "error")]
    Error {
        ename: String,
        evalue: String,
        traceback: ContentRef,
    },
}

// =============================================================================
// Manifest creation and resolution
// =============================================================================

/// Create an output manifest from a raw Jupyter output JSON value.
///
/// Applies the inlining threshold to data fields:
/// - Data smaller than the threshold is inlined
/// - Data larger than the threshold is stored in the blob store
///
/// Returns the manifest as a JSON string.
pub async fn create_manifest(
    output: &Value,
    blob_store: &BlobStore,
    threshold: usize,
) -> io::Result<String> {
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
            OutputManifest::Stream { name, text }
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
            OutputManifest::Error {
                ename,
                evalue,
                traceback,
            }
        }
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("unknown output_type: {}", output_type),
            ))
        }
    };

    serde_json::to_string(&manifest).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// Store a manifest JSON string in the blob store.
///
/// Returns the blob hash that can be stored in the CRDT.
pub async fn store_manifest(manifest_json: &str, blob_store: &BlobStore) -> io::Result<String> {
    blob_store
        .put(manifest_json.as_bytes(), MANIFEST_MEDIA_TYPE)
        .await
}

/// Get the display_id from a manifest JSON string, if present.
///
/// Used by UpdateDisplayData to find the output to update.
pub fn get_display_id(manifest_json: &str) -> Option<String> {
    let manifest: OutputManifest = serde_json::from_str(manifest_json).ok()?;
    match manifest {
        OutputManifest::DisplayData { transient, .. }
        | OutputManifest::ExecuteResult { transient, .. } => transient.display_id,
        _ => None,
    }
}

/// Update display data in a manifest with new data and metadata.
///
/// Returns the updated manifest JSON if the manifest is a display_data or execute_result
/// with matching display_id, otherwise returns None.
pub async fn update_manifest_display_data(
    manifest_json: &str,
    display_id: &str,
    new_data: &serde_json::Value,
    new_metadata: &serde_json::Map<String, serde_json::Value>,
    blob_store: &BlobStore,
    threshold: usize,
) -> io::Result<Option<String>> {
    let manifest: OutputManifest = serde_json::from_str(manifest_json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    // Check if this manifest has the matching display_id
    let matches = match &manifest {
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
            // Convert new_data Value to ContentRef map
            let data = convert_value_to_content_refs(new_data, blob_store, threshold).await?;
            let metadata = new_metadata.clone().into_iter().collect();
            let updated = OutputManifest::DisplayData {
                data,
                metadata,
                transient,
            };
            let json = serde_json::to_string(&updated)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(Some(json))
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
                execution_count,
                transient,
            };
            let json = serde_json::to_string(&updated)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
            Ok(Some(json))
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
pub async fn resolve_manifest(manifest_json: &str, blob_store: &BlobStore) -> io::Result<Value> {
    let manifest: OutputManifest = serde_json::from_str(manifest_json)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

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
                output["metadata"] = Value::Object(metadata.into_iter().collect());
            } else {
                output["metadata"] = Value::Object(serde_json::Map::new());
            }
            if !transient.is_empty() {
                let mut transient_map = serde_json::Map::new();
                if let Some(display_id) = transient.display_id {
                    transient_map.insert("display_id".to_string(), Value::String(display_id));
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
                output["metadata"] = Value::Object(metadata.into_iter().collect());
            } else {
                output["metadata"] = Value::Object(serde_json::Map::new());
            }
            if !transient.is_empty() {
                let mut transient_map = serde_json::Map::new();
                if let Some(display_id) = transient.display_id {
                    transient_map.insert("display_id".to_string(), Value::String(display_id));
                }
                output["transient"] = Value::Object(transient_map);
            }
            Ok(output)
        }
        OutputManifest::Stream { name, text } => {
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
async fn resolve_data_bundle(
    data: HashMap<String, ContentRef>,
    blob_store: &BlobStore,
) -> io::Result<HashMap<String, Value>> {
    let mut result = HashMap::new();

    for (mime_type, content_ref) in data {
        let value = if is_binary_mime(&mime_type) {
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
        result.insert(mime_type, value);
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

/// Check if a MIME type represents binary content.
///
/// Binary MIME types are base64-encoded on the Jupyter wire protocol.
/// We decode them to raw bytes before storing in the blob store so that:
/// - The blob store holds actual binary content (33% smaller than base64)
/// - The HTTP blob server serves correct Content-Type with real bytes
/// - Future binary formats (Arrow IPC, Parquet) work without changes
fn is_binary_mime(mime: &str) -> bool {
    if mime.starts_with("image/") {
        // SVG is plain XML text in Jupyter, not base64-encoded binary.
        return !mime.ends_with("+xml");
    }
    if mime.starts_with("audio/") || mime.starts_with("video/") {
        return true;
    }

    // application/* is binary by default, with carve-outs for text-like formats.
    if let Some(subtype) = mime.strip_prefix("application/") {
        // Text-like application types that should NOT be treated as binary
        let is_text = subtype == "json"
            || subtype == "javascript"
            || subtype == "ecmascript"
            || subtype == "xml"
            || subtype == "xhtml+xml"
            || subtype == "mathml+xml"
            || subtype == "sql"
            || subtype == "graphql"
            || subtype == "x-latex"
            || subtype == "x-tex"
            || subtype.ends_with("+json")
            || subtype.ends_with("+xml");
        return !is_text;
    }

    false
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

        let manifest_json = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();

        let manifest: OutputManifest = serde_json::from_str(&manifest_json).unwrap();
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

        let manifest_json = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();

        let manifest: OutputManifest = serde_json::from_str(&manifest_json).unwrap();
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

        let manifest_json = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();

        let manifest: OutputManifest = serde_json::from_str(&manifest_json).unwrap();
        assert!(matches!(manifest, OutputManifest::Error { ename, .. } if ename == "ValueError"));
    }

    #[tokio::test]
    async fn test_store_manifest() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        let manifest = r#"{"output_type":"stream","name":"stdout","text":{"inline":"hello"}}"#;
        let hash = store_manifest(manifest, &store).await.unwrap();

        // Verify it's stored with correct media type
        let meta = store.get_meta(&hash).await.unwrap().unwrap();
        assert_eq!(meta.media_type, MANIFEST_MEDIA_TYPE);
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

        let manifest_json = create_manifest(&original, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let resolved = resolve_manifest(&manifest_json, &store).await.unwrap();

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

        let manifest_json = create_manifest(&original, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let resolved = resolve_manifest(&manifest_json, &store).await.unwrap();

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

        let manifest_json = create_manifest(&original, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let resolved = resolve_manifest(&manifest_json, &store).await.unwrap();

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

        let manifest_json = create_manifest(&original, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let resolved = resolve_manifest(&manifest_json, &store).await.unwrap();

        assert_eq!(resolved["output_type"], "error");
        assert_eq!(resolved["ename"], "ZeroDivisionError");
        assert_eq!(resolved["evalue"], "division by zero");
        assert!(resolved["traceback"].is_array());
        assert_eq!(resolved["traceback"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn test_large_data_uses_blob() {
        let dir = TempDir::new().unwrap();
        let store = test_store(&dir);

        // Create output with data larger than threshold
        let large_html = "<html>".to_string() + &"x".repeat(10000) + "</html>";
        let output = serde_json::json!({
            "output_type": "display_data",
            "data": {
                "text/plain": "small",
                "text/html": large_html
            },
            "metadata": {}
        });

        let manifest_json = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let manifest: OutputManifest = serde_json::from_str(&manifest_json).unwrap();

        if let OutputManifest::DisplayData { data, .. } = manifest {
            // text/plain should be inlined (< 8KB)
            assert!(data.get("text/plain").unwrap().is_inline());
            // text/html should be a blob (> 8KB)
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

        let manifest_json = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();
        let resolved = resolve_manifest(&manifest_json, &store).await.unwrap();

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
        let manifest_json = create_manifest(&output, &store, DEFAULT_INLINE_THRESHOLD)
            .await
            .unwrap();

        // Resolve manifest (should base64-encode raw bytes back for nbformat)
        let resolved = resolve_manifest(&manifest_json, &store).await.unwrap();

        assert_eq!(resolved["output_type"], "display_data");
        assert_eq!(resolved["data"]["text/plain"], "<Figure>");
        // The resolved base64 should match what the kernel originally sent
        assert_eq!(resolved["data"]["image/png"], base64_from_kernel);
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
}
