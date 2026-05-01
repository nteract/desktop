---
name: automerge-document-model
description: >
  Understand Automerge's internal document model: ops, changes, actors,
  the OpSet, ChangeGraph, save/load lifecycle, fork/merge semantics, and
  the AutoCommit wrapper. Use when debugging save/load issues, reasoning
  about document size, understanding fork/merge at the data structure
  level, diagnosing #1187-class panics, or evaluating why concurrent
  sync can corrupt indices.
---

# Automerge Document Model Internals

Use this skill when working at the level below sync: document structure,
change application, save/load round-trips, fork/merge, actor management,
or diagnosing panics in the op application pipeline. Complements
`automerge-sync` (which covers the sync protocol) with the data model
that sync operates on.

## The Five Core Types

### OpId — The Universal Identifier

Every operation has an `OpId(counter, actor_index)`. Counter is globally
monotonic per actor. Actor index is a position in the document's actor
table (not the ActorId bytes). OpIds are used as:

- Object identifiers (`ObjId` is a newtype over `OpId`)
- Element identifiers in lists (`ElemId` is a newtype over `OpId`)
- Operation references for predecessor tracking

The special `ROOT` OpId is `(0, 0)` — the implicit root Map object.
`HEAD` ElemId is also `(0, 0)` — the sentinel before position 0 in lists.

### ActorId — Peer Identity

A `TinyVec<[u8; 16]>` — 16 bytes inline (UUID-sized), heap-allocated
if larger. Lexicographic byte ordering is load-bearing: change encoding
depends on actors being sorted by their byte representation. nteract
uses UTF-8 encoded labels like `"runtimed"` or `"human:<session-uuid>"`.

**Actor table:** Each document maintains a `Vec<ActorId>` in `OpSet.actors`.
Ops store only the index (`ActorIdx`) into this table, not the full
ActorId bytes. This is why actor ordering matters — if two documents
disagree on which index maps to which actor, ops are misinterpreted.

### Change — A Batch of Operations

```rust
Change {
    stored: StoredChange<'static, Verified>,  // columnar-encoded ops
    compression: CompressionState,             // raw or DEFLATE
    len: usize,                                // number of ops
}
```

Key properties:
- `actor_id()` — the actor that created this change
- `seq()` — monotonic sequence number per actor (1, 2, 3, ...)
- `deps()` — change hashes this change depends on (causal parents)
- `hash()` — SHA-256 of the change bytes (the `ChangeHash`)
- `start_op()` — first OpId counter in this change
- `max_op()` — `start_op + len - 1`

Changes are **causally ordered**: a change can only be applied after
all changes in its `deps` have been applied. Changes that arrive before
their deps go into the `ChangeQueue` (pending).

### ChangeGraph — The DAG of History

```rust
ChangeGraph {
    edges: Vec<Edge>,                           // child→parent edges
    hashes: Vec<ChangeHash>,                    // one per node
    actors: Vec<ActorIdx>,                      // actor of each change
    parents: Vec<Option<EdgeIdx>>,              // first edge per node
    seq: Vec<u32>,                              // seq per change
    max_ops: Vec<u32>,                          // cumulative op count
    heads: BTreeSet<ChangeHash>,                // current heads
    nodes_by_hash: HashMap<ChangeHash, NodeIdx>,// hash→node lookup
    clock_cache: HashMap<NodeIdx, SeqClock>,    // cached vector clocks
    seq_index: Vec<Vec<NodeIdx>>,               // per-actor change list
}
```

This is the core structure for:
- **Heads:** The `heads` set = changes with no children. This is what
  sync messages advertise and what `get_heads()` returns.
- **Causal ordering:** `deps_for_hash()` walks parent edges.
- **Change lookup:** `has_change()`, `get_hash_for_actor_seq()`.
- **Clock computation:** Vector clock from any change, cached every
  16 changes for efficiency.

**nteract usage:** `get_change_by_hash()` is the containment check
behind `required_heads` — the daemon checks whether each listed hash
exists in the ChangeGraph before processing a request.

### OpSet — The Materialized Document

```rust
OpSet {
    actors: Vec<ActorId>,    // actor table
    obj_info: ObjIndex,      // object metadata index
    cols: Columns,           // columnar op storage
    text_encoding: TextEncoding,
}
```

The OpSet stores all applied operations in a columnar format (using
the `hexane` crate). Operations are stored sorted by `(object, key,
lamport_timestamp)` for efficient querying. The `ObjIndex` tracks
which objects exist, their types, and byte ranges in the columnar store.

**This is what save/load round-trips reconstruct.** `save()` serializes
the OpSet + ChangeGraph into a compact `Document` format. `load()`
deserializes and rebuilds the OpSet from the stored columns.

## The Automerge Struct

```rust
Automerge {
    queue: ChangeQueue,        // pending changes (deps not yet met)
    change_graph: ChangeGraph, // full history DAG
    deps: HashSet<ChangeHash>, // current heads (fast access)
    ops: OpSet,                // materialized operations
    actor: Actor,              // current actor (Unused or Cached index)
}
```

### save() and load()

