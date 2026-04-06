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

/// MIME type for Jupyter widget view references.
const WIDGET_VIEW_MIME: &str = "application/vnd.jupyter.widget-view+json";

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
