# Blob GC correctness

**Status:** Shipped (#1770)
**Date:** 2026-04-14
**Related:** #1762 (dx), [spec: ipynb save without base64 inlining](./2026-04-14-ipynb-save-blob-refs-design.md)

## Motivation

The blob-store GC (`crates/runtimed/src/daemon.rs`, ~lines 2591–2700) runs every 30 minutes, marks blobs referenced by active rooms, and sweeps everything else older than 1 hour. Two scenarios can delete a blob that is still semantically in use:

- **A. Close + reopen within the GC window.** A user opens an untitled notebook with a parquet output, closes it (room evicted from `notebook_rooms` after 30s). 1 hour later the GC finds no active reference, deletes the blob. The persisted `notebook-docs/{hash}.automerge` file still exists (24 h retention), but its refs aren't walked. Reopen between hour 1 and hour 24 → blob URLs 404 → rich output lost.
- **F. Daemon restart before any client reconnects.** Daemon restarts, `notebook_rooms` is empty, first GC pass within 30 min sees zero refs and deletes every blob older than 1 hour.

Once [`.ipynb` save switches to blob refs](./2026-04-14-ipynb-save-blob-refs-design.md), scenario A extends to saved-and-closed notebooks as well — not just untitled ones. The same GC scan that was merely an optimization becomes a correctness dependency.

## Non-goals

- Walking arbitrary `.ipynb` files anywhere on the user filesystem. The daemon doesn't own the user's filesystem; we can't discover every saved notebook.
- A persistent reference-count in the blob metadata. Too invasive for what's effectively a correctness patch on the existing mark-and-sweep loop.
- Changing the GC cadence (stays at 30 minutes).

## Changes

Three small changes to the GC loop in `crates/runtimed/src/daemon.rs`:

### 1. Refuse to sweep when `notebook_rooms` is empty

```rust
if room_arcs.is_empty() {
    info!("[runtimed] GC: 0 active rooms; skipping sweep this cycle");
} else {
    // existing mark + sweep
}
```

Zero active rooms is a classic fail-open scenario: either every notebook is genuinely closed (safe to sweep) or the daemon just started and hasn't loaded refs yet (unsafe). The two are indistinguishable at this layer, so err toward keeping data. The cost is at most one extra GC cycle of staleness on a truly idle daemon — not a problem.

### 2. Walk `notebook-docs/*.automerge` in the mark phase

The daemon owns `config.notebook_docs_dir`. Files there (emergency persist + re-open state for untitled notebooks) contain `RuntimeStateDoc` references we currently ignore.

Add a mark pass that opens each `.automerge` file, reads its `RuntimeStateDoc`, and calls the existing `collect_blob_hashes` / `collect_blob_hashes_recursive` helpers on its executions + comms. Skip files already represented by an active room (we already have their refs).

Batched the same way the in-memory walk is: up to N docs per tick, `tokio::task::yield_now()` between batches. Reading `.automerge` files is I/O-bound; if it starves the loop we drop the batch size.

### 3. Extend the blob grace period

`blob_max_age` goes from **1 hour** to **30 days**. Rationale: after Spec 2 ships, saved-and-closed notebooks rely on the blob store surviving until they're reopened. A week-long vacation shouldn't eat someone's rich outputs. Disk is cheap; data loss isn't.

Introduce a constant `BLOB_GC_GRACE_SECS` (default 30 * 24 * 3600) and, for dev flexibility, honor a `RUNTIMED_BLOB_GC_GRACE_SECS` env var override.

## Out of scope / follow-ups

- **Explicit purge CLI** (`runt daemon vacuum`) for users who want to reclaim disk aggressively. Nice to have; not this spec.
- **Walking `.ipynb` files on disk.** When a `.ipynb` is loaded into a room, its refs land in the room's `RuntimeStateDoc` and the existing mark phase sees them. Closed saved notebooks are protected by the 30-day grace period instead. If that proves insufficient in practice, a later spec can add a "recent-save refs" manifest.

## Testing

- Unit: `collect_blob_hashes` already has coverage; add a test for the `walk_persisted_automerge_docs` helper with a fixture `.automerge` file containing a known blob ref.
- Integration: write a `.automerge` file to a temp `notebook_docs_dir`, put a blob in a temp `blob_store`, run one GC cycle, assert the blob survives.
- Integration: assert GC with zero active rooms is a no-op (no deletions).
- Integration: bump the grace-period test constant to a short value (e.g., 2s) via env var, assert the sweep deletes blobs older than that when they're actually unreferenced.

## Rollout

Ship before Spec 2 (ipynb save without base64). Order matters: Spec 2 moves the "truth" of dx outputs into the blob store. Spec 1 makes the blob store robust enough to hold that truth.
