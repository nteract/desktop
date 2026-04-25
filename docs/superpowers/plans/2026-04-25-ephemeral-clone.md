# Ephemeral Clone Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the "Save As Copy" clone flow with "fork into a new untitled notebook" — daemon forks the source room into a new ephemeral room (cells + metadata + fresh env_id, outputs cleared, trust cleared), returns the new UUID, Tauri opens a new window that attaches to the room via `Handshake::NotebookSync`.

**Architecture:** Daemon-side fork using existing `get_or_create_room` + `NotebookDoc::fork`. New protocol request `CloneAsEphemeral`, new response `NotebookCloned { notebook_id, working_dir }`. New `OpenMode::Attach` + `initialize_notebook_sync_attach` that wraps `notebook_sync::connect::connect_relay`. Frontend `cloneNotebookFile` drops the Save dialog; everything happens server-side.

**Tech Stack:** Rust (daemon, Tauri), TypeScript (frontend + Vitest), Automerge (`NotebookDoc::fork`), serde_json (typed wire protocol).

**Spec:** `docs/superpowers/specs/2026-04-25-ephemeral-clone-design.md`

---

## Task 1: Wire protocol — add CloneAsEphemeral, rename NotebookCloned

**Files:**
- Modify: `crates/notebook-protocol/src/protocol.rs:388-394` (remove `CloneNotebook`)
- Modify: `crates/notebook-protocol/src/protocol.rs:499-503` (rewrite `NotebookCloned`)
- Modify: `crates/notebook-protocol/src/protocol.rs:310-394` (add `CloneAsEphemeral` request variant)

- [ ] **Step 1: Delete the old `CloneNotebook` request variant**

Open `crates/notebook-protocol/src/protocol.rs` and delete lines 388-394 (the `/// Clone the notebook to a new path ...` doc comment plus the `CloneNotebook { path: String }` variant).

- [ ] **Step 2: Add the new `CloneAsEphemeral` request variant**

Insert, in the same location where `CloneNotebook` was, within the `pub enum NotebookRequest` definition (keep enum variants in a sensible order — wherever makes the surrounding diff cleanest; immediately after `SaveNotebook` is fine):

```rust
    /// Fork the current notebook into a new ephemeral (in-memory only) room.
    ///
    /// Creates a new UUID, copies cells + metadata from the source room,
    /// resets env_id, clears outputs and trust. The new room exists only
    /// on the daemon until a peer connects to it via `Handshake::NotebookSync`
    /// and optionally promotes it to file-backed via Save-As.
    ///
    /// Outputs and execution counts are NOT copied — forking widget state
    /// into a kernel-less room would render disconnected live comms.
    CloneAsEphemeral {
        /// Source notebook UUID. Must refer to a room currently loaded in
        /// the daemon (file-backed, untitled, or ephemeral).
        source_notebook_id: String,
    },
```

- [ ] **Step 3: Rewrite the `NotebookCloned` response**

Replace lines 499-503 (the old response variant) with:

```rust
    /// Notebook forked into a new ephemeral room.
    NotebookCloned {
        /// UUID of the newly-created ephemeral room.
        notebook_id: String,
        /// Effective working directory the cloned room inherits from its
        /// source: the source's .ipynb parent if file-backed, or the
        /// source room's explicit working_dir for untitled sources.
        /// Passed through to new-window creation for project-file resolution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
    },
```

- [ ] **Step 4: Build the protocol crate**

Run: `cargo check -p notebook-protocol`
Expected: PASS (may emit warnings about dead code in test modules; those are fine).

- [ ] **Step 5: Commit**

```bash
git add crates/notebook-protocol/src/protocol.rs
git commit -m "feat(protocol): replace CloneNotebook with CloneAsEphemeral request"
```

---

## Task 2: Daemon — rewrite the clone request handler

**Files:**
- Delete: `crates/runtimed/src/notebook_sync_server/persist.rs:390-510` (the whole `clone_notebook_to_disk` function including its resolved-outputs and raw-attachments post-processing)
- Rewrite: `crates/runtimed/src/requests/clone_notebook.rs`
- Modify: `crates/runtimed/src/notebook_sync_server/metadata.rs:3393-3395` (update request dispatcher)

- [ ] **Step 1: Read the existing clone handler and surrounding context**

Run: `grep -n "clone_notebook_to_disk\|pub(crate) use" crates/runtimed/src/notebook_sync_server/persist.rs | head`

Confirm `clone_notebook_to_disk` lives only in `persist.rs` and is exported via the module `pub(crate) use persist::*;` re-export in `notebook_sync_server/mod.rs`. Deleting the function removes it from the re-export automatically.

- [ ] **Step 2: Delete `clone_notebook_to_disk`**

In `crates/runtimed/src/notebook_sync_server/persist.rs`, delete the function `clone_notebook_to_disk` and its doc comment. It's the block that starts with:

```rust
/// Clone the notebook to a new path with a fresh env_id and cleared outputs.
///
/// This is used for "Save As Copy" functionality - creates a new independent notebook
/// ...
pub(crate) async fn clone_notebook_to_disk(
    room: &NotebookRoom,
    target_path: &str,
) -> Result<String, String> {
```

and ends with the function's closing `}`. Currently ~120 lines (persist.rs:390-510 approximately; verify the exact range with your editor).

