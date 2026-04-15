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

#[cfg(test)]
mod tests {
    use super::*;
    use runtimed_client::resolved_output::{DataValue, Output};
    use std::collections::HashMap;

    fn data(pairs: &[(&str, DataValue)]) -> HashMap<String, DataValue> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn strip_ansi_removes_color_codes() {
        assert_eq!(strip_ansi("\x1b[31mred\x1b[0m"), "red");
        assert_eq!(strip_ansi("\x1b[1;32mbold green\x1b[0m"), "bold green");
        assert_eq!(strip_ansi("plain"), "plain");
    }

    #[test]
    fn strip_ansi_removes_osc_sequence() {
        // OSC: ESC ] ... BEL — used for window titles, hyperlinks. Must not
        // leak through to the LLM as cruft.
        assert_eq!(strip_ansi("\x1b]0;title\x07after"), "after");
    }

    #[test]
    fn best_text_picks_highest_priority_mime() {
        // `text/llm+plain` is synthesized by repr-llm and is the preferred
        // representation for LLM consumption — must beat text/plain even
        // when both are present.
        let d = data(&[
            ("text/plain", DataValue::Text("fallback".into())),
            ("text/llm+plain", DataValue::Text("llm-optimized".into())),
        ]);
        assert_eq!(best_text_from_data(&d).as_deref(), Some("llm-optimized"));
    }

    #[test]
    fn best_text_falls_back_to_application_json() {
        let d = data(&[(
            "application/json",
            DataValue::Json(serde_json::json!({"a": 1})),
        )]);
        let Some(text) = best_text_from_data(&d) else {
            panic!("json should render");
        };
        assert!(text.contains("\"a\""));
        assert!(text.contains('1'));
    }

    #[test]
    fn best_text_ignores_binary_only_data() {
        let d = data(&[("image/png", DataValue::Binary(vec![0x89, 0x50]))]);
        assert_eq!(best_text_from_data(&d), None);
    }

    #[test]
    fn best_text_truncates_oversize_payloads() {
        // Safety net for heavy types with no text/llm+plain synthesis.
        // Truncation must include the size hint so the LLM sees that
        // content was dropped.
        let big = "a".repeat(16 * 1024);
        let d = data(&[("text/plain", DataValue::Text(big))]);
        let Some(text) = best_text_from_data(&d) else {
            panic!("should return truncated");
        };
        assert!(text.contains("[truncated"));
        assert!(text.contains("16 KB total"));
    }

    #[test]
    fn truncate_text_respects_char_boundaries() {
        // 4-byte characters (e.g. emoji) right at the boundary must not
        // cut mid-codepoint and produce invalid UTF-8.
        let emoji = "🚀".repeat(3000); // 4 bytes each = 12 KB
        let d = data(&[("text/plain", DataValue::Text(emoji))]);
        let Some(text) = best_text_from_data(&d) else {
            panic!("should return truncated");
        };
        assert!(text.contains("[truncated"));
        // If we cut mid-codepoint, the format! would have panicked already.
    }

    #[test]
    fn format_stream_output_strips_ansi() {
        let o = Output::stream("stdout", "\x1b[31merror\x1b[0m in the output");
        assert_eq!(
            format_output_text(&o).as_deref(),
            Some("error in the output")
        );
    }

    #[test]
    fn format_empty_stream_output_is_none() {
        let o = Output::stream("stdout", "");
        assert_eq!(format_output_text(&o), None);
    }

    #[test]
    fn format_error_output_joins_ename_evalue_and_traceback() {
        let o = Output::error(
            "NameError",
            "name 'x' is not defined",
            vec![
                "\x1b[31mTraceback (most recent call last):\x1b[0m".into(),
                "  File \"<stdin>\", line 1, in <module>".into(),
            ],
        );
        let Some(text) = format_output_text(&o) else {
            panic!("error output should format");
        };
        assert!(text.contains("NameError: name 'x' is not defined"));
        assert!(text.contains("Traceback"));
        // ANSI codes in traceback must be stripped too.
        assert!(!text.contains('\x1b'));
    }

