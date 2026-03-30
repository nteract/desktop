//! Editing tools: replace_match, replace_regex.

use std::time::Duration;

use rmcp::model::{CallToolRequestParams, CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::editing;
use crate::execution;
use crate::formatting;
use crate::structured;
use crate::NteractMcp;

use super::{arg_str, tool_error};

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReplaceMatchParams {
    /// The cell ID to edit.
    pub cell_id: String,
    /// Literal text to find (must match exactly once).
    #[serde(rename = "match")]
    pub match_text: String,
    /// Literal replacement text.
    pub content: String,
    /// Text that must appear before the match.
    #[serde(default)]
    pub context_before: Option<String>,
    /// Text that must appear after the match.
    #[serde(default)]
    pub context_after: Option<String>,
    /// Execute the cell immediately after edit.
    #[serde(default)]
    pub and_run: Option<bool>,
    /// Max seconds to wait for execution.
    #[serde(default)]
    pub timeout_secs: Option<f64>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ReplaceRegexParams {
    /// The cell ID to edit.
    pub cell_id: String,
    /// Regex pattern (must match exactly once). MULTILINE enabled.
    pub pattern: String,
    /// Literal replacement text.
    pub content: String,
    /// Execute the cell immediately after edit.
    #[serde(default)]
    pub and_run: Option<bool>,
    /// Max seconds to wait for execution.
    #[serde(default)]
    pub timeout_secs: Option<f64>,
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

    let and_run = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("and_run"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let timeout_secs = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("timeout_secs"))
        .and_then(|v| v.as_f64())
        .unwrap_or(30.0);

    let session = server.session.read().await;
    let session = match session.as_ref() {
        Some(s) => s,
        None => {
            return tool_error(
                "No active notebook session. Call join_notebook or open_notebook first.",
            )
        }
    };

    let handle = &session.handle;

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
    crate::presence::emit_cursor(handle, cell_id, line, col).await;

    if and_run {
        let result = execution::execute_and_wait(
            handle,
            cell_id,
            Duration::from_secs_f64(timeout_secs),
            &server.blob_base_url,
            &server.blob_store_path,
        )
        .await;
        return build_execution_result(&result, handle, server).await;
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
    let pattern = arg_str(request, "pattern")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: pattern", None))?;
    let content = arg_str(request, "content")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: content", None))?;

    let and_run = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("and_run"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let timeout_secs = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("timeout_secs"))
        .and_then(|v| v.as_f64())
        .unwrap_or(30.0);

    let session = server.session.read().await;
    let session = match session.as_ref() {
        Some(s) => s,
        None => {
            return tool_error(
                "No active notebook session. Call join_notebook or open_notebook first.",
            )
        }
    };

    let handle = &session.handle;

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
    crate::presence::emit_cursor(handle, cell_id, line, col).await;

    if and_run {
        let result = execution::execute_and_wait(
            handle,
            cell_id,
            Duration::from_secs_f64(timeout_secs),
            &server.blob_base_url,
            &server.blob_store_path,
        )
        .await;
        return build_execution_result(&result, handle, server).await;
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

/// Build a CallToolResult from an ExecutionResult with structured content.
async fn build_execution_result(
    result: &execution::ExecutionResult,
    handle: &notebook_sync::handle::DocHandle,
    server: &NteractMcp,
) -> Result<CallToolResult, McpError> {
    let header = formatting::format_cell_header(
        &result.cell_id,
        "code",
        result.execution_count.as_deref(),
        Some(&result.status),
    );

    let output_text = formatting::format_outputs_text(&result.outputs);
    let text = if !output_text.is_empty() {
        format!("{header}\n\n{output_text}")
    } else {
        header
    };

    let items = vec![Content::text(text)];

    // Build structured content for MCP Apps widget using the protocol's
    // structured_content field instead of a text-based fallback.
    let cell_snapshot = handle.get_cell(&result.cell_id);
    let structured_content = if let Some(snap) = cell_snapshot {
        let outputs = runtimed_client::output_resolver::resolve_cell_outputs(
            &snap.outputs,
            &server.blob_base_url,
            &server.blob_store_path,
        )
        .await;
        let resolved = runtimed_client::resolved_output::ResolvedCell {
            id: snap.id,
            cell_type: snap.cell_type,
            position: snap.position,
            source: snap.source,
            execution_count: snap.execution_count.parse().ok(),
            outputs,
            metadata_json: serde_json::to_string(&snap.metadata).unwrap_or_default(),
        };
        Some(structured::cell_structured_content(
            &resolved,
            &result.status,
        ))
    } else {
        None
    };

    let mut call_result = CallToolResult::success(items);
    call_result.structured_content = structured_content;
    Ok(call_result)
}
