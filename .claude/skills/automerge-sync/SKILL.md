---
name: automerge-sync
description: >
  Understand and work with Automerge sync protocol internals. Use when
  debugging sync failures, changing reconnection logic, adding new sync
  streams, or reasoning about why peers converge (or don't). Covers
  sync::State lifecycle, bloom filters, in-flight suppression, and the
  nteract catch_unwind recovery pattern.
---

# Automerge Sync Internals

Use this skill when working on sync behavior: reconnection, peer state,
convergence bugs, new frame types, or catch_unwind recovery. This encodes
knowledge from reading the actual automerge sync implementation, not just
the docs.

## How Automerge Sync Actually Works

The protocol is a back-and-forth negotiation between two peers over a
reliable in-order stream. Each peer maintains a `sync::State` per remote
peer. The loop is:

1. Initiator calls `generate_sync_message()` with an empty `State`
2. Receiver calls `receive_sync_message()` then `generate_sync_message()`
3. Repeat until both return `None` (converged)

**Source:** `rust/automerge/src/sync.rs` in `automerge/automerge`

### What's In a Sync Message

```
Message {
    heads: Vec<ChangeHash>,        // "here's what I have"
    need: Vec<ChangeHash>,         // "I'm missing these specific changes"
    have: Vec<Have>,               // bloom filter of what I have since last_sync
    changes: ChunkList,            // actual change data to apply
    flags: Option<MessageFlags>,   // capabilities + transient signals
    version: MessageVersion,       // V1 or V2
}
```

The `Have` struct contains `last_sync` (heads at last successful sync) and a
`BloomFilter` summarizing changes since then. The bloom filter has 1% false
positive rate (10 bits/entry, 7 probes). False positives mean "I probably
have this" -- the sender sends a change only if all peer bloom filters say
they *don't* have it.

### The `sync::State` Fields

13 fields, but only `shared_heads` survives `encode()`/`decode()`:

| Field | Purpose | Persists? |
|-------|---------|-----------|
| `shared_heads` | Hashes both peers agree they share | Yes |
| `last_sent_heads` | Our heads when we last sent a message | No |
| `their_heads` | Their most recently advertised heads | No |
| `their_need` | Specific changes they requested | No |
| `their_have` | Their bloom filter summaries | No |
| `sent_hashes` | Changes we've already sent this session | No |
| `in_flight` | True while awaiting ack (suppresses duplicate sends) | No |
| `have_responded` | True after we've sent at least one message | No |
| `their_capabilities` | MessageV2, SyncReset support | No |
| `read_only` | We ignore their changes | No |
| `peer_read_only` | They ignore our changes | No |
| `needs_reset` | Next message gets SyncReset flag | No |

**Critical insight:** `encode()` only serializes `shared_heads`. Everything
else is session-ephemeral. This means:

- `sync::State::new()` is safe for reconnection -- you lose optimization
  (resend changes the peer already has) but never lose correctness
- Persisting encoded sync state is only useful when reconnecting to the
  *same* peer identity
- The automerge-repo `beginSync` hack (encode/decode round-trip) exists
  solely to clear `in_flight` and `sent_hashes` while preserving
  `shared_heads`

### In-Flight Suppression

`generate_sync_message()` returns `None` when `in_flight` is `true` AND
`last_sent_heads == our_heads` AND `have_responded` is `true`. This
prevents duplicate messages while awaiting an ack.

`receive_sync_message()` always sets `in_flight = false` on entry -- any
incoming message counts as an ack.

**Mistake to avoid:** Don't try to work around `None` returns from
`generate_sync_message()`. The suppression is correct. If you need to
force a fresh exchange, reset the sync state.

### How Changes Are Selected

`generate_sync_message()` in `Automerge`:

1. Compute `our_need` = missing deps from their advertised heads
2. Build a bloom filter from our changes since `shared_heads`
3. If they have `their_have` + `their_need`, compute which changes to send:
   - Filter through their bloom filter (send what they probably don't have)
   - Deduplicate against `sent_hashes`
   - If sending >1/3 of the doc, send the whole doc as V2 (more efficient)
4. The `Message.heads` we send becomes their next `their_heads` for us

### How Changes Are Applied

`receive_sync_message_inner()`:

1. Set `in_flight = false` (ack)
2. Process `MessageFlags` -- discover capabilities, handle SyncReset
3. If changes present and not read-only, `load_incremental` them
4. Advance `shared_heads` based on what we both now have
5. Trim `sent_hashes` to only changes they haven't seen
6. **Empty heads detection:** If they send `heads: []`, reset
   `last_sent_heads` and `sent_hashes` to trigger full resync (they lost
   all data)

### Version Negotiation

V1 is the original format. V2 allows compressed document encoding (faster
first sync). Backward-compatible: V1 messages append a `MessageFlags`
section that old implementations ignore. New implementations read the flags
to discover V2 support. Once discovered, subsequent messages use V2.

## nteract's Sync Architecture

nteract runs two sync streams over one socket connection:

| Stream | Frame | Document | Ownership |
|--------|-------|----------|-----------|
| Notebook | `0x00` AutomergeSync | `SharedDocState.doc` | Bidirectional |
| RuntimeState | `0x05` RuntimeStateSync | `SharedDocState.state_doc` | Daemon-authoritative |

Each stream has its own `sync::State` (`peer_state` and `state_peer_state`).
They share the same `Arc<Mutex<SharedDocState>>` but operate independently.

### The Sync Task Loop

`sync_task.rs` runs a biased `tokio::select!` with priority:

1. **Frame** (incoming daemon frames) -- highest, keeps socket drained
2. **Changed** (local mutations) -- generates outbound sync
3. **Command** (requests, confirm_sync) -- daemon RPC
4. **Maintenance** (50ms tick) -- watchdog, confirm_sync retries

The mutex is `std::sync::Mutex` (not tokio), never held across `.await`.
Recovery from mutex poisoning: `unwrap_or_else(|e| e.into_inner())`.

### catch_unwind Recovery Pattern

automerge 0.7 has a known panic in `BatchApply::apply` when the internal
patch log actor table gets out of order during concurrent sync
(automerge/automerge#1187). Both sync handlers use the same pattern:

```rust
// Step 1: catch_unwind around receive_sync_message
let recv_result = std::panic::catch_unwind(AssertUnwindSafe(|| {
    state.receive_sync_message(msg)
}));
match recv_result {
    Ok(Ok(())) => {}
    Ok(Err(e)) => { warn!(...); return; }
    Err(_panic) => {
        rebuild_shared_doc_state(&mut state);
        return;
    }
}

// Step 2: catch_unwind around generate_sync_message (can also panic)
match std::panic::catch_unwind(AssertUnwindSafe(|| {
    state.generate_sync_message().map(|msg| msg.encode())
})) {
    Ok(bytes) => bytes,
    Err(_) => {
        rebuild_shared_doc_state(&mut state);
        None
    }
}
```

**Rebuild for notebook doc** (`rebuild_shared_doc_state`):
1. `save()` the doc to bytes
2. `AutoCommit::load()` from those bytes (clears corrupted indices)
3. **Cell-count guard:** if rebuilt doc has fewer cells, skip rebuild
   (only reset sync state) to prevent silent cell loss
4. Preserve actor ID
5. `peer_state = sync::State::new()` to force fresh handshake

**Rebuild for RuntimeStateDoc** (`rebuild_state_doc`):
1. Round-trip `save()`/`load()` via `rebuild_from_save()`
2. `state_peer_state = sync::State::new()`

Both follow the principle: **reset transport state, preserve document truth.**

### Why Resetting `sync::State` Works

When you set `peer_state = sync::State::new()`:
- `shared_heads` becomes `[]` (empty)
- `in_flight` becomes `false`
- `have_responded` becomes `false`

This means the next `generate_sync_message()` will:
- Send our current heads
- Build a bloom filter from *all* our changes (since shared_heads is empty)
- The daemon will respond with any changes we're missing

The cost is bandwidth (may resend changes the peer already has), but
correctness is guaranteed. The bloom filter negotiation quickly converges
to only sending what's actually missing.

## Reconnection Patterns

### nteract: Fresh State

nteract uses `sync::State::new()` on every reconnection. This is correct
because:
- The daemon may have received changes from other peers during disconnect
- The client may have applied local mutations during disconnect
- A fresh handshake discovers what's missing from both sides
- `SharedDocState::try_new()` initializes both `peer_state` and
  `state_peer_state` as `sync::State::new()`

### automerge-repo: Encode/Decode Round-Trip

automerge-repo's `DocSynchronizer.beginSync()` does:
```typescript
const reparsedSyncState = A.decodeSyncState(A.encodeSyncState(syncState))
```

This preserves `shared_heads` but clears all session-ephemeral fields.
The comment calls it a "HACK" to prevent infinite loops from failed
in-flight messages. It's actually using the designed encode/decode contract
correctly -- `encode()` only serializes what should survive reconnection.

### samod: Remove and Forget

samod's `SubductionEngine.handle_connection_lost()` removes the connection
and calls `incremental.peer_disconnected(peer_id)`. No sync state is
preserved. Each new connection starts fresh. This is the simplest correct
approach.

## Common Mistakes

### 1. Sharing sync::State across peers

Each remote peer needs its own `sync::State`. The state tracks what *that
specific peer* has told us. Sharing it between peers causes:
- Duplicate sends (already sent to peer A, but State thinks peer B needs it)
- Missing sends (peer B never told us their heads, but State has peer A's)
- In-flight suppression for the wrong peer

### 2. Expecting generate_sync_message to always return something

After local mutations, `generate_sync_message()` may return `None` if
there's an in-flight unacked message. This is correct behavior.

### 3. Blocking the frame reader

The sync task must keep draining incoming frames. Any command handler that
blocks waiting for a specific response starves broadcasts, state sync,
and sync replies. Use waiters/pending-request registration instead.

### 4. Forgetting catch_unwind on new sync handlers

If you add a new Automerge sync stream (e.g., PoolStateSync `0x06`),
it needs the same catch_unwind + rebuild pattern. Any `receive_sync_message`
or `generate_sync_message` call on a concurrently-synced document is
vulnerable to automerge#1187.

### 5. Skipping the cell-count guard

`save()` on a panic-corrupted doc may drop ops, producing fewer cells.
The guard in `rebuild_shared_doc_state` prevents silent cell loss by
falling back to sync-state-only reset when the rebuilt doc has fewer cells.

### 6. Holding the mutex across I/O

The lock scope must be a block that drops before any `.await`. Compute
the ack bytes inside the lock, send them outside it.

### 7. Resetting sync state when you should preserve it

Don't reset sync state just because local mutations happened. Reset it
when the *transport* breaks (reconnect, panic recovery). Resetting
after every mutation would make every sync round a full exchange.

## Confirm Sync Design

`confirm_sync` verifies the daemon has merged specific local changes:

1. Caller captures current heads via `DocHandle::confirm_sync()`
2. A `ConfirmSync` command goes to the sync task with target heads
3. The sync task registers target heads as a waiter
4. Normal inbound `AutomergeSync` handling checks waiters after each receive
5. When `shared_heads` include all target heads, the waiter resolves
6. Timeout: 10s total, 200ms retry ticks

This is non-blocking -- the frame loop keeps draining while the waiter
resolves in the background.

## Key Source Files

| File | What to read |
|------|-------------|
| `automerge/src/sync.rs` | `generate_sync_message`, `receive_sync_message_inner`, `advance_heads` |
| `automerge/src/sync/state.rs` | `State` struct, `encode`/`decode`, `set_read_only` |
| `automerge/src/sync/bloom.rs` | `BloomFilter`, 1% FP rate params |
| `automerge-repo/.../DocSynchronizer.ts` | Per-peer `#syncStates`, `beginSync` encode/decode hack |
| `samod/subduction-sans-io/src/engine.rs` | `handle_connection_lost` -- simplest correct reconnection |
| `crates/notebook-sync/src/sync_task.rs` | Biased select, catch_unwind, rebuild |
| `crates/notebook-sync/src/shared.rs` | `SharedDocState`, dual sync states |

## Decision Framework

When you need to decide how to handle sync state:

| Situation | Action |
|-----------|--------|
| Transport disconnect + reconnect | Reset sync::State (new() or encode/decode round-trip) |
| automerge panic caught | Rebuild doc (save/load), reset sync::State |
| Local mutation happened | Do nothing -- next generate_sync_message handles it |
| Adding a new sync stream | New frame type, new sync::State field, catch_unwind guard |
| Peer lost all data (empty heads) | Already handled -- receive_sync_message resets sent_hashes |
| Switching read-only to read-write | set_read_only() handles reset + SyncReset flag |
