# Ephemeral clone: replace "Save As Copy" with "fork into a new untitled notebook"

## Problem

"Clone Notebook" today forces the user through a Save-As dialog that writes a new `.ipynb` to disk, then opens the file in a new window. This is awkward:

- User is interrupted by a file picker mid-task
- The cloned file is committed to a path before the user knows what they want to do with it
- There is no way to "just fork to experiment" without committing to disk

The goal is to make Clone open a new untitled/ephemeral notebook seeded from the current doc — outputs and trust cleared, fresh `env_id`, inherited cells + metadata + runtime + working directory — with no disk I/O. The user can Save-As later if they want persistence; until then the fork lives purely in memory and disappears on close.

## Scope

### In scope

- Remove `clone_notebook_to_disk` (daemon) and `clone_notebook_to_path` (Tauri).
- Replace the wire request `NotebookRequest::CloneNotebook { path }` and response `NotebookResponse::NotebookCloned { path }`.
- Add a new daemon request `NotebookRequest::CloneAsEphemeral { source_notebook_id }` and response `NotebookResponse::NotebookCloned { notebook_id, working_dir }`.
- Daemon: fork the source room's Automerge doc into a new ephemeral room with fresh UUID / env_id / trust cleared; outputs and execution counts are NOT copied.
- Tauri: new `clone_notebook_to_ephemeral` command that issues the request and opens a new window using a new `OpenMode::Attach { notebook_id, working_dir, runtime }` variant that connects via `Handshake::NotebookSync`.
- Frontend: `cloneNotebookFile` drops the Save dialog and invokes the new command directly.
- Tests: update `apps/notebook/src/lib/__tests__/notebook-file-ops.test.ts`, the `test_save_notebook_to_disk_preserves_raw_cell_attachments_from_cache` test stays (save path unchanged), new daemon-level test for `CloneAsEphemeral`.

### Out of scope

- Bulk "clone many notebooks" flows.
- Cross-daemon clones.
- Persisting the ephemeral clone automatically (user must explicitly Save-As to get a `.ipynb`).
- Preserving outputs, comms, or kernel state on the clone. Clearing is intentional — forking comms into a kernel-less room would render disconnected widgets.
- Any change to `OpenMode::Create`'s existing session-restore behavior. The clone piggybacks on it.

## Architecture

### Lifecycle

```
User selects Clone Notebook from menu
  ↓
Frontend invokes clone_notebook_to_ephemeral Tauri command
  ↓
Tauri command sends NotebookRequest::CloneAsEphemeral { source_notebook_id }
  ↓
Daemon:
  1. Look up source room by notebook_id
  2. Derive working_dir: source_path.parent() ?? room.identity.working_dir
  3. Create new ephemeral room (new UUID, no persist file)
  4. Seed the new room's doc with forked cells + metadata
     - Cells: all cells from source, cloned by value (source text, cell_type, position, metadata, attachments)
     - Metadata: all of source's metadata snapshot, with:
       - Fresh env_id (UUID)
       - trust_signature / trust_timestamp cleared (new machine-local trust flow)
       - runt.schema_version preserved
  5. Return NotebookResponse::NotebookCloned { notebook_id, working_dir }
  ↓
Tauri command receives the response
  ↓
Tauri command reads current notebook's runtime ("python" / "deno") from the
active window's daemon state
  ↓
Tauri command invokes open_notebook_window with
  OpenMode::Create {
      notebook_id: Some(new_uuid),
      working_dir: working_dir.map(PathBuf::from),
      runtime: inherited_runtime,
  }
  ↓
New window opens with OpenMode::Attach { notebook_id: new_uuid,
  working_dir, runtime }, which connects via Handshake::NotebookSync —
  the pure "attach to an existing room by UUID" handshake. No create,
  no load, no session restore. The room already exists on the daemon
  (we just made it).
  ↓
Auto-launch: when the first peer (this new window) attaches, the daemon's
existing auto-launch path resolves the kernel from the room's seeded
metadata + working_dir, producing the same env_source as the source.
```

No disk write. No Save dialog. The new window lives until the user closes it or explicitly Save-As's it (promoting it to file-backed via the existing `finalize_untitled_promotion` path).

### Why daemon-side fork, not frontend-side

The daemon already holds the authoritative source doc and can fork cells + metadata via the existing `NotebookDoc::fork()` API. Routing the fork through the frontend would require:
- Marshaling the whole cell list + metadata through the Tauri command
- Ensuring the frontend's WASM peer is synced with the daemon before forking
- Duplicating the fork logic between WASM (frontend) and daemon

Keeping it in the daemon means:
- One code path, reusing `NotebookDoc::fork()` and the existing ephemeral-room creation plumbing
- MCP server and Python bindings can clone the same way
- No frontend-daemon sync race

### Why `Handshake::NotebookSync`, not `CreateNotebook`

There are three existing handshakes:
- `CreateNotebook` — create a brand-new room
- `OpenNotebook { path }` — load an .ipynb from disk into a room
- `NotebookSync { notebook_id }` — attach as a peer to a room that already exists

