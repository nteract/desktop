# UUID-First Notebook Identity Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make UUID the permanent, immutable room identifier throughout the daemon, MCP tools, and wire protocol. Path becomes a mutable property on the room, looked up via a secondary `path_index`.

**Architecture:** The `rooms` HashMap is re-keyed from `HashMap<String, Arc<NotebookRoom>>` (where the key is *sometimes* a UUID and *sometimes* a canonical path) to `HashMap<Uuid, Arc<NotebookRoom>>` (always UUID). A new `path_index: HashMap<PathBuf, Uuid>` is maintained alongside for path→room lookup. Saving an untitled notebook becomes a simple path/index update rather than a 180-line re-keying dance. `rekey_ephemeral_room`, `redirect_map`, the `looks_like_path` heuristic, and `new_notebook_id` in save responses are all deleted.

**Tech Stack:** Rust (tokio, automerge-rs, uuid), PyO3 bindings, TypeScript frontend (Tauri desktop app), MCP protocol via `runt-mcp` crate.

---

## Design decisions locked in

Before any coding begins, these are the choices this plan commits to. Do not revisit them during implementation — if one turns out wrong, stop and revise the plan.

1. **Key type becomes `Uuid`** (not `String`). `HashMap<Uuid, Arc<NotebookRoom>>`. This makes the "always a UUID" invariant a compile-time guarantee instead of runtime discipline. Wire-facing IDs still serialize as strings (hyphenated UUID format).

2. **Three room states remain, distinguished differently:**
   - **ephemeral scratch** (`is_ephemeral = true`): no `.ipynb`, no `.automerge`. Dies with the daemon. (Currently rare.)
   - **untitled persisted** (`is_ephemeral = false`, `room.path = None`): has `.automerge`, survives restart. Filename = SHA256(UUID).
   - **file-backed** (`is_ephemeral = false`, `room.path = Some(path)`): `.ipynb` is source of truth; `.automerge` is transient cache. On restart, the `.automerge` file is snapshotted and deleted; the daemon loads from `.ipynb`.

   The discriminator changes from `is_untitled_notebook(notebook_id)` (parse key as UUID) to `room.path.is_none()`.

3. **Path-collision on save fails cleanly.** If a user saves an untitled notebook to a path that another open room already occupies (e.g., `open_notebook(foo.ipynb)` then save-as-foo.ipynb), the save returns an error: `NotebookSaveError::PathAlreadyOpen { uuid }`. The agent must close the conflicting session first. This deletes the 60-line interloper-merge block inside `rekey_ephemeral_room`.

4. **CRDT `notebook_id` field is always a UUID** (never a path). On save, the field does **not** change — the UUID is stable for the life of the room. File-backed notebooks re-opened after daemon restart get a fresh session UUID (the old UUID was only a session handle).

5. **Wire protocol:**
   - `Handshake::OpenNotebook { path }` is unchanged on the wire. The daemon resolves it via `path_index` and returns the session UUID in `NotebookConnectionInfo.notebook_id`.
   - `NotebookResponse::NotebookSaved` drops `new_notebook_id` entirely. Saved notebook's ID is the same UUID as before the save.
   - A new broadcast `NotebookBroadcast::PathChanged { path: Option<PathBuf> }` replaces the rekey flavor of `RoomRenamed`. `RoomRenamed` is deleted.

6. **MCP tools return UUIDs.** `open_notebook`, `create_notebook`, and `save_notebook` all return `notebook_id` as a UUID string. The `looks_like_path()` heuristic is deleted — `open_notebook(path=...)` accepts a path, `open_notebook(notebook_id=...)` accepts a UUID, no heuristic between them. A single `notebook` parameter that accepted either is replaced by the two explicit parameters.

7. **Daemon restart of file-backed notebooks:** agents call `open_notebook(path)` after restart → new session UUID. MCP subscriptions already reset on restart, so this is not a regression.

8. **Migration is zero-touch.** Untitled notebooks' `.automerge` files already key by UUID. File-backed `.automerge` files are transient cache and get snapshotted/deleted on restart as today. No schema migration, no data migration.

---

## File structure

Files this plan creates or modifies. Brief mental model included so later tasks reference them without re-introducing context.

### Created
- `crates/runtimed/src/notebook_sync_server/path_index.rs` — the `PathIndex` type and its tests. Small focused unit with a clear interface (`insert`, `remove`, `lookup`).

### Modified — daemon core
- `crates/runtimed/src/notebook_sync_server.rs` — `NotebookRoom` struct (add `id: Uuid`, change `notebook_path: RwLock<PathBuf>` to `path: RwLock<Option<PathBuf>>`), `NotebookRooms` type alias (key → `Uuid`), `get_or_create_room`, `find_room_by_notebook_path` → `find_room_by_path`, `SaveNotebook` handler, eviction loop, deletion of `rekey_ephemeral_room`.
- `crates/runtimed/src/daemon.rs` — deletion of `redirect_map` field, `RedirectEntry` struct, and its lookup sites in `handle_open_notebook` and `handle_create_notebook`. `handle_open_notebook` gains a `path → uuid` resolution via the rooms lock.

### Modified — schema and protocol
- `crates/notebook-doc/src/lib.rs` — `NotebookDoc::new_inner` signature: accepts `notebook_id: Uuid` instead of `&str`. `notebook_id()` accessor returns `Option<Uuid>` instead of `Option<String>`. TypeScript bindings regenerated (automatic via `cargo xtask build`).
- `crates/notebook-protocol/src/connection.rs` — `Handshake::CreateNotebook.notebook_id: Option<String>` → `Option<Uuid>`. `NotebookConnectionInfo.notebook_id: String` stays (wire format) but documented as always-UUID. `NotebookResponse::NotebookSaved.new_notebook_id: Option<String>` removed.
- `crates/notebook-protocol/src/protocol.rs` — `NotebookBroadcast::RoomRenamed` removed. `NotebookBroadcast::PathChanged { path: Option<PathBuf> }` added.

### Modified — MCP tools
- `crates/runt-mcp/src/tools/session.rs` — `open_notebook`, `create_notebook`, `save_notebook` response JSON. Delete `looks_like_path`. The `notebook` convenience parameter splits into `path` / `notebook_id`.

### Modified — frontend/desktop
- `apps/notebook/src/` — anywhere that reads `new_notebook_id` or handles `RoomRenamed` needs updating. Most code already treats `notebook_id` as opaque, so the surface is small.

### Modified — Python bindings
- `crates/runtimed-py/src/` — if any types reference `notebook_id` as `String`, check they still round-trip. Python API already treats it as opaque.

