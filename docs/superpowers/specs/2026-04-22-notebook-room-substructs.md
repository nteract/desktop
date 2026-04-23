# Refactor: Break up `NotebookRoom` into composed substructs

> **Status: ready.** The `RuntimeStateHandle` migration (#2059) is on main. `state: RuntimeStateHandle` replaces the old `state_doc: Arc<RwLock<RuntimeStateDoc>>` + `state_changed_tx: broadcast::Sender<()>` pair — one self-notifying handle with its own `changed_tx` and sync Mutex. Field inventory below reflects post-merge state.

## Problem

`NotebookRoom` is a 27-field god struct in `crates/runtimed/src/notebook_sync_server/room.rs`. It mixes:

- Immutable notebook identity (`id`, `persist_path`, `blob_store`, `is_ephemeral`)
- Automerge document + runtime state (`doc`, `state`, `nbformat_attachments`)
- Persistence bookkeeping (`persist_tx`, `flush_request_tx`, `last_save_heads`, `last_save_sources`, `last_self_write`, `watcher_shutdown_tx`)
- Broadcast channels (`changed_tx`, `kernel_broadcast_tx`, `presence_tx`)
- Per-connection accounting (`active_peers`, `had_peers`, `is_loading`)
- Path / working directory mutation (`path`, `working_dir`)
- Trust state (`trust_state`)
- Runtime-agent lifecycle (eight fields — out of scope, see § Out of scope)

Lock shapes are inconsistent: `Arc<RwLock<T>>`, `Arc<Mutex<T>>`, naked `RwLock<T>`, `AtomicBool`, `AtomicUsize`, `AtomicU64`, and `Option<channel_tx>`. Every function that touches `room.*` has to re-derive which fields are safe to hold across which awaits.

The test helper `test_room_with_path` builds a room by hand-constructing 27 fields with sensible defaults — because most production tests need only two or three of them, and there's no smaller unit to construct.

This is the single biggest readability win available in `runtimed` today. It also unblocks the env-launch extraction from the code-review proposal: you can't cleanly move the auto-launch code out of `metadata.rs` while the room is an opaque 27-field bag.

## Fix

Split `NotebookRoom`'s fields into five composed substructs, each owning its own fields directly (no extra `Arc` wrapping) and exposing focused methods. `NotebookRoom` becomes a thin container that composes them.

Field access count (from grep on `crates/runtimed/src`) informed the boundaries: the goal is substructs where **the fields that change together, travel together**, and where **a callsite typically uses fields from one substruct at a time**.

### Proposed shape

```rust
// crates/runtimed/src/notebook_sync_server/room.rs

pub struct NotebookRoom {
    pub id: uuid::Uuid,
    pub identity: RoomIdentity,
    pub doc_state: RoomDocState,
    pub broadcasts: RoomBroadcasts,
    pub persistence: Option<RoomPersistence>,  // None for ephemeral rooms
    pub connections: RoomConnections,
    pub state: runtime_doc::RuntimeStateHandle,  // stays top-level — self-contained handle (owns its own changed_tx and sync Mutex)
    pub trust_state: Arc<RwLock<TrustState>>,    // stays top-level — accessed from both launch + sync paths
    pub blob_store: Arc<BlobStore>,              // stays top-level — passed by value to many subsystems
    pub kernel: RoomKernelState,                 // stays as-is for PR 3 to refactor into an actor
}
```

### Each substruct

**`RoomIdentity`** — immutable-ish notebook identity.
```rust
pub struct RoomIdentity {
    pub persist_path: PathBuf,
    pub is_ephemeral: AtomicBool,
    pub path: RwLock<Option<PathBuf>>,           // Some(x) once saved; changes on untitled → saved
    pub working_dir: RwLock<Option<PathBuf>>,    // untitled-notebook project file detection
}
```
Field count in source: `persist_path` (9), `is_ephemeral` (6), `path` (29), `working_dir` (4). 48 call sites total. No async locks — all std::sync because no accesses hold across awaits.

**`RoomDocState`** — Automerge doc and related per-notebook data.
```rust
pub struct RoomDocState {
    pub doc: Arc<RwLock<NotebookDoc>>,
    pub nbformat_attachments: Arc<RwLock<HashMap<String, serde_json::Value>>>,
}
```
Field count: `doc` (123), `nbformat_attachments` (7). The `Arc<RwLock<_>>` pattern stays — many callsites fork the lock.

*Why not pull `state: RuntimeStateHandle` in here?* The handle is already self-contained — it owns its sync Mutex and its own `changed_tx` — and it's `Clone`. Putting it inside another substruct buys nothing and costs a field-path level (`room.doc_state.state.with_doc(...)` vs `room.state.with_doc(...)`). Keep it top-level; this is the exception that proves the "group what changes together" rule: the handle's internals already do that job.

**`RoomBroadcasts`** — fan-out channels that don't belong to a specific handle.
```rust
pub struct RoomBroadcasts {
    pub changed_tx: broadcast::Sender<()>,
    pub kernel_broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    pub presence_tx: broadcast::Sender<(String, Vec<u8>)>,
    pub presence: Arc<RwLock<PresenceState>>,
}
```
Field count: `changed_tx` (18), `kernel_broadcast_tx` (7), `presence_tx` (6), `presence` (8). `presence` is the state that goes with `presence_tx`; keep them together. `state.subscribe()` replaces the old `state_changed_tx.subscribe()` — no top-level channel needed.

**`RoomPersistence`** — Option-shaped: `None` for ephemeral rooms, `Some` for file-backed.
```rust
pub struct RoomPersistence {
    pub persist_tx: watch::Sender<Option<Vec<u8>>>,
    pub flush_request_tx: mpsc::UnboundedSender<FlushRequest>,
    pub last_save_heads: Arc<RwLock<Vec<automerge::ChangeHash>>>,
    pub last_save_sources: Arc<RwLock<HashMap<String, String>>>,
    pub last_self_write: Arc<AtomicU64>,
    pub watcher_shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
}
```
`NotebookRoom::persistence: Option<RoomPersistence>`. Today `persist_tx` and `flush_request_tx` are separate `Option`s, with the invariant "both are `None` or both are `Some`" — making them a single `Option<RoomPersistence>` captures that invariant in the type system.

`last_save_*` and `last_self_write` are always-present today but only meaningful for file-backed rooms. Moving them into `RoomPersistence` codifies that: ephemeral rooms don't need the fields and never read them. For completeness, callers that *do* read these fields already check `is_ephemeral` or go through autosave paths that only exist for file-backed rooms, so the move is safe.

`watcher_shutdown_tx` lives here because the watcher only runs for file-backed rooms (see `catalog.rs:82-89`).

Field count: `persist_tx` (9), `flush_request_tx` (0 — only constructed), `last_save_heads` (1), `last_save_sources` (7), `last_self_write` (2), `watcher_shutdown_tx` (2).

**`RoomConnections`** — per-connection accounting.
```rust
pub struct RoomConnections {
    pub active_peers: AtomicUsize,
    pub had_peers: AtomicBool,
    pub is_loading: AtomicBool,
}
```
Field count: `active_peers` (15), `had_peers` (2), `is_loading` (4). The existing `try_start_loading()` / `finish_loading()` helpers on `NotebookRoom` move here.

### What stays top-level

**`id`** — one `uuid::Uuid`, read 14 times. No reason to nest.

**`trust_state`** — 20 callsites across metadata, load, launch_kernel, requests. Not obviously "persistence" (trust is re-verified from the live doc) nor "doc state" (the TrustState struct is not an Automerge doc). Keep top-level. Revisit if it later gets a clear home.

**`blob_store`** — 9 callsites. A cloneable `Arc<BlobStore>` that's passed to subsystems (runtime agent spawn, output resolver, blob server). Doesn't belong to any one substruct.

**`kernel` (the 8 runtime-agent fields)** — out of scope. Farmed out to the runtime-agent-work branch. These stay exactly where they are today in `NotebookRoom` for now, so this refactor is a pure move of the other 20 fields.

### What gets deleted

- 10 redundant `Arc<_>` wrappers. Today `runtime_agent_generation: Arc<AtomicU64>`, `next_queue_seq: Arc<AtomicU64>`, `last_self_write: Arc<AtomicU64>` are all `Arc`-wrapped atomics. `NotebookRoom` itself is already behind `Arc` everywhere (`Arc<NotebookRoom>` in the rooms map), so atomics inside it never need an extra `Arc`. Drop the extra allocations — callers that clone to move into tasks already clone the outer `Arc<NotebookRoom>`.
- Similarly, `Arc<RwLock<T>>` where `T` is small: `Arc<RwLock<Option<PathBuf>>>` for `working_dir`, `Arc<RwLock<Option<Instant>>>` for `auto_launch_at` (kernel cluster — deferred). Drop the outer `Arc` for fields that are only ever read through the room, since the room is already `Arc`.
- `pub fn kernel_info` and `pub fn has_kernel` methods on `NotebookRoom` — these inspect runtime-agent fields + `state_doc`. They don't cleanly fit either the kernel cluster or the doc cluster. Leave them on `NotebookRoom` as coordination helpers for now; PR 3 moves them.

### Callsite impact

Every callsite that reads `room.X` where `X` moved to a substruct now reads `room.<sub>.X`. ~230 rewrites total across the affected files — down from the pre-migration estimate of ~330 because `state: RuntimeStateHandle` (top-level, 54 callsites) doesn't move.

The common access patterns:
- `room.doc.read().await` → `room.doc_state.doc.read().await`
- `room.persist_tx.as_ref()` → `room.persistence.as_ref().map(|p| &p.persist_tx)` (tightened: single `Option` unwrap instead of two)
- `room.path.read().await.clone()` → `room.identity.path.read().await.clone()`
- `room.changed_tx.send(())` → `room.broadcasts.changed_tx.send(())`
- `room.active_peers.fetch_add(1, Ordering::SeqCst)` → `room.connections.active_peers.fetch_add(1, Ordering::SeqCst)`
- `room.state.with_doc(|d| ...)` — unchanged (top-level)
- `room.state.subscribe()` — unchanged (top-level)

### Tests

The existing `test_room_with_path` helper shrinks dramatically: most tests use 3-5 fields, and constructing each substruct is independent.

```rust
// before:
let room = NotebookRoom { /* 27 fields */ };

// after:
let (state_changed_tx, _) = broadcast::channel(16);
let room = NotebookRoom {
    id,
    identity: RoomIdentity::new(persist_path, path, /* ephemeral */ false),
    doc_state: RoomDocState::new(doc),
    broadcasts: RoomBroadcasts::default(),
    persistence: Some(RoomPersistence::new_debounced(persist_path.clone())),
    connections: RoomConnections::default(),
    state: RuntimeStateHandle::new(RuntimeStateDoc::new(), state_changed_tx),
    trust_state: Arc::new(RwLock::new(trust)),
    blob_store,
    kernel: RoomKernelState::default(),  // unchanged — stays for PR 3
};
```

Each substruct gets a `::new()` or `::default()` constructor, so most tests become five lines shorter.

### Migration path

Two PRs, each atomic:

**PR A** — introduce the substructs with construction parity, don't move field access yet.
- Define the five substructs.
- Update `NotebookRoom::new_fresh` and `NotebookRoom::load_or_create` to construct through them.
- Add pass-through getters on `NotebookRoom` that preserve the old `room.doc` / `room.path` / etc. surface so every existing callsite keeps compiling without change.
- Leave a `#[deprecated]` note on the pass-throughs so reviewers can see the target shape.

**PR B** — migrate call sites.
- Remove the pass-through getters.
- Update every `room.X` to `room.<sub>.X`.
- This PR is mechanical and moderately large (~230 lines of find/replace across the affected files).

Two PRs means PR A can land on its own and be verified with the existing test suite unchanged. PR B is the churny one and reviewers can skim it knowing the fields haven't changed.

## Out of scope

- **Runtime-agent fields** (`runtime_agent_handle`, `runtime_agent_request_tx`, `pending_runtime_agent_connect_tx`, `runtime_agent_generation`, `runtime_agent_env_path`, `runtime_agent_launched_config`, `current_runtime_agent_id`, `next_queue_seq`, `auto_launch_at`). These need the actor-pattern rewrite from the code-review proposal and are being handled on the runtime-agent-work branch. This refactor preserves them exactly as-is on `NotebookRoom` so the two changes don't collide.
- **RuntimeStateHandle internals** — already its own type with self-contained notification. This refactor treats it as an opaque field on `NotebookRoom`.
- **Extracting env-launch out of `metadata.rs`** — a separate follow-on proposal. Independent of this refactor, but this refactor makes it easier.
- **Splitting `notebook_sync_server/metadata.rs`** — separate concern.

## Properties

- Wire format unchanged. No protocol or schema version bump.
- No behavior change. Every field keeps its same lock discipline and same access pattern.
- Tests continue to pass (after the migration — PR A keeps them all green via pass-through getters).
- PR A ships in one atomic commit, workspace green at both ends.
- PR B is mechanical: reviewer verifies "is `room.<sub>.field` the same field it used to be called `room.field`?" and nothing else.

## Non-goals

- Not introducing traits or abstract interfaces.
- Not changing any public API (Python bindings, MCP, wire protocol).
- Not fixing the generation-counter race in the runtime-agent fields (PR 3's job).
- Not renaming fields.

## Testing

- Existing test suite passes unchanged after PR A.
- No new tests required — this is a refactor, not a feature.
- After PR B: one smoke test per substruct constructor (`RoomPersistence::new_debounced` spawns the debouncer, `RoomConnections::default` starts at zero, etc.).

## Risk

- **Blast radius.** ~230 call-site edits in PR B. Mechanical, but large. Mitigation: PR A lands pass-throughs first so the field rename is a find/replace pass, not a semantic change.
- **Atomic constructor invariant.** `persist_tx` and `flush_request_tx` being in one `RoomPersistence` is better than two separate `Option`s — but the test `room.persist_tx.is_none()` at `tests.rs:428` needs rewriting as `room.persistence.is_none()`. That's a semantic improvement (the test gets more faithful to the invariant), not a regression.
- **Missed callers.** Use compiler errors to find them — deleting `pub doc: Arc<RwLock<NotebookDoc>>` and re-adding under `RoomDocState` will surface every `room.doc` usage at compile time.

## Deliverables

- `crates/runtimed/src/notebook_sync_server/room.rs` → substruct definitions, updated constructors.
- (In PR B) every `room.<field>` access in the 26 affected files → `room.<sub>.<field>`.
- Updated `test_room_with_path` helper.
- No changes to protocol types, Automerge schema, or Python/TS bindings.
