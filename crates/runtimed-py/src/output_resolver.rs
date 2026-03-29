//! Output resolution for converting blob hashes and JSON to Output objects.
//!
//! This module provides standalone async functions for resolving outputs,
//! used by both Session (during execution) and Cell (when fetching from Automerge).

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine as _;
use serde_json::Value;

use crate::output::{DataValue, Output};

/// Classification of a MIME type for the Python API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MimeKind {
    /// UTF-8 text: text/*, image/svg+xml, application/javascript, etc.
    Text,
    /// Raw binary bytes: image/png, audio/*, video/*, etc.
    Binary,
    /// JSON data → Python dict/list: application/json, *+json
    Json,
}

/// Classify a MIME type into Text, Binary, or Json.
fn mime_kind(mime: &str) -> MimeKind {
    // JSON types → native Python dicts
    if mime == "application/json" {
        return MimeKind::Json;
    }
    if let Some(subtype) = mime.strip_prefix("application/") {
        if subtype.ends_with("+json") {
            return MimeKind::Json;
        }
    }

    // Binary images (but NOT SVG — that's XML text)
    if mime.starts_with("image/") {
        return if mime.ends_with("+xml") {
            MimeKind::Text
        } else {
            MimeKind::Binary
        };
    }

    // Audio/video are always binary
    if mime.starts_with("audio/") || mime.starts_with("video/") {
        return MimeKind::Binary;
    }

    // application/* is binary by default, with carve-outs for text-like formats
    if let Some(subtype) = mime.strip_prefix("application/") {
        let is_text = subtype == "javascript"
            || subtype == "ecmascript"
            || subtype == "xml"
            || subtype == "xhtml+xml"
            || subtype == "mathml+xml"
            || subtype == "sql"
            || subtype == "graphql"
            || subtype == "x-latex"
            || subtype == "x-tex"
            || subtype.ends_with("+xml");
        return if is_text {
            MimeKind::Text
        } else {
            MimeKind::Binary
        };
    }

    // Everything else (text/*, unknown) is text
    MimeKind::Text
}

