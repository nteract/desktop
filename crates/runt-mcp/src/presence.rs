//! Presence helpers for the MCP server.
//!
//! Emits cursor positions and cell focus to the daemon so peers
//! (the notebook app) see where the agent is working. All presence
//! is best-effort — errors are silently ignored.

use notebook_doc::presence::{self, CursorPosition};
use notebook_sync::handle::DocHandle;

/// Emit a cursor position (line, column in a cell).
///
/// Shows a blinking cursor in the notebook app at the specified position.
/// `peer_label` is the MCP client's display name (e.g. "Claude Code").
pub async fn emit_cursor(
    handle: &DocHandle,
    cell_id: &str,
    line: u32,
    column: u32,
    peer_label: &str,
) {
    let data = presence::encode_cursor_update_labeled(
        "local",
        Some(peer_label),
        &CursorPosition {
            cell_id: cell_id.to_string(),
            line,
            column,
        },
    );
    let _ = handle.send_presence(data).await;
}

/// Emit a cell focus (agent is working on this cell, no specific cursor).
///
/// Shows a presence dot on the cell without a blinking cursor.
/// `peer_label` is the MCP client's display name (e.g. "Claude Code").
pub async fn emit_focus(handle: &DocHandle, cell_id: &str, peer_label: &str) {
    let data = presence::encode_focus_update_labeled("local", Some(peer_label), cell_id);
    let _ = handle.send_presence(data).await;
}

/// Announce presence immediately after connecting to a notebook.
///
/// Without this, the peer is invisible in the presence UI until it performs
/// an action that emits presence (e.g. editing a cell).
pub async fn announce(handle: &DocHandle, peer_label: &str) {
    let data = if let Some(cell_id) = handle.first_cell_id() {
        presence::encode_focus_update_labeled("local", Some(peer_label), &cell_id)
    } else {
        presence::encode_custom_update_labeled("local", Some(peer_label), &[])
    };
    let _ = handle.send_presence(data).await;
}

/// Convert a character offset in source to (line, column) — both 0-based.
/// Column is counted in Unicode code points (not bytes).
///
/// Matches the Python `offset_to_line_col()` implementation.
pub fn offset_to_line_col(source: &str, offset: usize) -> (u32, u32) {
    let before = &source[..offset.min(source.len())];
    let line = before.chars().filter(|&c| c == '\n').count() as u32;
    // Count characters (not bytes) after the last newline
    let after_last_newline = match before.rfind('\n') {
        Some(pos) => &before[pos + 1..],
        None => before,
    };
    let col = after_last_newline.chars().count() as u32;
    (line, col)
}
