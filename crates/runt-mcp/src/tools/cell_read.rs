//! Read-only cell tools: get_cell, get_all_cells.

use rmcp::model::{CallToolRequestParams, CallToolResult, Content};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use runtimed_client::output_resolver;

use crate::formatting;
use crate::NteractMcp;

use super::{arg_bool, arg_str, tool_error, tool_success};

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

    let cell = match handle.get_cell(cell_id) {
        Some(c) => c,
        None => return tool_error(&format!("Cell not found: {cell_id}")),
    };

    // Get execution_count from RuntimeStateDoc (the source of truth)
    let ec = get_cell_execution_count_from_runtime(&handle, cell_id);

    // Outputs live in RuntimeStateDoc keyed by execution_id. Fetch them
    // via the dedicated lookup rather than reading a (now-gone) field
    // on CellSnapshot.
    let mut raw_outputs = handle.get_cell_outputs(cell_id).unwrap_or_default();

    // If the cell has been executed but outputs haven't synced yet,
    // force a sync round-trip to process pending RuntimeStateSync frames.
    if raw_outputs.is_empty() && !ec.is_empty() {
        let _ = handle.confirm_sync().await;
        raw_outputs = handle.get_cell_outputs(cell_id).unwrap_or_default();
    }

    // Resolve outputs (with widget state synthesis)
    let comms = handle.get_runtime_state().ok().map(|rs| rs.comms);
    let outputs = output_resolver::resolve_cell_outputs_for_llm(
        &raw_outputs,
        &server.blob_base_url,
        &server.blob_store_path,
        comms.as_ref(),
    )
    .await;

    // Get execution status from RuntimeState
    let status = get_cell_status(&handle, cell_id);

    let ec_display = if ec.is_empty() {
        None
    } else {
        Some(ec.as_str())
    };
    let header =
        formatting::format_cell_header(&cell.id, &cell.cell_type, ec_display, status.as_deref());

    // Include tags if present
    let tags = cell.tags();
    let header_with_tags = if tags.is_empty() {
        header
    } else {
        format!("{header}\nTags: {}", tags.join(", "))
    };

    // Return multiple Content items: header+source, then one per output
    let mut items = Vec::new();

    if !cell.source.is_empty() {
        items.push(Content::text(format!(
            "{header_with_tags}\n\n{}",
            cell.source
        )));
    } else {
        items.push(Content::text(header_with_tags));
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
    let include_outputs = arg_bool(request, "include_outputs").unwrap_or(false);
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

    // Build cell status map, execution count map, and comms from RuntimeState.
    // Outputs live in RuntimeStateDoc keyed by execution_id; fetch them in
    // bulk once so large notebooks don't pay O(N) state-doc reads.
    let cell_status_map = build_cell_status_map(&handle);
    let cell_ec_map = build_cell_execution_count_map(&handle);
    let comms = handle.get_runtime_state().ok().map(|rs| rs.comms);
    let outputs_by_cell = handle.get_all_outputs();
    let empty_outputs: Vec<serde_json::Value> = Vec::new();

    match format {
        "json" => {
            let mut json_cells = Vec::new();
            for cell in slice {
                let status = cell_status_map.get(&cell.id).map(String::as_str);
                let ec: Option<i64> = cell_ec_map.get(&cell.id).and_then(|s| s.parse().ok());
                let raw_outputs = outputs_by_cell.get(&cell.id).unwrap_or(&empty_outputs);

                // Resolve outputs through the output resolver so that
                // text/llm+plain is synthesized and viz specs are summarized.
                let resolved = output_resolver::resolve_cell_outputs_for_llm(
                    raw_outputs,
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
                let ec = cell_ec_map.get(&cell.id).map(String::as_str);
                let raw_outputs = outputs_by_cell.get(&cell.id).unwrap_or(&empty_outputs);
                let outputs = output_resolver::resolve_cell_outputs_for_llm(
                    raw_outputs,
                    &server.blob_base_url,
                    &server.blob_store_path,
                    comms.as_ref(),
                )
                .await;
                let header = formatting::format_cell_header(&cell.id, &cell.cell_type, ec, status);
                let tags = cell.tags();
                let header = if tags.is_empty() {
                    header
                } else {
                    format!("{header}\nTags: {}", tags.join(", "))
                };
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
                let ec = cell_ec_map.get(&cell.id).map(String::as_str);
                let line = formatting::format_cell_summary(
                    start + i,
                    &cell.id,
                    &cell.cell_type,
                    &cell.source,
                    ec,
                    status,
                    preview_chars,
                );
                let raw_outputs = outputs_by_cell.get(&cell.id).unwrap_or(&empty_outputs);
                if include_outputs && !raw_outputs.is_empty() {
                    let outputs = output_resolver::resolve_cell_outputs_for_llm(
                        raw_outputs,
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

/// Get the execution_count for a cell from RuntimeStateDoc.
///
/// Looks at the executions map for the most recent execution of this cell
/// that has an execution_count set (from the kernel's execute_input message).
/// Returns an empty string if no execution_count is found, matching the
/// convention used by formatting functions.
pub fn get_cell_execution_count_from_runtime(
    handle: &notebook_sync::handle::DocHandle,
    cell_id: &str,
) -> String {
    if let Ok(state) = handle.get_runtime_state() {
        if let Some(exec) = state
            .executions
            .values()
            .filter(|e| e.cell_id == cell_id && e.execution_count.is_some())
            .max_by_key(|e| e.execution_count)
        {
            if let Some(count) = exec.execution_count {
                return count.to_string();
            }
        }
    }
    String::new()
}

/// Get cell execution status from RuntimeState.
///
/// Checks queue first (running/queued), then falls back to the executions
/// map for terminal status (done/error). Without this fallback, agents
/// cannot distinguish "executed with output" from "never ran."
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
        // Check executions map for terminal status (most recent execution for this cell)
        if let Some(exec) = state
            .executions
            .values()
            .filter(|e| e.cell_id == cell_id)
            .max_by_key(|e| e.execution_count)
        {
            if exec.status == "done" || exec.status == "error" {
                return Some(exec.status.clone());
            }
        }
    }
    None
}

/// Build a map of cell_id -> execution_count string from RuntimeState.
///
/// For each cell, finds the most recent execution with an execution_count
/// and stores it as a string (e.g. "5"). Cells without execution_count
/// are absent from the map.
pub fn build_cell_execution_count_map(
    handle: &notebook_sync::handle::DocHandle,
) -> std::collections::HashMap<String, String> {
    let mut map: std::collections::HashMap<String, (i64, String)> =
        std::collections::HashMap::new();
    if let Ok(state) = handle.get_runtime_state() {
        for exec in state.executions.values() {
            if let Some(count) = exec.execution_count {
                let entry = map.entry(exec.cell_id.clone());
                match entry {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert((count, count.to_string()));
                    }
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        if count > e.get().0 {
                            e.insert((count, count.to_string()));
                        }
                    }
                }
            }
        }
    }
    map.into_iter().map(|(k, (_, v))| (k, v)).collect()
}

/// Build a map of cell_id -> status from RuntimeState.
pub fn build_cell_status_map(
    handle: &notebook_sync::handle::DocHandle,
) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    if let Ok(state) = handle.get_runtime_state() {
        // Terminal statuses from executions (written first, active queue overrides below)
        for exec in state.executions.values() {
            if exec.status == "done" || exec.status == "error" {
                map.entry(exec.cell_id.clone())
                    .or_insert_with(|| exec.status.clone());
            }
        }
        // Active queue statuses override terminal ones
        if let Some(ref e) = state.queue.executing {
            map.insert(e.cell_id.clone(), "running".to_string());
        }
        for e in &state.queue.queued {
            map.insert(e.cell_id.clone(), "queued".to_string());
        }
    }
    map
}