The clone flow already creates the room daemon-side (in the `CloneAsEphemeral` handler), so by the time the new window connects it just needs to sync. `NotebookSync` is the precise fit — no create, no load, just attach. `connect_relay` in `crates/notebook-sync/src/connect.rs` already issues this handshake; we reuse it.

Alternatives rejected:
- Extend `CreateNotebook` with a `seed_from: Option<String>` — bloats the handshake and entangles two operations (create-new vs fork-existing).
- Route through `CreateNotebook` with `notebook_id` hint as "pseudo session restore" — semantically wrong. Session restore is for rooms persisted across daemon restarts; our room is freshly minted in memory.

## Components

### Protocol (`crates/notebook-protocol/src/protocol.rs`)

**Remove:**
```rust
NotebookRequest::CloneNotebook { path: String }
NotebookResponse::NotebookCloned { path: String }  // replaced, not removed
```

**Add:**
```rust
NotebookRequest::CloneAsEphemeral {
    /// Source notebook to fork. Must refer to a room currently loaded in the
    /// daemon (file-backed or untitled). Ephemeral rooms can be cloned too.
    source_notebook_id: String,
}

NotebookResponse::NotebookCloned {
    /// UUID of the newly-created ephemeral room.
    notebook_id: String,
    /// Effective working directory the new window should use for project-file
    /// resolution. Derived from source_path.parent() or source room's
    /// working_dir. None only if both are missing (rare).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    working_dir: Option<String>,
}
```

Wire compatibility: this is a breaking change to the request/response shape, but `CloneNotebook` has exactly one caller (the current Tauri command) which we're deleting in the same PR, and `NotebookCloned` is only handled in that one spot. Safe to change in one atomic commit.

### Daemon (`crates/runtimed/src/`)

**Delete:**
- `clone_notebook_to_disk` in `notebook_sync_server/persist.rs` (~120 lines, plus the now-unused raw-attachments computation specific to clone)
- The old `requests/clone_notebook.rs` handler body

**Add/modify:**
- `requests/clone_notebook.rs`: new handler for `NotebookRequest::CloneAsEphemeral`. Orchestrates the fork:
  1. `daemon.rooms.get(&source_notebook_id)` — source room
  2. Compute `working_dir` as above
  3. Generate a fresh UUID for the clone
  4. Create a new ephemeral room using the existing ephemeral-creation path (likely `NotebookRoom::new_fresh` or similar — need to identify the canonical entrypoint during implementation)
  5. Fork the source doc via `NotebookDoc::fork()`, apply the resets (fresh env_id, cleared trust signatures), drop outputs, merge into the new room's doc
  6. Return the new UUID + working_dir
- Need to confirm during implementation: the ephemeral room must exist in the daemon's room map before the new window connects. If the connecting window races ahead of the room registration, the handshake will fail. Use the same atomicity patterns as existing `CreateNotebook`.

Refactor note: I expect a small helper `fn clear_runtime_state_for_clone(snapshot: &mut NotebookMetadataSnapshot)` that encapsulates the "fresh env_id, cleared trust" reset. Reusing the same logic from the (deleted) `clone_notebook_to_disk` for the new path.

### Tauri glue (`crates/notebook/src/lib.rs`)

**Delete:**
- `clone_notebook_to_path` command
- Its registration in the `invoke_handler!` list

**Add:**
- `clone_notebook_to_ephemeral` command. Signature:
  ```rust
  #[tauri::command]
  async fn clone_notebook_to_ephemeral(
      window: tauri::Window,
      app: tauri::AppHandle,
      registry: tauri::State<'_, WindowNotebookRegistry>,
  ) -> Result<String, String>  // returns new notebook_id for frontend to track
  ```
  Flow:
  1. Resolve the current window's sync handle + notebook_id
  2. Send `NotebookRequest::CloneAsEphemeral { source_notebook_id }`
  3. Receive `NotebookCloned { notebook_id, working_dir }`
  4. Read the source window's runtime from its `WindowNotebookContext.runtime` field in the registry
  5. Call `open_notebook_window(&app, registry, OpenMode::Attach { notebook_id: new_uuid, working_dir, runtime }, None)` — new OpenMode variant added in this PR. `initialize_notebook_sync_attach` is a thin wrapper around `notebook_sync::connect::connect_relay`.
  6. Return the new notebook_id
- Menu wiring stays: `MENU_CLONE_NOTEBOOK` handler invokes this command

### Frontend (`apps/notebook/src/lib/notebook-file-ops.ts`)

**Simplify `cloneNotebookFile`:**
```typescript
export async function cloneNotebookFile(host: NotebookHost): Promise<void> {
  try {
    await invoke("clone_notebook_to_ephemeral");
  } catch (e) {
    logger.error("[notebook-file-ops] Clone failed:", e);
  }
}
```
No more `get_default_save_directory`, no `saveFile` dialog, no `open_notebook_in_new_window` — the Tauri command handles window opening.