### Test files modified
- `crates/runtimed/src/notebook_sync_server.rs` — inline `#[cfg(test)]` tests. `test_rekey_ephemeral_room_starts_autosave` (lines ~11387–11486) gets rewritten as `test_save_untitled_notebook_updates_path_index`. New tests for `PathIndex`, `path_collision_on_save`, `path_changed_broadcast`.
- `crates/notebook-doc/src/lib.rs` — `NotebookDoc` tests updated to pass `Uuid` instead of `&str`.
- `python/runtimed/tests/test_daemon_integration.py` — tests that assert on `notebook_id` shape may need updating if any checked for a path-shaped ID.

---

## Execution phases

Nine phases. Each phase ends in a working build with tests passing — you can `commit` and walk away after any phase boundary.

| Phase | Name | Scope |
|-------|------|-------|
| 1 | Safety baseline | Confirm build + tests green on the branch |
| 2 | Add `PathIndex` | New module, tested in isolation, not yet wired |
| 3 | Add `id: Uuid` to `NotebookRoom` | Non-breaking additive field |
| 4 | Change `NotebookRoom.notebook_path` to `path: RwLock<Option<PathBuf>>` | Isomorphic change; untitled rooms go from `PathBuf::from(uuid_string)` to `None` |
| 5 | Switch `NotebookRooms` key type to `Uuid` and wire up `PathIndex` | The core surgery |
| 6 | Rewrite `SaveNotebook` handler (no more rekey) | Delete `rekey_ephemeral_room`, add path-collision error, add `PathChanged` broadcast |
| 7 | Delete `redirect_map`, `RoomRenamed`, `looks_like_path`, `new_notebook_id` | Dead-code removal once the new path stabilizes |
| 8 | MCP tool response cleanup | `open_notebook`/`save_notebook` always return UUIDs |
| 9 | End-to-end verification | Lint, integration tests, manual smoke |

---

## Phase 1: Safety baseline

### Task 1.1: Confirm branch and green build

**Files:** None — verification only.

- [ ] **Step 1: Verify branch**

Run: `git branch --show-current`
Expected: `feat/uuid-first-notebook-identity`

- [ ] **Step 2: Build clean**

Run: `cargo xtask build --rust-only`
Expected: build succeeds with no errors.

- [ ] **Step 3: Run full test suite**

Run: `cargo test -p runtimed -p notebook-doc -p notebook-protocol -p notebook-sync -p runt-mcp`
Expected: all pass. If any fail on `main`, that's a separate issue — record, then file a fix PR before continuing this plan.

- [ ] **Step 4: Run the tokio mutex lint**

Run: `cargo test -p runtimed --test tokio_mutex_lint`
Expected: pass. This plan must not regress it.

- [ ] **Step 5: Commit plan document**

```bash
git add docs/superpowers/plans/2026-04-16-uuid-first-notebook-identity.md
git commit -m "docs(plan): uuid-first notebook identity implementation plan"
```

---

## Phase 2: Add `PathIndex`

Introduce the secondary index as a self-contained, tested unit before wiring it anywhere. This keeps the risk low — Phase 2 can ship alone even if later phases are paused.

### Task 2.1: Create `PathIndex` module with failing tests

**Files:**
- Create: `crates/runtimed/src/notebook_sync_server/path_index.rs`
- Modify: `crates/runtimed/src/notebook_sync_server.rs` (add `mod path_index;` near the top, export `pub use path_index::PathIndex;`)

- [ ] **Step 1: Create the module skeleton**

Create `crates/runtimed/src/notebook_sync_server/path_index.rs` with:

```rust
//! Secondary index mapping canonical `.ipynb` paths to the UUID of the room
//! currently serving that file. Consulted by `open_notebook(path)` to reuse
//! an already-open room instead of creating a second one.
//!
//! **Invariant:** each canonical path maps to at most one UUID. `insert` that
//! would violate this returns `Err(PathIndexError::PathAlreadyOpen)` — the
//! caller decides whether to fail the request or merge (today: fail).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, Default)]
pub struct PathIndex {
    inner: HashMap<PathBuf, Uuid>,
}

#[derive(Debug, thiserror::Error)]
pub enum PathIndexError {
    #[error("path already open in room {uuid}: {path}")]
    PathAlreadyOpen { uuid: Uuid, path: PathBuf },
}

impl PathIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lookup(&self, path: &Path) -> Option<Uuid> {
        self.inner.get(path).copied()
    }

    pub fn insert(&mut self, path: PathBuf, uuid: Uuid) -> Result<(), PathIndexError> {
        match self.inner.get(&path) {
            Some(&existing) if existing == uuid => Ok(()), // idempotent
            Some(&existing) => Err(PathIndexError::PathAlreadyOpen { uuid: existing, path }),
            None => {
                self.inner.insert(path, uuid);
                Ok(())
            }
        }
    }

    pub fn remove(&mut self, path: &Path) -> Option<Uuid> {
        self.inner.remove(path)
    }

    pub fn remove_by_uuid(&mut self, uuid: Uuid) -> Option<PathBuf> {
        let path = self.inner.iter().find(|(_, &u)| u == uuid).map(|(p, _)| p.clone())?;
        self.inner.remove(&path);
        Some(path)
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}
```

- [ ] **Step 2: Add failing tests at the bottom of the new file**

Append to `path_index.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn path(s: &str) -> PathBuf { PathBuf::from(s) }

    #[test]
    fn empty_index_returns_none_on_lookup() {
        let idx = PathIndex::new();
        assert!(idx.lookup(&path("/tmp/foo.ipynb")).is_none());
        assert!(idx.is_empty());
    }

    #[test]
    fn insert_then_lookup_returns_uuid() {
        let mut idx = PathIndex::new();
        let uuid = Uuid::new_v4();
        idx.insert(path("/tmp/foo.ipynb"), uuid).unwrap();
        assert_eq!(idx.lookup(&path("/tmp/foo.ipynb")), Some(uuid));
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn insert_same_uuid_twice_is_idempotent() {
        let mut idx = PathIndex::new();
        let uuid = Uuid::new_v4();
        idx.insert(path("/tmp/foo.ipynb"), uuid).unwrap();
        idx.insert(path("/tmp/foo.ipynb"), uuid).unwrap();
        assert_eq!(idx.len(), 1);
    }

    #[test]
    fn insert_conflicting_uuid_returns_error() {
        let mut idx = PathIndex::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        idx.insert(path("/tmp/foo.ipynb"), a).unwrap();
        let err = idx.insert(path("/tmp/foo.ipynb"), b).unwrap_err();
        match err {
            PathIndexError::PathAlreadyOpen { uuid, path: p } => {
                assert_eq!(uuid, a);
                assert_eq!(p, path("/tmp/foo.ipynb"));
            }
        }
    }

    #[test]
    fn remove_returns_uuid_and_clears_entry() {
        let mut idx = PathIndex::new();
        let uuid = Uuid::new_v4();
        idx.insert(path("/tmp/foo.ipynb"), uuid).unwrap();
        assert_eq!(idx.remove(&path("/tmp/foo.ipynb")), Some(uuid));
        assert!(idx.is_empty());
        assert!(idx.lookup(&path("/tmp/foo.ipynb")).is_none());
    }

    #[test]
    fn remove_missing_returns_none() {
        let mut idx = PathIndex::new();
        assert!(idx.remove(&path("/nope")).is_none());
    }

    #[test]
    fn remove_by_uuid_clears_entry() {
        let mut idx = PathIndex::new();
        let uuid = Uuid::new_v4();
        idx.insert(path("/tmp/foo.ipynb"), uuid).unwrap();
        assert_eq!(idx.remove_by_uuid(uuid), Some(path("/tmp/foo.ipynb")));
        assert!(idx.is_empty());
    }

    #[test]
    fn different_paths_with_different_uuids_coexist() {
        let mut idx = PathIndex::new();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        idx.insert(path("/tmp/a.ipynb"), a).unwrap();
        idx.insert(path("/tmp/b.ipynb"), b).unwrap();
        assert_eq!(idx.lookup(&path("/tmp/a.ipynb")), Some(a));
        assert_eq!(idx.lookup(&path("/tmp/b.ipynb")), Some(b));
        assert_eq!(idx.len(), 2);
    }
}
```

