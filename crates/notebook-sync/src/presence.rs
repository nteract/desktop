//! Shared best-effort presence helpers for notebook clients.
//!
//! These helpers centralize the common pattern used by MCP, Python, and Node
//! bindings: encode a labeled presence update and send it through `DocHandle`.
//! Presence is UX-only, so all send helpers deliberately swallow errors after
//! logging at debug level.

use notebook_doc::presence::{self, CursorPosition};

use crate::handle::DocHandle;

/// Get the Automerge actor label from the handle, or `None` when unavailable.
pub fn actor_label(handle: &DocHandle) -> Option<String> {
    handle.get_actor_id().ok().filter(|s| !s.is_empty())
}

/// Announce this peer immediately after connecting to a notebook.
///
/// Without this, the peer may be invisible in collaborator UI until it performs
/// an operation that emits cursor/focus presence.
pub async fn announce(handle: &DocHandle, peer_label: Option<&str>) {
    let actor = actor_label(handle);
    let encoded = if let Some(cell_id) = handle.first_cell_id() {
        presence::encode_focus_update_labeled("local", peer_label, actor.as_deref(), &cell_id)
    } else {
        presence::encode_custom_update_labeled("local", peer_label, actor.as_deref(), &[])
    };
    send_encoded(handle, encoded, "announce").await;
}

/// Emit a cursor position in a cell.
pub async fn emit_cursor(
    handle: &DocHandle,
    cell_id: &str,
    line: u32,
    column: u32,
    peer_label: Option<&str>,
) {
    let actor = actor_label(handle);
    let encoded = presence::encode_cursor_update_labeled(
        "local",
        peer_label,
        actor.as_deref(),
        &CursorPosition {
            cell_id: cell_id.to_string(),
            line,
            column,
        },
    );
    send_encoded(handle, encoded, "emit_cursor").await;
}

/// Emit a cursor at the end of a source string.
pub async fn emit_cursor_at_end(
    handle: &DocHandle,
    cell_id: &str,
    source: &str,
    peer_label: Option<&str>,
) {
    let (line, column) = offset_to_line_col(source, source.len());
    emit_cursor(handle, cell_id, line, column, peer_label).await;
}

/// Emit cell focus without a specific cursor position.
pub async fn emit_focus(handle: &DocHandle, cell_id: &str, peer_label: Option<&str>) {
    let actor = actor_label(handle);
    let encoded =
        presence::encode_focus_update_labeled("local", peer_label, actor.as_deref(), cell_id);
    send_encoded(handle, encoded, "emit_focus").await;
}

/// Convert a character offset in source to (line, column), both 0-based.
///
/// Column is counted in Unicode scalar values after the last newline, matching
/// the existing Python and MCP behavior.
pub fn offset_to_line_col(source: &str, offset: usize) -> (u32, u32) {
    let before = &source[..offset.min(source.len())];
    let line = before.chars().filter(|&c| c == '\n').count() as u32;
    let after_last_newline = match before.rfind('\n') {
        Some(pos) => &before[pos + 1..],
        None => before,
    };
    let column = after_last_newline.chars().count() as u32;
    (line, column)
}

async fn send_encoded(
    handle: &DocHandle,
    encoded: Result<Vec<u8>, notebook_doc::presence::PresenceError>,
    context: &str,
) {
    match encoded {
        Ok(data) => {
            let _ = handle.send_presence(data).await;
        }
        Err(e) => log::debug!("{context}: failed to encode presence: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::offset_to_line_col;

    #[test]
    fn offset_to_line_col_counts_unicode_columns() {
        assert_eq!(offset_to_line_col("abc", 3), (0, 3));
        assert_eq!(offset_to_line_col("a\nbc", 4), (1, 2));
        assert_eq!(offset_to_line_col("α\nβγ", "α\nβ".len()), (1, 1));
    }
}
