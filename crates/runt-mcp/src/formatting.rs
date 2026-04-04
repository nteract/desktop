//! Output formatting for MCP tool results.
//!
//! Converts notebook outputs to text for LLM consumption, with ANSI stripping
//! and MIME type priority. Matches the Python MCP server's formatting behavior.

use regex::Regex;
use std::sync::LazyLock;

use runtimed_client::resolved_output::{DataValue, Output};

/// ANSI escape code regex — matches color codes, cursor movement, OSC sequences.
#[allow(clippy::expect_used)] // Static regex, always valid
static ANSI_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\x1b\[[0-9;]*[A-Za-z]|\x1b\].*?\x07|\x1b\(B").expect("valid ANSI regex")
});

/// MIME types to try for text output, in priority order.
/// text/llm+plain is a custom MIME for LLM-friendly representations.
const TEXT_MIME_PRIORITY: &[&str] = &[
    "text/llm+plain",
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
pub fn outputs_to_content_items(outputs: &[Output]) -> Vec<rmcp::model::Content> {
    outputs
        .iter()
        .filter_map(format_output_text)
        .map(rmcp::model::Content::text)
        .collect()
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
                "idle" => "✓",
                "error" => "✗",
                "running" => "◐",
                "queued" => "⧗",
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