**save()** produces a single `Document` chunk:
1. Serializes the OpSet columns (all operations, sorted)
2. Serializes the ChangeGraph metadata (actors, hashes, deps, seqs)
3. Optionally DEFLATE-compresses
4. Appends any orphaned (queued) changes as raw change chunks

**load()** rebuilds from the Document chunk:
1. Parses the columnar data
2. Reconstructs the OpSet (actors, columns, object index)
3. Reconstructs the ChangeGraph (nodes, edges, heads)
4. Verifies head hashes match (unless `VerificationMode::DontCheck`)
5. Processes any trailing change chunks via `load_incremental`

**Key insight for nteract:** The save/load round-trip reconstructs a
fresh OpSet from columnar data. This clears any corrupted in-memory
indices (like the actor table ordering issue in #1187) because the
columnar format is the canonical representation. That's why nteract's
`rebuild_from_save()` works as a recovery mechanism.

### load_incremental()

Adds changes to an existing document:
1. If document is empty, delegates to `load()` (more efficient)
2. Parses change chunks from the input
3. Calls `apply_changes_log_patches()` for causal application
4. Returns the number of new ops applied

This is what `receive_sync_message` calls internally — sync messages
carry changes that get applied via `load_incremental`.

### save_after(heads)

Saves only changes *after* the given heads — returns raw change chunks,
not a Document format. Useful for incremental saves. `save_incremental()`
(on AutoCommit) uses this with the `save_cursor` to emit only new changes.

## Fork and Merge

### fork()

```rust
pub fn fork(&self) -> Self {
    let mut f = self.clone();      // full deep clone
    f.set_actor(ActorId::random()); // new actor identity
    f
}
```

A fork is a complete clone of the document with a new actor ID. Both
documents share the same history up to the fork point. Mutations on
either side create changes with different actors, so they compose
cleanly on merge (no counter collisions).

**Cost:** O(document size) — the entire OpSet, ChangeGraph, and queue
are cloned. For large notebooks, this is significant.

### fork_at(heads)

Creates a new document containing only changes up to `heads`:
1. Walks backward from `heads` through the ChangeGraph collecting hashes
2. Creates a fresh `Automerge::new()`
3. Extracts changes for those hashes and applies them

**Much more expensive than fork()** — it doesn't clone; it replays
changes from scratch. Used for time-travel views. nteract avoids this
in production paths due to automerge/automerge#1327.

### merge(other)

```rust
pub fn merge(&mut self, other: &mut Self) -> Result<Vec<ChangeHash>, AutomergeError> {
    let changes = self.get_changes_added(other);
    self.apply_changes_log_patches(changes, &mut PatchLog::inactive())?;
    Ok(self.get_heads())
}
```

Merge extracts changes from `other` that `self` doesn't have and applies
them. After merge, `self` contains all ops from both documents.

**The DuplicateSeqNumber trap:** If two forks share the same ActorId
(because `fork()` clones the actor and the caller forgot to change it),
both will produce changes with the same `(actor, seq)` pair. The second
merge returns `DuplicateSeqNumber`. nteract's `fork_with_actor()` exists
specifically to prevent this.

## The AutoCommit Wrapper

nteract uses `AutoCommit`, not raw `Automerge`:

```rust
AutoCommit {
    doc: Automerge,                              // inner document
    transaction: Option<(PatchLog, TransactionInner)>,  // open tx
    patch_log: PatchLog,                         // diff tracking
    diff_cursor: Vec<ChangeHash>,                // last-diffed heads
    save_cursor: Vec<ChangeHash>,                // last-saved heads
    isolation: Option<Vec<ChangeHash>>,          // isolation heads
}
```

**Auto-transaction:** Mutations open a transaction implicitly. The
transaction accumulates ops. Calling any read method, `save()`, `fork()`,
`merge()`, or `sync()` first calls `ensure_transaction_closed()`, which
commits the pending transaction and advances the document heads.

**Isolation mode:** `isolate(heads)` limits the visible document to a
specific point in history. `integrate()` returns to the latest heads.
Mutations while isolated create changes that depend on the isolation
heads, not the document tips.

**PatchLog:** Tracks diffs between `diff_cursor` and current heads for
incremental materialization. `diff_incremental()` returns patches and
advances the cursor. This is how nteract's WASM side computes
`CellChangeset` — by diffing the PatchLog after receiving sync frames.

## Why #1187 Panics Happen

The panic occurs in `BatchApply::apply()` during `PatchLog::migrate_actors()`:

1. **Setup:** Two peers sync concurrently. Peer A introduces actor X;
   Peer B introduces actor Y.
2. **Actor ordering:** The actor table must be sorted lexicographically.
   When a new actor is inserted, existing PatchLog entries contain
   OpIds with actor indices that may shift.
3. **migrate_actors()** tries to rewrite PatchLog entries to match the
   new actor ordering. It expects actors to only be appended in sorted
   order. If concurrent sync messages interleave such that the PatchLog
   sees actor indices that don't match the OpSet's actor table order,
   `migrate_actors()` returns `PatchLogMismatch`.
4. **The unwrap:** `log.migrate_actors(&doc.ops().actors).unwrap()` in
   `BatchApply::apply()` (line 807) panics on mismatch.

**Why save/load fixes it:** After a save/load round-trip:
- The OpSet's columnar data re-sorts actors correctly
- The PatchLog is empty (no pending diffs)
- The ChangeGraph is rebuilt from column metadata
- `sync::State::new()` starts a fresh sync handshake

**Why catch_unwind is needed:** The panic happens inside `receive_sync_message`
→ `load_incremental` → `apply_changes` → `BatchApply::apply`. Since this
is an automerge bug (not a logic error in nteract), catching and rebuilding
is the correct workaround until upstream fixes the actor migration logic.

## Document Size Factors

Understanding what makes documents grow:

| Factor | Growth pattern | Impact |
|--------|---------------|--------|
| Operations (puts, splices, deletes) | O(total mutations) | Largest factor |
| Actor table | O(unique peers) | Small per entry, but affects all OpIds |
| ChangeGraph | O(total changes) | Metadata overhead per change |
| Tombstones (deletes) | Accumulate forever | Can dominate in heavily-edited text |
| Change queue | O(out-of-order changes) | Temporary; resolved when deps arrive |

**save() compacts:** The Document format is significantly smaller than
the in-memory representation because it uses columnar encoding with
delta and run-length compression.

**Decompaction is not built-in:** Automerge doesn't garbage-collect
tombstones or squash history. To compact, you'd need to create a new
document and replay only the current visible state — losing history.

## nteract-Specific Usage Patterns

### Per-Cell O(1) Accessors

nteract's WASM bindings use direct Automerge map lookups:
`get_cell_source(id)`, `get_cell_type(id)`, etc. These work because
cells are keyed by ID in an Automerge Map. The OpSet's `ObjIndex`
provides O(1) object lookup; reading a specific key in a map is
O(log n) in the key's op history (usually small).

### Dual Documents

Each notebook room has two Automerge documents:
1. **NotebookDoc** — bidirectional, contains cell content
2. **RuntimeStateDoc** — daemon-authoritative, contains outputs/state

Each has its own OpSet, ChangeGraph, actor table, and sync::State.
save/load rebuilds work independently on each.

### fork_and_merge for Synchronous Mutations

```rust
doc.fork_and_merge(|fork| {
    fork.update_source("cell-1", "x = 1\n");
});
```

Safe for synchronous blocks — actor collision is harmless because
merge completes before any concurrent fork exists. No unique actor
needed.

### fork_with_actor for Async Mutations

```rust
let mut fork = doc.fork_with_actor("runtimed:iopub:kernel-abc");
// ... async work ...
fork.set_outputs(cell_id, outputs);
doc.merge(&mut fork)?;
```

Required when the fork crosses an `.await` — another fork might exist
concurrently, and shared actors cause DuplicateSeqNumber on merge.

## Decision Framework

| Situation | Approach |
|-----------|----------|
| Need to recover from corrupted indices | `save()` → `load()` round-trip (rebuilds OpSet from columns) |
| Need to check if peer has specific changes | `change_graph.has_change(&hash)` (O(1) hash lookup) |
| Need document at earlier point | `fork_at(heads)` — expensive, avoid in hot paths |
| Need concurrent async mutations | `fork_with_actor()` — unique actor per concurrent fork |
| Need synchronous batch mutation | `fork_and_merge()` — shared actor is safe |
| Need to shrink document bytes | `save()` uses columnar + DEFLATE; no history compaction available |
| Need to understand document size | Count ops, actors, tombstones; save() gives compressed size |
| Debugging a panic in apply | Check actor table ordering; likely #1187-class bug; catch_unwind + rebuild |
| Need incremental save for wire | `save_after(heads)` for changes since last sync point |

## Key Source Files

| File | What it teaches |
|------|----------------|
| `automerge/src/automerge.rs` | `Automerge` struct, fork/merge/save/load, change application |
| `automerge/src/autocommit.rs` | `AutoCommit` wrapper, auto-transaction, isolation, PatchLog |
| `automerge/src/change.rs` | `Change` struct, actor_id/seq/deps/hash accessors |
| `automerge/src/change_graph.rs` | `ChangeGraph` DAG, heads, clock computation, causal queries |
| `automerge/src/op_set2/op_set.rs` | `OpSet` columnar storage, actor table, object index |
| `automerge/src/op_set2/change/batch.rs` | `BatchApply::apply`, actor migration, the #1187 panic site |
| `automerge/src/patches/patch_log.rs` | `PatchLog`, `migrate_actors` — the function that panics |
| `automerge/src/types.rs` | `OpId`, `ActorId`, `ObjId`, `ElemId`, `OpType` |
| `automerge/src/storage/` | Columnar encoding, Document format, change parsing |
| `notebook-doc/src/lib.rs` | nteract's `NotebookDoc` wrapper: fork/merge/save/load/rebuild |
| `notebook-sync/src/shared.rs` | `SharedDocState`, `rebuild_state_doc`, dual-doc management |
| `notebook-sync/src/sync_task.rs` | catch_unwind guards, `rebuild_shared_doc_state` with cell-count guard |
