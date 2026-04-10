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

// ── Consolidated cell metadata tools ────────────────────────────

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetCellTagsParams {
    /// ID of the cell.
    pub cell_id: String,
    /// Tags to add.
    #[serde(default)]
    pub add: Option<Vec<String>>,
    /// Tags to remove.
    #[serde(default)]
    pub remove: Option<Vec<String>>,
}

/// Add and/or remove tags on a cell in one call.
pub async fn set_cell_tags(
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

    let mut tags: Vec<String> = metadata
        .get("tags")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let to_add: Vec<String> = arg_string_array(request, "add").unwrap_or_default();
    let to_remove: Vec<String> = arg_string_array(request, "remove").unwrap_or_default();

    // Remove first, then add (so you can replace tags atomically)
    tags.retain(|t| !to_remove.contains(t));
    for tag in &to_add {
        if !tags.contains(tag) {
            tags.push(tag.clone());
        }
    }

    let tag_refs: Vec<&str> = tags.iter().map(|s| s.as_str()).collect();
    handle
        .set_cell_tags(cell_id, &tag_refs)
        .map_err(|e| McpError::internal_error(format!("Failed to set tags: {e}"), None))?;

    tool_success(&format!("Tags for {cell_id}: {tags:?}"))
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SetCellVisibilityParams {
    /// IDs of cells to update.
    pub cell_ids: Vec<String>,
    /// Hide source code (input).
    #[serde(default)]
    pub source_hidden: Option<bool>,
    /// Hide outputs.
    #[serde(default)]
    pub outputs_hidden: Option<bool>,
}

/// Set visibility of source and/or outputs on one or more cells.
pub async fn set_cell_visibility(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let handle = require_handle!(server);

    let cell_ids: Vec<String> = arg_string_array(request, "cell_ids").unwrap_or_default();
    let source_hidden = arg_bool(request, "source_hidden");
    let outputs_hidden = arg_bool(request, "outputs_hidden");

    if source_hidden.is_none() && outputs_hidden.is_none() {
        return tool_error("Provide source_hidden and/or outputs_hidden.");
    }

    let mut not_found = Vec::new();
    let mut updated = 0;

    for cell_id in &cell_ids {
        let mut found = false;
        if let Some(hidden) = source_hidden {
            match handle.set_cell_source_hidden(cell_id, hidden) {
                Ok(true) => found = true,
                Ok(false) => {}
                Err(_) => {}
            }
        }
        if let Some(hidden) = outputs_hidden {
            match handle.set_cell_outputs_hidden(cell_id, hidden) {
                Ok(true) => found = true,
                Ok(false) => {}
                Err(_) => {}
            }
        }
        if found {
            updated += 1;
        } else {
            not_found.push(cell_id.as_str());
        }
    }

    let mut msg = format!("Updated visibility on {updated} cell(s)");
    if let Some(h) = source_hidden {
        msg.push_str(&format!(", source_hidden={h}"));
    }
    if let Some(h) = outputs_hidden {
        msg.push_str(&format!(", outputs_hidden={h}"));
    }
    if !not_found.is_empty() {
        msg.push_str(&format!("; not found: {not_found:?}"));
    }
    tool_success(&msg)
}