- [ ] **Step 3: Wire module into `notebook_sync_server.rs`**

In `crates/runtimed/src/notebook_sync_server.rs`, add near the top (after existing `use` declarations):

```rust
mod path_index;
pub use path_index::{PathIndex, PathIndexError};
```

- [ ] **Step 4: Run the tests — confirm they PASS**

Run: `cargo test -p runtimed path_index::tests`
Expected: 8 tests pass.

Why the tests go green on first run: the module is self-contained — the test harness and implementation land in one shot. This is a refactor-style task, not new behavior discovery.

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server/path_index.rs \
        crates/runtimed/src/notebook_sync_server.rs
git commit -m "feat(runtimed): add PathIndex for canonical-path → uuid lookup"
```

---

## Phase 3: Add `id: Uuid` to `NotebookRoom`

Add the field without changing behavior. Every code path that creates a room now records its UUID explicitly. Rooms keyed by path in the current HashMap will have `id` set to a freshly-generated UUID that doesn't yet match the key — that's fine, the key is still the path for now; we'll switch in Phase 5.

### Task 3.1: Add the field and initialize it everywhere

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server.rs` — `NotebookRoom` struct (~line 1019), `NotebookRoom::new_fresh` (~line 1215), any other `NotebookRoom { ... }` struct-literal sites.

- [ ] **Step 1: Add failing test**

