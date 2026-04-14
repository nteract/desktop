//! Output formatting for MCP tool results.
//!
//! Converts notebook outputs to text for LLM consumption, with ANSI stripping
//! and MIME type priority. Matches the Python MCP server's formatting behavior.

use base64::Engine;
use regex::Regex;
use std::sync::LazyLock;

use runtimed_client::resolved_output::{DataValue, Output};

/// ANSI escape code regex — matches color codes, cursor movement, OSC sequences.
#[allow(clippy::expect_used)] // Static regex, always valid
static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b\[[0-9;]*[A-Za-z]|\x1b\].*?\x07|\x1b\(B").expect("valid ANSI regex")
});

/// MIME types to try for text output, in priority order.
/// Matches the `CONTENT_PRIORITY` order in `output_resolver.rs` with
/// `application/json` appended as a formatting-layer fallback.
const TEXT_MIME_PRIORITY: &[&str] = &[
    "text/llm+plain",
    "text/latex",
    "text/markdown",
    "text/plain",
    "application/json",
];

/// Maximum text size (bytes) before truncation in `best_text_from_data`.
/// Acts as a safety net for heavy types that don't have `text/llm+plain` synthesis.
const MAX_TEXT_BYTES: usize = 8 * 1024;

/// Strip ANSI escape codes from text.
pub fn strip_ansi(text: &str) -> String {
    ANSI_RE.replace_all(text, "").to_string()
}

/// Extract the best text representation from an output's data dictionary.
/// Returns None if no suitable text MIME type is found.
///
/// Text exceeding 8 KB is truncated with a size note appended.
pub fn best_text_from_data(data: &std::collections::HashMap<String, DataValue>) -> Option<String> {
    for mime in TEXT_MIME_PRIORITY {
        if let Some(value) = data.get(*mime) {
            let text = match value {
                DataValue::Text(s) => Some(s.clone()),
                DataValue::Json(v) => Some(serde_json::to_string_pretty(v).unwrap_or_default()),
                DataValue::Binary(_) => None,
            };
            return text.map(|s| truncate_text(&s));
        }
    }
    None
}

/// Truncate text to `MAX_TEXT_BYTES`, appending a size note if truncated.
fn truncate_text(s: &str) -> String {
    if s.len() <= MAX_TEXT_BYTES {
        return s.to_string();
    }
    // Find a char boundary at or before MAX_TEXT_BYTES
    let mut end = MAX_TEXT_BYTES;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let total_kb = s.len() / 1024;
    format!("{}\n... [truncated, {} KB total]", &s[..end], total_kb)
}

