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
    pub doc: Arc<RwLock<NotebookDoc>>,           // top-level — 123 callsites, no natural sibling
    pub state: runtime_doc::RuntimeStateHandle,  // top-level — self-contained handle (owns its own changed_tx and sync Mutex)
    pub trust_state: Arc<RwLock<TrustState>>,    // top-level — accessed from both launch + sync paths
    pub blob_store: Arc<BlobStore>,              // top-level — passed by value to many subsystems

    pub identity: RoomIdentity,
    pub broadcasts: RoomBroadcasts,
    pub persistence: Option<RoomPersistence>,    // None for ephemeral rooms
    pub connections: RoomConnections,

    // The 8 runtime-agent fields — stay as-is for PR 3 (actor pattern).
    pub runtime_agent_handle: Arc<Mutex<Option<RuntimeAgentHandle>>>,
    pub runtime_agent_request_tx: Arc<Mutex<Option<RuntimeAgentRequestSender>>>,
    pub pending_runtime_agent_connect_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    pub runtime_agent_generation: AtomicU64,
    pub runtime_agent_env_path: RwLock<Option<PathBuf>>,
    pub runtime_agent_launched_config: RwLock<Option<LaunchedEnvConfig>>,
    pub current_runtime_agent_id: RwLock<Option<String>>,
    pub next_queue_seq: AtomicU64,
}
```

Four substructs, not five — `RoomDocState` is dropped per D1.

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

**`RoomDocState`** — Automerge doc only. Thinnest substruct; maybe too thin, but `doc` is the hottest field by a factor of 2× and deserves a clear home.
```rust
pub struct RoomDocState {
    pub doc: Arc<RwLock<NotebookDoc>>,
}
```
Field count: `doc` (123). If `RoomDocState` feels over-engineered for one field, consider skipping it and keeping `doc: Arc<RwLock<NotebookDoc>>` top-level. Calling it out as a decision point — see § Design decisions.

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
    pub last_save_sources: RwLock<HashMap<String, String>>,
    pub last_self_write: AtomicU64,
    pub watcher_shutdown_tx: Mutex<Option<oneshot::Sender<()>>>,
    pub nbformat_attachments: RwLock<HashMap<String, serde_json::Value>>,
    pub is_loading: AtomicBool,
}
```
`NotebookRoom::persistence: Option<RoomPersistence>`. Today `persist_tx` and `flush_request_tx` are separate `Option`s, with the invariant "both are `None` or both are `Some`" — making them a single `Option<RoomPersistence>` captures that invariant in the type system.

`last_save_sources` and `last_self_write` are always-present today but only meaningful for file-backed rooms. Moving them into `RoomPersistence` codifies that: ephemeral rooms don't need the fields and never read them.

`watcher_shutdown_tx` lives here because the watcher only runs for file-backed rooms (see `catalog.rs:82-89`).

`nbformat_attachments` is disk-coupled (populated from the .ipynb file on load, read on save and on markdown-asset resolution). Ephemeral rooms don't need it — they never have nbformat attachments to round-trip. Moving it here drops the field from `RoomDocState`.

`is_loading` is a streaming-load mutex used during initial .ipynb load to prevent double-loading. It gates the persistence-read path, not the general connection-count path, so it belongs here rather than in `RoomConnections`. The `try_start_loading()` / `finish_loading()` helpers on `NotebookRoom` move to `RoomPersistence` as inherent methods.

Field count: `persist_tx` (9), `flush_request_tx` (0 — only constructed), `last_save_sources` (7), `last_self_write` (2), `watcher_shutdown_tx` (2), `nbformat_attachments` (7), `is_loading` (4).

**`RoomConnections`** — per-connection accounting.
```rust
pub struct RoomConnections {
    pub active_peers: AtomicUsize,
    pub had_peers: AtomicBool,
}
```
Field count: `active_peers` (15), `had_peers` (2).

### What stays top-level

**`id`** — one `uuid::Uuid`, read 14 times. No reason to nest.

**`trust_state`** — 20 callsites across metadata, load, launch_kernel, requests. Not obviously "persistence" (trust is re-verified from the live doc) nor "doc state" (the TrustState struct is not an Automerge doc). Keep top-level. Revisit if it later gets a clear home.

**`blob_store`** — 9 callsites. A cloneable `Arc<BlobStore>` that's passed to subsystems (runtime agent spawn, output resolver, blob server). Doesn't belong to any one substruct.

**`kernel` (the 8 runtime-agent fields)** — out of scope. Farmed out to the runtime-agent-work branch. These stay exactly where they are today in `NotebookRoom` for now, so this refactor is a pure move of the other 20 fields.

### What gets deleted

