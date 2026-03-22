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

## Common review questions

- Is this change writing to the store without a matching CRDT write?
- Is this change re-writing daemon-authored state from the frontend?
- Is the change on the local-mutation path, inbound sync path, or both?
- Does the sync rollback or retry logic preserve convergence if delivery fails?
