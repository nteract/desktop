# CRDT Ownership

## Ownership map

- Frontend-authored CRDT writes: cell source, structure, cell metadata, notebook metadata
- Daemon-authored CRDT writes: outputs and execution-side notebook state written from kernel activity
- Store-only frontend projections: daemon execution count updates, daemon output clears, runtime-state UI updates

## Rules

- Write persistent notebook state to the WASM handle first, then let materialization update the store.
- Use store-only updates only for immediate UI feedback that already matches the CRDT, or for daemon-authored projections.
- Do not write to the CRDT in response to daemon broadcasts. That re-authors the same change and can create dirty-state or sync bugs.
- Treat CodeMirror source editing as a dedicated bridge. Avoid bypassing it with ad hoc source update flows.

## Fork+Merge for Daemon Async Mutations

Any daemon code that reads from the CRDT doc, does async work (subprocess, I/O, network), then writes back **must** use `fork()` + `merge()`. Direct mutation after an async gap can silently overwrite concurrent edits.

- **Async pattern:** `fork()` before the `.await`, mutate the fork, `merge()` after. The fork must be created *before* async work starts.
- **Sync pattern:** `doc.fork_and_merge(|fork| { ... })` — handles fork/merge ordering automatically.
- **Historic save comparison:** compare against `last_save_sources`, then `fork()` at current heads and `merge()`. Avoid `fork_at(...)` in current daemon paths because of automerge/automerge#1327.

Key methods on `NotebookDoc`: `fork()`, `get_heads()`, `merge()`, `fork_and_merge(f)`.

## Common review questions

- Is this change writing to the store without a matching CRDT write?
- Is this change re-writing daemon-authored state from the frontend?
- Is the change on the local-mutation path, inbound sync path, or both?
- Does the sync rollback or retry logic preserve convergence if delivery fails?
- Does this code read doc state, await something, then write back? If so, is it using fork+merge?
