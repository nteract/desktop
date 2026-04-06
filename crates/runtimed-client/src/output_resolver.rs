//! Output resolution for converting structured manifest Values to Output objects.
//!
//! This module provides standalone async functions for resolving outputs,
//! used by both the MCP server and the Python bindings.
//!
//! Outputs in the RuntimeStateDoc CRDT are structured `serde_json::Value`
//! manifest objects containing ContentRef entries (inline/blob). The single
//! entry point is `resolve_output()` which dispatches on `output_type`.

use std::collections::HashMap;
use std::path::PathBuf;

use base64::Engine as _;
use notebook_doc::runtime_state::CommDocEntry;
use serde_json::Value;

use crate::resolved_output::{DataValue, Output};

pub use notebook_doc::mime::{mime_kind, MimeKind};

/// MIME type for Jupyter widget view references.
const WIDGET_VIEW_MIME: &str = "application/vnd.jupyter.widget-view+json";

/// Priority order for selecting the best text MIME type for LLM consumption.
///
/// Walk this list against the manifest's MIME keys; resolve the first match.
/// `text/llm+plain` is either author-provided or synthesized by us when no
/// direct text type exists (viz specs, widget state, binary-only outputs) or
/// when the best text type was too large and got summarized.
pub const CONTENT_PRIORITY: &[&str] = &[
    "text/llm+plain",
    "text/latex",
    "text/markdown",
    "text/plain",
];

/// MIME prefixes that have dedicated synthesizers producing `text/llm+plain`.
/// When any MIME in the manifest starts with one of these, run synthesis
/// before the text priority walk.
const SYNTHESIS_PREFIXES: &[&str] = &[
    "application/vnd.plotly.v",
    "application/vnd.vegalite.v",
    "application/vnd.vega.v",
    "application/vnd.jupyter.widget-view",
];

/// Exact MIME matches that have dedicated synthesizers.
const SYNTHESIS_EXACT: &[&str] = &["application/geo+json"];

/// When `text/plain` exceeds this size, synthesize a truncated `text/llm+plain`.
const LLM_TEXT_MAX_SIZE: usize = 4 * 1024;

/// Bytes to keep from head and tail when truncating into `text/llm+plain`.
const LLM_TEXT_SIDE_SIZE: usize = 2 * 1024;

/// Metadata extracted from a ContentRef Value without resolving content.
///
/// Enables inspection of manifest entries (MIME type, size, blob hash) without
/// any I/O. After #1558 inlined manifests into the RuntimeStateDoc, this is
/// all we need to decide what to fetch for LLM consumption.
pub struct ContentRefMeta<'a> {
    /// True if the content is inlined in the CRDT (< 1KB).
    pub is_inline: bool,
    /// Content size in bytes. For inline refs this is the string length;
    /// for blob refs it comes from the `size` field written at storage time.
    pub size: u64,
    /// Blob hash, if the content is stored in the blob store.
    pub blob_hash: Option<&'a str>,
}

/// Check if a manifest data map contains any MIME types with dedicated synthesizers.
///
/// These MIME types produce richer `text/llm+plain` than the generic `text/plain`
/// repr, so synthesis should run before the text priority walk.
pub fn has_synthesizable_mime(data_map: &serde_json::Map<String, Value>) -> bool {
    data_map.keys().any(|mime| {
        SYNTHESIS_EXACT.contains(&mime.as_str())
            || SYNTHESIS_PREFIXES
                .iter()
                .any(|prefix| mime.starts_with(prefix))
    })
}

/// Extract metadata from a ContentRef Value without resolving the content.
///
/// Works with both `{"inline": "..."}` and `{"blob": "hash", "size": N}` shapes.
pub fn content_ref_meta(content_ref: &Value) -> ContentRefMeta<'_> {
    if let Some(inline) = content_ref.get("inline") {
        let size = inline.as_str().map(|s| s.len() as u64).unwrap_or(0);
        ContentRefMeta {
            is_inline: true,
            size,
            blob_hash: None,
        }
    } else if let Some(blob_hash) = content_ref.get("blob").and_then(|v| v.as_str()) {
        let size = content_ref
            .get("size")
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        ContentRefMeta {
            is_inline: false,
            size,
            blob_hash: Some(blob_hash),
        }
    } else {
        ContentRefMeta {
            is_inline: false,
            size: 0,
            blob_hash: None,
        }
    }
}

