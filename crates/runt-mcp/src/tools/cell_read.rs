//! Read-only cell tools: get_cell, get_all_cells.

use rmcp::model::{CallToolRequestParams, CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use runtimed_client::output_resolver;

use crate::formatting;
use crate::NteractMcp;

use super::{arg_str, require_handle, tool_error, tool_success};

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetCellParams {
    /// The cell ID to retrieve.
    pub cell_id: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct GetAllCellsParams {
    /// Output format: "summary" (default), "json", or "rich".
    #[serde(default = "default_format")]
    pub format: Option<String>,
    /// Starting cell index (0-based).
    #[serde(default)]
    pub start: Option<i64>,
    /// Number of cells to return (null = all).
    #[serde(default)]
    pub count: Option<i64>,
    /// Include output previews in summary format.
    #[serde(default)]
    pub include_outputs: Option<bool>,
    /// Max chars for source preview in summary format.
    #[serde(default)]
    pub preview_chars: Option<i64>,
}

fn default_format() -> Option<String> {
    Some("summary".to_string())
}

/// Get a single cell by ID with source and outputs.
pub async fn get_cell(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let cell_id = arg_str(request, "cell_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: cell_id", None))?;

    let handle = require_handle!(server);

    // No presence on read — get_cell is read-only, shouldn't move the cursor.

    let mut cell = match handle.get_cell(cell_id) {
        Some(c) => c,
        None => return tool_error(&format!("Cell not found: {cell_id}")),
    };

    // If the cell has been executed but outputs haven't synced yet,
    // force a sync round-trip to process pending RuntimeStateSync frames.
    if cell.outputs.is_empty()
        && !cell.execution_count.is_empty()
        && cell.execution_count != "0"
        && cell.execution_count != "null"
    {
        let _ = handle.confirm_sync().await;
        if let Some(c) = handle.get_cell(cell_id) {
            cell = c;
        }
    }

    // Resolve outputs (with widget state synthesis)
    let comms = handle.get_runtime_state().ok().map(|rs| rs.comms);
    let outputs = output_resolver::resolve_cell_outputs(
        &cell.outputs,
        &server.blob_base_url,
        &server.blob_store_path,
        comms.as_ref(),
    )
    .await;

    // Get execution status from RuntimeState
    let status = get_cell_status(&handle, cell_id);

    let header = formatting::format_cell_header(
        &cell.id,
        &cell.cell_type,
        Some(&cell.execution_count),
        status.as_deref(),
    );

    // Return multiple Content items: header+source, then one per output
    let mut items = Vec::new();

    if !cell.source.is_empty() {
        items.push(Content::text(format!("{header}\n\n{}", cell.source)));
    } else {
        items.push(Content::text(header));
    }

    // Each output as a separate Content item (matches Python _cell_to_content)
    items.extend(formatting::outputs_to_content_items(&outputs));

    Ok(CallToolResult::success(items))
}

/// Get all cells with configurable format.
pub async fn get_all_cells(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let handle = require_handle!(server);

    let format = arg_str(request, "format").unwrap_or("summary");
    let start = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("start"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as usize;
    let count = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("count"))
        .and_then(|v| v.as_i64())
        .map(|v| v as usize);
    let include_outputs = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("include_outputs"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let preview_chars = request
        .arguments
        .as_ref()
        .and_then(|a| a.get("preview_chars"))
        .and_then(|v| v.as_i64())
        .unwrap_or(60) as usize;

    let cells = handle.get_cells();
    let end = match count {
        Some(c) => (start + c).min(cells.len()),
        None => cells.len(),
    };
    let slice = &cells[start.min(cells.len())..end.min(cells.len())];

    // Build cell status map and comms from RuntimeState
    let cell_status_map = build_cell_status_map(&handle);
    let comms = handle.get_runtime_state().ok().map(|rs| rs.comms);

    match format {
        "json" => {
            let mut json_cells = Vec::new();
            for cell in slice {
                let status = cell_status_map.get(&cell.id).map(String::as_str);
                let ec: Option<i64> = cell.execution_count.parse().ok();

                // Resolve outputs through the output resolver so that
                // text/llm+plain is synthesized and viz specs are summarized.
                let resolved = output_resolver::resolve_cell_outputs(
                    &cell.outputs,
                    &server.blob_base_url,
                    &server.blob_store_path,
                    comms.as_ref(),
                )
                .await;
                let output_texts: Vec<String> = resolved
                    .iter()
                    .filter_map(formatting::format_output_text)
                    .collect();

                // Extract tags from cell metadata
                let tags: Vec<String> = cell
                    .metadata
                    .get("tags")
                    .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
                    .unwrap_or_default();

                json_cells.push(serde_json::json!({
                    "cell_id": cell.id,
                    "cell_type": cell.cell_type,
                    "execution_count": ec,
                    "source": cell.source,
                    "outputs": output_texts,
                    "status": status,
                    "tags": tags,
                }));
            }
            let text = serde_json::to_string_pretty(&json_cells).unwrap_or_default();
            Ok(CallToolResult::success(vec![Content::text(text)]))
        }
        "rich" => {
            let mut items = Vec::new();
            for cell in slice {
                let status = cell_status_map.get(&cell.id).map(String::as_str);
                let outputs = output_resolver::resolve_cell_outputs(
                    &cell.outputs,
                    &server.blob_base_url,
                    &server.blob_store_path,
                    comms.as_ref(),
                )
                .await;
                let header = formatting::format_cell_header(
                    &cell.id,
                    &cell.cell_type,
                    Some(&cell.execution_count),
                    status,
                );
                let output_text = formatting::format_outputs_text(&outputs);
                let text = if !cell.source.is_empty() {
                    format!("{header}\n\n{}", cell.source)
                } else {
                    header
                };
                items.push(Content::text(text));
                if !output_text.is_empty() {
                    items.push(Content::text(output_text));
                }
            }
            Ok(CallToolResult::success(items))
        }
        _ => {
            // summary format
            let mut lines = Vec::new();
            for (i, cell) in slice.iter().enumerate() {
                let status = cell_status_map.get(&cell.id).map(String::as_str);
                let line = formatting::format_cell_summary(
                    start + i,
                    &cell.id,
                    &cell.cell_type,
                    &cell.source,
                    Some(&cell.execution_count),
                    status,
                    preview_chars,
                );
                if include_outputs && !cell.outputs.is_empty() {
                    let outputs = output_resolver::resolve_cell_outputs(
                        &cell.outputs,
                        &server.blob_base_url,
                        &server.blob_store_path,
                        comms.as_ref(),
                    )
                    .await;
                    let output_text = formatting::format_outputs_text(&outputs);
                    if !output_text.is_empty() {
                        // Collapse to single line (matches Python format)
                        let output_line: String =
                            output_text.split_whitespace().collect::<Vec<_>>().join(" ");
                        let char_count = output_line.chars().count();
                        let output_preview = if char_count > preview_chars {
                            let truncated: String =
                                output_line.chars().take(preview_chars).collect();
                            let remaining = char_count - preview_chars;
                            format!("{truncated}…[+{remaining} chars]")
                        } else {
                            output_line
                        };
                        lines.push(format!("{line}\n  └─ {output_preview}"));
                    } else {
                        lines.push(line);
                    }
                } else {
                    lines.push(line);
                }
            }
            tool_success(&lines.join("\n"))
        }
    }
}

/// Get cell execution status from RuntimeState.
fn get_cell_status(handle: &notebook_sync::handle::DocHandle, cell_id: &str) -> Option<String> {
    if let Ok(state) = handle.get_runtime_state() {
        if state
            .queue
            .executing
            .as_ref()
            .is_some_and(|e| e.cell_id == cell_id)
        {
            return Some("running".to_string());
        }
        if state.queue.queued.iter().any(|e| e.cell_id == cell_id) {
            return Some("queued".to_string());
        }
    }
    None
}

/// Build a map of cell_id -> status from RuntimeState.
pub fn build_cell_status_map(
    handle: &notebook_sync::handle::DocHandle,
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(state) = handle.get_runtime_state() {
        if let Some(ref e) = state.queue.executing {
            map.insert(e.cell_id.clone(), "running".to_string());
        }
        for e in &state.queue.queued {
            map.insert(e.cell_id.clone(), "queued".to_string());
        }
    }
    map
}