Add near the other room tests (find any existing `mod tests` inside `notebook_sync_server.rs`; if there isn't one in the relevant region, grep for `#[test]` and pick the nearest; otherwise add inline `#[cfg(test)] #[test]` at the end of the file before the final `}`).

```rust
#[tokio::test]
async fn notebook_room_has_uuid_id_populated() {
    let tmp = tempfile::tempdir().unwrap();
    let blob_store = Arc::new(BlobStore::new(tmp.path().to_path_buf()));
    // For now notebook_id is still a string; id is independent.
    let uuid = Uuid::new_v4();
    let room = NotebookRoom::new_fresh(
        &uuid.to_string(),
        tmp.path(),
        blob_store,
        false, // ephemeral
    );
    assert_eq!(room.id, uuid);
}
```

- [ ] **Step 2: Run it — confirm it fails to compile**

Run: `cargo test -p runtimed notebook_room_has_uuid_id_populated`
Expected: compile error — `NotebookRoom` has no field `id`.

- [ ] **Step 3: Add the `id` field**

In `NotebookRoom` struct (~line 1019), add at the top (before `doc`):

```rust
pub struct NotebookRoom {
    /// Permanent, immutable UUID for this room. Used as the map key once
    /// Phase 5 lands; for now coexists with the string-keyed map.
    pub id: Uuid,
    /// The canonical Automerge notebook document.
    pub doc: Arc<RwLock<NotebookDoc>>,
    // ...rest unchanged
```

- [ ] **Step 4: Update `NotebookRoom::new_fresh` to take and store a UUID**

Change `new_fresh` signature (~line 1215) from:

```rust
pub fn new_fresh(
    notebook_id: &str,
    docs_dir: &Path,
    blob_store: Arc<BlobStore>,
    ephemeral: bool,
) -> Self
```

to:

```rust
pub fn new_fresh(
    notebook_id: &str,
    docs_dir: &Path,
    blob_store: Arc<BlobStore>,
    ephemeral: bool,
) -> Self {
    // For this phase, derive `id` from `notebook_id` if it parses as a UUID,
    // else mint a fresh one. This keeps path-keyed rooms working — their
    // `id` is decoupled from their key until Phase 5.
    let id = Uuid::parse_str(notebook_id).unwrap_or_else(|_| Uuid::new_v4());
    // ...existing body...
}
```

and in the `Self { ... }` literal near the end of the function, add `id,` to the field list.

- [ ] **Step 5: Find every other `NotebookRoom { ... }` literal**

Run: `cargo build -p runtimed 2>&1 | head -100`
Expected: compile errors pointing at any struct-literal sites that haven't been updated. For each, add `id: Uuid::new_v4(),` (or derive from available UUID context if obvious).

Alternate (search ahead of the build): `Grep` for `NotebookRoom \{` in `crates/runtimed/src/`. Expect 1–3 sites beyond `new_fresh`.

- [ ] **Step 6: Run the test**

Run: `cargo test -p runtimed notebook_room_has_uuid_id_populated`
Expected: pass.

- [ ] **Step 7: Run the full runtimed test suite**

Run: `cargo test -p runtimed`
Expected: all pass, no regressions.

- [ ] **Step 8: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server.rs
git commit -m "feat(runtimed): add NotebookRoom.id UUID field (unwired)"
```

---

## Phase 4: Change `NotebookRoom.notebook_path` → `path: RwLock<Option<PathBuf>>`

Today `notebook_path: RwLock<PathBuf>` holds either the canonical `.ipynb` path (file-backed rooms) or `PathBuf::from(uuid_string)` (untitled — a pseudo-path). Rename and switch to `Option<PathBuf>` so untitled rooms are `None` — a clearer representation and prep for Phase 6.

### Task 4.1: Rename field and narrow the type

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server.rs` — field declaration, all reads and writes of `notebook_path`.

- [ ] **Step 1: Add a failing test for the new semantics**

```rust
#[tokio::test]
async fn untitled_room_has_path_none() {
    let tmp = tempfile::tempdir().unwrap();
    let blob_store = Arc::new(BlobStore::new(tmp.path().to_path_buf()));
    let room = NotebookRoom::new_fresh(
        &Uuid::new_v4().to_string(),
        tmp.path(),
        blob_store,
        false,
    );
    assert!(room.path.read().await.is_none());
}

#[tokio::test]
async fn file_backed_room_has_path_some() {
    let tmp = tempfile::tempdir().unwrap();
    let blob_store = Arc::new(BlobStore::new(tmp.path().to_path_buf()));
    let fake_path = tmp.path().join("note.ipynb");
    let room = NotebookRoom::new_fresh(
        fake_path.to_str().unwrap(),
        tmp.path(),
        blob_store,
        false,
    );
    let guard = room.path.read().await;
    assert_eq!(guard.as_deref(), Some(fake_path.as_path()));
}
```

- [ ] **Step 2: Confirm compile failure**

Run: `cargo test -p runtimed untitled_room_has_path_none`
Expected: compile error — `NotebookRoom` has no field `path`.

- [ ] **Step 3: Rename field and change its type**

In the `NotebookRoom` struct definition, replace:

```rust
/// The notebook file path (notebook_id is the path).
/// Wrapped in RwLock so it can be updated when an ephemeral (UUID-keyed) room
/// is re-keyed to a file-path room on save.
pub notebook_path: RwLock<PathBuf>,
```

with:

```rust
/// The `.ipynb` path, when this room is file-backed. `None` for untitled and
/// ephemeral rooms. Mutated when an untitled room is saved to disk (see
/// `handle_save_notebook`).
pub path: RwLock<Option<PathBuf>>,
```

- [ ] **Step 4: Update `new_fresh` initialization**

Replace the existing initializer (near line 1267):

```rust
let notebook_path = PathBuf::from(notebook_id);
```

and the struct-literal field `notebook_path: RwLock::new(notebook_path)` with logic that produces `None` for UUID-looking IDs:

```rust
let path = if is_untitled_notebook(notebook_id) {
    None
} else {
    Some(PathBuf::from(notebook_id))
};
// ... in the Self { ... } literal:
path: RwLock::new(path),
```

- [ ] **Step 5: Fix every read/write site**

Run: `cargo build -p runtimed 2>&1 | grep -E "notebook_path|error\[E" | head -80`
Expected: compile errors at every reader/writer of `notebook_path`.

Categorize each site:

| Site | New code |
|------|----------|
| `*room.notebook_path.read().await` used as `PathBuf` | `room.path.read().await.clone().unwrap_or_else(\|\| PathBuf::from("untitled"))` — but **prefer refactoring** the caller to handle `Option` |
| `room.notebook_path.try_read().map(...)` in `find_room_by_notebook_path` | Now reads `Option<PathBuf>`; match on it before comparing |
| `*room.notebook_path.write().await = new_path;` (inside `rekey_ephemeral_room`, will be deleted in Phase 6) | `*room.path.write().await = Some(new_path);` — for now, keep rekey functional |
| Trust state verification (near line 1285) that passes `notebook_path` to `verify_trust_from_file` | Guard with `if let Some(p) = &path` — untitled notebooks don't read trust from disk |
| Any `info!`/`warn!` log formatters referencing `notebook_path` | `format!("{:?}", path.as_ref())` |

Do these mechanically; the compiler is your checklist.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p runtimed`
Expected: pass. If the two new tests fail, inspect whether `is_untitled_notebook` classification matches expectations for the fake path (the `file_backed_room_has_path_some` test uses a non-UUID string, which must return false from `is_untitled_notebook`).

- [ ] **Step 7: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server.rs
git commit -m "refactor(runtimed): NotebookRoom.notebook_path -> path: Option<PathBuf>"
```

---

## Phase 5: Switch rooms map key to `Uuid` and wire `PathIndex`

This is the core surgery. The rooms map becomes `HashMap<Uuid, Arc<NotebookRoom>>`. A new `path_index: Arc<Mutex<PathIndex>>` lives alongside. All lookups that take a path route through `path_index` first. `redirect_map` stays in place for this phase — we delete it in Phase 7 after everything is stable.

### Task 5.1: Change the `NotebookRooms` type alias

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server.rs` — type alias at line 1482, every usage.

- [ ] **Step 1: Change the type alias**

At line 1482, replace:

```rust
pub type NotebookRooms = Arc<Mutex<HashMap<String, Arc<NotebookRoom>>>>;
```

with:

```rust
pub type NotebookRooms = Arc<Mutex<HashMap<uuid::Uuid, Arc<NotebookRoom>>>>;
```

- [ ] **Step 2: Confirm compile failure**

Run: `cargo build -p runtimed 2>&1 | grep -E "error\[E" | head -40`
Expected: many compile errors at every `rooms.get("some_string")`, `rooms.insert(string, ...)`, etc.

- [ ] **Step 3: Introduce a shared sibling `path_index`**

Find the `Daemon` or daemon-state struct that owns `rooms` (likely in `daemon.rs` — grep for `NotebookRooms` to find the owner). Add a sibling field:

```rust
pub(crate) path_index: Arc<Mutex<crate::notebook_sync_server::PathIndex>>,
```

Initialize at daemon construction: `path_index: Arc::new(Mutex::new(PathIndex::new())),`. Pass it into any function that also takes `rooms`.

- [ ] **Step 4: Update `find_room_by_notebook_path` (rename to `find_room_by_path`)**

Currently it scans every room comparing `notebook_path`. With `path_index` this is an O(1) lookup. Replace the function (~line 1502–1531) with:

```rust
/// Look up an open room by its canonical .ipynb path.
///
/// Returns `None` if no room is currently serving that path. Callers that
/// need to either find-or-create must do so under a held rooms lock plus
/// a path_index lock (always acquire rooms FIRST, then path_index, to
/// prevent deadlock).
pub async fn find_room_by_path(
    rooms: &NotebookRooms,
    path_index: &Arc<Mutex<PathIndex>>,
    path: &Path,
) -> Option<Arc<NotebookRoom>> {
    let uuid = {
        let idx = path_index.lock().await;
        idx.lookup(path)?
    };
    rooms.lock().await.get(&uuid).cloned()
}
```

Fix all call sites to pass the new args. There are probably 2–4.

- [ ] **Step 5: Update `get_or_create_room` to key by Uuid**

Current signature (~line 1533):

```rust
pub(crate) async fn get_or_create_room(
    rooms: &NotebookRooms,
    notebook_id: &str,
    docs_dir: &Path,
    blob_store: Arc<BlobStore>,
    ephemeral: bool,
) -> Arc<NotebookRoom>
```

New signature:

```rust
pub(crate) async fn get_or_create_room(
    rooms: &NotebookRooms,
    path_index: &Arc<Mutex<PathIndex>>,
    uuid: Uuid,
    path: Option<PathBuf>,
    docs_dir: &Path,
    blob_store: Arc<BlobStore>,
    ephemeral: bool,
) -> Arc<NotebookRoom>
```

The body:
1. Lock `rooms`. If `uuid` already present, return its Arc clone.
2. Construct the room via `NotebookRoom::new_fresh` (which should now accept a `Uuid` — update its signature in this step as well; internally it builds the string form only for disk filenames via `notebook_doc_filename`).
3. Insert into rooms map under `uuid`.
4. If `path.is_some()`, also insert into `path_index` (must be done while holding rooms lock to prevent TOCTOU). Expect `PathIndexError::PathAlreadyOpen` should not happen here since we just created; log and continue if it does.
5. Drop rooms lock, spawn file watcher and autosave debouncer if `path.is_some()`.

- [ ] **Step 6: Update `NotebookRoom::new_fresh` to accept `Uuid`**

Rename the parameter from `notebook_id: &str` to `uuid: Uuid`, plus add an explicit `path: Option<PathBuf>`:

```rust
pub fn new_fresh(
    uuid: Uuid,
    path: Option<PathBuf>,
    docs_dir: &Path,
    blob_store: Arc<BlobStore>,
    ephemeral: bool,
) -> Self
```

Inside the function:
- Replace `is_untitled_notebook(notebook_id)` classification with `path.is_none()`.
- Pass `uuid.to_string()` to `notebook_doc_filename` (keeps on-disk naming unchanged).
- Pass `uuid` into `NotebookDoc::new_with_actor` (which also needs to accept a UUID — this is Phase 6).

For this phase, if `NotebookDoc::new_with_actor` still takes `&str`, pass `&uuid.to_string()` as a temporary bridge. Add a `// TODO(phase-6): drop to_string once NotebookDoc accepts Uuid`.

- [ ] **Step 7: Fix every caller and usage**

Grep for every `rooms.get(`, `rooms.insert(`, `rooms.contains_key(`, `rooms.remove(` in the `runtimed` crate and fix each one. Typical patterns:

| Before | After |
|--------|-------|
| `rooms.get(&notebook_id_string)` | `rooms.get(&uuid)` |
| `rooms.insert(notebook_id_string, room)` | `rooms.insert(uuid, room)` |
| `rooms.contains_key(&canonical_path_string)` | Use `path_index.lookup(&canonical_path).is_some()` |

The eviction loop (line 2088–2240) uses `rooms_guard.get(&notebook_id_for_eviction)` and `Arc::ptr_eq`. Change `notebook_id_for_eviction: String` to `room_id_for_eviction: Uuid` (pulled from `room.id` at task-spawn time) and update the lookup.

- [ ] **Step 8: The test for the new `find_room_by_path`**

Add:

```rust
#[tokio::test]
async fn find_room_by_path_returns_room_after_index_insert() {
    let tmp = tempfile::tempdir().unwrap();
    let blob_store = Arc::new(BlobStore::new(tmp.path().to_path_buf()));
    let rooms: NotebookRooms = Arc::new(Mutex::new(HashMap::new()));
    let path_index = Arc::new(Mutex::new(PathIndex::new()));
    let uuid = Uuid::new_v4();
    let fake = tmp.path().join("note.ipynb");
    let room = Arc::new(NotebookRoom::new_fresh(
        uuid,
        Some(fake.clone()),
        tmp.path(),
        blob_store,
        false,
    ));
    rooms.lock().await.insert(uuid, room.clone());
    path_index.lock().await.insert(fake.clone(), uuid).unwrap();

    let found = find_room_by_path(&rooms, &path_index, &fake).await;
    assert!(found.is_some());
    assert!(Arc::ptr_eq(&found.unwrap(), &room));
}

#[tokio::test]
async fn find_room_by_path_returns_none_when_not_indexed() {
    let tmp = tempfile::tempdir().unwrap();
    let rooms: NotebookRooms = Arc::new(Mutex::new(HashMap::new()));
    let path_index = Arc::new(Mutex::new(PathIndex::new()));
    let found = find_room_by_path(&rooms, &path_index, &tmp.path().join("nope.ipynb")).await;
    assert!(found.is_none());
}
```

- [ ] **Step 9: Build and test**

Run: `cargo build -p runtimed` — fix any remaining compile errors.
Run: `cargo test -p runtimed` — all pass.

- [ ] **Step 10: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server.rs \
        crates/runtimed/src/daemon.rs \
        crates/runtimed/src/notebook_sync_server/path_index.rs
git commit -m "refactor(runtimed): key rooms map by Uuid, add path_index"
```

---

## Phase 6: Rewrite `SaveNotebook` handler (no more rekey)

With path now mutable on the room and `path_index` doing the secondary lookup, saving an untitled notebook becomes: (a) write `.ipynb`, (b) update `room.path = Some(canonical)`, (c) insert `path_index`, (d) spawn file watcher + autosave debouncer, (e) broadcast `PathChanged`. No map mutation.

### Task 6.1: Add `PathChanged` broadcast variant and `PathAlreadyOpen` error

**Files:**
- Modify: `crates/notebook-protocol/src/protocol.rs` — `NotebookBroadcast` enum, `NotebookResponse::NotebookSaved`, error types.

- [ ] **Step 1: Add `PathChanged` variant**

In `NotebookBroadcast` enum, append:

```rust
/// Sent when the room's `.ipynb` path changes (untitled→saved, save-as rename).
/// Peers update local bookkeeping but do not reconnect — the UUID is stable.
PathChanged {
    path: Option<std::path::PathBuf>,
},
```

Keep `RoomRenamed` for now (deletion in Phase 7).

- [ ] **Step 2: Add the `PathAlreadyOpen` error to the save response**

`NotebookResponse::NotebookSaved` currently: `{ path: String, new_notebook_id: Option<String> }`. We leave `new_notebook_id` in place this phase (delete in Phase 7). Add a sibling response variant:

```rust
SaveError {
    error: SaveErrorKind,
},

// ...

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SaveErrorKind {
    PathAlreadyOpen { uuid: String, path: String },
    Io { message: String },
}
```

Regenerate TypeScript bindings (happens automatically as part of `cargo xtask build`; if not, run `cargo xtask typescript-bindings` or equivalent — see `.claude/skills/typescript-bindings.md`).

### Task 6.2: Rewrite the `SaveNotebook` handler

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server.rs` — `handle_notebook_request` SaveNotebook arm (~line 6342), the rekey trigger block (~line 2611), `save_notebook_to_disk`.

- [ ] **Step 1: Add a failing integration test**

Add to the test module:

```rust
#[tokio::test]
async fn saving_untitled_notebook_updates_path_index_and_keeps_uuid() {
    let harness = TestDaemonHarness::start().await;  // existing helper, or adapt
    let uuid = harness.create_untitled_notebook().await;
    let save_target = harness.tmp.path().join("note.ipynb");

    let resp = harness.save_notebook(uuid, &save_target).await;
    assert!(matches!(resp, NotebookResponse::NotebookSaved { .. }));

    // UUID unchanged.
    let rooms = harness.daemon.rooms.lock().await;
    assert!(rooms.contains_key(&uuid));

    // path_index populated.
    let idx = harness.daemon.path_index.lock().await;
    assert_eq!(idx.lookup(&save_target.canonicalize().unwrap()), Some(uuid));

    // room.path now Some.
    let room = rooms.get(&uuid).unwrap();
    assert_eq!(room.path.read().await.as_deref(), Some(save_target.canonicalize().unwrap().as_path()));
}

#[tokio::test]
async fn saving_to_already_open_path_returns_path_already_open_error() {
    let harness = TestDaemonHarness::start().await;
    let path = harness.write_ipynb("existing.ipynb", "{}").await;
    let existing_uuid = harness.open_notebook(&path).await;

    let untitled_uuid = harness.create_untitled_notebook().await;
    let resp = harness.save_notebook(untitled_uuid, &path).await;

    match resp {
        NotebookResponse::SaveError { error: SaveErrorKind::PathAlreadyOpen { uuid, path: p } } => {
            assert_eq!(uuid, existing_uuid.to_string());
            assert_eq!(p, path.canonicalize().unwrap().to_string_lossy());
        }
        other => panic!("expected PathAlreadyOpen, got {:?}", other),
    }
}
```

If no `TestDaemonHarness` exists, look in `crates/runtimed/src/notebook_sync_server.rs` for existing test scaffolding (the repo has `test_rekey_ephemeral_room_starts_autosave` which sets up a daemon directly — model after that).

- [ ] **Step 2: Confirm the new tests fail**

Run: `cargo test -p runtimed saving_untitled_notebook_updates_path_index_and_keeps_uuid saving_to_already_open_path`
Expected: fail (either compile error on `SaveErrorKind` if you haven't added it, or behavior mismatch).

- [ ] **Step 3: Write the new SaveNotebook logic**

Replace the existing SaveNotebook arm in `handle_notebook_request`. The new flow:

```rust
NotebookRequest::SaveNotebook { format_cells, path } => {
    let save_target_raw = match path {
        Some(p) => p,
        None => {
            // Saving an already-file-backed room writes back to its current path.
            match room.path.read().await.as_ref() {
                Some(p) => p.to_string_lossy().to_string(),
                None => return NotebookResponse::SaveError {
                    error: SaveErrorKind::Io {
                        message: "cannot save untitled notebook without a path".to_string(),
                    },
                },
            }
        }
    };

    // Write the .ipynb. save_notebook_to_disk handles canonicalization + merging
    // existing metadata.
    let written = match save_notebook_to_disk(room, Some(&save_target_raw), format_cells).await {
        Ok(p) => p,
        Err(e) => return NotebookResponse::SaveError {
            error: SaveErrorKind::Io { message: e.to_string() },
        },
    };
    let canonical = PathBuf::from(&written);

    // Was this room previously untitled? Determine before we mutate anything.
    let was_untitled = room.path.read().await.is_none();

    if was_untitled {
        // Attempt to claim the path in the index. If another room already holds it,
        // fail the save cleanly (no merge).
        let path_index = daemon.path_index.clone();
        if let Err(PathIndexError::PathAlreadyOpen { uuid, path: p }) =
            path_index.lock().await.insert(canonical.clone(), room.id)
        {
            return NotebookResponse::SaveError {
                error: SaveErrorKind::PathAlreadyOpen {
                    uuid: uuid.to_string(),
                    path: p.to_string_lossy().to_string(),
                },
            };
        }

        // Update the room's path.
        *room.path.write().await = Some(canonical.clone());

        // Transition from untitled-persisted to file-backed: stop writing .automerge,
        // start .ipynb autosave + file watcher.
        if let Some(old_persist_tx) = room.persist_tx.as_ref() {
            // Send a sentinel so the debouncer task exits — the task owns the file.
            let _ = old_persist_tx.send(None);
        }
        // Delete the now-stale .automerge file.
        if room.persist_path.exists() {
            if let Err(e) = tokio::fs::remove_file(&room.persist_path).await {
                warn!("[notebook-sync] failed to remove stale persist file: {}", e);
            }
        }

        // Start .ipynb autosave debouncer + file watcher.
        let shutdown_tx = spawn_notebook_file_watcher(canonical.clone(), Arc::clone(room));
        *room.watcher_shutdown_tx.lock().await = Some(shutdown_tx);
        spawn_autosave_debouncer(canonical.to_string_lossy().to_string(), Arc::clone(room));

        // Clear ephemeral bookkeeping in the doc.
        room.is_ephemeral.store(false, Ordering::Relaxed);
        {
            let mut doc = room.doc.write().await;
            let _ = doc.delete_metadata("ephemeral");
        }

        // Broadcast to peers (including Python clients and the frontend) so they
        // can update their local display path.
        let _ = room.kernel_broadcast_tx.send(NotebookBroadcast::PathChanged {
            path: Some(canonical.clone()),
        });
    } else {
        // Saving an already-file-backed notebook: if path changed (save-as),
        // update path_index + broadcast. If same path, nothing to do beyond the write.
        let previous_path = room.path.read().await.clone();
        if previous_path.as_ref() != Some(&canonical) {
            let path_index = daemon.path_index.clone();
            {
                let mut idx = path_index.lock().await;
                if let Err(PathIndexError::PathAlreadyOpen { uuid, path: p }) =
                    idx.insert(canonical.clone(), room.id)
                {
                    return NotebookResponse::SaveError {
                        error: SaveErrorKind::PathAlreadyOpen {
                            uuid: uuid.to_string(),
                            path: p.to_string_lossy().to_string(),
                        },
                    };
                }
                if let Some(old) = previous_path.as_ref() {
                    idx.remove(old);
                }
            }
            *room.path.write().await = Some(canonical.clone());
            let _ = room.kernel_broadcast_tx.send(NotebookBroadcast::PathChanged {
                path: Some(canonical.clone()),
            });
        }
    }

    NotebookResponse::NotebookSaved {
        path: written,
        new_notebook_id: None, // retained for wire compat this phase, deleted in Phase 7
    }
}
```

(Adjust to match the exact `save_notebook_to_disk` signature — it currently returns `Result<String, SaveError>`; if the internal error type is richer, wrap into `SaveErrorKind::Io { message }`.)

- [ ] **Step 4: Remove the old rekey trigger block**

Delete the block at lines ~2611–2649 that calls `rekey_ephemeral_room` and stamps `new_notebook_id` into the response. The new handler above does everything inline.

- [ ] **Step 5: Keep `rekey_ephemeral_room` definition in place for this phase**

We won't call it anymore, but deleting it is Phase 7 cleanup. Add an attribute:

```rust
#[allow(dead_code)]
async fn rekey_ephemeral_room(...) -> Option<String> { ... }
```

- [ ] **Step 6: Run the new tests**

Run: `cargo test -p runtimed saving_untitled_notebook_updates_path_index_and_keeps_uuid saving_to_already_open_path`
Expected: pass.

- [ ] **Step 7: Run the full runtimed suite, including `test_rekey_ephemeral_room_starts_autosave`**

Run: `cargo test -p runtimed`
Expected: the old rekey test may fail because it called `rekey_ephemeral_room` directly. Either:
- Delete `test_rekey_ephemeral_room_starts_autosave` (that's Phase 7), or
- Update it to go through the SaveNotebook handler instead.

Pick option 2 for now — it proves the new path preserves the critical property (cell added post-save flushes to disk).

- [ ] **Step 8: Run the tokio mutex lint**

Run: `cargo test -p runtimed --test tokio_mutex_lint`
Expected: pass. The new code holds `path_index` and `room.path` locks but never across `.await` — verify by inspecting the handler.

- [ ] **Step 9: Commit**

```bash
git add crates/notebook-protocol/src/protocol.rs \
        crates/runtimed/src/notebook_sync_server.rs
git commit -m "feat(runtimed): rewrite SaveNotebook without room rekeying"
```

---

## Phase 7: Delete dead code

Everything in this phase is pure removal. After each deletion, the full test suite must still pass.

### Task 7.1: Delete `rekey_ephemeral_room` and the old test

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server.rs`

- [ ] **Step 1: Confirm no callers remain**

Run: `rg 'rekey_ephemeral_room' crates/`
Expected: matches only in the function definition and its test.

- [ ] **Step 2: Delete the function and its original test**

Remove `rekey_ephemeral_room` (lines ~4467–4640 in the original file) and `test_rekey_ephemeral_room_starts_autosave` (original lines ~11387–11486, adjusted for Phase 6 changes).

- [ ] **Step 3: Build and test**

Run: `cargo test -p runtimed`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server.rs
git commit -m "refactor(runtimed): delete rekey_ephemeral_room"
```

### Task 7.2: Delete `redirect_map` and `RedirectEntry`

**Files:**
- Modify: `crates/runtimed/src/daemon.rs`

- [ ] **Step 1: Confirm no readers**

Run: `rg 'redirect_map|RedirectEntry' crates/runtimed/src/`
Expected: matches only in the struct field declaration, constructor initialization, and the two lookup sites in `handle_open_notebook` and `handle_create_notebook`.

- [ ] **Step 2: Delete the field, struct, and both lookup sites**

- Remove `redirect_map` field from the `Daemon` struct.
- Remove `RedirectEntry` struct.
- Remove the redirect-check blocks in `handle_open_notebook` (~lines 1627–1640) and `handle_create_notebook` (~lines 1821–1838).
- Remove initialization in the `Daemon::new` (or equivalent) constructor.

- [ ] **Step 3: Build**

Run: `cargo build -p runtimed`
Expected: clean.

- [ ] **Step 4: Test**

Run: `cargo test -p runtimed -p notebook-sync`
Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed/src/daemon.rs
git commit -m "refactor(runtimed): delete redirect_map (no longer needed)"
```

### Task 7.3: Delete `RoomRenamed` broadcast and `new_notebook_id` field

**Files:**
- Modify: `crates/notebook-protocol/src/protocol.rs`, any handler/consumer of `RoomRenamed`, `crates/runtimed/src/notebook_sync_server.rs` (save response struct literal).

- [ ] **Step 1: Find consumers of `RoomRenamed`**

Run: `rg 'RoomRenamed' --glob '!docs/**' --glob '!.claude/**'`
Expected: matches in protocol.rs, the daemon (already removed in Phase 6 for the save path; double-check no stragglers), and potentially frontend.

- [ ] **Step 2: Remove the variant from the enum**

Delete `RoomRenamed { new_notebook_id: String }` from `NotebookBroadcast`.

- [ ] **Step 3: Remove the `new_notebook_id` field from `NotebookResponse::NotebookSaved`**

From:

```rust
NotebookSaved {
    path: String,
    new_notebook_id: Option<String>,
},
```

to:

```rust
NotebookSaved {
    path: String,
},
```

- [ ] **Step 4: Fix every construction site**

Run: `cargo build 2>&1 | grep -E 'error\[E|new_notebook_id' | head -40`
Fix each — typically just drop the `new_notebook_id: None,` line.

- [ ] **Step 5: Regenerate TypeScript bindings**

Run: `cargo xtask build --rust-only` (or the ts-rs regeneration command — see `.claude/skills/typescript-bindings.md`). Frontend consumers of `NotebookSaved` lose the `new_notebook_id` field; those must be updated.

Run: `rg 'new_notebook_id' apps/ crates/runt-mcp/`
Expected: readers that need updating. Most will be `if (new_notebook_id)` branches that can be deleted (the UUID no longer changes on save).

- [ ] **Step 6: Build and test everything**

Run: `cargo xtask lint --fix && cargo xtask build --rust-only && cargo test`
Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(protocol): delete RoomRenamed broadcast and new_notebook_id"
```

---

## Phase 8: MCP tool response cleanup

Now that the daemon guarantees UUID stability, the MCP tools don't need heuristics or rekey-handling.

### Task 8.1: Delete `looks_like_path` and split the `notebook` parameter

**Files:**
- Modify: `crates/runt-mcp/src/tools/session.rs`

- [ ] **Step 1: Remove `looks_like_path` (lines ~246–251)**

Delete the function. It has no other callers.

- [ ] **Step 2: Replace the single `notebook` parameter split logic in `open_notebook`**

Current flow (lines ~260–380):
1. Read `notebook` / `path` / `notebook_id` from request params.
2. Call `looks_like_path` on whichever was provided.
3. Branch on path vs session id.

New flow:
1. If `path` is provided → call `connect_open(path)`.
2. If `notebook_id` is provided → parse as UUID, then call `connect(uuid)`.
3. Else → return error `"must provide exactly one of `path` or `notebook_id`"`.

The JSON-schema doc for the tool (the `description` in the tool registration site) must be updated accordingly — remove the "or UUID" dual-meaning of `notebook`.

- [ ] **Step 3: Update `save_notebook`**

Currently reads `new_notebook_id` off the response (line ~618) to update the session's mutable notebook_id. Delete that mutation — the UUID is stable.

Remove the `previous_notebook_id` field from the response JSON.

- [ ] **Step 4: Update `create_notebook`**

If `looks_like_path` was used here too, remove. Ensure the returned `notebook_id` is always a UUID string.

- [ ] **Step 5: Test the MCP tools via integration test**

Run: `cargo test -p runt-mcp`
Expected: pass. If there's no test for `save_notebook` asserting the UUID is stable across save, add one:

```rust
#[tokio::test]
async fn save_untitled_notebook_preserves_uuid() {
    let harness = McpTestHarness::start().await;
    let created = harness.call_tool("create_notebook", json!({"runtime": "python"})).await;
    let uuid_before = created["notebook_id"].as_str().unwrap().to_string();

    let saved = harness.call_tool("save_notebook", json!({"path": harness.tmp_path("n.ipynb")})).await;
    let uuid_after = saved["notebook_id"].as_str().unwrap().to_string();

    assert_eq!(uuid_before, uuid_after);
    assert!(Uuid::parse_str(&uuid_after).is_ok());
}
```

- [ ] **Step 6: Commit**

```bash
git add crates/runt-mcp/src/tools/session.rs
git commit -m "refactor(runt-mcp): drop looks_like_path, split notebook param into path/notebook_id"
```

---

## Phase 9: End-to-end verification

### Task 9.1: Lint, typecheck, typescript bindings

- [ ] **Step 1: Auto-format**

Run: `cargo xtask lint --fix`
Expected: clean.

- [ ] **Step 2: Regenerate TypeScript bindings**

Run: `cargo xtask build --rust-only`
Expected: any `.ts` files under `apps/notebook/src/bindings/` (or wherever ts-rs emits) are regenerated. Check `git diff` for surprising changes.

- [ ] **Step 3: Frontend typecheck**

Run: `cd apps/notebook && pnpm run typecheck` (or the project's TS check — check `package.json` scripts)
Expected: clean. Fix any readers that still expect `new_notebook_id` or `RoomRenamed`.

### Task 9.2: Integration tests

- [ ] **Step 1: Run Python integration tests**

Follow `contributing/testing.md` — the command typically looks like:

```bash
RUNTIMED_SOCKET_PATH="$(./target/debug/runt daemon status --json | jq -r .socket_path)" \
  python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_daemon_integration.py -v
```

Expected: pass. If any tests hard-code expectations that `notebook_id` equals a path for saved notebooks, update them (the new invariant is always-UUID).

- [ ] **Step 2: Run the daemon smoke path manually via MCP**

Using `nteract-dev` supervisor tools:

```
up rebuild=true
```

Then via the MCP client (nteract-dev tools are available directly):

1. `create_notebook` → note the UUID
2. `save_notebook(path=/tmp/uuid_test.ipynb)` → verify returned `notebook_id` equals the UUID from step 1
3. Open a fresh session with `open_notebook(path=/tmp/uuid_test.ipynb)` → verify returned `notebook_id` is a UUID (possibly different from step 1 — that's fine, different session)

- [ ] **Step 3: Desktop app smoke test**

The user runs:
```bash
cargo xtask notebook
```

Verify: untitled notebook saves cleanly, reopens cleanly, no console errors about `new_notebook_id` or `RoomRenamed`.

- [ ] **Step 4: Run tokio mutex lint one more time**

Run: `cargo test -p runtimed --test tokio_mutex_lint`
Expected: pass.

### Task 9.3: Final cleanup

- [ ] **Step 1: Search for stale doc references**

Run: `rg -i 'rekey|redirect_map|new_notebook_id|looks_like_path|RoomRenamed' --glob '!target/**' --glob '!.git/**'`
Expected: only matches in the plan document, this file, CHANGELOG (if any), and git history. Any code or doc matches get cleaned up now.

- [ ] **Step 2: Update any `contributing/` docs that describe the old behavior**

Specifically check:
- `contributing/runtimed.md` — any "re-keying on save" language
- `contributing/protocol.md` — list of broadcast variants
- `CLAUDE.md` — high-risk invariants section

Rewrite each affected section to reflect UUID-first identity.

- [ ] **Step 3: Commit doc updates**

```bash
git add -A
git commit -m "docs: reflect uuid-first notebook identity"
```

- [ ] **Step 4: Final verification**

Run: `cargo xtask lint && cargo test && cargo test -p runtimed --test tokio_mutex_lint`
Expected: all green.

- [ ] **Step 5: Create PR** (when user is ready)

Title: `feat(runtimed)!: uuid-first notebook identity`

The `!` marks a breaking change (MCP tool response shape changed, broadcast variant removed). Body should list the phase summaries and call out the wire-protocol deltas so downstream consumers (desktop, Python bindings) can plan their update.

---

## Self-review checklist

- **Spec coverage:** Every bullet in the original proposal's "What gets deleted" and "What gets added" is covered. `rekey_ephemeral_room` (7.1), `redirect_map` (7.2), `RoomRenamed` (7.3), `looks_like_path` (8.1), `new_notebook_id` (7.3), `path_index` (2 + 5), UUID-returning `open_notebook` (8.1), ephemeral recovery semantics (decision 2 + 4, unchanged behavior).
- **Migration story:** Design decision 8 covers the zero-touch migration. `.automerge` files for untitled notebooks continue to work; file-backed `.automerge` files continue to be transient cache.
- **Collision semantics:** Decision 3 + Task 6.2 step 3 spell out the `PathAlreadyOpen` error path. The 60-line interloper merge goes away with `rekey_ephemeral_room`.
- **Broadcast successor:** `PathChanged` (6.1) replaces `RoomRenamed` — clients that tracked path for UI purposes now get explicit path updates, decoupled from UUID.
- **Type coherence:** `NotebookRoom.id: Uuid`, `NotebookRooms = HashMap<Uuid, ...>`, `PathIndex` values are `Uuid`, `NotebookDoc::notebook_id()` returns `Option<Uuid>` (phased in during Phase 5 step 6 with a `to_string` bridge until Phase 6 when the signature tightens). Wire format is still `String` (UUID hyphenated), documented as always-UUID.
- **TDD cadence:** Each new behavior has a failing test before code (Tasks 2.1, 3.1, 4.1, 5.1 step 8, 6.2, 8.1 step 5). Refactor-only phases (7) rely on the full suite as the regression net.
- **Commit boundaries:** 10 commits total, each leaving the tree in a green state.
