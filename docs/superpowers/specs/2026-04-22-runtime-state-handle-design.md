# RuntimeStateHandle Design

## Status

- **Phase A**: Shipped (#2056). `runtime-doc` and `automunge` crates extracted.
- **Phase B**: Next. `RuntimeStateHandle` implementation + caller migration.
- **Phase C**: Future. Remove re-export shim from `notebook-doc`.

## Current State (post Phase A)

```
automunge (leaf, ~300 lines)
  └── automerge, serde_json

runtime-doc
  ├── automunge
  ├── automerge, serde, serde_json, tokio, tracing
  └── Owns: RuntimeStateDoc, RuntimeStateError, StreamOutputState, snapshot types

notebook-doc
  ├── automunge (for NotebookDoc's JSON operations)
  ├── runtime-doc (re-export shim only)
  └── Owns: NotebookDoc, cell operations, metadata, diff
```

`runtime-doc/src/handle.rs` is a placeholder. RuntimeStateDoc is still accessed via `Arc<tokio::sync::RwLock<RuntimeStateDoc>>` + manual `state_changed_tx.send(())` in the daemon.

## Phase B: RuntimeStateHandle

### The handle

```rust
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

#[derive(Clone)]
pub struct RuntimeStateHandle {
    doc: Arc<Mutex<RuntimeStateDoc>>,
    changed_tx: broadcast::Sender<()>,
}
```

`std::sync::Mutex`, not `tokio::sync::RwLock`. Automerge writes are microsecond-fast, pure in-memory. The `!Send` guard on `MutexGuard` makes holding it across `.await` a compile error.

### API

**`with_doc`** - synchronous mutation, auto-notification via heads comparison:

```rust
pub fn with_doc<F, T>(&self, f: F) -> Result<T, RuntimeStateError>
where
    F: FnOnce(&mut RuntimeStateDoc) -> Result<T, RuntimeStateError>,
```

**`fork`** - start async work (current heads only, never `fork_at`):

```rust
pub fn fork(&self, actor_label: &str) -> Result<RuntimeStateDoc, RuntimeStateError>
```

**`merge`** - complete async work, auto-notification:

```rust
pub fn merge(&self, fork: &mut RuntimeStateDoc) -> Result<(), RuntimeStateError>
```

**`read`** - read-only access (same mutex, no notification):

```rust
pub fn read<F, T>(&self, f: F) -> Result<T, RuntimeStateError>
where
    F: FnOnce(&RuntimeStateDoc) -> T,
```

**`subscribe`** - change notifications for peer sync loops:

```rust
pub fn subscribe(&self) -> broadcast::Receiver<()>
```

### Merge failure semantics

- `merge() -> Err`: document unchanged, no recovery needed (error fires before mutation in Automerge).
- Panic during merge apply: `std::sync::Mutex` poisons automatically. All subsequent calls return `Err(LockPoisoned)`. Room reconstructs via fresh doc + re-sync.
- No `rebuild_from_save()` in the handle. Save/load on a half-merged doc could persist bad state.

### fork_at avoidance

The handle exposes `fork()` only. `fork_at(historical_heads)` triggers `MissingOps` panics (automerge/automerge#1327). Whether this is an Automerge bug or a violated invariant is an open investigation.

### Caller migration

**NotebookRoom** replaces two fields with one:

```rust
// Before
pub state_doc: Arc<RwLock<RuntimeStateDoc>>,
pub state_changed_tx: broadcast::Sender<()>,

// After
pub state: RuntimeStateHandle,
```

**Sync writes** (95% of call sites):

```rust
// Before (4 steps)
let mut sd = room.state_doc.write().await;
if let Err(e) = sd.set_kernel_status("idle") {
    warn!("[runtime-state] {}", e);
}
let _ = state_changed_tx.send(());

// After (1 step)
if let Err(e) = room.state.with_doc(|sd| sd.set_kernel_status("idle")) {
    warn!("[runtime-state] {}", e);
}
```

**Batched writes** (atomic, single notification):

```rust
if let Err(e) = self.state.with_doc(|sd| {
    sd.create_execution(&eid, &cell_id)?;
    sd.set_queue(exec, &queued)?;
    Ok(())
}) {
    warn!("[runtime-state] {}", e);
}
```

**Fork/merge** (IOPub outputs):

```rust
let mut fork = state.fork(&actor_id)?;
// ... async blob work ...
state.merge(&mut fork)?;
```

### Files to modify

| File | Change |
|------|--------|
| `crates/runtime-doc/src/handle.rs` | Implement RuntimeStateHandle |
| `crates/runtime-doc/src/lib.rs` | Export RuntimeStateHandle |
| `crates/runtimed/Cargo.toml` | Add runtime-doc dependency |
| `crates/runtimed/src/notebook_sync_server/room.rs` | Replace two fields with handle |
| `crates/runtimed/src/kernel_state.rs` | Use handle |
| `crates/runtimed/src/jupyter_kernel.rs` | Use handle for writes + fork/merge |
| `crates/runtimed/src/runtime_agent.rs` | Use handle |
| `crates/runtimed/src/requests/*.rs` | Use handle |
| `crates/runtimed/src/notebook_sync_server/peer.rs` | Use handle |
| `crates/runtimed/src/notebook_sync_server/metadata.rs` | Use handle |
| `crates/runtimed/src/notebook_sync_server/mod.rs` | Use handle |

### Checkpoints

1. Handle implemented + unit tests pass (`cargo test -p runtime-doc`)
2. NotebookRoom switched, all callers migrated, `cargo build` clean
3. Full test suite + clippy + lint pass
4. `codex review`

## Phase C: Remove re-export shim (future)

Update downstream crates (`runtimed-wasm`, `notebook-sync`, `runt-mcp`, `runtimed-client`, `runtimed-py`) to import from `runtime-doc` directly. Delete `notebook-doc/src/runtime_state.rs`. Remove `runtime-doc` dep from `notebook-doc`.

## References

- **samod** (`alexjg/samod`): `DocHandle::with_document` uses `Arc<Mutex<>>` + closure API. `begin_modification()` checks readiness.
- **notebook-sync** (`crates/notebook-sync/src/handle.rs:199`): Our existing `DocHandle::with_doc` for NotebookDoc. Same pattern.
- **automerge core**: Merge is not transactional (`TODO: Make this fallible`). `Err` path is pre-mutation. Panic path has no rollback.
- **automorph** (`codeberg.org/dpp/automorph`): Read-before-write pattern. Future replacement for automunge when 0.8 ships.
