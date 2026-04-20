//! Observability for the automerge MissingOps workaround.
//!
//! The daemon carries a workaround for an upstream automerge bug
//! ([automerge/automerge#1327]) where the change collector panics with
//! `MissingOps` on documents with interleaved text splices and merges.
//! Two code paths blunt the panic but silently:
//!
//! 1. `catch_automerge_panic` (in `runtimed`) swallows the panic and asks
//!    callers to call `rebuild_from_save()`.
//! 2. `NotebookDoc::rebuild_from_save` itself has a defensive branch that
//!    _skips_ the rebuild if the round-tripped doc would have fewer cells,
//!    to avoid silent cell loss. That leaves the doc in its panic-adjacent
//!    state and only logs a warning.
//!
//! Neither path had any telemetry. These counters give `runt diagnostics`
//! visibility into how often the workaround fires, so the operator can
//! tell whether this library-bug workaround is cold or hot.
//!
//! All counters are process-global. Writes use `Ordering::Relaxed` — we
//! only need monotonicity and eventual visibility, not happens-before
//! ordering relative to other state. Local only; no network upload.
//!
//! ## Scope: per-process
//!
//! These counters live in the process that imports `notebook-doc`. The
//! `runtimed` daemon spawns one runtime-agent subprocess per notebook
//! room; that subprocess also calls `catch_automerge_panic` and has
//! its own independent counter values. `runt diagnostics` snapshots
//! only the daemon's counters over the socket, so subprocess hits
//! won't tick the numbers. The structured `warn!` emitted on every
//! increment still lands in `runtimed.log` (the daemon tees subprocess
//! stderr), and the diagnostics bundle archives that log — grep
//! `reason=panic_caught` on the bundle for a complete view.
//!
//! [automerge/automerge#1327]: https://github.com/automerge/automerge/issues/1327

use std::sync::atomic::{AtomicU64, Ordering};

/// Panics from automerge internals that `catch_automerge_panic` swallowed.
/// Bumped from `runtimed` via [`record_panic_caught`].
static PANICS_CAUGHT: AtomicU64 = AtomicU64::new(0);

/// Times `NotebookDoc::rebuild_from_save` refused to swap in the rebuilt
/// doc because cell count regressed. The original (still-panicking) doc
/// is retained; callers typically re-load from the last good `.ipynb`.
static REBUILDS_LOSSY_SKIPPED_NOTEBOOK: AtomicU64 = AtomicU64::new(0);

/// Times `RuntimeStateDoc::rebuild_from_save` failed to load the rebuilt
/// bytes. No skip-on-regression branch exists today, but load failure is
/// the equivalent silent failure mode.
static REBUILDS_FAILED_RUNTIME_STATE: AtomicU64 = AtomicU64::new(0);

/// Times `PoolDoc::rebuild_from_save` failed to load the rebuilt bytes.
/// As with RuntimeStateDoc, there is no skip-on-regression branch.
static REBUILDS_FAILED_POOL: AtomicU64 = AtomicU64::new(0);

/// Snapshot of the automerge health counters at one point in time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AutomergeHealth {
    /// Count of panics caught by `catch_automerge_panic` in `runtimed`.
    pub panics_caught: u64,
    /// Count of `NotebookDoc::rebuild_from_save` calls that refused to
    /// swap in the rebuilt doc because cell count regressed.
    pub rebuilds_lossy_skipped_notebook: u64,
    /// Count of `RuntimeStateDoc::rebuild_from_save` calls where the
    /// rebuilt bytes failed to load.
    pub rebuilds_failed_runtime_state: u64,
    /// Count of `PoolDoc::rebuild_from_save` calls where the rebuilt
    /// bytes failed to load.
    pub rebuilds_failed_pool: u64,
}

impl AutomergeHealth {
    /// Read all counters at once. Each load is `Relaxed`, so the snapshot
    /// is not strictly consistent across counters; that's acceptable for
    /// a human-readable diagnostic readout.
    pub fn snapshot() -> Self {
        Self {
            panics_caught: PANICS_CAUGHT.load(Ordering::Relaxed),
            rebuilds_lossy_skipped_notebook: REBUILDS_LOSSY_SKIPPED_NOTEBOOK
                .load(Ordering::Relaxed),
            rebuilds_failed_runtime_state: REBUILDS_FAILED_RUNTIME_STATE.load(Ordering::Relaxed),
            rebuilds_failed_pool: REBUILDS_FAILED_POOL.load(Ordering::Relaxed),
        }
    }

