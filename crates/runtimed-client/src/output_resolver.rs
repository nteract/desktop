//! Output resolution for converting blob hashes and JSON to Output objects.
//!
//! This module provides standalone async functions for resolving outputs,
//! used by both the MCP server and the Python bindings.

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine as _;
use serde_json::Value;

use crate::resolved_output::{DataValue, Output};

/// Classification of a MIME type for output data.
///
/// This is the canonical Rust implementation of MIME classification.
/// Must stay in sync with:
/// - `crates/runtimed/src/output_store.rs` — `is_binary_mime()`
/// - `apps/notebook/src/lib/manifest-resolution.ts` — `isBinaryMime()`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MimeKind {
    /// UTF-8 text: text/*, image/svg+xml, application/javascript, etc.
    Text,
    /// Raw binary bytes: image/png, audio/*, video/*, etc.
    Binary,
    /// JSON data: application/json, *+json
    Json,
}

/// Classify a MIME type into Text, Binary, or Json.
pub fn mime_kind(mime: &str) -> MimeKind {
    // JSON types
    if mime == "application/json" {
        return MimeKind::Json;
    }
    if let Some(subtype) = mime.strip_prefix("application/") {
        if subtype.ends_with("+json") || subtype.ends_with(".json") {
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
pub fn is_blob_hash(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Resolve an output string to an Output.
///
/// The output string can be:
/// - Raw JSON for backward compatibility
/// - A blob hash (64-char hex SHA-256) pointing to an output manifest
pub async fn resolve_output_string(
    output_str: &str,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Option<Output> {
    // Try to parse as raw JSON first
    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(output_str) {
        let output_type = parsed.get("output_type")?.as_str()?;
        return output_from_json(output_type, &parsed);
    }

    // If it looks like a blob hash, try to resolve it
    if is_blob_hash(output_str) {
        log::debug!("[output_resolver] Detected blob hash: {}", output_str);

        // First try: read directly from disk
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

    // Unable to resolve - return a fallback error output
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

/// Convert a JSON data map (mime -> value) to DataValue entries.
///
/// Binary MIME types are base64-decoded from Jupyter's wire format.
/// JSON MIME types are parsed into serde_json::Value.
/// Text MIME types are kept as strings.
pub fn json_data_to_datavalues(
    data: &serde_json::Map<String, Value>,
) -> HashMap<String, DataValue> {
    let mut output_data = HashMap::new();
    let mut has_image = false;

    for (mime, value) in data {
        let dv = match mime_kind(mime) {
            MimeKind::Binary => {
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
                if let Some(s) = value.as_str() {
                    match serde_json::from_str::<Value>(s) {
                        Ok(parsed) => DataValue::Json(parsed),
                        Err(_) => DataValue::Text(s.to_string()),
                    }
                } else {
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

    // Synthesize text/llm+plain for binary images
    if has_image && !output_data.contains_key("text/llm+plain") {
        let mut parts: Vec<String> = Vec::new();
        if let Some(DataValue::Text(ref plain)) = output_data.get("text/plain") {
            parts.push(plain.clone());
        }
        for (mime, dv) in &output_data {
            if let DataValue::Binary(bytes) = dv {
                if mime.starts_with("image/") {
                    parts.push(format!(
                        "Image output ({}, {} KB)",
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

    // Synthesize text/llm+plain for visualization specs (Plotly, Vega-Lite, Vega)
    synthesize_llm_plain_for_viz(&mut output_data);

    // Synthesize text/llm+plain for other heavy types (SVG, HTML, large JSON)
    synthesize_llm_plain_for_heavy_types(&mut output_data);

    output_data
}

/// Convert a blob manifest to an Output.
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
            let mut image_descriptions: Vec<String> = Vec::new();
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
                    // Extract blob metadata
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
                            let mut desc = format!("Image output ({}, {} KB)", mime_type, size_kb);
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

            // Synthesize text/llm+plain for binary images
            if !image_descriptions.is_empty() && !output_data.contains_key("text/llm+plain") {
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

            // Synthesize text/llm+plain for visualization specs (Plotly, Vega-Lite, Vega)
            synthesize_llm_plain_for_viz(&mut output_data);

            // Synthesize text/llm+plain for other heavy types (SVG, HTML, large JSON)
            synthesize_llm_plain_for_heavy_types(&mut output_data);

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

            let traceback_val = manifest.get("traceback")?;
            let traceback = if let Some(arr) = traceback_val.as_array() {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            } else {
                let tb_str =
                    resolve_text_ref(traceback_val, blob_base_url, blob_store_path).await?;
                serde_json::from_str::<Vec<String>>(&tb_str).ok()?
            };

            Some(Output::error(&ename, &evalue, traceback))
        }
        _ => None,
    }
}

/// Resolve a content reference, returning a [`DataValue`].
///
/// Content refs can be:
/// - `{"inline": "actual content"}` -- content is inline
/// - `{"blob": "hash", "size": N}` -- content is in the blob store
pub async fn resolve_content_ref(
    content_ref: &serde_json::Value,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
    mime_type: Option<&str>,
) -> Option<DataValue> {
    let kind = mime_type.map(mime_kind).unwrap_or(MimeKind::Text);

    if let Some(inline) = content_ref.get("inline") {
        if let Some(s) = inline.as_str() {
            return Some(match kind {
                MimeKind::Binary => base64::engine::general_purpose::STANDARD
                    .decode(s)
                    .map(DataValue::Binary)
                    .unwrap_or_else(|_| DataValue::Text(s.to_string())),
                MimeKind::Json => serde_json::from_str::<Value>(s)
                    .map(DataValue::Json)
                    .unwrap_or_else(|_| DataValue::Text(s.to_string())),
                MimeKind::Text => DataValue::Text(s.to_string()),
            });
        }

        // Handle inline JSON values (not wrapped in a string)
        if kind == MimeKind::Json && (inline.is_object() || inline.is_array()) {
            return Some(DataValue::Json(inline.clone()));
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

/// Synthesize `text/llm+plain` from visualization specs (Plotly, Vega-Lite, Vega).
///
/// Skips if `text/llm+plain` already exists (author-provided summaries win).
fn synthesize_llm_plain_for_viz(output_data: &mut HashMap<String, DataValue>) {
    if output_data.contains_key("text/llm+plain") {
        return;
    }
    let viz_summary = output_data.iter().find_map(|(mime, dv)| {
        if let DataValue::Json(ref spec) = dv {
            repr_llm::summarize_viz(mime, spec)
        } else {
            None
        }
    });
    if let Some(summary) = viz_summary {
        let mut parts: Vec<String> = Vec::new();
        if let Some(DataValue::Text(ref plain)) = output_data.get("text/plain") {
            parts.push(plain.clone());
        }
        parts.push(summary);
        output_data.insert(
            "text/llm+plain".to_string(),
            DataValue::Text(parts.join("\n")),
        );
    }
}

/// Synthesize `text/llm+plain` for heavy non-viz media types.
///
/// Handles:
/// - `image/svg+xml` — always summarize (raw XML is never useful to LLMs)
/// - `text/html` — summarize only when `text/plain` also exists
/// - `application/json` — structural summary for large (> 2KB) values
///
/// Skips if `text/llm+plain` already exists.
fn synthesize_llm_plain_for_heavy_types(output_data: &mut HashMap<String, DataValue>) {
    if output_data.contains_key("text/llm+plain") {
        return;
    }

    let has_text_plain = output_data.contains_key("text/plain");
    let mut descriptions: Vec<String> = Vec::new();

    // SVG: always describe — raw XML is useless to LLMs
    if let Some(DataValue::Text(ref svg)) = output_data.get("image/svg+xml") {
        descriptions.push(format!("SVG image output ({} KB)", svg.len() / 1024));
    }

    // HTML: describe only when text/plain exists (otherwise HTML may be the only repr)
    if has_text_plain {
        if let Some(DataValue::Text(ref html)) = output_data.get("text/html") {
            descriptions.push(format!("HTML output ({} KB)", html.len() / 1024));
        }
    }

    // Large JSON: structural summary via repr-llm
    if let Some(DataValue::Json(ref val)) = output_data.get("application/json") {
        if let Some(summary) = repr_llm::summarize_json(val) {
            descriptions.push(summary);
        }
    }

    if descriptions.is_empty() {
        return;
    }

    let mut parts: Vec<String> = Vec::new();
    if let Some(DataValue::Text(ref plain)) = output_data.get("text/plain") {
        parts.push(plain.clone());
    }
    parts.extend(descriptions);
    output_data.insert(
        "text/llm+plain".to_string(),
        DataValue::Text(parts.join("\n")),
    );
}
