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
- `clearOutputsLocal(cellId)` — user hit Ctrl-Enter, clear in CRDT + store
- `clearOutputsFromDaemon(cellId)` — daemon broadcast, store only
- `applyExecutionCountFromDaemon(cellId, count)` — daemon broadcast, store only

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

## Future Direction

- **Execution lifecycle states** (queued, executing, done) should be UI-only derived state, not CRDT fields. The daemon broadcasts queue changes; the frontend tracks them in React state for rendering cell status indicators.

- **The cell store should eventually become fully derived.** Every field comes from either CRDT materialization or daemon broadcasts. No component should write to it directly except through the defined local/daemon paths.