/// Format a single output as text for LLM consumption.
pub fn format_output_text(output: &Output) -> Option<String> {
    match output.output_type.as_str() {
        "stream" => {
            let text = output.text.as_deref().unwrap_or("");
            let stripped = strip_ansi(text);
            if stripped.is_empty() {
                None
            } else {
                Some(stripped)
            }
        }
        "error" => {
            let mut parts = Vec::new();
            if let Some(ename) = &output.ename {
                let evalue = output.evalue.as_deref().unwrap_or("");
                parts.push(format!("{ename}: {evalue}"));
            }
            if let Some(traceback) = &output.traceback {
                let stripped: Vec<String> = traceback.iter().map(|t| strip_ansi(t)).collect();
                parts.push(stripped.join("\n"));
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n\n"))
            }
        }
        "display_data" | "execute_result" => {
            if let Some(data) = &output.data {
                best_text_from_data(data)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Format all outputs as a single text string (double-newline separated).
pub fn format_outputs_text(outputs: &[Output]) -> String {
    outputs
        .iter()
        .filter_map(format_output_text)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Convert outputs to separate Content items (one per output).
/// This gives MCP clients richer structure than a single concatenated string.
/// When outputs exist but have no text representation, appends a summary
/// so agents know execution produced output they can't see.
pub fn outputs_to_content_items(outputs: &[Output]) -> Vec<rmcp::model::Content> {
    let mut items: Vec<rmcp::model::Content> = Vec::new();
    let mut omitted_count = 0usize;
    let mut omitted_mimes: Vec<String> = Vec::new();

    for output in outputs {
        if let Some(text) = format_output_text(output) {
            items.push(rmcp::model::Content::text(text));
        } else if output.output_type == "display_data" || output.output_type == "execute_result" {
            omitted_count += 1;
            if let Some(data) = &output.data {
                let mimes: Vec<&str> = data
                    .keys()
                    .map(|k| k.as_str())
                    .filter(|k| !k.starts_with("text/llm"))
                    .collect();
                if !mimes.is_empty() {
                    omitted_mimes.push(mimes.join(", "));
                }
            }
        }
    }

    if omitted_count > 0 {
        let detail = if omitted_mimes.is_empty() {
            String::new()
        } else {
            format!(" ({})", omitted_mimes.join("; "))
        };
        items.push(rmcp::model::Content::text(format!(
            "[{omitted_count} output(s) with non-text content{detail} — visible in the notebook UI]"
        )));
    }

    items
}

/// Format a compact one-line cell summary (matches Python _format_cell_summary).
///
/// Example output:
///   0 | markdown | id=cell-1be2a179 | # Crate Download Analysis
///   1 | code | running | id=cell-e18fcc2a | exec=4 | import requests…[+45 chars]
pub fn format_cell_summary(
    index: usize,
    cell_id: &str,
    cell_type: &str,
    source: &str,
    execution_count: Option<&str>,
    status: Option<&str>,
    preview_chars: usize,
) -> String {
    let mut parts = vec![index.to_string(), cell_type.to_string()];

    // Status (running/queued) comes before id, like in Python
    if let Some(st) = status {
        if !st.is_empty() {
            parts.push(st.to_string());
        }
    }

    parts.push(format!("id={cell_id}"));

    // execution_count as exec=N (only for code cells with a value)
    if let Some(ec) = execution_count {
        if !ec.is_empty() && cell_type == "code" {
            parts.push(format!("exec={ec}"));
        }
    }

    // Source preview — collapse to single line, strip whitespace
    if !source.is_empty() {
        let source_line: String = source.split_whitespace().collect::<Vec<_>>().join(" ");
        let char_count = source_line.chars().count();
        let preview = if char_count > preview_chars {
            let truncated: String = source_line.chars().take(preview_chars).collect();
            let remaining = char_count - preview_chars;
            format!("{truncated}…[+{remaining} chars]")
        } else {
            source_line
        };
        parts.push(preview);
    }

    parts.join(" | ")
}

/// Format a cell header line (matches Python _format_header).
///
/// Example: ━━━ cell-abc12345 (code) ✓ idle [3] ━━━
pub fn format_cell_header(
    cell_id: &str,
    cell_type: &str,
    execution_count: Option<&str>,
    status: Option<&str>,
) -> String {
    let mut parts = vec![format!("━━━ {cell_id}")];

    parts.push(format!("({cell_type})"));

    if let Some(st) = status {
        if !st.is_empty() {
            let icon = match st {
                "idle" | "done" => "✓",
                "error" => "✗",
                "running" => "◐",
                "queued" => "⧗",
                "cancelled" => "⊘",
                _ => "?",
            };
            parts.push(format!("{icon} {st}"));
        }
    }

    if let Some(ec) = execution_count {
        if !ec.is_empty() {
            parts.push(format!("[{ec}]"));
        }
    }

    parts.push("━━━".to_string());
    parts.join(" ")
}

/// Maximum image file size to inline as base64 in MCP responses (10 MB).
const MAX_INLINE_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

/// Image MIME types that MCP clients can render natively.
fn is_inlinable_image_mime(mime: &str) -> bool {
    matches!(
        mime,
        "image/png" | "image/jpeg" | "image/gif" | "image/webp"
    )
}

/// Read image blobs from an output's `blob_paths` and return `Content::image` items.
fn inline_image_items(output: &Output) -> Vec<rmcp::model::Content> {
    let blob_paths = match &output.blob_paths {
        Some(paths) => paths,
        None => return Vec::new(),
    };

    let mut items = Vec::new();
    for (mime, path_str) in blob_paths {
        if !is_inlinable_image_mime(mime) {
            continue;
        }
        let path = std::path::Path::new(path_str);
        let metadata = match std::fs::metadata(path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if metadata.len() > MAX_INLINE_IMAGE_BYTES {
            continue;
        }
        if let Ok(bytes) = std::fs::read(path) {
            let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
            items.push(rmcp::model::Content::image(b64, mime.clone()));
        }
    }
    items
}

/// Convert outputs to Content items, inlining images from the blob store.
///
/// Like [`outputs_to_content_items`] but also reads image blobs referenced
/// by `blob_paths` and returns them as `Content::image()`. Used by `get_cell()`
/// where agents want to see output images without extra tool calls.
pub fn outputs_to_content_items_with_images(outputs: &[Output]) -> Vec<rmcp::model::Content> {
    let mut items: Vec<rmcp::model::Content> = Vec::new();
    let mut omitted_count = 0usize;
    let mut omitted_mimes: Vec<String> = Vec::new();

    for output in outputs {
        let text = format_output_text(output);
        let images = inline_image_items(output);

        if let Some(text) = text {
            items.push(rmcp::model::Content::text(text));
        }

        if !images.is_empty() {
            items.extend(images);
        } else if output.output_type == "display_data" || output.output_type == "execute_result" {
            // Count as omitted only if we got neither text nor images
            if format_output_text(output).is_none() {
                omitted_count += 1;
                if let Some(data) = &output.data {
                    let mimes: Vec<&str> = data
                        .keys()
                        .map(|k| k.as_str())
                        .filter(|k| !k.starts_with("text/llm"))
                        .collect();
                    if !mimes.is_empty() {
                        omitted_mimes.push(mimes.join(", "));
                    }
                }
            }
        }
    }

    if omitted_count > 0 {
        let detail = if omitted_mimes.is_empty() {
            String::new()
        } else {
            format!(" ({})", omitted_mimes.join("; "))
        };
        items.push(rmcp::model::Content::text(format!(
            "[{omitted_count} output(s) with non-text content{detail} — visible in the notebook UI]"
        )));
    }

    items
}

/// Walk structured content JSON and replace image blob URLs with base64 data URIs.
///
/// The structured_content JSON has shape `{"cell": {"outputs": [{"data": {"image/png": "http://…/blob/HASH"}}]}}`.
/// For each image MIME key whose value is a blob URL, reads the blob file and replaces
/// the URL with `data:{mime};base64,{b64}`.
pub fn resolve_image_blobs_in_json(
    mut value: serde_json::Value,
    blob_store_path: &Option<std::path::PathBuf>,
) -> serde_json::Value {
    let store = match blob_store_path {
        Some(p) => p,
        None => return value,
    };
    let outputs = match value
        .get_mut("cell")
        .and_then(|c| c.get_mut("outputs"))
        .and_then(|o| o.as_array_mut())
    {
        Some(arr) => arr,
        None => return value,
    };
    for output in outputs {
        let data = match output.get_mut("data").and_then(|d| d.as_object_mut()) {
            Some(d) => d,
            None => continue,
        };
        let image_mimes: Vec<String> = data
            .keys()
            .filter(|k| is_inlinable_image_mime(k))
            .cloned()
            .collect();
        for mime in image_mimes {
            let url = match data.get(&mime).and_then(|v| v.as_str()) {
                Some(u) => u.to_string(),
                None => continue,
            };
            let hash = match url.rsplit_once("/blob/") {
                Some((_, h)) => h,
                None => continue,
            };
            if hash.len() < 2 {
                continue;
            }
            let path = store.join(&hash[..2]).join(&hash[2..]);
            let metadata = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if metadata.len() > MAX_INLINE_IMAGE_BYTES {
                continue;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                data.insert(
                    mime.clone(),
                    serde_json::Value::String(format!("data:{mime};base64,{b64}")),
                );
            }
        }
    }
    value
}
