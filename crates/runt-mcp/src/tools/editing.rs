//! Editing tools: replace (consolidated replace_match + replace_regex).

use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::editing;
use crate::execution;
use crate::NteractMcp;

use super::{arg_bool, arg_str, tool_error};

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReplaceParams {
    /// The cell ID to edit.
    pub cell_id: String,
    /// Mode: "literal" (default) or "regex".
    #[serde(default = "default_mode")]
    pub mode: Option<String>,
    /// Literal text or regex pattern to find (must match exactly once). For regex: MULTILINE enabled, DOTALL off.
    #[serde(rename = "match")]
    pub match_text: String,
    /// Replacement text.
    pub content: String,
    /// Text before the match (literal mode only, for disambiguation).
    #[serde(default)]
    pub context_before: Option<String>,
    /// Text after the match (literal mode only, for disambiguation).
    #[serde(default)]
    pub context_after: Option<String>,
    /// Execute the cell immediately after edit.
    #[serde(default)]
    pub and_run: Option<bool>,
    /// Max seconds to wait for execution.
    #[serde(default)]
    pub timeout_secs: Option<f64>,
}

fn default_mode() -> Option<String> {
    Some("literal".to_string())
}

/// Consolidated replace: dispatches to literal or regex logic.
pub async fn replace(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let mode = arg_str(request, "mode").unwrap_or("literal");
    match mode {
        "literal" => replace_match(server, request).await,
        "regex" => replace_regex(server, request).await,
        _ => Err(McpError::invalid_params(
            format!("Unknown replace mode: {mode}. Use \"literal\" or \"regex\"."),
            None,
        )),
    }
}

/// Replace matched text in a cell.
pub async fn replace_match(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let cell_id = arg_str(request, "cell_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: cell_id", None))?;
    let match_text = arg_str(request, "match")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: match", None))?;
    let content = arg_str(request, "content")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: content", None))?;

    let context_before = arg_str(request, "context_before").filter(|s| !s.is_empty());
    let context_after = arg_str(request, "context_after").filter(|s| !s.is_empty());

    let and_run = arg_bool(request, "and_run").unwrap_or(false);
    let timeout_secs = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("timeout_secs"))
        .and_then(|v| v.as_f64())
        .unwrap_or(30.0);

    let handle = require_handle!(server);

    let source = match handle.get_cell_source(cell_id) {
        Some(s) => s,
        None => return tool_error(&format!("Cell \"{cell_id}\" not found")),
    };

    // Resolve the match
    let span = match editing::resolve_match(&source, match_text, context_before, context_after) {
        Ok(span) => span,
        Err(e) => {
            return Err(McpError::internal_error(
                format!("{e} (source_length={})", source.len()),
                None,
            ));
        }
    };

    // Convert byte offsets to code point offsets for Automerge splice
    let cp_start = editing::byte_offset_to_codepoint(&source, span.start);
    let cp_end = editing::byte_offset_to_codepoint(&source, span.end);
    let cp_delete = cp_end - cp_start;

    handle
        .splice_source(cell_id, cp_start, cp_delete, content)
        .map_err(|e| McpError::internal_error(format!("Failed to splice source: {e}"), None))?;

    // Cursor at end of replacement text
    let new_source = crate::editing::apply_replacement(&source, &span, content);
    let end_offset = span.start + content.len();
    let (line, col) = crate::presence::offset_to_line_col(&new_source, end_offset);
    let peer_label = server.get_peer_label().await;
    crate::presence::emit_cursor(&handle, cell_id, line, col, &peer_label).await;

    if and_run {
        let result = execution::execute_and_wait(
            &handle,
            cell_id,
            Duration::from_secs_f64(timeout_secs),
            &server.blob_base_url,
            &server.blob_store_path,
        )
        .await;
        return super::build_execution_result(&result, &handle, server).await;
    }

    // Return diff
    let old_text = &source[span.start..span.end];
    let diff = format_edit_diff(cell_id, old_text, content);
    Ok(CallToolResult::success(vec![Content::text(diff)]))
}

/// Replace a regex-matched span in a cell.
pub async fn replace_regex(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let cell_id = arg_str(request, "cell_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: cell_id", None))?;
    // Accept "match" (consolidated schema) or "pattern" (legacy replace_regex)
    let pattern = arg_str(request, "match")
        .or_else(|| arg_str(request, "pattern"))
        .ok_or_else(|| {
            McpError::invalid_params("Missing required parameter: match (or pattern)", None)
        })?;
    let content = arg_str(request, "content")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: content", None))?;

    let and_run = arg_bool(request, "and_run").unwrap_or(false);
    let timeout_secs = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("timeout_secs"))
        .and_then(|v| v.as_f64())
        .unwrap_or(30.0);

    let handle = require_handle!(server);

    let source = match handle.get_cell_source(cell_id) {
        Some(s) => s,
        None => return tool_error(&format!("Cell \"{cell_id}\" not found")),
    };

    // Resolve the regex
    let span = match editing::resolve_regex(&source, pattern) {
        Ok(span) => span,
        Err(e) => {
            return Err(McpError::internal_error(
                format!("{e} (source_length={})", source.len()),
                None,
            ));
        }
    };

    // Convert byte offsets to code point offsets for Automerge splice
    let cp_start = editing::byte_offset_to_codepoint(&source, span.start);
    let cp_end = editing::byte_offset_to_codepoint(&source, span.end);
    let cp_delete = cp_end - cp_start;

    handle
        .splice_source(cell_id, cp_start, cp_delete, content)
        .map_err(|e| McpError::internal_error(format!("Failed to splice source: {e}"), None))?;

    // Cursor at end of replacement text
    let new_source = crate::editing::apply_replacement(&source, &span, content);
    let end_offset = span.start + content.len();
    let (line, col) = crate::presence::offset_to_line_col(&new_source, end_offset);
    let peer_label = server.get_peer_label().await;
    crate::presence::emit_cursor(&handle, cell_id, line, col, &peer_label).await;

    if and_run {
        let result = execution::execute_and_wait(
            &handle,
            cell_id,
            Duration::from_secs_f64(timeout_secs),
            &server.blob_base_url,
            &server.blob_store_path,
        )
        .await;
        return super::build_execution_result(&result, &handle, server).await;
    }

    // Return diff
    let old_text = &source[span.start..span.end];
    let diff = format_edit_diff(cell_id, old_text, content);
    Ok(CallToolResult::success(vec![Content::text(diff)]))
}

/// Format a unified diff for an edit operation.
fn format_edit_diff(cell_id: &str, old_text: &str, new_text: &str) -> String {
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();

    let mut diff_parts = Vec::new();
    diff_parts.push(format!("Edited cell \"{cell_id}\":"));
    diff_parts.push("--- before".to_string());
    diff_parts.push("+++ after".to_string());

    for line in &old_lines {
        diff_parts.push(format!("-{line}"));
    }
    for line in &new_lines {
        diff_parts.push(format!("+{line}"));
    }

    diff_parts.join("\n")
}
