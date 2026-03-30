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

/// Strip ANSI escape codes from text.
pub fn strip_ansi(text: &str) -> String {
    ANSI_RE.replace_all(text, "").to_string()
}

/// Extract the best text representation from an output's data dictionary.
/// Returns None if no suitable text MIME type is found.
pub fn best_text_from_data(data: &std::collections::HashMap<String, DataValue>) -> Option<String> {
    for mime in TEXT_MIME_PRIORITY {
        if let Some(value) = data.get(*mime) {
            return match value {
                DataValue::Text(s) => Some(s.clone()),
                DataValue::Json(v) => Some(serde_json::to_string_pretty(v).unwrap_or_default()),
                DataValue::Binary(_) => None,
            };
        }
    }
    None
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

/// Format all outputs as a single text string.
pub fn format_outputs_text(outputs: &[Output]) -> String {
    outputs
        .iter()
        .filter_map(format_output_text)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Format a compact one-line cell summary.
pub fn format_cell_summary(
    index: usize,
    cell_id: &str,
    cell_type: &str,
    source: &str,
    execution_count: Option<&str>,
    status: Option<&str>,
    preview_chars: usize,
) -> String {
    let ec = execution_count.unwrap_or("");
    let st = status.unwrap_or("");
    let preview = if source.len() > preview_chars {
        format!("{}…", &source[..preview_chars])
    } else {
        source.to_string()
    };
    // Replace newlines with ↵ for single-line display
    let preview = preview.replace('\n', "↵");
    format!("{index} | {cell_type} | {st} | id={cell_id} | [{ec}] | {preview}")
}

/// Format a cell header line.
pub fn format_cell_header(
    cell_id: &str,
    cell_type: &str,
    execution_count: Option<&str>,
    status: Option<&str>,
) -> String {
    let ec_display = execution_count
        .filter(|s| !s.is_empty())
        .map(|s| format!("[{s}]"))
        .unwrap_or_default();
    let status_display = status
        .filter(|s| !s.is_empty())
        .map(|s| format!(" ({s})"))
        .unwrap_or_default();
    format!("── {cell_type} {ec_display}{status_display} ── {cell_id}")
}