- 10 redundant `Arc<_>` wrappers. Today `runtime_agent_generation: Arc<AtomicU64>`, `next_queue_seq: Arc<AtomicU64>`, `last_self_write: Arc<AtomicU64>` are all `Arc`-wrapped atomics. `NotebookRoom` itself is already behind `Arc` everywhere (`Arc<NotebookRoom>` in the rooms map), so atomics inside it never need an extra `Arc`. Drop the extra allocations — callers that clone to move into tasks already clone the outer `Arc<NotebookRoom>`.
- Similarly, `Arc<RwLock<T>>` where `T` is small: `Arc<RwLock<Option<PathBuf>>>` for `working_dir`, `Arc<RwLock<Vec<ChangeHash>>>` for `last_save_heads`, `Arc<RwLock<HashMap<...>>>` for `last_save_sources`, `last_save_heads`, `nbformat_attachments`. Drop the outer `Arc` for fields that are only ever read through the room.
- `pub fn kernel_info` and `pub fn has_kernel` methods on `NotebookRoom` — these inspect runtime-agent fields + `state`. They don't cleanly fit either the kernel cluster or the doc cluster. Leave them on `NotebookRoom` as coordination helpers for now; PR 3 moves them.

### Simplifications found during scoping (optional, should ship separately)

Three "delete first, then split" cleanups surfaced during the field audit. Each is small enough to ship as a one-file PR *before* starting on substructs, which shrinks the substruct refactor's surface area.

**S1 — Delete `auto_launch_at`.** Write-only field. `peer.rs:458` stores `Some(Instant::now())` when auto-launch triggers; nothing ever reads it. The comment claims a 30-second eviction grace period, but eviction reads `active_peers` + `room_eviction_delay_ms`, not `auto_launch_at`. Delete the field, delete the write.

**S2 — Delete `last_save_heads`.** Written at `persist.rs:262`, read nowhere. The comment explains it was for `fork_at(last_save_heads)` which is disabled due to automerge/automerge#1327. Keeping it as "in case we re-enable" costs an `Arc<RwLock<_>>` allocation on every room construction and one write per save. Delete both sides; if we re-enable `fork_at` someday, bring it back.

**S3 — Verify `had_peers` is still needed.** Written once at `peer.rs:418` (`= true` on first connect), read only at `daemon.rs:2773` inside a diagnostic `RoomInfo` response (the rooms-list RPC). Kyle recalls eviction using it historically; current code doesn't. Before deleting, grep all `.rs` for the atomic name, search recent git history for eviction logic that touched it, and confirm the RoomsList consumer (if any) actually uses the field. If the only reader is a never-consulted diagnostic field, delete both sides. Otherwise keep.

Each simplification is independent and costs <30 lines to remove. Landing them first means fewer fields to relocate in PR B.

### Design decisions

Four choices that merit calling out before implementation starts:

