//! Reserved `nteract.dx.*` comm-target namespace.
//!
//! Every `comm_open` / `comm_msg` / `comm_close` whose `target_name` starts
//! with `nteract.dx.` is filtered out of [`RuntimeStateDoc::comms`] and
//! [`NotebookBroadcast::Comm`], and dropped with a `warn!` log carrying
//! the raw target name.
//!
//! v1 has no live handlers — the blob upload path rides IOPub `display_data`
//! buffers instead (see [`output_store::preflight_ref_buffers`]). The
//! namespace stays reserved for future bidirectional subsystems
//! (push-down predicates, streaming Arrow, `dx.attach`); when those
//! ship, they grow named variants on [`DxTarget`] with dispatch handlers.

/// Reserved comm-target namespace prefix.
pub const DX_NAMESPACE_PREFIX: &str = "nteract.dx.";

/// Reserved target name for kernel-initiated blob uploads.
///
/// Not used in v1. Blob bytes ride IOPub `display_data` buffers.
pub const DX_BLOB_TARGET: &str = "nteract.dx.blob";

/// Returns true if `target_name` starts with `nteract.dx.` and has at least
/// one character after the prefix (so `"nteract.dx"` and `"nteract.dx."`
/// don't match — those stay reservable as explicit namespace-root names).
pub fn is_dx_target(target_name: &str) -> bool {
    target_name.starts_with(DX_NAMESPACE_PREFIX) && target_name.len() > DX_NAMESPACE_PREFIX.len()
}

/// Classification of a comm target in the reserved namespace.
///
/// v1 returns only [`DxTarget::Unknown`]; future subsystems grow named
/// variants (e.g. `Query`, `Stream`) with their own dispatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DxTarget {
    /// Reserved dx target with no handler in this version. Carries the raw
    /// target name for log observability.
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