    #[test]
    fn format_unknown_output_type_is_none() {
        let o = Output {
            output_type: "weird-future-type".into(),
            ..Output::stream("stdout", "")
        };
        assert_eq!(format_output_text(&o), None);
    }

    #[test]
    fn format_outputs_text_joins_with_blank_line() {
        let outputs = vec![
            Output::stream("stdout", "first"),
            Output::stream("stdout", "second"),
        ];
        assert_eq!(format_outputs_text(&outputs), "first\n\nsecond");
    }

    #[test]
    fn outputs_to_content_items_summarizes_image_only() {
        // A bare image output has no text rep; the agent needs to know
        // something was produced, hence the "[N output(s) with non-text
        // content]" footer.
        let outputs = vec![Output::display_data(data(&[(
            "image/png",
            DataValue::Binary(vec![0; 10]),
        )]))];
        let items = outputs_to_content_items(&outputs);
        assert_eq!(items.len(), 1);
        let rendered = format!("{:?}", items[0]);
        assert!(rendered.contains("1 output"));
        assert!(rendered.contains("image/png"));
    }

    #[test]
    fn outputs_to_content_items_excludes_llm_mimes_from_summary() {
        // `text/llm+plain` is an LLM synthesis artifact — if it exists we'd
        // already have rendered the output as text, so we should never be
        // in the omitted branch. But if we were, the summary should not
        // leak the llm+plain mime type to the agent.
        let outputs = vec![Output::display_data(data(&[
            ("image/png", DataValue::Binary(vec![0; 10])),
            ("text/llm+plain", DataValue::Text("hidden".into())),
        ]))];
        let items = outputs_to_content_items(&outputs);
        // Rendered as text via text/llm+plain — no omitted footer.
        assert_eq!(items.len(), 1);
        let rendered = format!("{:?}", items[0]);
        assert!(rendered.contains("hidden"));
    }

    #[test]
    fn format_cell_summary_truncates_long_source() {
        let summary = format_cell_summary(
            3,
            "cell-abc",
            "code",
            "import numpy as np\nimport pandas as pd",
            Some("5"),
            Some("idle"),
            15,
        );
        assert!(summary.starts_with("3 | code | idle | id=cell-abc | exec=5 | "));
        assert!(summary.contains("…[+"));
        assert!(summary.contains(" chars]"));
    }

    #[test]
    fn format_cell_summary_skips_exec_for_markdown() {
        // Markdown cells don't have execution counts; the exec= field
        // must not appear even if a value was threaded through.
        let summary = format_cell_summary(0, "cell-md", "markdown", "# Hello", Some("1"), None, 50);
        assert!(!summary.contains("exec="));
        assert!(summary.contains("# Hello"));
    }

    #[test]
    fn format_cell_summary_collapses_whitespace() {
        // Multi-line or multi-space source must render on a single line.
        let summary = format_cell_summary(
            0,
            "cell-x",
            "code",
            "x = 1\n\n\n  y   =    2",
            None,
            None,
            100,
        );
        assert!(summary.contains("x = 1 y = 2"));
        assert!(!summary.contains('\n'));
    }

    #[test]
    fn format_cell_header_chooses_icon_by_status() {
        let idle = format_cell_header("cell-a", "code", Some("3"), Some("idle"));
        assert!(idle.contains("✓ idle"));
        assert!(idle.contains("[3]"));

        let err = format_cell_header("cell-b", "code", None, Some("error"));
        assert!(err.contains("✗ error"));

        let running = format_cell_header("cell-c", "code", None, Some("running"));
        assert!(running.contains("◐ running"));

        let queued = format_cell_header("cell-d", "code", None, Some("queued"));
        assert!(queued.contains("⧗ queued"));

        let unknown = format_cell_header("cell-e", "code", None, Some("bogus"));
        assert!(unknown.contains("? bogus"));
    }
}