**D1 — Keep `doc` in a `RoomDocState` substruct, or leave it top-level?**
- *Pro substruct:* consistency with the other substructs; leaves a clear place to add per-doc helpers later (materializers, snapshots).
- *Pro top-level:* 123 callsites, one field — the nesting buys nothing today. `RoomDocState` would be a one-field substruct.
- *Recommendation:* leave `doc` top-level. Revisit only if a natural second field appears (it doesn't today — `nbformat_attachments` is disk-coupled, `state` is a separate handle). Drop `RoomDocState` from the plan.

**D2 — Put `is_loading` in `RoomConnections` or `RoomPersistence`?**
The field is a streaming-load mutex. It gates the "read .ipynb from disk" path. If we read the name literally (`is_loading` = "a peer is currently loading"), it sounds like connection accounting. If we read the behavior (prevents two reads of the disk file), it's persistence. Behavior wins.
- *Recommendation:* `RoomPersistence`. `try_start_loading`/`finish_loading` become methods on `RoomPersistence` (ephemeral rooms can't have `is_loading` because they have no persistence — even better, the caller pattern becomes `room.persistence.as_ref().and_then(|p| p.try_start_loading())`).

**D3 — Hold `nbformat_attachments` in `RoomDocState` or `RoomPersistence`?**
Populated on .ipynb load (only for file-backed rooms), read on save and on markdown-asset resolution. Never written by live edits — it's a "preserve through round-trip" cache.
- *Recommendation:* `RoomPersistence`. Ephemeral rooms don't need it.

**D4 — Keep the `Arc<RwLock<_>>` shell on fields that don't need shared ownership?**
Fields like `last_save_sources` and `nbformat_attachments` are `Arc<RwLock<HashMap<_, _>>>` today. The `Arc` is unnecessary — `NotebookRoom` itself is always behind an `Arc`, so nested `Arc`s don't buy cheap cloning. Dropping the outer `Arc` means `room.persistence.as_ref().unwrap().last_save_sources.read().await` instead of `room.last_save_sources.read().await.clone()` — one fewer allocation per read.
- *Recommendation:* drop nested `Arc`s on fields owned exclusively by the room. Callers that need to move the data into a task can `.clone()` the data itself (already happens at several callsites).

### Callsite impact

Every callsite that reads `room.X` where `X` moved to a substruct now reads `room.<sub>.X`. ~105 rewrites total across the affected files — the biggest reduction comes from D1 (keep `doc` top-level, preserves 123 callsites untouched).

The common access patterns:
- `room.doc.read().await` — unchanged (top-level per D1)
- `room.state.with_doc(|d| ...)` — unchanged (top-level)
- `room.state.subscribe()` — unchanged (top-level)
- `room.persist_tx.as_ref()` → `room.persistence.as_ref().map(|p| &p.persist_tx)` (tightened: single `Option` unwrap instead of two)
- `room.path.read().await.clone()` → `room.identity.path.read().await.clone()`
- `room.changed_tx.send(())` → `room.broadcasts.changed_tx.send(())`
- `room.active_peers.fetch_add(1, Ordering::SeqCst)` → `room.connections.active_peers.fetch_add(1, Ordering::SeqCst)`
- `room.try_start_loading()` → `room.persistence.as_ref().is_some_and(|p| p.try_start_loading())`
- `room.nbformat_attachments.read().await.clone()` → `room.persistence.as_ref().map(|p| p.nbformat_attachments.read()).transpose()`...

(The `is_loading` / `nbformat_attachments` rewrites get slightly noisier because they now live under `Option<RoomPersistence>`. Ephemeral rooms today treat these as always-present with "default empty" semantics; after the refactor, ephemeral rooms skip them entirely. At most callsites this is a small simplification: the ephemeral branch short-circuits instead of reading an empty map.)

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

Three PRs, each atomic and small-ish. Do them in order.

**PR 0 — Simplifications (deletions first).**
- S1: delete `auto_launch_at`. One field, one writer, no readers. Drop the field, drop the write site (peer.rs:458-459), drop the two initializers in room.rs.
- S2: delete `last_save_heads`. One field, one writer (persist.rs:262), no readers. Drop both sides.
- S3: verify `had_peers` is still needed for eviction as Kyle recalls (grep recent git log for eviction code that touched it). If yes: keep. If no: delete both the write (peer.rs:418) and the diagnostic field (daemon.rs:2773 `RoomInfo.had_peers`, which is a wire type — will need a protocol version bump to remove safely, so probably keep for now).
- Drop nested `Arc<_>` wrappers on `next_queue_seq`, `runtime_agent_generation`, `last_self_write` — the room is already behind `Arc` so these extra allocations are dead weight.
- No new substructs yet. Everything stays on `NotebookRoom`, just smaller and cleaner.
- Expected ~50-80 lines removed.

**PR A — introduce the substructs with construction parity, don't move field access yet.**
- Define the four substructs (`RoomIdentity`, `RoomBroadcasts`, `RoomPersistence`, `RoomConnections`).
- Update `NotebookRoom::new_fresh` and `NotebookRoom::load_or_create` to construct through them.
- Add pass-through getters on `NotebookRoom` that preserve the old `room.path` / `room.changed_tx` / etc. surface so every existing callsite keeps compiling without change.
- Leave a `#[deprecated]` note on the pass-throughs so reviewers can see the target shape.

**PR B — migrate call sites.**
- Remove the pass-through getters.
- Update every `room.X` to `room.<sub>.X` for fields that moved.
- `room.doc` and `room.state` stay untouched per D1.
- Mechanical, ~105 lines of find/replace across the affected files.

Three PRs means PR 0 ships obvious cleanups first (low risk, zero mental overhead), PR A lands on its own and is verified with the existing test suite unchanged, and PR B is the churny one but reviewers can skim it knowing only field paths changed.

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

- **Blast radius.** ~105 call-site edits in PR B (down from ~230 because D1 keeps `doc` top-level). Mechanical. Mitigation: PR A lands pass-throughs first so the field rename is a find/replace pass, not a semantic change.
- **PR 0 deletions are irreversible once pushed.** If S3 turns out to be wrong and `had_peers` really was load-bearing, reverting means re-adding the atomic. Cheap to fix but worth a careful grep first.
- **Atomic constructor invariant.** `persist_tx` and `flush_request_tx` being in one `RoomPersistence` is better than two separate `Option`s — but the test `room.persist_tx.is_none()` at `tests.rs:428` needs rewriting as `room.persistence.is_none()`. That's a semantic improvement (the test gets more faithful to the invariant), not a regression.
- **Missed callers.** Use compiler errors to find them — deleting `pub doc: Arc<RwLock<NotebookDoc>>` and re-adding under `RoomDocState` will surface every `room.doc` usage at compile time.

## Deliverables

- `crates/runtimed/src/notebook_sync_server/room.rs` → substruct definitions, updated constructors.
- (In PR B) every `room.<field>` access in the 26 affected files → `room.<sub>.<field>`.
- Updated `test_room_with_path` helper.
- No changes to protocol types, Automerge schema, or Python/TS bindings.