/// Resolve a structured output manifest Value to an Output.
///
/// The Value must be a JSON object with an `output_type` field.
/// All outputs are structured manifests with ContentRef entries
/// (inline/blob) for their data fields.
pub async fn resolve_output(
    output: &serde_json::Value,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
) -> Option<Output> {
    let output_type = output.get("output_type")?.as_str()?;
    output_from_manifest(output_type, output, blob_base_url, blob_store_path).await
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

    for (mime, value) in data {
        let dv = match mime_kind(mime) {
            MimeKind::Binary => {
                if let Some(s) = value.as_str() {
                    match base64::engine::general_purpose::STANDARD.decode(s) {
                        Ok(bytes) => DataValue::Binary(bytes),
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

    // Synthesis priority: viz > heavy types > binary media.
    // Viz summaries are more useful than "Image output (image/png, X KB)" when
    // both exist (e.g. Altair emits png fallback + vegalite+json).
    synthesize_llm_plain_for_viz(&mut output_data);
    synthesize_llm_plain_for_heavy_types(&mut output_data);
    synthesize_llm_plain_for_binary_media(&mut output_data);

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

                    output_data.insert(mime_type.clone(), content);
                }
            }

            // Synthesis priority: viz > heavy types > binary media.
            // Viz summaries are more useful than "Image output (image/png, X KB)" when
            // both exist (e.g. Altair emits png fallback + vegalite+json).
            synthesize_llm_plain_for_viz(&mut output_data);
            synthesize_llm_plain_for_heavy_types(&mut output_data);
            synthesize_llm_plain_for_binary_media_with_urls(
                &mut output_data,
                &blob_urls_map,
                &blob_paths_map,
            );

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

    // Fallback: handle raw Jupyter values (not ContentRef objects).
    // This supports legacy outputs from pre-v3 .automerge migrations where
    // data values are plain strings, arrays, or JSON objects rather than
    // ContentRef entries.
    if let Some(s) = content_ref.as_str() {
        return Some(match kind {
            MimeKind::Binary => base64::engine::general_purpose::STANDARD
                .decode(s)
                .map(DataValue::Binary)
                .unwrap_or_else(|_| DataValue::Text(s.to_string())),
            MimeKind::Json => serde_json::from_str::<serde_json::Value>(s)
                .map(DataValue::Json)
                .unwrap_or_else(|_| DataValue::Text(s.to_string())),
            MimeKind::Text => DataValue::Text(s.to_string()),
        });
    }

    // Handle array values (Jupyter sometimes uses ["line1\n", "line2\n"])
    if let Some(arr) = content_ref.as_array() {
        let joined: String = arr
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join("");
        return Some(DataValue::Text(joined));
    }

    // Handle JSON object values that aren't ContentRef
    if content_ref.is_object()
        && content_ref.get("inline").is_none()
        && content_ref.get("blob").is_none()
    {
        return Some(DataValue::Json(content_ref.clone()));
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
///
/// Each element in `raw_outputs` is a structured manifest Value with an
/// `output_type` field and ContentRef entries for data.
///
/// When `comms` is provided, widget view outputs (`application/vnd.jupyter.widget-view+json`)
/// are resolved to human-readable `text/llm+plain` summaries by looking up the referenced
/// widget's current state in the comms map.
pub async fn resolve_cell_outputs(
    raw_outputs: &[serde_json::Value],
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
    comms: Option<&HashMap<String, CommDocEntry>>,
) -> Vec<Output> {
    let mut outputs = Vec::with_capacity(raw_outputs.len());
    for manifest in raw_outputs {
        if let Some(mut output) = resolve_output(manifest, blob_base_url, blob_store_path).await {
            if let (Some(comms), Some(ref mut data)) = (comms, &mut output.data) {
                synthesize_llm_plain_for_widgets(data, comms);
            }
            outputs.push(output);
        }
    }
    outputs
}

// ── LLM-selective resolution ────────────────────────────────────────
//
// These functions resolve only what the MCP LLM text path needs from
// output manifests. Instead of fetching every blob, they walk
// CONTENT_PRIORITY to find the single best text MIME, resolve just that,
// and synthesize `text/llm+plain` from manifest metadata when no direct
// text representation exists.

/// Resolve all outputs for a cell, fetching only what the LLM text path needs.
///
/// Drop-in replacement for [`resolve_cell_outputs`] in MCP tool handlers.
/// Streams and errors are resolved normally (text-only, cheap). For
/// `display_data`/`execute_result`, only the highest-priority text MIME
/// is resolved; everything else is described from manifest metadata.
pub async fn resolve_cell_outputs_for_llm(
    raw_outputs: &[serde_json::Value],
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
    comms: Option<&HashMap<String, CommDocEntry>>,
) -> Vec<Output> {
    let mut outputs = Vec::with_capacity(raw_outputs.len());
    for manifest in raw_outputs {
        if let Some(output) =
            resolve_output_for_llm(manifest, blob_base_url, blob_store_path, comms).await
        {
            outputs.push(output);
        }
    }
    outputs
}

/// Resolve a single output manifest for LLM consumption.
///
/// Streams and errors pass through the normal resolver (text-only, cheap).
/// Display data / execute results use [`resolve_display_for_llm`] which
/// only fetches the highest-priority text MIME type.
pub async fn resolve_output_for_llm(
    manifest: &serde_json::Value,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
    comms: Option<&HashMap<String, CommDocEntry>>,
) -> Option<Output> {
    let output_type = manifest.get("output_type")?.as_str()?;

    match output_type {
        // Stream and error are text-only — resolve as normal.
        "stream" => {
            let name = manifest.get("name")?.as_str()?;
            let text_ref = manifest.get("text")?;
            let text = resolve_text_ref(text_ref, blob_base_url, blob_store_path).await?;
            Some(Output::stream(name, &text))
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
        "display_data" | "execute_result" => {
            resolve_display_for_llm(output_type, manifest, blob_base_url, blob_store_path, comms)
                .await
        }
        _ => None,
    }
}

/// Selectively resolve a display_data or execute_result manifest for LLM use.
///
/// 1. Walk [`CONTENT_PRIORITY`] against the manifest's MIME keys.
/// 2. If a priority MIME exists, resolve just that one ContentRef.
///    - If `text/plain` and too large, synthesize a truncated `text/llm+plain`.
/// 3. If no priority MIME exists, synthesize `text/llm+plain`:
///    - Resolve viz JSON specs for `summarize_viz`.
///    - Resolve widget JSON for widget state summaries.
///    - Describe binary/heavy-text MIMEs from ContentRef metadata (no fetch).
/// 4. Return an Output with only the resolved/synthesized entries.
async fn resolve_display_for_llm(
    output_type: &str,
    manifest: &serde_json::Value,
    blob_base_url: &Option<String>,
    blob_store_path: &Option<PathBuf>,
    comms: Option<&HashMap<String, CommDocEntry>>,
) -> Option<Output> {
    let data_map = manifest.get("data")?.as_object()?;

    let mut output_data: HashMap<String, DataValue> = HashMap::new();
    let prefer_synthesis = has_synthesizable_mime(data_map);

    // Phase 1: If the manifest has MIMEs with dedicated synthesizers, run
    // synthesis first. These produce text/llm+plain that's more useful than
    // a generic text/plain repr (e.g., "Scatter chart: x vs y" vs "alt.Chart(...)").
    if prefer_synthesis {
        // 1a: Resolve JSON types for viz summarization.
        for (mime, content_ref) in data_map {
            if mime_kind(mime) == MimeKind::Json {
                if let Some(content) =
                    resolve_content_ref(content_ref, blob_base_url, blob_store_path, Some(mime))
                        .await
                {
                    output_data.insert(mime.to_string(), content);
                }
            }
        }
        synthesize_llm_plain_for_viz(&mut output_data);

        // 1b: Widget synthesis (if viz didn't produce text/llm+plain).
        if !output_data.contains_key("text/llm+plain") {
            if let Some(widget_ref) = data_map.get(WIDGET_VIEW_MIME) {
                if !output_data.contains_key(WIDGET_VIEW_MIME) {
                    if let Some(content) = resolve_content_ref(
                        widget_ref,
                        blob_base_url,
                        blob_store_path,
                        Some(WIDGET_VIEW_MIME),
                    )
                    .await
                    {
                        output_data.insert(WIDGET_VIEW_MIME.to_string(), content);
                    }
                }
                if let Some(comms) = comms {
                    synthesize_llm_plain_for_widgets(&mut output_data, comms);
                }
            }
        }

        // 1c: Large JSON summary (if still no text/llm+plain).
        if !output_data.contains_key("text/llm+plain") {
            if let Some(DataValue::Json(ref val)) = output_data.get("application/json") {
                if let Some(summary) = repr_llm::summarize_json(val) {
                    output_data.insert("text/llm+plain".to_string(), DataValue::Text(summary));
                }
            }
        }
    }

    // Phase 2: Walk content priority order for the best text representation.
    // If phase 1 produced text/llm+plain, it wins at position 1.
    // If synthesis was skipped or failed, this picks the first available text MIME.
    if !output_data.contains_key("text/llm+plain") {
        for &mime in CONTENT_PRIORITY {
            let Some(content_ref) = data_map.get(mime) else {
                continue;
            };
            let Some(content) =
                resolve_content_ref(content_ref, blob_base_url, blob_store_path, Some(mime)).await
            else {
                continue; // Resolution failed, try next priority.
            };

            // For text/plain: if too large, synthesize truncated text/llm+plain.
            if mime == "text/plain" {
                if let DataValue::Text(ref text) = content {
                    if text.len() > LLM_TEXT_MAX_SIZE {
                        let truncated = truncate_head_tail(text, LLM_TEXT_SIDE_SIZE);
                        output_data
                            .insert("text/llm+plain".to_string(), DataValue::Text(truncated));
                    }
                }
            }

            output_data.insert(mime.to_string(), content);
            break;
        }
    }

    // Phase 3: No synthesis and no priority MIME — describe from manifest metadata.
    if output_data.is_empty() {
        let mut descriptions: Vec<String> = Vec::new();

        for (mime, content_ref) in data_map {
            // Skip JSON (may have been resolved in phase 1 without producing synthesis).
            if mime_kind(mime) == MimeKind::Json || mime == WIDGET_VIEW_MIME {
                continue;
            }

            let meta = content_ref_meta(content_ref);
            let label = mime_label(mime);
            let kb = meta.size / 1024;
            let mut desc = format!("{label} output ({mime}, {kb} KB)");

            // Append blob URL so the LLM can fetch if it wants.
            if let Some(hash) = meta.blob_hash {
                if let Some(base_url) = blob_base_url {
                    desc.push_str(&format!("\n{}/blob/{}", base_url, hash));
                }
            }
            descriptions.push(desc);
        }

        if !descriptions.is_empty() {
            output_data.insert(
                "text/llm+plain".to_string(),
                DataValue::Text(descriptions.join("\n")),
            );
        }
    }

    // Build the output.
    if output_type == "execute_result" {
        let execution_count = manifest.get("execution_count").and_then(|v| v.as_i64())?;
        Some(Output::execute_result(output_data, execution_count))
    } else {
        Some(Output::display_data(output_data))
    }
}

/// Truncate text keeping head and tail, since errors and results tend to
/// appear at the bottom while context (imports, setup) is at the top.
fn truncate_head_tail(text: &str, side_bytes: usize) -> String {
    if text.len() <= side_bytes * 2 {
        return text.to_string();
    }

    // Find char boundary at or before the head limit.
    let mut head_end = side_bytes;
    while head_end > 0 && !text.is_char_boundary(head_end) {
        head_end -= 1;
    }

    // Find char boundary at or after the tail start.
    let mut tail_start = text.len() - side_bytes;
    while tail_start < text.len() && !text.is_char_boundary(tail_start) {
        tail_start += 1;
    }

    let omitted = tail_start - head_end;
    format!(
        "{}\n[... truncated {} bytes ...]\n{}",
        &text[..head_end],
        omitted,
        &text[tail_start..],
    )
}

/// Human-readable label for a MIME type in synthesis descriptions.
fn mime_label(mime: &str) -> &str {
    if mime == "image/svg+xml" {
        "SVG image"
    } else if mime.starts_with("image/") {
        "Image"
    } else if mime.starts_with("audio/") {
        "Audio"
    } else if mime.starts_with("video/") {
        "Video"
    } else if mime == "text/html" {
        "HTML"
    } else if mime == "text/latex" {
        "LaTeX"
    } else {
        "Content"
    }
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

// ── Binary media synthesis ──────────────────────────────────────────

/// Synthesize `text/llm+plain` for binary media types (images, audio, video).
///
/// Produces a short description like "Image output (image/png, 45 KB)" or
/// "Audio output (audio/wav, 118 KB)". Skips if `text/llm+plain` already
/// exists (viz or heavy-type synthesis already ran).
fn synthesize_llm_plain_for_binary_media(output_data: &mut HashMap<String, DataValue>) {
    if output_data.contains_key("text/llm+plain") {
        return;
    }

    let mut descriptions: Vec<String> = Vec::new();
    for (mime, dv) in output_data.iter() {
        if let DataValue::Binary(bytes) = dv {
            let label = if mime.starts_with("image/") {
                "Image"
            } else if mime.starts_with("audio/") {
                "Audio"
            } else if mime.starts_with("video/") {
                "Video"
            } else {
                "Binary"
            };
            descriptions.push(format!(
                "{label} output ({mime}, {} KB)",
                bytes.len() / 1024
            ));
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

/// Like [`synthesize_llm_plain_for_binary_media`] but appends blob URLs when available.
///
/// Used by `output_from_manifest` where blob URL maps are already computed.
fn synthesize_llm_plain_for_binary_media_with_urls(
    output_data: &mut HashMap<String, DataValue>,
    blob_urls: &HashMap<String, String>,
    blob_paths: &HashMap<String, String>,
) {
    if output_data.contains_key("text/llm+plain") {
        return;
    }

    let mut descriptions: Vec<String> = Vec::new();
    for (mime, dv) in output_data.iter() {
        if let DataValue::Binary(bytes) = dv {
            let label = if mime.starts_with("image/") {
                "Image"
            } else if mime.starts_with("audio/") {
                "Audio"
            } else if mime.starts_with("video/") {
                "Video"
            } else {
                "Binary"
            };
            let mut desc = format!("{label} output ({mime}, {} KB)", bytes.len() / 1024);
            if let Some(url) = blob_urls.get(mime) {
                desc.push_str(&format!("\n{url}"));
            } else if let Some(path) = blob_paths.get(mime) {
                desc.push_str(&format!("\n{path}"));
            }
            descriptions.push(desc);
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

// ── Widget state synthesis ──────────────────────────────────────────

/// Synthesize `text/llm+plain` from widget view references.
///
/// When an output contains `application/vnd.jupyter.widget-view+json`,
/// extracts the `model_id` (which is a comm_id), looks up the widget's
/// current state from the comms map, and produces a human-readable summary.
fn synthesize_llm_plain_for_widgets(
    output_data: &mut HashMap<String, DataValue>,
    comms: &HashMap<String, CommDocEntry>,
) {
    if output_data.contains_key("text/llm+plain") {
        return;
    }
    let model_id = match output_data.get(WIDGET_VIEW_MIME) {
        Some(DataValue::Json(val)) => val.get("model_id").and_then(|v| v.as_str()),
        _ => None,
    };
    let Some(model_id) = model_id else { return };
    let Some(entry) = comms.get(model_id) else {
        return;
    };

    let summary = format_widget_summary(model_id, entry, comms);
    output_data.insert("text/llm+plain".to_string(), DataValue::Text(summary));
}

/// Format a human-readable one-line summary of a widget's current state.
///
/// Examples:
///   `IntSlider 25fdf9…: 2 (0–10)`
///   `HBox 789abc…: [IntSlider 25fdf9…: 2, Text def012…: "hello"]`
///   `Output 345678…: 2 output(s)`
pub fn format_widget_summary(
    comm_id: &str,
    entry: &CommDocEntry,
    comms: &HashMap<String, CommDocEntry>,
) -> String {
    let name = entry
        .model_name
        .strip_suffix("Model")
        .unwrap_or(&entry.model_name);
    let short_id = &comm_id[..6.min(comm_id.len())];

    match name {
        // Numeric sliders — value + range
        "IntSlider" | "FloatSlider" | "FloatLogSlider" => {
            let val = state_display(&entry.state, "value");
            let min = state_display(&entry.state, "min");
            let max = state_display(&entry.state, "max");
            format!("{name} {short_id}\u{2026}: {val} ({min}\u{2013}{max})")
        }
        "IntRangeSlider" | "FloatRangeSlider" => {
            let val = state_display(&entry.state, "value");
            format!("{name} {short_id}\u{2026}: {val}")
        }

        // Numeric inputs
        "IntText" | "FloatText" | "BoundedIntText" | "BoundedFloatText" => {
            let val = state_display(&entry.state, "value");
            format!("{name} {short_id}\u{2026}: {val}")
        }

        // Text inputs
        "Text" | "Textarea" | "Combobox" => {
            let val = entry
                .state
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let preview = truncate_str(val, 40);
            format!("{name} {short_id}\u{2026}: {preview:?}")
        }

        // SECURITY: Password widget values must never be included in summaries.
        // These summaries are sent to LLM/MCP consumers as text/llm+plain, so
        // exposing the raw value would leak secrets to any downstream agent or
        // tool that reads cell outputs.
        "Password" => format!("Password {short_id}\u{2026}: ****"),

        // Boolean/toggle
        "Checkbox" | "Valid" | "ToggleButton" => {
            let val = state_display(&entry.state, "value");
            format!("{name} {short_id}\u{2026}: {val}")
        }

        // Selection widgets — resolve selected label
        "Dropdown" | "Select" | "RadioButtons" | "ToggleButtons" | "SelectionSlider" => {
            let labels = entry
                .state
                .get("_options_labels")
                .and_then(|v| v.as_array());
            let idx = entry.state.get("index").and_then(|v| v.as_u64());
            let selected = labels
                .zip(idx)
                .and_then(|(l, i)| l.get(i as usize))
                .and_then(|v| v.as_str());
            match selected {
                Some(s) => format!("{name} {short_id}\u{2026}: {s:?}"),
                None => format!("{name} {short_id}\u{2026}: (no selection)"),
            }
        }

        // Multi-select
        "SelectMultiple" => {
            let idx = entry
                .state
                .get("index")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            format!("{name} {short_id}\u{2026}: {idx} selected")
        }

        // Progress
        "IntProgress" | "FloatProgress" => {
            let val = state_display(&entry.state, "value");
            let max = state_display(&entry.state, "max");
            format!("{name} {short_id}\u{2026}: {val}/{max}")
        }

        // Button — show label
        "Button" => {
            let desc = entry
                .state
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            format!("Button {short_id}\u{2026}: {desc:?}")
        }

        // Output widget — show captured output count
        "Output" => {
            let n = entry.outputs.len();
            format!("Output {short_id}\u{2026}: {n} output(s)")
        }

        // Container widgets — show children inline
        "HBox" | "VBox" | "Box" | "GridBox" | "Tab" | "Accordion" | "Stack" => {
            let children = resolve_children(&entry.state, comms);
            format!("{name} {short_id}\u{2026}: [{children}]")
        }

        // Display widgets
        "HTML" | "HTMLMath" | "Label" => {
            let val = entry
                .state
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let preview = truncate_str(val, 60);
            format!("{name} {short_id}\u{2026}: {preview:?}")
        }
        "Image" => format!("Image {short_id}\u{2026}"),

        // Color/Date/Time pickers
        "ColorPicker" | "DatePicker" | "TimePicker" => {
            let val = state_display(&entry.state, "value");
            format!("{name} {short_id}\u{2026}: {val}")
        }

        // Anywidget — detect via _anywidget_id or model_module == "anywidget".
        // Extract the class name from _anywidget_id (e.g., "altair.jupyter.jupyter_chart.JupyterChart")
        // and fingerprint chart types from state keys.
        "Any" if entry.model_module == "anywidget" => {
            let widget_name = entry
                .state
                .get("_anywidget_id")
                .and_then(|v| v.as_str())
                .and_then(|id| id.rsplit('.').next())
                .unwrap_or("Anywidget");

            // Fingerprint: if state has "spec" with an actual dict (not a $blob sentinel),
            // try to identify the chart type via viz summarization.
            if let Some(spec) = entry.state.get("spec") {
                // Skip $blob sentinels — the spec was blob-stored and we don't have
                // the content here. Just report it as a chart widget.
                let is_blob_sentinel = spec
                    .as_object()
                    .is_some_and(|obj| obj.contains_key("$blob"));

                if !is_blob_sentinel {
                    if let Some(summary) =
                        repr_llm::summarize_viz("application/vnd.vegalite.v5+json", spec)
                    {
                        return format!("{widget_name} {short_id}\u{2026}: {summary}");
                    }
                    // spec exists but summarizer didn't match — check for title
                    let title = spec.get("title").and_then(|v| v.as_str()).or_else(|| {
                        spec.get("title")
                            .and_then(|v| v.get("text"))
                            .and_then(|v| v.as_str())
                    });
                    if let Some(title) = title {
                        return format!("{widget_name} {short_id}\u{2026}: \"{title}\"");
                    }
                }

                // Has spec (inline or blob) — it's a chart widget
                return format!("{widget_name} {short_id}\u{2026} (chart)");
            }

            format!("{widget_name} {short_id}\u{2026}")
        }

        // Fallback — show description or value if available
        _ => match entry.state.get("description").and_then(|v| v.as_str()) {
            Some(d) if !d.is_empty() => format!("{name} {short_id}\u{2026}: {d:?}"),
            _ => match entry.state.get("value") {
                Some(v) => {
                    format!("{name} {short_id}\u{2026}: {}", format_json_compact(v))
                }
                None => format!("{name} {short_id}\u{2026}"),
            },
        },
    }
}

/// Resolve `IPY_MODEL_xxx` children references to short summaries (one level deep).
fn resolve_children(state: &Value, comms: &HashMap<String, CommDocEntry>) -> String {
    let Some(children) = state.get("children").and_then(|v| v.as_array()) else {
        return String::new();
    };
    children
        .iter()
        .filter_map(|child| {
            let ref_str = child.as_str()?;
            let cid = ref_str.strip_prefix("IPY_MODEL_")?;
            let entry = comms.get(cid)?;
            let name = entry
                .model_name
                .strip_suffix("Model")
                .unwrap_or(&entry.model_name);
            let short_id = &cid[..6.min(cid.len())];
            // SECURITY: Never include the value of Password widgets in child
            // summaries. These flow to LLM/MCP consumers as text/llm+plain
            // and would leak secrets to downstream agents or tools.
            let val = if is_secret_widget(&entry.model_name) {
                String::new()
            } else {
                entry
                    .state
                    .get("value")
                    .map(|v| format!(": {}", format_json_compact(v)))
                    .unwrap_or_default()
            };
            Some(format!("{name} {short_id}\u{2026}{val}"))
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Returns true for widget types whose values must never appear in summaries.
///
/// Password widgets store their raw plaintext value in state. Exposing it in
/// text/llm+plain would leak secrets to any LLM/MCP consumer that reads cell
/// outputs. This check is used both in the direct Password summary branch and
/// in container child resolution to ensure secrets are never surfaced.
fn is_secret_widget(model_name: &str) -> bool {
    model_name == "PasswordModel"
}

/// Get a display string for a state key.
fn state_display(state: &Value, key: &str) -> String {
    state
        .get(key)
        .map(format_json_compact)
        .unwrap_or_else(|| "?".to_string())
}

/// Format a JSON value compactly for display.
fn format_json_compact(v: &Value) -> String {
    match v {
        Value::String(s) => format!("{s:?}"),
        Value::Null => "null".to_string(),
        other => other.to_string(),
    }
}

/// Truncate a string to `max` characters, appending an ellipsis if truncated.
fn truncate_str(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max).collect();
        format!("{truncated}\u{2026}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ── content_ref_meta ────────────────────────────────────────

    #[test]
    fn content_ref_meta_inline() {
        let cr = json!({"inline": "hello world"});
        let meta = content_ref_meta(&cr);
        assert!(meta.is_inline);
        assert_eq!(meta.size, 11);
        assert!(meta.blob_hash.is_none());
    }

    #[test]
    fn content_ref_meta_blob() {
        let cr = json!({"blob": "abc123def456", "size": 50_000});
        let meta = content_ref_meta(&cr);
        assert!(!meta.is_inline);
        assert_eq!(meta.size, 50_000);
        assert_eq!(meta.blob_hash, Some("abc123def456"));
    }

    #[test]
    fn content_ref_meta_blob_missing_size() {
        let cr = json!({"blob": "abc123"});
        let meta = content_ref_meta(&cr);
        assert!(!meta.is_inline);
        assert_eq!(meta.size, 0);
        assert_eq!(meta.blob_hash, Some("abc123"));
    }

    #[test]
    fn content_ref_meta_malformed() {
        let cr = json!({"unexpected": true});
        let meta = content_ref_meta(&cr);
        assert!(!meta.is_inline);
        assert_eq!(meta.size, 0);
        assert!(meta.blob_hash.is_none());
    }

    #[test]
    fn content_ref_meta_raw_string_legacy() {
        // Legacy outputs are plain strings, not ContentRef objects
        let cr = json!("some raw text");
        let meta = content_ref_meta(&cr);
        assert!(!meta.is_inline);
        assert_eq!(meta.size, 0);
        assert!(meta.blob_hash.is_none());
    }

    // ── truncate_head_tail ──────────────────────────────────────

    #[test]
    fn truncate_short_text_unchanged() {
        let text = "short";
        assert_eq!(truncate_head_tail(text, 100), "short");
    }

    #[test]
    fn truncate_exactly_at_boundary() {
        // 2 * side_bytes = total, should NOT truncate
        let text = "a".repeat(200);
        assert_eq!(truncate_head_tail(&text, 100), text);
    }

    #[test]
    fn truncate_one_over_boundary() {
        // 201 bytes, side=100 → head 100 + tail 100, middle 1 byte truncated
        let text = "a".repeat(201);
        let result = truncate_head_tail(&text, 100);
        assert!(result.contains("[... truncated 1 bytes ...]"));
        assert!(result.starts_with(&"a".repeat(100)));
        assert!(result.ends_with(&"a".repeat(100)));
    }

    #[test]
    fn truncate_large_text() {
        let text = format!("HEAD{}{}", "x".repeat(10_000), "TAIL");
        let result = truncate_head_tail(&text, 10);
        assert!(result.starts_with("HEAD"));
        assert!(result.ends_with("TAIL"));
        assert!(result.contains("[... truncated"));
    }

    #[test]
    fn truncate_multibyte_unicode_boundary() {
        // '€' is 3 bytes (U+20AC). Put it right at the cut boundary.
        // side_bytes=5, so we need text > 10 bytes total.
        let text = "ab€€€€cd"; // 2 + 4*3 + 2 = 16 bytes
        let result = truncate_head_tail(text, 5);
        // Should find a valid char boundary, not panic
        assert!(result.contains("[... truncated"));
        // Verify the result is valid UTF-8 (it is, since it's a String)
        assert!(!result.is_empty());
    }

    #[test]
    fn truncate_preserves_head_and_tail_content() {
        let head = "ERROR at line 1\n";
        let middle = "x".repeat(10_000);
        let tail = "\nTraceback: something failed";
        let text = format!("{head}{middle}{tail}");
        let result = truncate_head_tail(&text, 100);
        // Head content preserved
        assert!(result.starts_with("ERROR at line 1"));
        // Tail content preserved
        assert!(result.ends_with("something failed"));
    }

    // ── mime_label ──────────────────────────────────────────────

    #[test]
    fn mime_labels() {
        assert_eq!(mime_label("image/svg+xml"), "SVG image");
        assert_eq!(mime_label("image/png"), "Image");
        assert_eq!(mime_label("image/jpeg"), "Image");
        assert_eq!(mime_label("audio/wav"), "Audio");
        assert_eq!(mime_label("audio/mpeg"), "Audio");
        assert_eq!(mime_label("video/mp4"), "Video");
        assert_eq!(mime_label("text/html"), "HTML");
        assert_eq!(mime_label("text/latex"), "LaTeX");
        assert_eq!(mime_label("application/octet-stream"), "Content");
    }

    // ── CONTENT_PRIORITY constant ───────────────────────────────

    #[test]
    fn content_priority_order() {
        assert_eq!(CONTENT_PRIORITY[0], "text/llm+plain");
        assert_eq!(CONTENT_PRIORITY[1], "text/latex");
        assert_eq!(CONTENT_PRIORITY[2], "text/markdown");
        assert_eq!(CONTENT_PRIORITY[3], "text/plain");
    }

    // ── has_synthesizable_mime ───────────────────────────────────

    #[test]
    fn synthesizable_plotly() {
        let data = serde_json::json!({
            "application/vnd.plotly.v1+json": inline_ref("{}"),
            "text/plain": inline_ref("Figure()"),
        });
        let Some(map) = data.as_object() else {
            panic!("should be object");
        };
        assert!(has_synthesizable_mime(map));
    }

    #[test]
    fn synthesizable_vegalite() {
        let data = serde_json::json!({
            "application/vnd.vegalite.v5+json": inline_ref("{}"),
            "text/plain": inline_ref("alt.Chart(...)"),
        });
        let Some(map) = data.as_object() else {
            panic!("should be object");
        };
        assert!(has_synthesizable_mime(map));
    }

    #[test]
    fn synthesizable_widget() {
        let data = serde_json::json!({
            "application/vnd.jupyter.widget-view+json": inline_ref("{}"),
            "text/plain": inline_ref("IntSlider(...)"),
        });
        let Some(map) = data.as_object() else {
            panic!("should be object");
        };
        assert!(has_synthesizable_mime(map));
    }

    #[test]
    fn synthesizable_geojson() {
        let data = serde_json::json!({
            "application/geo+json": inline_ref("{}"),
            "text/plain": inline_ref("<GeoJSON>"),
        });
        let Some(map) = data.as_object() else {
            panic!("should be object");
        };
        assert!(has_synthesizable_mime(map));
    }

    #[test]
    fn not_synthesizable_plain_only() {
        let data = serde_json::json!({
            "text/plain": inline_ref("hello"),
            "image/png": blob_ref("abc", 50_000),
        });
        let Some(map) = data.as_object() else {
            panic!("should be object");
        };
        assert!(!has_synthesizable_mime(map));
    }

    #[test]
    fn not_synthesizable_html_only() {
        let data = serde_json::json!({
            "text/html": inline_ref("<b>bold</b>"),
            "text/plain": inline_ref("bold"),
        });
        let Some(map) = data.as_object() else {
            panic!("should be object");
        };
        assert!(!has_synthesizable_mime(map));
    }

    // ── resolve_display_for_llm (async) ─────────────────────────

    /// Helper to build a display_data manifest with inline data entries.
    fn make_display_manifest(data: serde_json::Value) -> serde_json::Value {
        json!({
            "output_type": "display_data",
            "data": data,
        })
    }

    fn make_execute_result_manifest(data: serde_json::Value, ec: i64) -> serde_json::Value {
        json!({
            "output_type": "execute_result",
            "data": data,
            "execution_count": ec,
        })
    }

    /// Helper: inline ContentRef
    fn inline_ref(content: &str) -> serde_json::Value {
        json!({"inline": content})
    }

    /// Helper: blob ContentRef (won't resolve without a blob store)
    fn blob_ref(hash: &str, size: u64) -> serde_json::Value {
        json!({"blob": hash, "size": size})
    }

    #[tokio::test]
    async fn llm_resolves_text_plain_only() {
        let manifest = make_display_manifest(json!({
            "text/plain": inline_ref("hello world"),
            "image/png": blob_ref("abc123", 50_000),
        }));
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        // text/plain resolved
        assert!(matches!(data.get("text/plain"), Some(DataValue::Text(s)) if s == "hello world"));
        // image/png NOT resolved (no blob store, but more importantly: not attempted)
        assert!(!data.contains_key("image/png"));
    }

    #[tokio::test]
    async fn llm_latex_wins_over_plain() {
        let manifest = make_display_manifest(json!({
            "text/latex": inline_ref("$E=mc^2$"),
            "text/plain": inline_ref("E=mc^2"),
        }));
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        // text/latex should be resolved (it's higher priority than text/plain)
        assert!(matches!(data.get("text/latex"), Some(DataValue::Text(s)) if s == "$E=mc^2$"));
        // text/plain should NOT be resolved (lower priority, not needed)
        assert!(!data.contains_key("text/plain"));
    }

    #[tokio::test]
    async fn llm_author_provided_llm_plain_wins() {
        let manifest = make_display_manifest(json!({
            "text/llm+plain": inline_ref("Author's summary"),
            "text/plain": inline_ref("raw repr"),
            "image/png": blob_ref("img123", 100_000),
        }));
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        assert!(
            matches!(data.get("text/llm+plain"), Some(DataValue::Text(s)) if s == "Author's summary")
        );
        assert!(!data.contains_key("text/plain"));
        assert!(!data.contains_key("image/png"));
    }

    #[tokio::test]
    async fn llm_large_text_plain_gets_truncated() {
        let large_text = format!("START{}{}", "x".repeat(10_000), "END");
        let manifest = make_display_manifest(json!({
            "text/plain": inline_ref(&large_text),
        }));
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        // text/plain is resolved (it's the content)
        assert!(data.contains_key("text/plain"));
        // text/llm+plain is synthesized as truncated version
        let llm = match data.get("text/llm+plain") {
            Some(DataValue::Text(s)) => s.clone(),
            _ => panic!("expected text/llm+plain"),
        };
        assert!(llm.contains("[... truncated"));
        assert!(llm.starts_with("START"));
        assert!(llm.ends_with("END"));
    }

    #[tokio::test]
    async fn llm_text_plain_under_threshold_not_truncated() {
        // 4096 bytes exactly = LLM_TEXT_MAX_SIZE, should NOT truncate (> threshold, not >=)
        let text = "a".repeat(4096);
        let manifest = make_display_manifest(json!({
            "text/plain": inline_ref(&text),
        }));
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        // No text/llm+plain synthesized — text/plain is under/at threshold
        assert!(!data.contains_key("text/llm+plain"));
        assert!(matches!(data.get("text/plain"), Some(DataValue::Text(s)) if s.len() == 4096));
    }

    #[tokio::test]
    async fn llm_no_text_mime_binary_only_described() {
        let manifest = make_display_manifest(json!({
            "image/png": blob_ref("png123", 45_000),
            "image/svg+xml": blob_ref("svg456", 12_000),
        }));
        let blob_base = Some("http://localhost:9999".to_string());
        let Some(output) = resolve_output_for_llm(&manifest, &blob_base, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        // No image data resolved
        assert!(!data.contains_key("image/png"));
        assert!(!data.contains_key("image/svg+xml"));

        // text/llm+plain synthesized from metadata
        let Some(DataValue::Text(ref llm)) = data.get("text/llm+plain") else {
            panic!("expected text/llm+plain synthesis");
        };
        assert!(llm.contains("image/png"));
        assert!(llm.contains("45 KB") || llm.contains("43 KB")); // 45000/1024 ≈ 43
        assert!(llm.contains("http://localhost:9999/blob/png123"));
    }

    #[tokio::test]
    async fn llm_synthesis_wins_over_text_plain_for_viz() {
        // Altair-style output: vegalite JSON + text/plain
        // Synthesis should produce text/llm+plain from the viz spec,
        // NOT short-circuit on text/plain.
        let manifest = make_display_manifest(json!({
            "application/vnd.vegalite.v5+json": inline_ref(r#"{"mark":"point","encoding":{"x":{"field":"Horsepower","type":"quantitative"},"y":{"field":"Miles_per_Gallon","type":"quantitative"}}}"#),
            "text/plain": inline_ref("alt.Chart(...)"),
        }));
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        // text/llm+plain should exist from viz synthesis
        let Some(DataValue::Text(ref llm)) = data.get("text/llm+plain") else {
            panic!("expected text/llm+plain from viz synthesis");
        };
        assert!(llm.contains("Vega-Lite"));
        // text/plain should NOT have been resolved (synthesis handled it)
        assert!(!data.contains_key("text/plain"));
    }

    #[tokio::test]
    async fn llm_synthesis_wins_over_text_plain_for_plotly() {
        let manifest = make_display_manifest(json!({
            "application/vnd.plotly.v1+json": inline_ref(r#"{"data":[{"type":"bar","x":["A","B"],"y":[1,2]}],"layout":{"title":"Test"}}"#),
            "text/plain": inline_ref("Figure()"),
        }));
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        let Some(DataValue::Text(ref llm)) = data.get("text/llm+plain") else {
            panic!("expected text/llm+plain from Plotly synthesis");
        };
        assert!(llm.contains("Plotly"));
        assert!(!data.contains_key("text/plain"));
    }

    #[tokio::test]
    async fn llm_synthesis_wins_over_text_plain_for_geojson() {
        let manifest = make_display_manifest(json!({
            "application/geo+json": inline_ref(r#"{"type":"FeatureCollection","features":[{"type":"Feature","geometry":{"type":"Point","coordinates":[0,0]},"properties":{"name":"origin"}}]}"#),
            "text/plain": inline_ref("<GeoJSON object>"),
        }));
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        let Some(DataValue::Text(ref llm)) = data.get("text/llm+plain") else {
            panic!("expected text/llm+plain from GeoJSON synthesis");
        };
        assert!(llm.contains("GeoJSON"));
        assert!(!data.contains_key("text/plain"));
    }

    #[tokio::test]
    async fn llm_no_synthesis_falls_through_to_priority() {
        // Regular output without synthesizable MIMEs — priority walk works as before
        let manifest = make_display_manifest(json!({
            "text/html": inline_ref("<b>bold</b>"),
            "text/plain": inline_ref("bold"),
        }));
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        // text/plain wins via priority walk (no synthesis triggered)
        assert!(matches!(
            data.get("text/plain"),
            Some(DataValue::Text(s)) if s == "bold"
        ));
        assert!(!data.contains_key("text/llm+plain"));
    }

    #[tokio::test]
    async fn llm_synthesis_failure_falls_through_to_priority() {
        // Widget MIME present but no comms → synthesis produces nothing.
        // Should fall through to text/plain via priority walk.
        let manifest = make_display_manifest(json!({
            "application/vnd.jupyter.widget-view+json": inline_ref(r#"{"model_id":"abc123"}"#),
            "text/plain": inline_ref("IntSlider(value=42)"),
        }));
        // No comms passed → widget synthesis can't look up state
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        // Synthesis didn't produce text/llm+plain (no comms), so priority walk ran
        assert!(matches!(
            data.get("text/plain"),
            Some(DataValue::Text(s)) if s == "IntSlider(value=42)"
        ));
    }

    #[tokio::test]
    async fn llm_empty_data_map() {
        let manifest = make_display_manifest(json!({}));
        let output = resolve_output_for_llm(&manifest, &None, &None, None).await;
        // Empty data map still produces an output, just with empty data
        let Some(output) = output else {
            panic!("should produce output");
        };
        let Some(data) = output.data else {
            panic!("should have data");
        };
        assert!(data.is_empty());
    }

    #[tokio::test]
    async fn llm_stream_resolves_normally() {
        let manifest = json!({
            "output_type": "stream",
            "name": "stdout",
            "text": inline_ref("hello from stdout"),
        });
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        assert_eq!(output.output_type, "stream");
        assert_eq!(output.text.as_deref(), Some("hello from stdout"));
    }

    #[tokio::test]
    async fn llm_error_resolves_normally() {
        let manifest = json!({
            "output_type": "error",
            "ename": "ValueError",
            "evalue": "bad value",
            "traceback": ["line 1", "line 2"],
        });
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        assert_eq!(output.output_type, "error");
        assert_eq!(output.ename.as_deref(), Some("ValueError"));
        let Some(ref traceback) = output.traceback else {
            panic!("should have traceback");
        };
        assert_eq!(traceback.len(), 2);
    }

    #[tokio::test]
    async fn llm_execute_result_has_execution_count() {
        let manifest = make_execute_result_manifest(json!({"text/plain": inline_ref("42")}), 5);
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        assert_eq!(output.output_type, "execute_result");
        assert_eq!(output.execution_count, Some(5));
    }

    #[tokio::test]
    async fn llm_blob_text_plain_falls_through_on_failure() {
        // text/latex is a blob that can't resolve (no store), text/plain is inline
        let manifest = make_display_manifest(json!({
            "text/latex": blob_ref("latex_hash", 5000),
            "text/plain": inline_ref("fallback plain"),
        }));
        let Some(output) = resolve_output_for_llm(&manifest, &None, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        // text/latex resolution failed → fell through to text/plain
        assert!(!data.contains_key("text/latex"));
        assert!(
            matches!(data.get("text/plain"), Some(DataValue::Text(s)) if s == "fallback plain")
        );
    }

    #[tokio::test]
    async fn llm_only_html_gets_described() {
        // text/html is NOT in CONTENT_PRIORITY — should be described, not resolved
        let manifest = make_display_manifest(json!({
            "text/html": blob_ref("html_hash", 8_000),
        }));
        let blob_base = Some("http://localhost:9999".to_string());
        let Some(output) = resolve_output_for_llm(&manifest, &blob_base, &None, None).await else {
            panic!("resolve should succeed");
        };
        let Some(data) = output.data else {
            panic!("output should have data");
        };

        assert!(!data.contains_key("text/html"));
        let llm = match data.get("text/llm+plain") {
            Some(DataValue::Text(s)) => s.clone(),
            _ => panic!("expected text/llm+plain for HTML-only output"),
        };
        assert!(llm.contains("HTML"));
        assert!(llm.contains("text/html"));
        assert!(llm.contains("http://localhost:9999/blob/html_hash"));
    }

    #[tokio::test]
    async fn llm_resolve_cell_outputs_for_llm_mixed() {
        let manifests = vec![
            json!({
                "output_type": "stream",
                "name": "stdout",
                "text": inline_ref("print output"),
            }),
            make_display_manifest(json!({
                "text/plain": inline_ref("<Figure>"),
                "image/png": blob_ref("fig_png", 80_000),
            })),
        ];
        let outputs = resolve_cell_outputs_for_llm(&manifests, &None, &None, None).await;
        assert_eq!(outputs.len(), 2);
        assert_eq!(outputs[0].output_type, "stream");
        assert_eq!(outputs[1].output_type, "display_data");
        // The display_data should only have text/plain, not image/png
        let Some(ref data) = outputs[1].data else {
            panic!("display_data should have data");
        };
        assert!(data.contains_key("text/plain"));
        assert!(!data.contains_key("image/png"));
    }
}