**Update tests** (`apps/notebook/src/lib/__tests__/notebook-file-ops.test.ts`):
- Drop the `saveFile` + `get_default_save_directory` expectations
- Assert `invoke` is called exactly once with `"clone_notebook_to_ephemeral"`
- Keep the error-path test

## Data flow

```
  Source room                   Daemon                     New ephemeral room
  -----------                   ------                     ------------------
  notebook_id: A                                           notebook_id: B (fresh)
  path: /foo.ipynb              ← CloneAsEphemeral(A)     is_ephemeral: true
  working_dir: None                                        working_dir: Some("/foo")
    │                             1. Lookup A
    │                             2. Fork A.doc            doc: fork of A.doc, minus
    │                             3. Reset env_id/trust        outputs + execution_count,
    │                             4. Drop outputs              fresh env_id,
    │                             5. Register B               trust cleared
    │                           → NotebookCloned(B, "/foo")
    │
    └─────── unchanged ─────────┘                                 ↓
                                                          Frontend opens window
                                                          with OpenMode::Create(B, ...)
                                                          ↓
                                                          Handshake attaches to B
                                                          ↓
                                                          Auto-launch resolves env
                                                          from inherited working_dir
```

## Error handling

- **Source notebook_id not found** → `NotebookResponse::Error { error: "Source room not found: <id>" }`. Frontend logs and shows a toast.
- **Fork failure (doc corrupted, etc.)** → `NotebookResponse::Error { error: "..." }`. Rare; indicates a daemon-side invariant violation.
- **Window creation failure** → Tauri command returns an error to the frontend. The ephemeral room will be evicted by the normal no-peers timeout since no window ever connects.
- **Race: source room evicted between send and handler** → source room not found. Same as above.

No partial-success states. Either the clone produces a new addressable room, or the frontend sees a clean error.

## Tests

### Daemon unit test (`crates/runtimed/src/notebook_sync_server/tests.rs`)

`test_clone_as_ephemeral_forks_cells_and_clears_outputs`:
- Create a source room with 2 cells (one code with outputs, one markdown with attachments)
- Stamp trust_signature + trust_timestamp on the source metadata
- Dispatch `NotebookRequest::CloneAsEphemeral` via the request router
- Assert response is `NotebookCloned { notebook_id: <new>, working_dir: <source_dir> }`
- Look up the new room, assert:
  - UUID differs from source
  - `is_ephemeral` is true
  - Cells count matches source
  - Markdown attachments preserved
  - Code-cell outputs are empty
  - Execution counts are None
  - `env_id` differs from source
  - `trust_signature` and `trust_timestamp` are None

`test_clone_as_ephemeral_rejects_unknown_source`:
- Dispatch `CloneAsEphemeral` with a bogus UUID
- Assert an `Error` response with a message mentioning "not found"

### Frontend unit test (`apps/notebook/src/lib/__tests__/notebook-file-ops.test.ts`)

`cloneNotebookFile invokes clone_notebook_to_ephemeral`:
- Assert `invoke("clone_notebook_to_ephemeral")` is called once
- Assert no `saveFile` dialog is invoked
- Assert no `get_default_save_directory` or `open_notebook_in_new_window` is invoked

`cloneNotebookFile logs error on failure`:
- Have `invoke` reject
- Assert the error is caught and logged

### Manual QA checklist

- [ ] Clone a file-backed notebook. New window opens, no Save dialog. Cells and metadata match. Outputs are cleared.
- [ ] Run a cell in the clone. env_source matches the source (same project file resolution).
- [ ] Save-As the clone. File is written to disk; the source is untouched.
- [ ] Clone an untitled notebook. New window opens with the same cells. Both windows remain independent.
- [ ] Clone a notebook in a directory with pyproject.toml. Clone's kernel auto-launch uses the same pyproject.
- [ ] Close the clone window without saving. No file written anywhere. No .automerge persist file left behind.

## Migration and rollout

- Single PR, all-or-nothing. The wire change is atomic; there are no older clients in the wild that depend on `CloneNotebook`.
- No user-facing migration. Menu item name stays the same; the behavior just works differently.
- No telemetry added.

## Open questions resolved during brainstorming

- **What gets copied?** Cells + metadata + runtime + working_dir. Outputs and exec counts cleared. Fresh env_id + cleared trust.
- **Where does fork happen?** Daemon-side (authoritative source doc access, reusable for MCP).
- **Protocol shape?** New `CloneAsEphemeral` request, not a handshake extension.
- **working_dir None case?** Return `Option<String>` on the wire to match existing `Handshake::CreateNotebook` shape; in practice always Some for normal flows.
- **env_source inheritance?** Re-derived from seeded metadata + working_dir; no explicit transfer needed.
- **Kernel runtime?** Read from the source window's registry state on the Tauri side; passed explicitly into the new `OpenMode::Create`.

## Follow-ups (not in this PR)

- Codex P2 from #2189 (clone's empty-exec-counts fallback) is superseded: the code is being deleted, not fixed.
- Eventually we may want a "clone this notebook *to disk* at path X" for power users / automation. If so, it belongs as a separate `CloneToDisk` request, not mixed with the ephemeral flow.