    /// True if every counter is zero.
    pub fn is_clean(&self) -> bool {
        self.panics_caught == 0
            && self.rebuilds_lossy_skipped_notebook == 0
            && self.rebuilds_failed_runtime_state == 0
            && self.rebuilds_failed_pool == 0
    }
}

/// Record that `catch_automerge_panic` swallowed a panic. `label` is the
/// call-site label passed to `catch_automerge_panic`; it's embedded in
/// the structured log event so filtering by operation is easy.
pub fn record_panic_caught(label: &str, message: &str) {
    PANICS_CAUGHT.fetch_add(1, Ordering::Relaxed);
    // The warn! here is intentionally structured so log aggregators can
    // filter on `target = "automerge_health"` and `reason = "panic_caught"`.
    // `runtimed` uses `tracing`; we emit via its macros so dependencies
    // that use `log` bridge through `tracing-log`.
    emit_warn_panic_caught(label, message);
}

/// Record that `NotebookDoc::rebuild_from_save` skipped the swap because
/// cell count regressed.
pub fn record_notebook_rebuild_skipped(pre_cell_count: usize, post_cell_count: usize) {
    REBUILDS_LOSSY_SKIPPED_NOTEBOOK.fetch_add(1, Ordering::Relaxed);
    emit_warn_skip(
        "skip_rebuild_cells_lost",
        "notebook",
        pre_cell_count,
        post_cell_count,
    );
}

/// Record that `RuntimeStateDoc::rebuild_from_save` failed to load the
/// rebuilt bytes.
pub fn record_runtime_state_rebuild_failed() {
    REBUILDS_FAILED_RUNTIME_STATE.fetch_add(1, Ordering::Relaxed);
    emit_warn_failed("skip_rebuild_runtime_state_lost", "runtime_state");
}

/// Record that `PoolDoc::rebuild_from_save` failed to load the rebuilt
/// bytes.
pub fn record_pool_rebuild_failed() {
    REBUILDS_FAILED_POOL.fetch_add(1, Ordering::Relaxed);
    emit_warn_failed("skip_rebuild_pool_lost", "pool");
}

// ── Logging ─────────────────────────────────────────────────────────
//
// `notebook-doc` uses the `log` crate (gated by the `persistence` feature
// so the WASM build stays logging-free). `runtimed` bridges `log` into
// `tracing` at startup, so these warn! calls surface on the daemon's
// structured log stream. WASM callers increment the counters without
// logging, which is fine — the WASM peer's counters are read by nobody.

#[cfg(feature = "persistence")]
fn emit_warn_panic_caught(label: &str, message: &str) {
    log::warn!(
        target: "automerge_health",
        "[automerge-health] reason=panic_caught label={} message={}",
        label,
        message
    );
}

#[cfg(not(feature = "persistence"))]
fn emit_warn_panic_caught(_label: &str, _message: &str) {}

#[cfg(feature = "persistence")]
fn emit_warn_skip(reason: &str, doc: &str, pre: usize, post: usize) {
    log::warn!(
        target: "automerge_health",
        "[automerge-health] reason={} doc={} pre_cells={} post_cells={}",
        reason,
        doc,
        pre,
        post
    );
}

#[cfg(not(feature = "persistence"))]
fn emit_warn_skip(_reason: &str, _doc: &str, _pre: usize, _post: usize) {}

#[cfg(feature = "persistence")]
fn emit_warn_failed(reason: &str, doc: &str) {
    log::warn!(
        target: "automerge_health",
        "[automerge-health] reason={} doc={}",
        reason,
        doc
    );
}

#[cfg(not(feature = "persistence"))]
fn emit_warn_failed(_reason: &str, _doc: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counter increments are visible via snapshot. We can't reset a
    /// static `AtomicU64` between tests cheaply, so record a delta.
    #[test]
    fn test_panic_caught_increments() {
        let before = AutomergeHealth::snapshot().panics_caught;
        record_panic_caught("test", "hello");
        let after = AutomergeHealth::snapshot().panics_caught;
        assert_eq!(after, before + 1);
    }

    #[test]
    fn test_is_clean_on_fresh_snapshot() {
        // Can't assert fresh because other tests may have incremented.
        // But semantics of is_clean are deterministic given the fields.
        let h = AutomergeHealth::default();
        assert!(h.is_clean());
        let h = AutomergeHealth {
            panics_caught: 1,
            ..Default::default()
        };
        assert!(!h.is_clean());
    }
}
