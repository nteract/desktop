# CRDT Mutation Guide

How data flows between the frontend, CRDT, and daemon — and who's allowed to write what.

## Core Principle

**The Automerge CRDT is the source of truth. The cell store is a read-only projection.**

All persistent notebook state lives in the Automerge document. The React cell store (`notebook-cells.ts`) is a materialized view that components subscribe to. It should never diverge from the CRDT — if it does, that's a bug.

## Who Writes What

| Data | Written by | Frontend role |
|------|-----------|---------------|
| Cell source text | **Frontend** (user typing via CodeMirror bridge) | Author — `splice_source` into WASM handle |
| Cell structure (add/delete/move) | **Frontend** (user action via `commitMutation`) | Author — `add_cell`, `delete_cell`, `move_cell` on WASM handle |
| Cell metadata (tags, visibility) | **Frontend** (user action) | Author — `set_cell_source_hidden`, `set_cell_tags`, etc. |
| Notebook metadata (dependencies, runtime) | **Frontend** or **MCP agent** | Author — `add_uv_dependency`, `set_metadata`, etc. |
| Execution count | **Daemon** (kernel reports it) | Read-only — apply from daemon broadcast |
| Cell outputs | **Daemon** (kernel produces them) | Read-only — materialize from CRDT sync |
| Output clearing (pre-execute) | **Frontend** initiates, **daemon** also writes | Author for local clear, read-only for daemon broadcast |
| Kernel status | **Daemon** | Read-only — UI state, not in CRDT |
| Queue state (executing, queued) | **Daemon** | Read-only — UI state, not in CRDT |

## The Two Paths

### Local mutations (user-initiated)

The user does something in the UI → write to the WASM CRDT handle → sync to daemon → materialization updates the store.

```
User action → WASM handle mutation → scheduleFlush() (debounced) → daemon
                                   → store update (instant feedback)
```

Examples:
- Typing in a cell (`splice_source`)
- Clearing outputs before execution (`handle.clear_outputs`)
- Adding/deleting/moving cells (`commitMutation`)
- Changing cell visibility (`set_cell_source_hidden`)

The store write for instant feedback is safe because the CRDT and store agree — materialization will write the same value when it catches up.

### Daemon projections (daemon-initiated)

The daemon writes to its CRDT (outputs, execution count, etc.) → sync frame arrives → WASM `receive_frame` → materialization reads from WASM → store updated.

```
Daemon CRDT write → sync frame → WASM receive_frame → materializeFromBatch → store
                                                     → text attributions → CM bridge
```

The daemon also sends **broadcasts** for real-time events (kernel status, execution started, queue changes). Some of these trigger store-only updates for instant UI feedback:

```
Daemon broadcast → useDaemonKernel callback → store-only update
```

**Never write to the CRDT in response to a daemon broadcast.** The daemon already wrote to the CRDT. Writing again re-authors the same change under the frontend's actor, creates redundant sync traffic, and incorrectly marks the notebook as dirty.

## Naming Convention

Functions that write to the CRDT follow this pattern:

| Name pattern | Meaning |
|-------------|---------|
| `*Local` | User-initiated. Writes to CRDT + store + triggers sync. |
| `*FromDaemon` | Daemon-initiated. Store-only projection. No CRDT write, no sync, no dirty flag. |
| `apply*FromDaemon` | Same as above — applies daemon state to the store. |

Examples:
- `applyExecutionCountFromDaemon(cellId, count)` — daemon broadcast, store only

Clear Outputs is intentionally not modeled as a frontend `*Local`/`*FromDaemon`
pair. The frontend sends a daemon request; the daemon clears visible outputs by
removing the selected cells' `execution_id` pointers in the notebook doc.

## The CodeMirror CRDT Bridge

Source text has its own dedicated path that bypasses both the store and the old `updateCellSource` function:

```
Outbound (typing):
  CM transaction → ViewPlugin.update() → iterChanges →
  handle.splice_source(cellId, index, deleteCount, text) → onSourceChanged → store

Inbound (remote sync):
  receive_frame → text attributions → subscribeBroadcast →
  bridge.applyRemoteChanges() → CM dispatch (externalChangeAnnotation)
```

The bridge uses `externalChangeAnnotation` to prevent echo: inbound changes are annotated so the outbound path skips them.

## What NOT to Do