- [ ] **Step 3: Verify the daemon still compiles without the function**

Run: `cargo check -p runtimed`
Expected: compile errors in two places — `crates/runtimed/src/requests/clone_notebook.rs` (uses `clone_notebook_to_disk`) and `crates/runtimed/src/notebook_sync_server/metadata.rs` (dispatches `CloneNotebook`).

This is expected. We'll fix them in the next steps.

- [ ] **Step 4: Write the new clone request handler**

Overwrite `crates/runtimed/src/requests/clone_notebook.rs` with:

```rust
//! `NotebookRequest::CloneAsEphemeral` handler.
//!
//! Forks a source notebook into a new ephemeral room. The new room has:
//! - Fresh UUID, fresh env_id
//! - All cells, metadata, and markdown attachments from the source
//! - No outputs, execution_count = null on every code cell
//! - trust_signature / trust_timestamp cleared (new notebook, new machine)
//!
//! The room is registered in `daemon.notebook_rooms` before this function
//! returns. A peer can then attach via `Handshake::NotebookSync`.

use std::path::PathBuf;
use std::sync::Arc;

use uuid::Uuid;

use crate::daemon::Daemon;
use crate::notebook_sync_server::{get_or_create_room, NotebookRoom};
use crate::protocol::NotebookResponse;

pub(crate) async fn handle(
    daemon: &Arc<Daemon>,
    source_notebook_id: String,
) -> NotebookResponse {
    // 1. Look up source room.
    let source_uuid = match Uuid::parse_str(&source_notebook_id) {
        Ok(u) => u,
        Err(_) => {
            return NotebookResponse::Error {
                error: format!("Invalid source_notebook_id: {source_notebook_id}"),
            };
        }
    };
    let source_room = {
        let rooms = daemon.notebook_rooms.lock().await;
        match rooms.get(&source_uuid).cloned() {
            Some(r) => r,
            None => {
                return NotebookResponse::Error {
                    error: format!("Source notebook not found: {source_notebook_id}"),
                };
            }
        }
    };

    // 2. Derive working_dir: source_path.parent() ?? source.working_dir.
    let working_dir_path = derive_working_dir(&source_room).await;

    // 3. Mint a fresh UUID for the clone.
    let clone_uuid = Uuid::new_v4();

    // 4. Create the new ephemeral room (empty).
    let clone_room = get_or_create_room(
        &daemon.notebook_rooms,
        &daemon.path_index,
        clone_uuid,
        None, // ephemeral, no file path
        &daemon.config.notebook_docs_dir,
        daemon.blob_store.clone(),
        true, // ephemeral
    )
    .await;

    // 5. Seed the room's working_dir so project-file resolution finds the
    //    same pyproject.toml / environment.yml / pixi.toml the source uses.
    if let Some(ref wd) = working_dir_path {
        *clone_room.identity.working_dir.write().await = Some(wd.clone());
    }

    // 6. Fork cells + metadata + attachments.
    if let Err(e) = seed_clone_from_source(&source_room, &clone_room).await {
        // On seed failure, evict the partially-initialized room so we
        // don't leak an empty ephemeral.
        daemon.notebook_rooms.lock().await.remove(&clone_uuid);
        return NotebookResponse::Error {
            error: format!("Failed to seed cloned notebook: {e}"),
        };
    }

    NotebookResponse::NotebookCloned {
        notebook_id: clone_uuid.to_string(),
        working_dir: working_dir_path.map(|p| p.to_string_lossy().into_owned()),
    }
}

/// Effective working directory for a room: the parent of its .ipynb
/// if file-backed, or the explicit working_dir stored on the room for
/// untitled rooms. None only if both are absent.
async fn derive_working_dir(room: &NotebookRoom) -> Option<PathBuf> {
    if let Some(path) = room.identity.path.read().await.as_ref() {
        if let Some(parent) = path.parent() {
            return Some(parent.to_path_buf());
        }
    }
    room.identity.working_dir.read().await.clone()
}

/// Seed the clone room's Automerge doc from the source, then copy markdown
/// attachments. Called once, immediately after room creation; no other peer
/// can observe the room between `get_or_create_room` and this call.
async fn seed_clone_from_source(
    source: &NotebookRoom,
    clone: &Arc<NotebookRoom>,
) -> Result<(), String> {
    // Snapshot source state in a single lock scope to avoid tearing.
    let (cells, metadata_snapshot) = {
        let doc = source.doc.read().await;
        (doc.get_cells(), doc.get_metadata_snapshot())
    };
    let attachments = source.nbformat_attachments_snapshot().await;

    // Seed the clone's doc.
    {
        let mut clone_doc = clone.doc.write().await;

        for cell in &cells {
            // `add_cell_full` takes execution_count as the JSON-encoded
            // string stored on the Automerge doc. Source/markdown cells
            // naturally carry "null"; for code cells we force "null" here
            // to clear any stale count the source had.
            let encoded_exec_count = if cell.cell_type == "code" {
                "null".to_string()
            } else {
                cell.execution_count.clone()
            };
            clone_doc
                .add_cell_full(
                    &cell.id,
                    &cell.cell_type,
                    &cell.position,
                    &cell.source,
                    &encoded_exec_count,
                    &cell.metadata,
                )
                .map_err(|e| format!("add_cell_full({}): {e}", cell.id))?;
        }

        // Apply metadata with fresh env_id + cleared trust.
        if let Some(mut snapshot) = metadata_snapshot {
            snapshot.runt.env_id = Some(Uuid::new_v4().to_string());
            snapshot.runt.trust_signature = None;
            snapshot.runt.trust_timestamp = None;
            clone_doc
                .set_metadata_snapshot(&snapshot)
                .map_err(|e| format!("set_metadata_snapshot: {e}"))?;
        }

        // Ephemeral marker lives in raw metadata (set by new_fresh already),
        // no action here.
    }

    // Copy the markdown-attachment cache. Raw-cell attachments are included
    // too since nbformat_attachments doesn't discriminate by cell_type; the
    // save path re-injects them for raw cells via the existing
    // nbformat_convert wrapper.
    if !attachments.is_empty() {
        let mut cache = clone.persistence.nbformat_attachments.write().await;
        *cache = attachments;
    }

    Ok(())
}
```