/// Check if a string looks like a blob hash (64 hex characters).
fn is_blob_hash(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Resolve an output string to an Output.
///
/// The output string can be:
/// - Raw JSON for backward compatibility
/// - A blob hash (64-char hex SHA-256) pointing to an output manifest
///
/// When output_type is None, attempts to extract it from the JSON/manifest.
pub async fn resolve_output_string(
    output_str: &str,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Option<Output> {
    // Try to parse as raw JSON first
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output_str) {
        // Extract output_type from JSON
        let output_type = parsed.get("output_type")?.as_str()?;
        return output_from_json(output_type, &parsed);
    }

    // If it looks like a blob hash, try to resolve it
    if is_blob_hash(output_str) {
        log::debug!("[output_resolver] Detected blob hash: {}", output_str);

        // First try: read directly from disk (most reliable).
        // Manifests are always JSON text, so read_to_string is correct here.
        if let Some(store_path) = blob_store_path {
            let prefix = &output_str[..2];
            let rest = &output_str[2..];
            let blob_path = store_path.join(prefix).join(rest);
            log::debug!("[output_resolver] Trying blob path: {:?}", blob_path);

            if let Ok(contents) = tokio::fs::read_to_string(&blob_path).await {
                log::debug!(
                    "[output_resolver] Read blob file, contents len: {}",
                    contents.len()
                );
                if let Ok(manifest) = serde_json::from_str::<serde_json::Value>(&contents) {
                    // Extract output_type from manifest
                    if let Some(output_type) = manifest.get("output_type").and_then(|v| v.as_str())
                    {
                        return output_from_manifest(
                            output_type,
                            &manifest,
                            blob_base_url,
                            blob_store_path,
                        )
                        .await;
                    }
                }
            }
        }

        // Second try: fetch from blob server
        if let Some(base_url) = blob_base_url {
            let url = format!("{}/blob/{}", base_url, output_str);
            if let Ok(response) = reqwest::get(&url).await {
                if response.status().is_success() {
                    if let Ok(manifest) = response.json::<serde_json::Value>().await {
                        if let Some(output_type) =
                            manifest.get("output_type").and_then(|v| v.as_str())
                        {
                            return output_from_manifest(
                                output_type,
                                &manifest,
                                blob_base_url,
                                blob_store_path,
                            )
                            .await;
                        }
                    }
                }
            }
        }
    }

    // Unable to resolve - return a fallback error output to preserve visibility
    log::debug!(
        "[output_resolver] Failed to resolve output string: {}",
        &output_str[..output_str.len().min(100)]
    );
    Some(Output::stream(
        "stderr",
        &format!(
            "Failed to resolve output: {}",
            &output_str[..output_str.len().min(64)]
        ),
    ))
}

/// Convert a parsed JSON value to an Output.
///
/// For display_data/execute_result, binary MIME types (image/png, etc.)
/// are base64-decoded from Jupyter's wire format into raw bytes.
pub fn output_from_json(output_type: &str, json: &serde_json::Value) -> Option<Output> {
    match output_type {
        "stream" => {
            let name = json.get("name")?.as_str()?;
            let text = json.get("text")?.as_str()?;
            Some(Output::stream(name, text))
        }
        "display_data" => {
            let data = json.get("data")?.as_object()?;
            Some(Output::display_data(json_data_to_datavalues(data)))
        }
        "execute_result" => {
            let data = json.get("data")?.as_object()?;
            let execution_count = json.get("execution_count")?.as_i64()?;
            Some(Output::execute_result(
                json_data_to_datavalues(data),
                execution_count,
            ))
        }
        "error" => {
            let ename = json.get("ename")?.as_str()?.to_string();
            let evalue = json.get("evalue")?.as_str()?.to_string();
            let traceback = json
                .get("traceback")?
                .as_array()?
                .iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            Some(Output::error(&ename, &evalue, traceback))
        }
        _ => None,
    }
}

/// Convert a JSON data map (mime → value) to DataValue entries.
///
/// - Binary MIME types are base64-decoded from Jupyter's wire format → raw bytes
/// - JSON MIME types are parsed into serde_json::Value → Python dict
/// - Text MIME types (including SVG) are kept as strings
///
/// Also synthesizes `text/llm+plain` for outputs with binary images when
/// no LLM-friendly text is present. (Note: no blob URL available in the
/// raw-JSON path since the data came from the wire, not the blob store.)
fn json_data_to_datavalues(data: &serde_json::Map<String, Value>) -> HashMap<String, DataValue> {
    let mut output_data = HashMap::new();
    let mut has_image = false;

    for (mime, value) in data {
        let dv = match mime_kind(mime) {
            MimeKind::Binary => {
                // Jupyter sends binary data as base64 strings on the wire
                if let Some(s) = value.as_str() {
                    match base64::engine::general_purpose::STANDARD.decode(s) {
                        Ok(bytes) => {
                            if mime.starts_with("image/") {
                                has_image = true;
                            }
                            DataValue::Binary(bytes)
                        }
                        Err(_) => DataValue::Text(s.to_string()),
                    }
                } else {
                    DataValue::Text(value.to_string())
                }
            }
            MimeKind::Json => {
                // JSON data: if it's a string, parse it; if it's already an object, keep it
                if let Some(s) = value.as_str() {
                    match serde_json::from_str::<Value>(s) {
                        Ok(parsed) => DataValue::Json(parsed),
                        Err(_) => DataValue::Text(s.to_string()),
                    }
                } else {
                    // Already a JSON value (object, array, etc.)
                    DataValue::Json(value.clone())
                }
            }
            MimeKind::Text => {
                if let Some(s) = value.as_str() {
                    DataValue::Text(s.to_string())
                } else {
                    DataValue::Text(value.to_string())
                }
            }
        };
        output_data.insert(mime.clone(), dv);
    }

    // Synthesize text/llm+plain for binary images (raw-JSON path has no blob URL)
    if has_image && !output_data.contains_key("text/llm+plain") {
        let mut parts: Vec<String> = Vec::new();
        if let Some(DataValue::Text(ref plain)) = output_data.get("text/plain") {
            parts.push(plain.clone());
        }
        for (mime, dv) in &output_data {
            if let DataValue::Binary(bytes) = dv {
                if mime.starts_with("image/") {
                    parts.push(format!(
                        "📊 Image output ({}, {} KB)",
                        mime,
                        bytes.len() / 1024
                    ));
                }
            }
        }
        output_data.insert(
            "text/llm+plain".to_string(),
            DataValue::Text(parts.join("\n")),
        );
    }

    output_data
}

/// Convert a blob manifest to an Output.
///
/// The manifest has a format like:
/// {"output_type": "stream", "name": "stdout", "text": {"inline": "..."}}
///
/// For display_data/execute_result, each MIME type's content ref is resolved
/// according to its type: binary MIME types are returned as raw bytes
/// (`DataValue::Binary`); text MIME types as strings (`DataValue::Text`).
pub async fn output_from_manifest(
    output_type: &str,
    manifest: &serde_json::Value,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Option<Output> {
    match output_type {
        "stream" => {
            let name = manifest.get("name")?.as_str()?;
            let text_ref = manifest.get("text")?;
            let text = resolve_text_ref(text_ref, blob_base_url, blob_store_path).await?;
            Some(Output::stream(name, &text))
        }
        "display_data" | "execute_result" => {
            let data_map = manifest.get("data")?.as_object()?;
            let mut output_data = HashMap::new();

            // Track binary image blobs for text/llm+plain synthesis
            let mut image_descriptions: Vec<String> = Vec::new();

            // Track blob metadata for each MIME type
            let mut blob_urls_map: HashMap<String, String> = HashMap::new();
            let mut blob_paths_map: HashMap<String, String> = HashMap::new();

            for (mime_type, content_ref) in data_map {
                if let Some(content) = resolve_content_ref(
                    content_ref,
                    blob_base_url,
                    blob_store_path,
                    Some(mime_type.as_str()),
                )
                .await
                {
                    // Extract blob metadata for any MIME type with a blob reference
                    if let Some(blob_hash) = content_ref.get("blob").and_then(|v| v.as_str()) {
                        if blob_hash.len() >= 2 {
                            if let Some(base_url) = blob_base_url {
                                blob_urls_map.insert(
                                    mime_type.clone(),
                                    format!("{}/blob/{}", base_url, blob_hash),
                                );
                            }
                            if let Some(store_path) = blob_store_path {
                                let path = store_path.join(&blob_hash[..2]).join(&blob_hash[2..]);
                                blob_paths_map
                                    .insert(mime_type.clone(), path.to_string_lossy().to_string());
                            }
                        }
                    }

                    // Record binary image metadata for LLM description
                    if let DataValue::Binary(ref bytes) = content {
                        if mime_type.starts_with("image/") {
                            let size_kb = bytes.len() / 1024;
                            let mut desc =
                                format!("📊 Image output ({}, {} KB)", mime_type, size_kb);
                            if let Some(url) = blob_urls_map.get(mime_type) {
                                desc.push_str(&format!("\n{}", url));
                            } else if let Some(path) = blob_paths_map.get(mime_type) {
                                desc.push_str(&format!("\n{}", path));
                            }
                            image_descriptions.push(desc);
                        }
                    }

                    output_data.insert(mime_type.clone(), content);
                }
            }

            // Synthesize text/llm+plain if there are binary images and no
            // existing LLM-friendly text representation
            if !image_descriptions.is_empty() && !output_data.contains_key("text/llm+plain") {
                // Combine any existing text/plain with image descriptions
                let mut parts: Vec<String> = Vec::new();
                if let Some(DataValue::Text(ref plain)) = output_data.get("text/plain") {
                    parts.push(plain.clone());
                }
                parts.extend(image_descriptions);
                output_data.insert(
                    "text/llm+plain".to_string(),
                    DataValue::Text(parts.join("\n")),
                );
            }

            let mut output = if output_type == "execute_result" {
                let execution_count = manifest.get("execution_count")?.as_i64()?;
                Output::execute_result(output_data, execution_count)
            } else {
                Output::display_data(output_data)
            };
            if !blob_urls_map.is_empty() {
                output.blob_urls = Some(blob_urls_map);
            }
            if !blob_paths_map.is_empty() {
                output.blob_paths = Some(blob_paths_map);
            }
            Some(output)
        }
        "error" => {
            let ename = manifest.get("ename")?.as_str()?.to_string();
            let evalue = manifest.get("evalue")?.as_str()?.to_string();

            // Traceback can be a content ref ({"inline": "[...]"}) or a direct array
            let traceback_val = manifest.get("traceback")?;
            let traceback = if let Some(arr) = traceback_val.as_array() {
                // Direct array
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            } else {
                // Content reference - resolve it and parse as JSON array
                let tb_str =
                    resolve_text_ref(traceback_val, blob_base_url, blob_store_path).await?;
                serde_json::from_str::<Vec<String>>(&tb_str).ok()?
            };

            Some(Output::error(&ename, &evalue, traceback))
        }
        _ => None,
    }
}

/// Resolve a content reference from a blob manifest, returning a [`DataValue`].
///
/// Content refs can be:
/// - `{"inline": "actual content"}` — content is inline
/// - `{"blob": "hash", "size": N}` — content is in the blob store
///
/// For binary MIME types (images, etc.), the blob store holds raw bytes
/// (decoded from Jupyter's base64 wire format). This function returns the
/// raw bytes as `DataValue::Binary` — no re-encoding to base64.
///
/// For text MIME types, the blob store holds UTF-8 text which is returned
/// as `DataValue::Text`.
pub async fn resolve_content_ref(
    content_ref: &serde_json::Value,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
    mime_type: Option<&str>,
) -> Option<DataValue> {
    let kind = mime_type.map(mime_kind).unwrap_or(MimeKind::Text);

    if let Some(inline) = content_ref.get("inline") {
        let s = inline.as_str()?;
        return Some(match kind {
            MimeKind::Binary => {
                // Inline binary is base64-encoded; decode to raw bytes
                base64::engine::general_purpose::STANDARD
                    .decode(s)
                    .map(DataValue::Binary)
                    .unwrap_or_else(|_| DataValue::Text(s.to_string()))
            }
            MimeKind::Json => {
                // Inline JSON: parse if it's a string representation
                serde_json::from_str::<Value>(s)
                    .map(DataValue::Json)
                    .unwrap_or_else(|_| DataValue::Text(s.to_string()))
            }
            MimeKind::Text => DataValue::Text(s.to_string()),
        });
    }

    // Also handle inline JSON values (not wrapped in a string)
    if kind == MimeKind::Json {
        if let Some(inline) = content_ref.get("inline") {
            if inline.is_object() || inline.is_array() {
                return Some(DataValue::Json(inline.clone()));
            }
        }
    }

    let blob_hash = content_ref.get("blob").and_then(|v| v.as_str())?;

    // First try: read directly from disk
    if let Some(store_path) = blob_store_path {
        if blob_hash.len() >= 2 {
            let prefix = &blob_hash[..2];
            let rest = &blob_hash[2..];
            let blob_path = store_path.join(prefix).join(rest);

            match kind {
                MimeKind::Binary => {
                    if let Ok(bytes) = tokio::fs::read(&blob_path).await {
                        return Some(DataValue::Binary(bytes));
                    }
                }
                MimeKind::Json => {
                    if let Ok(contents) = tokio::fs::read_to_string(&blob_path).await {
                        if let Ok(parsed) = serde_json::from_str::<Value>(&contents) {
                            return Some(DataValue::Json(parsed));
                        }
                        return Some(DataValue::Text(contents));
                    }
                }
                MimeKind::Text => {
                    if let Ok(contents) = tokio::fs::read_to_string(&blob_path).await {
                        return Some(DataValue::Text(contents));
                    }
                }
            }
        }
    }

    // Second try: fetch from blob server
    if let Some(base_url) = blob_base_url {
        let url = format!("{}/blob/{}", base_url, blob_hash);

        if let Ok(response) = reqwest::get(&url).await {
            if response.status().is_success() {
                match kind {
                    MimeKind::Binary => {
                        if let Ok(bytes) = response.bytes().await {
                            return Some(DataValue::Binary(bytes.to_vec()));
                        }
                    }
                    MimeKind::Json => {
                        if let Ok(text) = response.text().await {
                            if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
                                return Some(DataValue::Json(parsed));
                            }
                            return Some(DataValue::Text(text));
                        }
                    }
                    MimeKind::Text => {
                        if let Ok(text) = response.text().await {
                            return Some(DataValue::Text(text));
                        }
                    }
                }
            }
        }
    }

    None
}

/// Convenience wrapper: resolve a content ref that is always text.
///
/// Used for stream text, error tracebacks, and other non-data fields
/// where the result is always a UTF-8 string.
async fn resolve_text_ref(
    content_ref: &serde_json::Value,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Option<String> {
    match resolve_content_ref(content_ref, blob_base_url, blob_store_path, None).await? {
        DataValue::Text(s) => Some(s),
        DataValue::Json(v) => Some(v.to_string()),
        DataValue::Binary(_) => None,
    }
}

/// Resolve all outputs for a cell snapshot.
///
/// Takes the raw output strings from CellSnapshot and resolves them to Output objects.
pub async fn resolve_cell_outputs(
    raw_outputs: &[String],
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Vec<Output> {
    let mut outputs = Vec::with_capacity(raw_outputs.len());
    for output_str in raw_outputs {
        if let Some(output) =
            resolve_output_string(output_str, blob_base_url, blob_store_path).await
        {
            outputs.push(output);
        }
    }
    outputs
}
