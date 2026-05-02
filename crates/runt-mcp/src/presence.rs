//! Presence helpers for the MCP server.
//!
//! Thin wrappers over `notebook_sync::presence` so MCP-specific call sites keep
//! their existing names while shared encoding/sending logic lives with
//! `DocHandle`.

use notebook_sync::handle::DocHandle;

/// Emit a cursor position (line, column in a cell).
pub async fn emit_cursor(
    handle: &DocHandle,
    cell_id: &str,
    line: u32,
    column: u32,
    peer_label: &str,
) {
    notebook_sync::presence::emit_cursor(handle, cell_id, line, column, Some(peer_label)).await;
}

/// Emit a cell focus (agent is working on this cell, no specific cursor).
pub async fn emit_focus(handle: &DocHandle, cell_id: &str, peer_label: &str) {
    notebook_sync::presence::emit_focus(handle, cell_id, Some(peer_label)).await;
}

/// Announce presence immediately after connecting to a notebook.
pub async fn announce(handle: &DocHandle, peer_label: &str) {
    notebook_sync::presence::announce(handle, Some(peer_label)).await;
}

/// Convert a character offset in source to (line, column), both 0-based.
pub fn offset_to_line_col(source: &str, offset: usize) -> (u32, u32) {
    notebook_sync::presence::offset_to_line_col(source, offset)
}