- [ ] **Step 5: Wire the new handler into the request dispatcher**

In `crates/runtimed/src/notebook_sync_server/metadata.rs`, replace the existing `CloneNotebook` dispatch block (around line 3393):

```rust
        NotebookRequest::CloneNotebook { path } => {
            crate::requests::clone_notebook::handle(room, path).await
        }
```

with:

```rust
        NotebookRequest::CloneAsEphemeral { source_notebook_id } => {
            crate::requests::clone_notebook::handle(&daemon, source_notebook_id).await
        }
```

Note: the dispatcher already has `daemon: std::sync::Arc<crate::daemon::Daemon>` in scope (see the fn signature of `handle_notebook_request`). The new handler takes `&daemon` — passing the Arc by reference avoids a clone.

- [ ] **Step 6: Build runtimed**

Run: `cargo check -p runtimed`
Expected: compiles cleanly. If there are errors about unused imports in `persist.rs` (e.g. helpers only used by the deleted `clone_notebook_to_disk`), remove the now-unused imports.

- [ ] **Step 7: Commit**

```bash
git add crates/notebook-protocol/src/protocol.rs \
        crates/runtimed/src/requests/clone_notebook.rs \
        crates/runtimed/src/notebook_sync_server/persist.rs \
        crates/runtimed/src/notebook_sync_server/metadata.rs
git commit -m "feat(runtimed): CloneAsEphemeral handler forks source into ephemeral room"
```

---

## Task 3: Daemon — test the clone handler forks cells and clears outputs

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server/tests.rs` (append two new `#[tokio::test]` functions)

- [ ] **Step 1: Find the test-helper function used by existing clone-adjacent tests**

Run: `grep -n "fn test_room_with_path\|fn test_daemon\|fn test_blob_store" crates/runtimed/src/notebook_sync_server/tests.rs | head`

Note the helper signatures you'll need — you'll see `test_room_with_path(&tmp, "name.ipynb")` in the existing save tests and a `test_daemon()` helper for tests that need the daemon.

- [ ] **Step 2: Write the happy-path test**

Append to `crates/runtimed/src/notebook_sync_server/tests.rs` (at the bottom, inside the existing `mod tests { ... }` if there is one, or alongside the other `#[tokio::test]` functions):

