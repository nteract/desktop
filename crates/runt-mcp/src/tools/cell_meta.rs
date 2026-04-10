//! Cell metadata tools: add_cell_tags, remove_cell_tags, set_cells_source_hidden, set_cells_outputs_hidden.

use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::ErrorData as McpError;
use schemars::JsonSchema;
use serde::Deserialize;

use crate::NteractMcp;

use super::{arg_bool, arg_str, arg_string_array, tool_error, tool_success};

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct AddCellTagsParams {
    /// ID of the cell.
    pub cell_id: String,
    /// Tags to add.
    pub tags: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RemoveCellTagsParams {
    /// ID of the cell.
    pub cell_id: String,
    /// Tags to remove.
    pub tags: Vec<String>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetCellsSourceHiddenParams {
    /// IDs of cells to update.
    pub cell_ids: Vec<String>,
    /// True to hide source, False to show.
    pub hidden: bool,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetCellsOutputsHiddenParams {
    /// IDs of cells to update.
    pub cell_ids: Vec<String>,
    /// True to hide outputs, False to show.
    pub hidden: bool,
}

/// Add tags to a cell's metadata. Existing tags are preserved.
pub async fn add_cell_tags(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let cell_id = arg_str(request, "cell_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: cell_id", None))?;

    let handle = require_handle!(server);

    // Get existing tags from cell metadata
    let metadata = match handle.get_cell_metadata(cell_id) {
        Some(m) => m,
        None => return tool_error(&format!("Cell {cell_id} not found")),
    };

    let existing_tags: Vec<String> = metadata
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // Parse new tags from request
    let new_tags: Vec<String> = arg_string_array(request, "tags").unwrap_or_default();

    // Merge: keep existing, add new ones that aren't already present
    let mut merged = existing_tags;
    for tag in &new_tags {
        if !merged.contains(tag) {
            merged.push(tag.clone());
        }
    }

    let tag_refs: Vec<&str> = merged.iter().map(|s| s.as_str()).collect();
    handle
        .set_cell_tags(cell_id, &tag_refs)
        .map_err(|e| McpError::internal_error(format!("Failed to set tags: {e}"), None))?;

    tool_success(&format!("Tags for {cell_id}: {merged:?}"))
}

/// Remove tags from a cell's metadata.
pub async fn remove_cell_tags(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let cell_id = arg_str(request, "cell_id")
        .ok_or_else(|| McpError::invalid_params("Missing required parameter: cell_id", None))?;

    let handle = require_handle!(server);

    let metadata = match handle.get_cell_metadata(cell_id) {
        Some(m) => m,
        None => return tool_error(&format!("Cell {cell_id} not found")),
    };

    let existing_tags: Vec<String> = metadata
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let tags_to_remove: Vec<String> = arg_string_array(request, "tags").unwrap_or_default();

    let filtered: Vec<String> = existing_tags
        .into_iter()
        .filter(|t| !tags_to_remove.contains(t))
        .collect();

    let tag_refs: Vec<&str> = filtered.iter().map(|s| s.as_str()).collect();
    handle
        .set_cell_tags(cell_id, &tag_refs)
        .map_err(|e| McpError::internal_error(format!("Failed to set tags: {e}"), None))?;

    tool_success(&format!("Tags for {cell_id}: {filtered:?}"))
}

/// Hide or show the source of one or more cells.
pub async fn set_cells_source_hidden(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let handle = require_handle!(server);

    let cell_ids: Vec<String> = arg_string_array(request, "cell_ids").unwrap_or_default();

    let hidden = arg_bool(request, "hidden").unwrap_or(false);

    let mut not_found = Vec::new();

    for cell_id in &cell_ids {
        match handle.set_cell_source_hidden(cell_id, hidden) {
            Ok(true) => {}
            Ok(false) => not_found.push(cell_id.as_str()),
            Err(_) => not_found.push(cell_id.as_str()),
        }
    }

    let updated = cell_ids.len() - not_found.len();
    let mut msg = format!("Set source_hidden={hidden} on {updated} cell(s)");
    if !not_found.is_empty() {
        msg.push_str(&format!("; not found: {not_found:?}"));
    }
    tool_success(&msg)
}

/// Hide or show the outputs of one or more cells.
pub async fn set_cells_outputs_hidden(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let handle = require_handle!(server);

    let cell_ids: Vec<String> = arg_string_array(request, "cell_ids").unwrap_or_default();

    let hidden = arg_bool(request, "hidden").unwrap_or(false);

    let mut not_found = Vec::new();

    for cell_id in &cell_ids {
        match handle.set_cell_outputs_hidden(cell_id, hidden) {
            Ok(true) => {}
            Ok(false) => not_found.push(cell_id.as_str()),
            Err(_) => not_found.push(cell_id.as_str()),
        }
    }

    let updated = cell_ids.len() - not_found.len();
    let mut msg = format!("Set outputs_hidden={hidden} on {updated} cell(s)");
    if !not_found.is_empty() {
        msg.push_str(&format!("; not found: {not_found:?}"));
    }
    tool_success(&msg)
}
