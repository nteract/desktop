---
name: sync-protocol-patterns
description: >
  Design patterns for sync protocols learned from automerge-repo and samod.
  Use when designing new protocol features, adding sync streams, changing
  causal ordering, or evaluating how upstream patterns could improve nteract.
  Complements automerge-sync (which covers the raw automerge sync protocol)
  with higher-level architectural patterns.
---

# Sync Protocol Patterns

Use this skill when designing protocol features, not just fixing bugs.
It distills architectural patterns from three production sync systems
and maps them to nteract's design decisions.

## Three Architectures Compared

### automerge-repo: Document-Centric, Transport-Agnostic

**Architecture:** `Repo` owns document lifecycle. `DocHandle` is the
mutation/event surface. `DocSynchronizer` manages per-peer sync. Network
adapters are pluggable message transports.

**Key pattern — Remote Heads Subscriptions:**

automerge-repo tracks *where* each peer's heads are, not just *that*
they changed. `RemoteHeadsSubscriptions` is a pub/sub system where:

- Each peer has a `StorageId` (stable identity across reconnections)
- Peers subscribe to specific `StorageId`s to track their heads
- When a peer's heads change, all subscribers are notified
- `handleImmediateRemoteHeadsChanged()` handles directly-connected peers
- `handleRemoteHeads()` handles transitively-relayed updates

This enables "I know peer X has seen my changes" without a blocking
round-trip — the answer arrives as a notification, not a response.

**nteract parallel:** This is what `required_heads` achieves for execute
requests. The daemon knows the client's heads from the sync stream and
can defer handling until the heads arrive. A future enhancement could
make this more general: the daemon could proactively notify clients when
it has processed specific heads, enabling fire-and-forget causal ordering.

**Key pattern — Document State Machine:**

DocHandle has an XState machine: `idle → loading → requesting → ready |
unavailable | unloaded | deleted`. Documents aren't immediately usable —
they must be loaded from storage or fetched from peers first.

nteract's DocHandle is simpler: always "ready" once constructed. This
works because nteract always connects to one known daemon (not a mesh of
peers), so the initial sync is a single handshake, not a discovery
protocol. If nteract ever needs offline-first or peer-to-peer sync, the
state machine pattern becomes necessary.

### samod/subduction: Sans-IO, Signed, Metadata-First

**Architecture:** `SubductionEngine` is a pure sans-IO state machine:
`handle(Input<C>) -> EngineOutput<C>`. No threads, no async, no IO.
The caller executes IO and feeds results back.

**Key pattern — Sans-IO State Machine:**

The engine accepts typed input events and returns output actions:

```rust
pub enum Input<C> {
    NewConnection { id: C, outgoing: bool, ... },
    ReceivedBytes { id: C, bytes: Vec<u8>, ... },
    ConnectionLost { id: C },
    SigningComplete { op_id: OpId, signature: ... },
    StorageComplete { op_id: OpId, result: ... },
    FindDocument { sed_id: ... },
    NewLocalChanges { sed_id: ..., changes: ... },
}

pub struct EngineOutput<C> {
    pub send: Vec<(C, Vec<u8>)>,
    pub storage_ops: Vec<IssuedOp>,
    pub sign_requests: Vec<SignRequest>,
    pub data_for_docs: Vec<(SedimentreeId, Vec<Vec<u8>>)>,
    pub search_status: Vec<(SedimentreeId, SearchStatus)>,
}
```

**Why this matters for nteract:** nteract's sync task is async Rust with
a biased `select!` loop. This works but makes testing harder — you need
real tokio runtimes and mock IO. A sans-IO core would let you test
protocol logic with pure function calls. Worth considering for any new
complex protocol state (e.g., multi-peer coordination, pool sync).

**Key pattern — Two-Phase Sync (Batch + Incremental):**

samod separates sync into two phases:

1. **Batch sync** (initial): Fingerprint-based reconciliation in 1.5
   round trips. The requestor sends a compact fingerprint summary; the
   responder computes the diff and sends missing data.
2. **Incremental sync** (steady-state): Subscription-based push. After
   batch sync establishes a baseline, peers subscribe to receive live
   updates as commits/fragments arrive.

The `IncrementalSync` type ties together:
- `SubscriptionTracker`: which peers watch which documents
- `PeerCounter`: per-peer monotonic send counters
- `RemoteHeadsTracker`: filters stale updates via counters

**nteract parallel:** nteract's initial sync handshake (automerge bloom
filter exchange) is the batch phase. The subsequent `AutomergeSync`
frame stream is the incremental phase. But nteract doesn't have the
subscription/counter infrastructure — if it ever needs to sync across
multiple daemon instances or support peer-to-peer, samod's model shows
how to layer incrementality on top.

**Key pattern — Storage Coordinator:**

`StorageCoordinator` manages multi-step async IO operations without
async/await. It:
- Issues `KvOp`s with `OpId` tracking
- Handles multi-phase operations (load commit → load blob → assemble)
- Maps sub-task completions to parent operations
- Returns `StorageComplete` when all constituent tasks finish

This is the same pattern as nteract's `confirm_sync` waiters and
`required_heads` — tracking pending async work and resolving when
conditions are met — but generalized to arbitrary multi-step operations.

**Key pattern — Per-Peer Monotonic Counters:**

Every incremental message carries a per-peer counter. Receivers filter
stale/out-of-order messages: if `incoming_counter <= last_seen`, drop it.
Counters are cleared on disconnect and restart from 1 on reconnect.

nteract doesn't currently need this (single daemon, ordered TCP stream),
but it would be essential for:
- Multiple daemon replicas
- WebSocket transport with potential reordering
- Multi-peer sync where messages may arrive out of causal order