```rust
#[tokio::test]
async fn test_clone_as_ephemeral_forks_cells_and_clears_outputs() {
    use crate::requests::clone_notebook;

    let tmp = tempfile::TempDir::new().unwrap();
    let daemon = test_daemon(&tmp).await;

    // Source room: file-backed, with a code cell (exec_count=3, has outputs)
    // and a markdown cell with attachments.
    let source_path = tmp.path().join("source.ipynb");
    let source_uuid = uuid::Uuid::new_v4();
    let source_room = crate::notebook_sync_server::get_or_create_room(
        &daemon.notebook_rooms,
        &daemon.path_index,
        source_uuid,
        Some(source_path.clone()),
        &daemon.config.notebook_docs_dir,
        daemon.blob_store.clone(),
        false,
    )
    .await;

    // Seed source doc: two cells + metadata + cache attachments.
    {
        let mut doc = source_room.doc.write().await;
        doc.add_cell(0, "code-1", "code").unwrap();
        doc.update_source("code-1", "x = 1").unwrap();
        doc.add_cell(1, "md-1", "markdown").unwrap();
        doc.update_source("md-1", "# hello").unwrap();
    }
    {
        // Seed markdown attachments.
        let mut cache = source_room.persistence.nbformat_attachments.write().await;
        cache.insert(
            "md-1".to_string(),
            serde_json::json!({"image.png": {"image/png": "base64data"}}),
        );
    }

    // Stamp a trust signature on the source metadata (must clear on clone).
    {
        let mut doc = source_room.doc.write().await;
        if let Some(mut snap) = doc.get_metadata_snapshot() {
            snap.runt.env_id = Some("source-env-id".to_string());
            snap.runt.trust_signature = Some("hmac-sha256:deadbeef".to_string());
            snap.runt.trust_timestamp = Some("2026-04-25T00:00:00Z".to_string());
            doc.set_metadata_snapshot(&snap).unwrap();
        }
    }

    // Dispatch the clone request.
    let response = clone_notebook::handle(&daemon, source_uuid.to_string()).await;

    // Assert response shape.
    let (clone_id, clone_working_dir) = match response {
        crate::protocol::NotebookResponse::NotebookCloned {
            notebook_id,
            working_dir,
        } => (notebook_id, working_dir),
        other => panic!("Expected NotebookCloned, got {other:?}"),
    };

    // Working dir inherited from source's parent.
    assert_eq!(
        clone_working_dir.as_deref(),
        Some(tmp.path().to_string_lossy().as_ref())
    );

    // UUID differs.
    let clone_uuid = uuid::Uuid::parse_str(&clone_id).unwrap();
    assert_ne!(clone_uuid, source_uuid);

    // Look up the new room in the daemon.
    let clone_room = daemon
        .notebook_rooms
        .lock()
        .await
        .get(&clone_uuid)
        .cloned()
        .expect("clone room should be registered in daemon");

    // Ephemeral.
    assert!(clone_room.identity.is_ephemeral.load(std::sync::atomic::Ordering::Acquire));

    // working_dir seeded on the room.
    assert_eq!(
        clone_room.identity.working_dir.read().await.as_deref(),
        Some(tmp.path())
    );

    // Doc content: same cells.
    let clone_cells = clone_room.doc.read().await.get_cells();
    assert_eq!(clone_cells.len(), 2);
    let cell_ids: Vec<&str> = clone_cells.iter().map(|c| c.id.as_str()).collect();
    assert!(cell_ids.contains(&"code-1"));
    assert!(cell_ids.contains(&"md-1"));

    // Execution count on code cell reset to null.
    let code_cell = clone_cells.iter().find(|c| c.id == "code-1").unwrap();
    assert_eq!(code_cell.execution_count, "null");

    // Metadata: fresh env_id, trust cleared.
    let clone_snap = clone_room
        .doc
        .read()
        .await
        .get_metadata_snapshot()
        .expect("clone should have metadata");
    assert!(clone_snap.runt.env_id.is_some());
    assert_ne!(clone_snap.runt.env_id.as_deref(), Some("source-env-id"));
    assert!(clone_snap.runt.trust_signature.is_none());
    assert!(clone_snap.runt.trust_timestamp.is_none());

    // Markdown attachments copied.
    let clone_attachments = clone_room.nbformat_attachments_snapshot().await;
    assert_eq!(
        clone_attachments.get("md-1"),
        Some(&serde_json::json!({"image.png": {"image/png": "base64data"}}))
    );
}
```

Note: the helper `test_daemon(&tmp)` is referenced. If it doesn't exist in `tests.rs`, search for the pattern used by other tests that take a daemon — you may need to adapt the setup (look at existing tests that dispatch `handle_notebook_request` to see how they construct a daemon). If no such helper exists, the simplest path is to borrow the setup code from an existing daemon-requiring test and inline it.

- [ ] **Step 3: Write the error-path test**

Directly after the happy-path test:

```rust
#[tokio::test]
async fn test_clone_as_ephemeral_rejects_unknown_source() {
    use crate::requests::clone_notebook;

    let tmp = tempfile::TempDir::new().unwrap();
    let daemon = test_daemon(&tmp).await;

    let bogus_uuid = uuid::Uuid::new_v4().to_string();
    let response = clone_notebook::handle(&daemon, bogus_uuid.clone()).await;

    match response {
        crate::protocol::NotebookResponse::Error { error } => {
            assert!(
                error.contains("not found") || error.contains(&bogus_uuid),
                "Expected 'not found' in error, got: {error}"
            );
        }
        other => panic!("Expected Error, got {other:?}"),
    }
}
```

- [ ] **Step 4: Run the new tests**

Run: `cargo test -p runtimed --lib test_clone_as_ephemeral 2>&1 | tail -20`
Expected: both tests pass.

If compile errors mention a missing `test_daemon` helper, inline the daemon setup from a neighbouring test (e.g. `test_save_notebook_to_disk_with_outputs`) — the pattern is `let daemon = <construct a Daemon>; ...`.

- [ ] **Step 5: Run the full sync_server test suite to catch regressions**

Run: `cargo test -p runtimed --lib notebook_sync_server 2>&1 | tail -5`
Expected: all tests pass (the old `test_save_notebook_to_disk_preserves_raw_cell_attachments_from_cache` still applies — the save path is untouched).