1. **Don't write to the store without writing to the CRDT first.** The store is a projection. If you write to the store and the CRDT doesn't agree, the next materialization will overwrite your change.

2. **Don't write to the CRDT in a daemon broadcast callback.** The daemon already wrote. You'd re-author the change, mark dirty, and generate redundant sync.

3. **Don't use `updateCellById` for persistent state changes.** Use it only for instant visual feedback *after* a CRDT write, or in `*FromDaemon` store projections.

4. **Don't bypass the bridge for source text.** The CodeMirror bridge handles character-level sync. Using `update_source` (Myers diff) from the UI would conflict with the bridge's splice tracking.

5. **Don't mutate the doc directly after an async gap.** If you read doc state, await something (subprocess, network, I/O), then write back, use `fork()` + `merge()` instead. Direct mutation after an async gap overwrites concurrent edits. See below.

6. **Don't `put_object()` at a key that another peer also creates.** Two independent `put_object(ROOT, "cells", Map)` calls from different Automerge actors create two *distinct* Map objects at the same key — an Automerge conflict. One wins; the loser's children (cells, deps, etc.) become invisible. Document structure (Maps, Lists at well-known keys like `cells` and `metadata`) must be created by exactly one peer — the daemon, in `new_inner()`. All other peers receive it via Automerge sync. This is why `NotebookDoc::bootstrap()` only seeds `schema_version` (a scalar), not the full document skeleton.

## Fork+Merge for Async Mutations (Daemon-Side)

When daemon code needs to read from the CRDT, do async work, and write results back, it **must** fork the doc before the async work and merge afterward. This treats the daemon's changes as concurrent with any edits that arrived during the async gap.

```rust
// 1. Fork BEFORE async work
let fork = {
    let mut doc = room.doc.write().await;
    doc.fork()
};

// 2. Do async work
let result = expensive_subprocess().await;

// 3. Apply on fork, merge back
let mut fork = fork;
fork.update_source(&cell_id, &result).ok();
let mut doc = room.doc.write().await;
doc.merge(&mut fork).ok();
```

**Why this matters:** Without fork+merge, the async gap is a data loss window. If a user types while ruff formats, or another peer edits while the file watcher processes, the write-back silently overwrites those changes. Fork+merge lets Automerge's text CRDT compose both sets of changes.

**Do not use `fork_at(heads)` / `fork_at_and_merge(...)` in daemon mutation paths right now.** `fork_at(...)` currently triggers automerge/automerge#1327 (`MissingOps` in the change collector) on documents with interleaved text splices and merges. For file-watcher and other external-content merges, compare against the saved-on-disk baseline (`last_save_sources`), then `fork()` at current heads and `merge()`.

### Helpers for Synchronous Blocks

For mutation blocks with no `.await` between fork and merge, use the helpers to avoid ordering mistakes:

```rust
// Fork at current heads, apply mutations, merge back
doc.fork_and_merge(|fork| {
    fork.update_source("cell-1", "x = 1\n");
    fork.set_cell_resolved_assets("cell-2", &assets);
});

```

These encapsulate the fork-before/merge-after pattern. For **async** work between fork and merge, use `fork()` and `merge()` directly — the fork must be created before the `.await` and merged after.

### Adoption Status

All async CRDT mutation paths in the daemon are now protected:

| Path | Protection |
|------|-----------|
| ExecuteCell / RunAllCells formatting | `fork()` before `tokio::spawn`, merge in task |
| `format_notebook_cells` (Cmd+S) | `fork()` before format loop, merge after |
| File watcher source updates | Compare against `last_save_sources`, then `fork()` + merge |
| File watcher order-changed rebuild | Compare against `last_save_sources`, then `fork()` + merge |
| `UpdateDisplayData` IOPub | `fork()` before blob I/O, merge after |
| `process_markdown_assets` | `fork()` before async resolution, merge after |
| `handle_sync_environment` | Fresh read (no CRDT write, only in-memory state) |

## Future Direction

- **Execution lifecycle states** (queued, executing, done) should be UI-only derived state, not CRDT fields. The daemon broadcasts queue changes; the frontend tracks them in React state for rendering cell status indicators.

- **The cell store should eventually become fully derived.** Every field comes from either CRDT materialization or daemon broadcasts. No component should write to it directly except through the defined local/daemon paths.