### nteract: Socket-Based, CRDT-Centric

**Architecture:** Direct Unix socket connection. Length-prefixed typed
frames. Two CRDT sync streams (notebook + runtime state) on one
connection. Daemon as authoritative server, clients as syncing peers.

**Where nteract is ahead:**
- `required_heads` is a clean causal ordering primitive that neither
  automerge-repo nor samod has in exactly this form
- The catch_unwind + rebuild pattern handles automerge bugs gracefully
- Per-cell O(1) WASM accessors avoid full-doc materialization

**Where nteract could learn from upstream:**
- automerge-repo's document state machine for handling offline/loading
  states more gracefully
- samod's sans-IO pattern for testable protocol logic
- samod's subscription tracker for future multi-peer scenarios

## Heads Tracking Across Ecosystems

How each system answers "has the remote peer seen my changes?"

| System | Mechanism | Blocking? | Granularity |
|--------|-----------|-----------|-------------|
| automerge (raw) | `shared_heads` in sync::State | No — updated on each message exchange | Per-peer |
| automerge-repo | `RemoteHeadsSubscriptions` with StorageId tracking | No — notification-based | Per-storage-peer |
| samod | `RemoteHeads` with monotonic per-peer counters | No — carried on every data message | Per-peer per-document |
| nteract `confirm_sync` | Client-side waiter on `shared_heads` | Client blocks, daemon free | Per-request |
| nteract `required_heads` | Daemon-side waiter on `get_change_by_hash` | Daemon defers, client free | Per-request |

**Key insight:** All systems converge on the same principle: **track heads
per peer, notify asynchronously, never block the sync loop.** But only
nteract gates *actions* on causal preconditions. automerge-repo's
`RemoteHeadsSubscriptions` is peer-awareness ("I know where you are"),
and samod's `RemoteHeads` on every data message is informational with
staleness filtering (counter <= last_seen → drop) — neither system
defers request processing until specific heads arrive. nteract's
`required_heads` is genuinely novel: request-scoped causal gating where
the daemon defers one specific request until its preconditions are met,
while the sync stream continues unblocked. This eliminates the
client-side round-trip that `confirm_sync` required.

## Connection Lifecycle Patterns

How each system handles disconnect → reconnect:

| System | On Disconnect | On Reconnect | State Preserved |
|--------|--------------|-------------|-----------------|
| automerge-repo | `endSync(peerId)` — removes from active list, keeps sync state | `beginSync` — encode/decode to clear in-flight, preserves shared_heads | Sync state (for same peer) |
| samod | `handle_connection_lost` — remove connection, call `peer_disconnected` | New connection → fresh handshake + batch sync | Nothing — clean slate |
| nteract | `MarkDisconnected` → clear session → stash target | `rejoin()` → new `sync::State::new()` + full handshake | Session identity (notebook_id/path) but not sync state |

**Design space:** nteract currently takes the "clean slate" approach for
sync state (like samod) but preserves session identity (like automerge-repo
preserves sync state). If latency on reconnect becomes a problem, the
automerge-repo approach (preserve `shared_heads` via encode/decode) could
reduce the initial sync burst.

## When Adding a New Sync Stream

If you need to add a new Automerge-synced document to the protocol (like
PoolStateSync frame `0x06`):

1. **Allocate a frame type** in `notebook-wire`
2. **Add sync state fields** to `SharedDocState` (doc + peer_state pair)
3. **Add catch_unwind guards** for both receive and generate paths
4. **Add a rebuild function** following the save→load→reset pattern
5. **Update the biased select loop** — decide priority relative to
   existing streams
6. **Consider subscription scope** — does every peer need this stream,
   or only specific consumers?
7. **Test with concurrent mutation** — the automerge#1187 panic only
   manifests under concurrent sync, so single-peer tests miss it

## Design Decision Checklist

When making protocol design decisions:

| Question | Pattern to apply |
|----------|-----------------|
| "Should this block the client or the daemon?" | Prefer daemon-side waits (required_heads pattern) |
| "How do I know the remote has my changes?" | Track heads per peer, don't block the sync loop |
| "Should I add a new frame type?" | Only if it's a separate Automerge doc or needs distinct priority |
| "How do I handle disconnection?" | Reset transport state, preserve document truth |
| "Should protocol logic be async?" | Consider sans-IO for testability (samod pattern) |
| "How do I order operations causally?" | Attach heads to requests, let the receiver defer |
| "Should state survive reconnection?" | Only shared_heads; all other sync state is session-ephemeral |

## Key Upstream Source Files

| File | What it teaches |
|------|----------------|
| `automerge-repo/src/RemoteHeadsSubscriptions.ts` | Pub/sub heads tracking across peers |
| `automerge-repo/src/DocHandle.ts` | Document state machine (idle → ready) |
| `automerge-repo/src/synchronizer/DocSynchronizer.ts` | Per-peer sync state management |
| `automerge-repo/src/network/messages.ts` | Message taxonomy (sync, request, ephemeral, remote-heads-changed) |
| `samod/subduction-sans-io/src/engine.rs` | Sans-IO protocol engine pattern |
| `samod/subduction-sans-io/src/incremental.rs` | Subscription + counter-based live sync |
| `samod/subduction-sans-io/src/batch_sync.rs` | Fingerprint-based batch reconciliation |
| `samod/subduction-sans-io/src/storage_coord.rs` | Multi-step async IO without async/await |
| `samod/subduction-sans-io/src/messages.rs` | Binary wire format with schema+size+tag envelope |