- [ ] **Step 6: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server/tests.rs
git commit -m "test(runtimed): clone_as_ephemeral happy-path + unknown-source cases"
```

---

## Task 4: Tauri — add `OpenMode::Attach` and the attach initializer

**Files:**
- Modify: `crates/notebook/src/lib.rs:309-323` (extend `OpenMode`)
- Modify: `crates/notebook/src/lib.rs:611-760` (add `initialize_notebook_sync_attach`)
- Modify: `crates/notebook/src/lib.rs:2302-2327` (extend `placeholder_id` and `create_window_context_for_daemon` call sites for the new variant)
- Modify: `crates/notebook/src/lib.rs:2355-2388` (dispatch `OpenMode::Attach` in the spawn closure)

- [ ] **Step 1: Extend `OpenMode` with the `Attach` variant**

In `crates/notebook/src/lib.rs`, find the `enum OpenMode` definition (around line 310) and add a new variant:

```rust
/// How to connect a new window to the daemon.
enum OpenMode {
    /// Open an existing notebook file. Daemon loads from disk.
    Open { path: PathBuf },
    /// Create a new empty notebook, or restore an untitled notebook from a previous session.
    ///
    /// If `notebook_id` is provided, the daemon reuses the existing room (and its persisted
    /// Automerge doc) instead of generating a new UUID. This handles session restore for
    /// untitled notebooks that were never saved to disk.
    Create {
        runtime: String,
        working_dir: Option<PathBuf>,
        notebook_id: Option<String>,
    },
    /// Attach to a room the daemon has already created. Used by clone: after
    /// `CloneAsEphemeral` seeds a new room, the new window attaches to it by
    /// UUID via `Handshake::NotebookSync`. No create, no load, no session
    /// restore — the room is addressable and the window just syncs.
    Attach {
        notebook_id: String,
        working_dir: Option<PathBuf>,
        runtime: String,
    },
}
```

- [ ] **Step 2: Add `initialize_notebook_sync_attach`**

Add a new async fn immediately after `initialize_notebook_sync_create` (around line 760 in the current file). Base it on `initialize_notebook_sync_create`, but replace `connect_create_relay` with `connect_relay` (pure attach-by-UUID path):

```rust
/// Attach a new window to a daemon room that already exists.
///
/// Used by the clone flow: `CloneAsEphemeral` on the daemon creates the
/// ephemeral room; the new window then opens its own connection with
/// `Handshake::NotebookSync` via `connect_relay` and joins as a peer.
/// No create, no load — the room is already materialized.
async fn initialize_notebook_sync_attach(
    window: tauri::WebviewWindow,
    notebook_id: String,
    runtime: String,
    notebook_sync: SharedNotebookSync,
    sync_generation: Arc<AtomicU64>,
    notebook_id_arc: Arc<Mutex<String>>,
) -> Result<(), String> {
    let current_generation = sync_generation.fetch_add(1, Ordering::SeqCst) + 1;

    let socket_path = runt_workspace::default_socket_path();
    info!(
        "[notebook-sync] Attaching to existing room: id={}, runtime={} ({})",
        notebook_id,
        runtime,
        socket_path.display(),
    );

    let (frame_tx, raw_frame_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

    let result = notebook_sync::connect::connect_relay(
        socket_path,
        notebook_id.clone(),
        frame_tx,
    )
    .await
    .map_err(|e| format!("sync connect (attach): {}", e))?;

    let handle = result.handle;

    // Update notebook_id to match the room we just attached to.
    if let Ok(mut id) = notebook_id_arc.lock() {
        *id = notebook_id.clone();
    }

    let ready_payload = DaemonReadyPayload {
        notebook_id: notebook_id.clone(),
        // `connect_relay` does not return a NotebookConnectionInfo, so we
        // populate cell_count=0 here; the frontend will receive the true
        // count via the initial Automerge sync.
        cell_count: 0,
        needs_trust_approval: false,
        ephemeral: true,
        notebook_path: None,
        runtime: Some(runtime),
    };

    setup_sync_receivers(
        window,
        notebook_id,
        handle,
        raw_frame_rx,
        notebook_sync,
        sync_generation,
        current_generation,
        ready_payload,
    )
    .await
}
```

Note: verify the shape of `setup_sync_receivers` matches — if its signature has changed in the repo you may need to pass different arguments. If the `DaemonReadyPayload` struct has fields not shown, fill them with sensible defaults matching `initialize_notebook_sync_create`.

- [ ] **Step 3: Verify the initializer compiles**

Run: `cargo check -p notebook`
Expected: compiles. You may get a warning about an unused `ready_payload` field if the struct layout has drifted; inspect and fix.

- [ ] **Step 4: Wire `OpenMode::Attach` through `create_notebook_window_for_daemon`**

In `crates/notebook/src/lib.rs`, `create_notebook_window_for_daemon` at around line 2238, extend the title/path/working_dir/runtime destructuring to include the Attach variant:

Before (approximate current code):

```rust
    let (title, path, working_dir, runtime) = match &mode {
        OpenMode::Open { path } => {
            // ... unchanged
        }
        OpenMode::Create { runtime, working_dir, .. } => {
            // ... unchanged
        }
    };
```

Change the match to also handle Attach:

```rust
    let (title, path, working_dir, runtime) = match &mode {
        OpenMode::Open { path } => {
            let title = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("Untitled.ipynb")
                .to_string();
            let runtime = settings::load_settings().default_runtime;
            (title, Some(path.clone()), None, runtime)
        }
        OpenMode::Create {
            runtime,
            working_dir,
            ..
        } => {
            let runtime_enum: Runtime = runtime.parse().unwrap_or(Runtime::Python);
            (
                "Untitled.ipynb".to_string(),
                None,
                working_dir.clone(),
                runtime_enum,
            )
        }
        OpenMode::Attach {
            runtime,
            working_dir,
            ..
        } => {
            let runtime_enum: Runtime = runtime.parse().unwrap_or(Runtime::Python);
            (
                // Cloned notebooks are untitled until Save-As.
                "Untitled.ipynb".to_string(),
                None,
                working_dir.clone(),
                runtime_enum,
            )
        }
    };
```

- [ ] **Step 5: Extend the `placeholder_id` match and the spawn dispatcher**

In the same function, around line 2302, extend the `placeholder_id` match:

```rust
    let placeholder_id = match &mode {
        OpenMode::Open { path } => path
            .canonicalize()
            .unwrap_or_else(|_| path.clone())
            .to_string_lossy()
            .to_string(),
        OpenMode::Create {
            notebook_id: Some(ref id),
            ..
        } => id.clone(),
        OpenMode::Create {
            notebook_id: None, ..
        } => String::new(),
        OpenMode::Attach { notebook_id, .. } => notebook_id.clone(),
    };
```

And in the spawn closure around line 2357, add an arm:

```rust
    tauri::async_runtime::spawn(async move {
        let result = match mode {
            OpenMode::Open { path } => {
                initialize_notebook_sync_open(
                    window,
                    path,
                    notebook_sync,
                    sync_generation,
                    notebook_id_arc,
                )
                .await
            }
            OpenMode::Create {
                runtime,
                working_dir,
                notebook_id,
            } => {
                initialize_notebook_sync_create(
                    window,
                    runtime,
                    working_dir,
                    notebook_id,
                    notebook_sync,
                    sync_generation,
                    notebook_id_arc,
                )
                .await
            }
            OpenMode::Attach {
                notebook_id,
                runtime,
                working_dir: _,
            } => {
                initialize_notebook_sync_attach(
                    window,
                    notebook_id,
                    runtime,
                    notebook_sync,
                    sync_generation,
                    notebook_id_arc,
                )
                .await
            }
        };
        if let Err(e) = result {
            warn!("[startup] Daemon notebook sync failed: {}", e);
        }
    });
```

(The `working_dir: _` destructure is intentional — we pass working_dir through the context already, the attach handshake does not carry it over the wire.)

- [ ] **Step 6: Build the notebook crate**

Run: `cargo check -p notebook`
Expected: compiles. If there's a match-exhaustive compile error elsewhere (some other `match &mode` with two variants), add the `OpenMode::Attach` arm with the same body as the `Create` arm for the common window-setup path.

- [ ] **Step 7: Commit**

```bash
git add crates/notebook/src/lib.rs
git commit -m "feat(notebook): OpenMode::Attach + initialize_notebook_sync_attach"
```

---

## Task 5: Tauri — replace `clone_notebook_to_path` with `clone_notebook_to_ephemeral`

**Files:**
- Modify: `crates/notebook/src/lib.rs:2189-2215` (delete `clone_notebook_to_path`, add `clone_notebook_to_ephemeral`)
- Modify: `crates/notebook/src/lib.rs:4200-4210` (update `invoke_handler` list)

- [ ] **Step 1: Delete the old command**

In `crates/notebook/src/lib.rs`, delete the entire `clone_notebook_to_path` function (currently at ~line 2189 to ~line 2215). It's the block starting with:

```rust
/// Clone the current notebook for saving as a new file.
/// The daemon handles generating fresh env_id and clearing outputs/execution counts.
#[tauri::command]
async fn clone_notebook_to_path(
    path: String,
    ...
```

- [ ] **Step 2: Add the new command**

Insert, in the same location, the new Tauri command. It:
1. Looks up the current window's sync handle and notebook_id
2. Reads the current window's `runtime` from the registry context
3. Sends `NotebookRequest::CloneAsEphemeral { source_notebook_id }`
4. On success, opens a new window with `OpenMode::Attach { notebook_id, working_dir, runtime }`

```rust
/// Fork the current notebook into a new ephemeral (in-memory only) notebook
/// and open it in a new window. Daemon seeds cells + metadata; trust and
/// outputs are cleared. The user can Save-As to persist later.
#[tauri::command]
async fn clone_notebook_to_ephemeral(
    window: tauri::Window,
    app: tauri::AppHandle,
    registry: tauri::State<'_, WindowNotebookRegistry>,
) -> Result<String, String> {
    let notebook_sync = notebook_sync_for_window(&window, registry.inner())?;

    // Capture the source window's runtime from its context so the clone
    // window inherits it. (The daemon doesn't return runtime — it's a
    // client-side display/menu concern.)
    let source_runtime = {
        let contexts = registry
            .inner()
            .contexts
            .lock()
            .map_err(|e| e.to_string())?;
        let ctx = contexts
            .get(window.label())
            .ok_or_else(|| format!("No context for window '{}'", window.label()))?;
        ctx.runtime
    };

    // Resolve the source notebook_id.
    let source_notebook_id = {
        let contexts = registry
            .inner()
            .contexts
            .lock()
            .map_err(|e| e.to_string())?;
        let ctx = contexts
            .get(window.label())
            .ok_or_else(|| format!("No context for window '{}'", window.label()))?;
        let id = ctx
            .notebook_id
            .lock()
            .map_err(|e| e.to_string())?
            .clone();
        if id.is_empty() {
            return Err("Source notebook has no id yet — wait for daemon:ready".into());
        }
        id
    };

    let sync_handle = notebook_sync.lock().await.clone();
    let handle = sync_handle.ok_or("Not connected to daemon")?;

    let (clone_id, clone_working_dir) = match handle
        .send_request(NotebookRequest::CloneAsEphemeral { source_notebook_id })
        .await
    {
        Ok(NotebookResponse::NotebookCloned { notebook_id, working_dir }) => {
            (notebook_id, working_dir)
        }
        Ok(NotebookResponse::Error { error }) => {
            return Err(format!("Daemon clone failed: {error}"));
        }
        Ok(other) => return Err(format!("Unexpected daemon response: {other:?}")),
        Err(e) => return Err(format!("Daemon request failed: {e}")),
    };

    info!("[clone] Daemon forked into ephemeral room: {clone_id}");

    // Open the new window attached to the just-created ephemeral room.
    let working_dir_path = clone_working_dir.as_deref().map(PathBuf::from);
    let mode = OpenMode::Attach {
        notebook_id: clone_id.clone(),
        working_dir: working_dir_path,
        runtime: source_runtime.to_string(),
    };
    create_notebook_window_for_daemon(&app, registry.inner(), mode, None)?;

    Ok(clone_id)
}
```

Note: if `WindowNotebookContext.runtime` is a `Runtime` enum, `source_runtime.to_string()` gives the string form (e.g. `"python"`). If it's already a `String`, drop the `.to_string()`. Check the struct definition at around line 3683 in the same file.

- [ ] **Step 3: Update the `invoke_handler` list**

Find the `tauri::Builder::default().invoke_handler(tauri::generate_handler![...])` block (search for `clone_notebook_to_path` in the list — around line 4205). Replace `clone_notebook_to_path,` with `clone_notebook_to_ephemeral,`.

- [ ] **Step 4: Build**

Run: `cargo check -p notebook`
Expected: compiles cleanly. No references to `clone_notebook_to_path` remain (verify with `grep -n clone_notebook_to_path crates/notebook/src/lib.rs` — should return nothing).

- [ ] **Step 5: Commit**

```bash
git add crates/notebook/src/lib.rs
git commit -m "feat(notebook): clone_notebook_to_ephemeral Tauri command"
```

---

## Task 6: Frontend — simplify `cloneNotebookFile`

**Files:**
- Modify: `apps/notebook/src/lib/notebook-file-ops.ts:70-86`

- [ ] **Step 1: Rewrite `cloneNotebookFile`**

In `apps/notebook/src/lib/notebook-file-ops.ts`, replace the current `cloneNotebookFile` function with:

```typescript
/**
 * Fork the current notebook into a new ephemeral (in-memory) notebook and
 * open it in a new window. No file dialog — the daemon seeds a new room
 * from the current doc, the window attaches to it. User can Save-As to
 * persist later.
 *
 * The `host` parameter is retained for signature compatibility with the
 * other file ops (save/open) but is currently unused; all state lookups
 * happen server-side in the Tauri command.
 */
export async function cloneNotebookFile(_host: NotebookHost): Promise<void> {
  try {
    await invoke("clone_notebook_to_ephemeral");
  } catch (e) {
    logger.error("[notebook-file-ops] Clone failed:", e);
  }
}
```

- [ ] **Step 2: Verify type-checking passes**

Run: `cd apps/notebook && pnpm exec tsc --noEmit 2>&1 | tail -10`
Expected: no errors related to `cloneNotebookFile`. If there's an unused-parameter lint, keep the `_host` underscore prefix or adjust per the project's linter config.

- [ ] **Step 3: Commit**

```bash
git add apps/notebook/src/lib/notebook-file-ops.ts
git commit -m "feat(notebook-ui): cloneNotebookFile drops the save dialog"
```

---

## Task 7: Frontend — update `cloneNotebookFile` tests

**Files:**
- Modify: `apps/notebook/src/lib/__tests__/notebook-file-ops.test.ts:157-201`

- [ ] **Step 1: Replace the `cloneNotebookFile` test block**

In `apps/notebook/src/lib/__tests__/notebook-file-ops.test.ts`, delete the entire existing `describe("cloneNotebookFile", ...)` block (currently lines 157-201) and replace it with:

```typescript
// ---------------------------------------------------------------------------
// cloneNotebookFile
// ---------------------------------------------------------------------------

describe("cloneNotebookFile", () => {
  it("invokes clone_notebook_to_ephemeral once and opens no dialog", async () => {
    mockInvoke.mockResolvedValueOnce("new-uuid-1234");

    await cloneNotebookFile(stubHost);

    const cloneCalls = mockInvoke.mock.calls.filter(
      ([cmd]) => cmd === "clone_notebook_to_ephemeral",
    );
    expect(cloneCalls).toHaveLength(1);

    // No dialog, no save-directory lookup, no path construction.
    expect(mockSaveDialog).not.toHaveBeenCalled();
    expect(
      mockInvoke.mock.calls.filter(
        ([cmd]) => cmd === "get_default_save_directory",
      ),
    ).toHaveLength(0);
    expect(
      mockInvoke.mock.calls.filter(
        ([cmd]) => cmd === "open_notebook_in_new_window",
      ),
    ).toHaveLength(0);
    expect(
      mockInvoke.mock.calls.filter(
        ([cmd]) => cmd === "clone_notebook_to_path",
      ),
    ).toHaveLength(0);
  });

  it("does not throw on error", async () => {
    mockInvoke.mockRejectedValue(new Error("clone failed"));

    await expect(cloneNotebookFile(stubHost)).resolves.toBeUndefined();
  });
});
```

- [ ] **Step 2: Run the test file**

Run: `cd apps/notebook && pnpm exec vitest run src/lib/__tests__/notebook-file-ops.test.ts 2>&1 | tail -15`
Expected: all tests in the file pass.

- [ ] **Step 3: Commit**

```bash
git add apps/notebook/src/lib/__tests__/notebook-file-ops.test.ts
git commit -m "test(notebook-ui): cloneNotebookFile asserts dialog-free flow"
```

---

## Task 8: Sweep for stragglers + build everything

**Files:**
- Verify: no references to removed symbols remain across the repo.

- [ ] **Step 1: Confirm old symbols are fully removed**

Run these checks — each should return nothing (`$?` is 1 for grep with no matches):

```bash
grep -rn "CloneNotebook " crates/ apps/ packages/ 2>/dev/null
grep -rn "clone_notebook_to_disk" crates/ apps/ packages/ 2>/dev/null
grep -rn "clone_notebook_to_path" crates/ apps/ packages/ 2>/dev/null
grep -rn "NotebookCloned.*path:" crates/ apps/ packages/ 2>/dev/null
```

If any match surfaces, triage:
- Rust: likely a stale `use` statement or doc-comment reference; delete it.
- TypeScript: likely a stray test or a menu wire-up you missed; update it to the new command.

- [ ] **Step 2: Build the workspace**

Run: `cargo check --workspace 2>&1 | tail -20`
Expected: compiles. If there are warnings about unused imports from the removed code paths, drop the imports.

- [ ] **Step 3: Run workspace lint**

Run: `cargo xtask lint`
Expected: passes. If Rust fmt is unhappy, run `cargo xtask lint --fix`.

- [ ] **Step 4: Run all relevant test suites**

```bash
cargo test -p notebook-protocol
cargo test -p runtimed --lib notebook_sync_server
cargo test -p runtimed --lib clone
cd apps/notebook && pnpm exec vitest run src/lib/__tests__/notebook-file-ops.test.ts
```

Expected: all pass.

- [ ] **Step 5: Commit any leftover cleanup**

If the sweep surfaced any leftover, stage and commit them now:

```bash
git status --short
# If anything is modified:
git add -A
git commit -m "chore: remove stragglers from legacy clone path"
```

---

## Task 9: Manual QA and PR

**Files:**
- No code changes. Manual verification.

- [ ] **Step 1: Rebuild the daemon + frontend for dev**

With `nteract-dev` available: `up rebuild=true`. Without: `cargo xtask dev-daemon` in one terminal, `cargo xtask notebook` in another.

- [ ] **Step 2: Manual checklist (each must pass)**

- [ ] Open a file-backed notebook with outputs. Hit Clone. A new window appears with no save dialog.
- [ ] Cells and metadata in the new window match the source. Outputs are empty, execution counts are null.
- [ ] Run a cell in the clone — the env_source label in the kernel banner matches the source notebook's env_source (confirming project-file resolution inherited the working dir).
- [ ] In the clone window, do Save As. The file is written to the chosen path. The original is unchanged.
- [ ] Clone an untitled notebook (File → New, then Clone). New window opens with the same cells. Close one window; the other still works.
- [ ] Close the clone window without saving. No `.ipynb` left on disk anywhere. Check `~/.cache/runt-nightly/notebook-docs/` — no `.automerge` file for the clone UUID.
- [ ] Clone a notebook in a directory with a `pyproject.toml`. The clone's kernel launches with the same uv env.

- [ ] **Step 3: Draft the PR**

Using the PR template from CLAUDE.md (write body to `/tmp/clone-ephemeral-pr.md` first, use `--body-file`):

```bash
gh pr create --title "feat(notebook): ephemeral clone replaces Save As Copy" \
             --body-file /tmp/clone-ephemeral-pr.md
```

PR body should cover: why (spec link), what changed (5 files at a glance), behavior changes (no save dialog, new window opens immediately, outputs/trust cleared, runtime + working_dir inherited), supersedes note for PR #2189's Codex P2 on the clone exec-count bug (resolved by removal).

---

## Self-review notes

**Spec coverage:** Tasks 1-2 cover protocol + daemon handler. Task 3 covers daemon tests. Tasks 4-5 cover Tauri glue (`OpenMode::Attach`, `initialize_notebook_sync_attach`, new command). Tasks 6-7 cover frontend + tests. Task 8 sweeps for dead code. Task 9 covers manual QA + PR. Every item in the spec's "In scope" list maps to a task.

**Type consistency:**
- `source_notebook_id: String` matches between protocol and handler
- `NotebookResponse::NotebookCloned { notebook_id, working_dir }` is used verbatim in the response match, the Tauri command destructure, and the daemon handler return
- `OpenMode::Attach { notebook_id, working_dir, runtime }` matches between the enum definition, the `create_notebook_window_for_daemon` match arms, and the Tauri command constructor

**One implementation-time lookup flagged:** Task 4 Step 2's `DaemonReadyPayload` field list. If its shape has drifted from `initialize_notebook_sync_create`, copy whatever fields are current (they're all display hints — notebook_id and ephemeral are the load-bearing ones).

**One test-harness assumption flagged:** Task 3 Step 2 assumes a `test_daemon(&tmp)` helper. If no such helper exists, the test can be adapted by borrowing the daemon-construction pattern from a neighbouring test (e.g. the existing save-with-outputs test already constructs enough of a daemon).
