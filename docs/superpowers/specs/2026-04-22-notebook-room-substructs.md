# Refactor: Break up `NotebookRoom` into composed substructs

> **Status: in-progress.** PR 0 (deletions, #2061) and PR A (RoomIdentity, #2064) are merged. `NotebookRoom` is now 24 direct fields + `identity: RoomIdentity` holding 4 more. Three substructs remain to extract: `RoomBroadcasts`, `RoomPersistence`, `RoomConnections`. Field inventory below reflects post-#2065 state (broadcast cleanup dropped two emissions without changing struct layout).

## Progress

**Done:**
- **PR 0** (#2061): deleted `auto_launch_at`, `last_save_heads`, stripped redundant `Arc<>` on `last_self_write`. Audited and kept `had_peers` (Python API depends on it).
- **PR A** (#2064): extracted `RoomIdentity` holding `persist_path`, `is_ephemeral`, `path`, `working_dir`. ~48 callsites migrated to `room.identity.X`. Dropped `Arc` on `working_dir`.

**Remaining:**
- **PR B** — `RoomBroadcasts`: `changed_tx`, `kernel_broadcast_tx`, `presence_tx`, `presence`.
- **PR C** — `RoomPersistence`: `persist_tx`, `flush_request_tx`, `last_save_sources`, `last_self_write`, `watcher_shutdown_tx`, `nbformat_attachments`, `is_loading`.
- **PR D** — `RoomConnections`: `active_peers`, `had_peers`.

## Problem

`NotebookRoom` remains a 24-direct-field struct (plus `identity`). It still mixes:

- Automerge document + runtime state (`doc`, `state`, `nbformat_attachments`)
- Persistence bookkeeping (`persist_tx`, `flush_request_tx`, `last_save_sources`, `last_self_write`, `watcher_shutdown_tx`, `is_loading`)
- Broadcast channels (`changed_tx`, `kernel_broadcast_tx`, `presence_tx`, `presence`)
- Per-connection accounting (`active_peers`, `had_peers`)
- Trust state (`trust_state`)
- Blob store handle (`blob_store`)
- Runtime-agent lifecycle (eight fields — out of scope, see § Out of scope)

Lock shapes remain inconsistent: `Arc<RwLock<T>>`, `Arc<Mutex<T>>`, naked `RwLock<T>`, `AtomicBool`, `AtomicUsize`, `AtomicU64`, and `Option<channel_tx>`. Every function that touches `room.*` has to re-derive which fields are safe to hold across which awaits.

This is the single biggest readability win remaining in `runtimed` today. It also unblocks the env-launch extraction from the code-review proposal: you can't cleanly move the auto-launch code out of `metadata.rs` while the room is an opaque field bag.

## Fix

Split `NotebookRoom`'s remaining fields into three more composed substructs (`RoomBroadcasts`, `RoomPersistence`, `RoomConnections`) on top of the already-shipped `RoomIdentity`. Each substruct owns its fields directly (no extra `Arc` wrapping) and exposes focused methods. `NotebookRoom` becomes a thin container that composes them.

Field access count (from grep on `crates/runtimed/src`, post-#2065) informed the boundaries: the goal is substructs where **the fields that change together, travel together**, and where **a callsite typically uses fields from one substruct at a time**.

### Target shape (after all three remaining PRs land)

```rust
// crates/runtimed/src/notebook_sync_server/room.rs

pub struct NotebookRoom {
    pub id: uuid::Uuid,
    pub doc: Arc<RwLock<NotebookDoc>>,           // top-level — 125 callsites, no natural sibling
    pub state: runtime_doc::RuntimeStateHandle,  // top-level — self-contained handle (owns its own changed_tx and sync Mutex)
    pub trust_state: Arc<RwLock<TrustState>>,    // top-level — accessed from both launch + sync paths
    pub blob_store: Arc<BlobStore>,              // top-level — passed by value to many subsystems

    pub identity: RoomIdentity,                  // DONE (PR #2064)
    pub broadcasts: RoomBroadcasts,              // PR B
    pub persistence: Option<RoomPersistence>,    // PR C. None for ephemeral rooms.
    pub connections: RoomConnections,            // PR D

    // The 8 runtime-agent fields — stay as-is. The runtime-agent-work branch
    // will refactor these into an actor pattern; keeping them at the top
    // level means this refactor doesn't collide with that work.
    pub runtime_agent_handle: Arc<Mutex<Option<RuntimeAgentHandle>>>,
    pub runtime_agent_request_tx: Arc<Mutex<Option<RuntimeAgentRequestSender>>>,
    pub pending_runtime_agent_connect_tx: Arc<Mutex<Option<oneshot::Sender<()>>>>,
    pub runtime_agent_generation: Arc<AtomicU64>,
    pub runtime_agent_env_path: Arc<RwLock<Option<PathBuf>>>,
    pub runtime_agent_launched_config: Arc<RwLock<Option<LaunchedEnvConfig>>>,
    pub current_runtime_agent_id: Arc<RwLock<Option<String>>>,
    pub next_queue_seq: Arc<AtomicU64>,
}
```

Four substructs total (`RoomIdentity` already shipped).

### Each substruct

**`RoomIdentity`** — DONE (PR #2064). `persist_path`, `is_ephemeral`, `path`, `working_dir`. Accessed via `room.identity.X` at 46 callsites. No async locks. Constructor: `RoomIdentity::new(persist_path, path, ephemeral)`. `Arc` on `working_dir` dropped during extraction.

**`RoomBroadcasts`** — PR B. Fan-out channels that don't belong to a specific handle.
```rust
pub struct RoomBroadcasts {
    pub changed_tx: broadcast::Sender<()>,
    pub kernel_broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    pub presence_tx: broadcast::Sender<(String, Vec<u8>)>,
    pub presence: Arc<RwLock<PresenceState>>,
}
```
Field counts (current): `changed_tx` (18), `kernel_broadcast_tx` (5), `presence_tx` (6), `presence` (8). `presence` is the state that goes with `presence_tx`; keep them together. `state.subscribe()` handles the old "runtime state changed" channel — no top-level broadcast for that anymore.

Total callsite migration for PR B: ~37.

**`RoomPersistence`** — PR C. Option-shaped: `None` for ephemeral rooms, `Some` for file-backed.
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

`nbformat_attachments` is disk-coupled (populated from the .ipynb file on load, read on save and on markdown-asset resolution). Ephemeral rooms don't need it — they never have nbformat attachments to round-trip.

`is_loading` is a streaming-load mutex used during initial .ipynb load to prevent double-loading. It gates the persistence-read path, not the general connection-count path, so it belongs here rather than in `RoomConnections`. The `try_start_loading()` / `finish_loading()` helpers on `NotebookRoom` move to `RoomPersistence` as inherent methods.

Field counts (current): `persist_tx` (9), `flush_request_tx` (1 — only constructed), `last_save_sources` (7), `last_self_write` (2), `watcher_shutdown_tx` (3), `nbformat_attachments` (7), `is_loading` (4).

Total callsite migration for PR C: ~33, with some turning into `Option::map`/`Option::and_then` chains. This is the biggest blast radius because of the `Option<_>` wrapping, and the one reviewers will want to see most carefully.

**`RoomConnections`** — PR D. Per-connection accounting.
```rust
pub struct RoomConnections {
    pub active_peers: AtomicUsize,
    pub had_peers: AtomicBool,
}
```
Field counts (current): `active_peers` (17), `had_peers` (2).

Total callsite migration for PR D: ~19. Smallest substruct, good candidate to ship last as a palate-cleanser.

### What stays top-level

**`id`** — one `uuid::Uuid`, read 14 times. No reason to nest.

**`doc`** — 125 callsites, one field. The nesting would buy nothing. Stays top-level.

**`state: RuntimeStateHandle`** — self-contained handle (owns its sync Mutex and its own `changed_tx`, clone-cheap). Nesting it would just add a field-path level. Stays top-level.

**`trust_state`** — 20 callsites across metadata, load, launch_kernel, requests. Not obviously "persistence" (trust is re-verified from the live doc) nor "doc state" (the TrustState struct is not an Automerge doc). Stays top-level. Revisit if it later gets a clear home.

**`blob_store`** — 9 callsites. A cloneable `Arc<BlobStore>` that's passed to subsystems (runtime agent spawn, output resolver, blob server). Doesn't belong to any one substruct. Stays top-level.

**The 8 runtime-agent fields** — out of scope. Farmed out to the runtime-agent-work branch, which plans to refactor them into an actor pattern. They stay at the top level for now so this refactor doesn't collide with that work.

### What gets deleted during each substruct PR

- Redundant `Arc<_>` wrappers on fields that the room owns exclusively. `NotebookRoom` itself is already behind `Arc` (in the `NotebookRooms` map), so `Arc<AtomicU64>`, `Arc<RwLock<HashMap<_>>>`, etc. on fields are dead allocations. Drop the outer `Arc` when extracting each substruct.
- Non-kernel examples already shipped: `working_dir` (in PR A), `last_self_write` (in PR 0).
- Remaining: `last_save_sources`, `nbformat_attachments` (both `Arc<RwLock<HashMap<_, _>>>`) — drop when they move into `RoomPersistence`.
- Kernel-cluster Arc atomics (`runtime_agent_generation`, `next_queue_seq`) deferred to the runtime-agent branch, which has a test that clones those individually and will need its own update alongside.

### Design decisions

Three choices locked in from the initial design phase, still applicable:

**D1 — Don't create a `RoomDocState` substruct.**
`doc` is the only field that would live there — `state` is a separate handle, `nbformat_attachments` is disk-coupled. A one-field substruct buys nothing.

**D2 — `is_loading` lives in `RoomPersistence`, not `RoomConnections`.**
The field is a streaming-load mutex. It gates the "read .ipynb from disk" path. If we read the name literally it sounds like connection accounting, but behavior wins: ephemeral rooms can't have `is_loading` because they have no persistence. `try_start_loading`/`finish_loading` become methods on `RoomPersistence`.

**D3 — `nbformat_attachments` lives in `RoomPersistence`.**
Populated on .ipynb load (only for file-backed rooms), read on save and on markdown-asset resolution. Never written by live edits. Ephemeral rooms don't need it.

**D4 — Drop the `Arc<_>` shell on fields the room owns exclusively.**
`NotebookRoom` is always behind an `Arc`, so nested `Arc`s don't buy cheap cloning. Dropping them means one fewer allocation per read. Callers that need to move data into a task clone the data itself (already happens at several callsites).

### Callsite impact

Every callsite that reads `room.X` where `X` moved to a substruct now reads `room.<sub>.X`. ~89 rewrites remaining across the affected files:

- **PR B (RoomBroadcasts):** ~37 callsites
- **PR C (RoomPersistence):** ~33 callsites (some turn into `Option::map` chains)
- **PR D (RoomConnections):** ~19 callsites

`doc` and `state` are left untouched at the top level per D1 — ~179 callsites preserved.

The common access patterns:
- `room.doc.read().await` — unchanged (top-level per D1)
- `room.state.with_doc(|d| ...)` — unchanged (top-level)
- `room.state.subscribe()` — unchanged (top-level)
- `room.identity.path.read().await.clone()` — unchanged (already shipped)
- `room.persist_tx.as_ref()` → `room.persistence.as_ref().map(|p| &p.persist_tx)` (tightened: single `Option` unwrap instead of two)
- `room.changed_tx.send(())` → `room.broadcasts.changed_tx.send(())`
- `room.active_peers.fetch_add(1, Ordering::SeqCst)` → `room.connections.active_peers.fetch_add(1, Ordering::SeqCst)`
- `room.try_start_loading()` → `room.persistence.as_ref().is_some_and(|p| p.try_start_loading())`

The `is_loading` / `nbformat_attachments` rewrites get slightly noisier because they live under `Option<RoomPersistence>`. Ephemeral rooms today treat these as always-present with "default empty" semantics; after the refactor, ephemeral rooms skip them entirely. At most callsites this is a small simplification: the ephemeral branch short-circuits instead of reading an empty map.

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
    broadcasts: RoomBroadcasts::default(),
    persistence: Some(RoomPersistence::new_debounced(persist_path.clone())),
    connections: RoomConnections::default(),
    doc: Arc::new(RwLock::new(doc)),
    state: RuntimeStateHandle::new(RuntimeStateDoc::new(), state_changed_tx),
    trust_state: Arc::new(RwLock::new(trust)),
    blob_store,
    // Runtime-agent fields unchanged (PR3 / actor pattern owns them).
    runtime_agent_handle: Arc::new(Mutex::new(None)),
    // ... etc.
};
```

Each substruct gets a `::new()` or `::default()` constructor, so most tests become several lines shorter.

### Migration path

Each remaining substruct ships as a single atomic PR. The pattern is proven (PR A shipped as #2064 and was mechanical): define the struct, move the fields, migrate callsites in one commit, compiler verifies.

The original spec planned a "scaffolding PR + migration PR" split with pass-through getters. Rust can't fake field access via methods without parens, so that split was impossible; one atomic PR per substruct turned out to be cleaner anyway — reviewers see one focused diff per substruct instead of a scaffolding-then-rename pair.

**Done:**
- **PR 0** (#2061): deletions + `Arc<AtomicU64>` → `AtomicU64` on `last_self_write`. ~50 lines removed.
- **PR A** (#2064): `RoomIdentity`. ~48 callsites migrated, `Arc` dropped on `working_dir`.

**Remaining (suggested order):**
- **PR B** — `RoomBroadcasts`: define struct with 4 fields, migrate ~37 callsites. Smallest risk since broadcast channels don't hold state — they just dispatch.
- **PR C** — `RoomPersistence`: the one with `Option<_>` shape. Define struct with 7 fields, migrate ~33 callsites (some become `Option::map`/`Option::and_then` chains). Drop nested `Arc<_>` on `last_save_sources` and `nbformat_attachments`. `try_start_loading`/`finish_loading` methods move onto `RoomPersistence`.
- **PR D** — `RoomConnections`: define struct with 2 fields, migrate ~19 callsites. Palate-cleanser.

Each PR stands alone: compiler verifies all callsites at commit time, test suite is the backstop, `cargo xtask clippy` + `cargo xtask lint` gate the commit.

## Out of scope

- **Runtime-agent fields** (`runtime_agent_handle`, `runtime_agent_request_tx`, `pending_runtime_agent_connect_tx`, `runtime_agent_generation`, `runtime_agent_env_path`, `runtime_agent_launched_config`, `current_runtime_agent_id`, `next_queue_seq`, `auto_launch_at`). These need the actor-pattern rewrite from the code-review proposal and are being handled on the runtime-agent-work branch. This refactor preserves them exactly as-is on `NotebookRoom` so the two changes don't collide.
- **RuntimeStateHandle internals** — already its own type with self-contained notification. This refactor treats it as an opaque field on `NotebookRoom`.
- **Extracting env-launch out of `metadata.rs`** — a separate follow-on proposal. Independent of this refactor, but this refactor makes it easier.
- **Splitting `notebook_sync_server/metadata.rs`** — separate concern.

## Properties

- Wire format unchanged. No protocol or schema version bump.
- No behavior change. Every field keeps its same lock discipline and same access pattern.
- Each substruct PR is atomic: the workspace compiles before and after the commit.
- Reviewer's job is simple: "is `room.<sub>.field` the same field it used to be called `room.field`?"

## Non-goals

- Not introducing traits or abstract interfaces.
- Not changing any public API (Python bindings, MCP, wire protocol).
- Not fixing the generation-counter race in the runtime-agent fields (that's the runtime-agent-work branch's job).
- Not renaming fields.

## Testing

- Existing test suite passes unchanged — each PR verifies against the full `cargo test -p runtimed --lib` suite.
- No new tests required for the moves themselves.
- `test_room_with_path` helper shrinks incrementally as each substruct lands (a constructor call replaces 4-7 hand-placed fields).

## Risk

- **Blast radius, not mechanical risk.** ~89 callsite edits remaining across 3 PRs. The compiler surfaces every miss — no grep-and-hope.
- **`RoomPersistence` is the riskiest of the three** because of the `Option<_>` wrapper. Some callsites currently treat "no persistence" as "empty maps" via always-present fields. After the refactor those callsites short-circuit instead. Expect some rewrites to read less naturally the first time through; trust the compiler to flag behavior-changing mistakes (type mismatches, missing arms).
- **Kernel-cluster `Arc<_>` strips are explicitly out of scope.** PR 0 stripped `Arc<AtomicU64>` on `last_self_write`. `next_queue_seq`, `runtime_agent_generation`, and a few others remain `Arc`-wrapped because a test clones them individually into a spawned task. The runtime-agent branch will touch this when it actor-ifies those fields.

## Deliverables

Per remaining PR:
- `crates/runtimed/src/notebook_sync_server/room.rs` → substruct definition, updated constructors.
- Every `room.<field>` access → `room.<sub>.<field>` across the affected files.
- Updated `test_room_with_path` helper (one constructor call replaces the moved-field initializers).
- No changes to protocol types, Automerge schema, or Python/TS bindings.
