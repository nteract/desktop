//! Compatibility wrapper for runtime-agent crash recovery.
//!
//! Runtime-agent ownership now lives in per-agent manifests. This module keeps
//! the existing startup call site narrow while the implementation moved to
//! `runtime_agent_manifest`.

pub fn reap_orphaned_agents() -> usize {
    crate::runtime_agent_manifest::reap_orphaned_agents()
}
