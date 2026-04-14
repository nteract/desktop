//! Reserved `nteract.dx.*` comm-target namespace.
//!
//! **v1 does not use a comm for blob uploads.** The upload path runs on the
//! Jupyter messaging envelope's ``buffers`` field, attached directly to the
//! kernel's ``display_data`` IOPub message — see [`preflight_ref_buffers`]
//! in `crate::output_store`. That avoids the synchronous round-trip deadlock
//! that would happen if a cell blocked on an ack while the kernel's shell
//! dispatcher (same asyncio loop) was busy executing the cell.
//!
//! The comm namespace stays reserved. Any `comm_open`, `comm_msg`, or
//! `comm_close` on a `nteract.dx.*` target:
//!
//! - is **not** persisted to [`RuntimeStateDoc::comms`]
//! - is **not** broadcast on `NotebookBroadcast::Comm`
//! - is dropped with a `warn!` log carrying the raw target name, so a future
//!   kernel that opens a reserved target we haven't implemented yet is visible
//!   in logs rather than silently leaking into widget state.
//!
//! Future interactive dx subsystems (push-down predicates, streaming Arrow,
//! `dx.attach` for chunked uploads) will grow variants on [`DxTarget`] and
//! live dispatch handlers. Those patterns run while the kernel is idle
//! (between cell executions) or on the control channel, so the v1 deadlock
//! does not apply to them.

/// Reserved comm-target namespace prefix. All targets starting with this
/// prefix are handled by dx subsystems and excluded from [`RuntimeStateDoc`]
/// persistence.
pub const DX_NAMESPACE_PREFIX: &str = "nteract.dx.";

/// Reserved target name: kernel → runtime-agent blob uploads.
///
/// **Not used in v1** — blob bytes travel as IOPub ``display_data`` buffers.
/// Reserved for a potential future bidirectional blob protocol (e.g. receiving
/// pre-signed upload instructions).
pub const DX_BLOB_TARGET: &str = "nteract.dx.blob";

/// Returns true if `target_name` is part of the reserved dx namespace.
///
/// Requires at least one character after the prefix so the literal strings
/// `"nteract.dx"` and `"nteract.dx."` do not match (reserving the ability
/// to define an explicit namespace root without accidentally matching it).
pub fn is_dx_target(target_name: &str) -> bool {
    target_name.starts_with(DX_NAMESPACE_PREFIX) && target_name.len() > DX_NAMESPACE_PREFIX.len()
}

/// Classification of a comm target within the reserved `nteract.dx.*`
/// namespace.
///
/// v1 has no live-dispatch handlers — [`DxTarget::Unknown`] is the only
/// variant returned. Future work (query / stream / attach) will grow
/// named variants that branch into dedicated handlers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DxTarget {
    /// A reserved dx target with no v1 handler. Carries the raw target name
    /// so observability logs can record *which* target came in. Always
    /// filtered out of widget state; the caller should log a `warn!`.
    Unknown(String),
}

/// Classify a `target_name`.
///
/// Returns `None` if `target_name` is not in the dx namespace.
pub fn classify_dx_target(target_name: &str) -> Option<DxTarget> {
    if !is_dx_target(target_name) {
        return None;
    }
    Some(DxTarget::Unknown(target_name.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn namespace_prefix_check() {
        assert!(is_dx_target("nteract.dx.blob"));
        assert!(is_dx_target("nteract.dx.query"));
        assert!(is_dx_target("nteract.dx.stream"));
        assert!(is_dx_target("nteract.dx.x"));
        // Literal namespace root and trailing-dot form must not match.
        assert!(!is_dx_target("nteract.dx"));
        assert!(!is_dx_target("nteract.dx."));
        // Unrelated targets never match.
        assert!(!is_dx_target("jupyter.widget"));
        assert!(!is_dx_target(""));
        assert!(!is_dx_target("dx.blob"));
    }

    #[test]
    fn classify_returns_unknown_with_raw_name() {
        assert_eq!(
            classify_dx_target("nteract.dx.blob"),
            Some(DxTarget::Unknown("nteract.dx.blob".to_string()))
        );
        assert_eq!(
            classify_dx_target("nteract.dx.query"),
            Some(DxTarget::Unknown("nteract.dx.query".to_string()))
        );
        assert_eq!(
            classify_dx_target("nteract.dx.future"),
            Some(DxTarget::Unknown("nteract.dx.future".to_string()))
        );
        assert_eq!(classify_dx_target("jupyter.widget"), None);
        assert_eq!(classify_dx_target("nteract.dx"), None);
        assert_eq!(classify_dx_target("nteract.dx."), None);
    }
}
