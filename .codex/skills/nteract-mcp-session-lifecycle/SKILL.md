---
name: nteract-mcp-session-lifecycle
description: Use when designing, reviewing, or changing MCP session management, daemon reconnection, notebook switching, or the interaction between tool dispatch and background session state. Covers the full state machine from process start through disconnect, rejoin, and concurrent multi-client operation.
---

# nteract MCP Session Lifecycle

Use this skill when a change touches session state, daemon reconnection,
notebook switching, or any path where tool calls and background events interact
with `Arc<RwLock<Option<NotebookSession>>>`. Treat this as a design guardrail
before editing; then read the automerge-protocol and notebook-sync skills for
sync-layer specifics.

## Source Map

### nteract crates

| File | Role |
|------|------|
| `crates/runt-mcp/src/session.rs` | `NotebookSession` struct, `SessionDropInfo`, `SessionDropReason` |
| `crates/runt-mcp/src/daemon_watch.rs` | `watch()` loop, `classify()` decision function, `rejoin()` |
| `crates/runt-mcp/src/tools/session.rs` | `open_notebook`, `create_notebook`, `save_notebook`, `disconnect_previous_session` |
| `crates/runt-mcp/src/lib.rs` | `NteractMcp` server, `session: Arc<RwLock<Option<NotebookSession>>>` |
| `crates/runt-mcp-proxy/src/proxy.rs` | `McpProxy`, `restart_child()`, `track_session()`, `REJOIN_ENV_VAR` |
| `crates/notebook-sync/src/handle.rs` | `DocHandle` — clone-able, `with_doc()` sync mutation, `send_request()` async RPC |
| `crates/notebook-sync/src/sync_task.rs` | Background network I/O loop, catch_unwind for AutomergeSync and RuntimeStateSync |
| `crates/notebook-sync/src/shared.rs` | `SharedDocState` — Automerge doc + per-peer sync state + RuntimeStateDoc |

### Upstream references

- `github.com/automerge/automerge` `rust/automerge/src/sync/state.rs` — `sync::State` fields, `in_flight`, `shared_heads`, reconnection semantics
- `github.com/automerge/automerge-repo` `packages/automerge-repo/src/DocHandle.ts` — XState state machine (idle/loading/requesting/ready/unavailable/unloaded/deleted)
- `github.com/automerge/automerge-repo` `packages/automerge-repo/src/synchronizer/DocSynchronizer.ts` — per-peer `#syncStates`, `beginSync`/`endSync`, reconnect encode/decode hack
- `github.com/automerge/automerge-repo` `packages/automerge-repo/src/network/NetworkSubsystem.ts` — adapter-based peer lifecycle, `peer-disconnected` cleanup
- `github.com/alexjg/samod` `subduction-sans-io/src/engine.rs` — sans-IO state machine pattern: `Input` events in, `EngineOutput` actions out
- `github.com/alexjg/samod` `samod-core/src/actors/document/peer_doc_connection.rs` — per-peer `sync::State` with `reset_sync_state()`

## First Questions

Before changing session lifecycle code, answer these:

1. **Which actor performs this state transition?** Tool call (user-initiated, foreground), daemon_watch (background, event-driven), proxy (process-level supervision), or daemon (room lifecycle)?
2. **What concurrent operations can be in flight?** A tool call and a rejoin can race. Multiple tool calls can be concurrent (they clone DocHandle and drop the session RwLock).
3. **Is this session state or document state?** Session state (`NotebookSession`, `notebook_id`, `notebook_path`) is ephemeral per-process. Document state (`SharedDocState`, Automerge doc) is per-peer and persists across the sync connection.
4. **What happens if the daemon restarts mid-operation?** The sync task's socket will EOF, the DocHandle's channels will close, and pending `send_request` calls will fail. The daemon_watch loop will emit `Disconnected` then `Connected`/`Upgraded`.
5. **Does this change the session's identity or just its transport?** Rejoin replaces the DocHandle (new socket, new sync state, same notebook_id). Switching notebooks replaces the entire session (different notebook_id).

## Architecture: Three Layers of Session Management

### Layer 1: MCP Process (`runt-mcp`)

The `NteractMcp` server holds session state in `Arc<RwLock<Option<NotebookSession>>>`. This is the single source of truth for "which notebook is this MCP server connected to."

**Session struct** (`NotebookSession`):
- `handle: DocHandle` — clone-able, wraps `Arc<Mutex<SharedDocState>>`
- `notebook_id: String` — always a UUID
- `notebook_path: Option<String>` — file path for file-backed notebooks, None for ephemeral
- `broadcast_rx: BroadcastReceiver` — for daemon events

**Writers to session state:**
1. **Tool calls** (`open_notebook`, `create_notebook`) — foreground, user-initiated
2. **daemon_watch** (`rejoin()`, `MarkDisconnected`) — background, event-driven
3. **shutdown** — process exit cleanup

**The RwLock pattern:** Tool calls acquire a _read_ lock to clone the DocHandle, then drop the lock before doing work. This enables concurrent tool execution. Session-establishing tools acquire a _write_ lock to install a new session. The daemon_watch also acquires write locks for disconnect/rejoin.

### Layer 2: Proxy Process (`runt-mcp-proxy`)

The proxy manages the MCP child process lifecycle. It does NOT hold Automerge state — it's a process supervisor:

- `last_notebook_id: Option<String>` — tracks which notebook the child was connected to
- `NTERACT_MCP_REJOIN_NOTEBOOK` — env var seeded on child respawn for session handoff
- `child_generation: u64` — monotonic counter to detect stale forwarded calls

The proxy's `restart_child()` → env var → child's `daemon_watch` initial target → `rejoin()` path is the cross-process session recovery mechanism.

### Layer 3: Daemon (`runtimed`)

The daemon manages notebook rooms, each with its own Automerge document and RuntimeStateDoc. Rooms have:

- `active_peers: usize` — incremented on connect, decremented on disconnect
- Eviction timer — starts when `active_peers` hits zero (default 30s `keep_alive_secs`)
- `path_index` — maps file paths to room UUIDs

The daemon is authoritative for room existence and kernel lifecycle. MCP clients are peers, not owners.

## State Machine

### Session States

```
            +----------+
            |  NoSession|  (initial, or after eviction/disconnect)
            +----+-----+
                 |
    +-----------++-----------+
    |                        |
    v                        v
+---+----+            +------+------+
| Joining|            | Rejoining   |  (daemon_watch background)
| (tool) |            | (async,     |
+---+----+            |  may race)  |
    |                 +------+------+
    |                        |
    v                        v
+---+----+            +------+------+
| Active |            | Active      |
| Session|            | Session     |
+---+----+            +------+------+
    |     \                  |
    |      \                 |
    v       v                v
+---+----+ +-----+    +-----+------+
| Switch-| |Discon|    | Discon-    |
| ing    | |nected|    | nected     |
+---+----+ +--+--+    +-----+------+
    |         |              |
    v         v              v
+---+----+ +--+------+ +----+-----+
| Active | | NoSession| | Rejoining|
| (new)  | | (stashed | | (with    |
+--------+ | target)  | | target)  |
           +----------+ +----------+
```

### Transitions

| From | Trigger | To | Actor | Notes |
|------|---------|-----|-------|-------|
| NoSession | `open_notebook` / `create_notebook` | Active | Tool call | Acquires write lock, installs new session |
| NoSession | `rejoin()` with `override_target` | Active | daemon_watch | Initial target from env var or disconnect_target |
| Active | `open_notebook` (different notebook) | Active (new) | Tool call | `disconnect_previous_session` first, then connect |
| Active | `DaemonEvent::Disconnected` | NoSession | daemon_watch | Clears session, stashes `disconnect_target` |
| Active | `DaemonEvent::Upgraded` (same version) | Active (rejoined) | daemon_watch | New DocHandle, same notebook_id |
| Active | `DaemonEvent::Upgraded` (different version) | Process exit | daemon_watch | Proxy respawns child with rejoin env var |
| Active | Room evicted (checked in `rejoin()`) | NoSession | daemon_watch | `list_rooms` check, session cleared |
| NoSession (stashed) | `DaemonEvent::Connected` | Rejoining | daemon_watch | Uses `disconnect_target` |

### Critical Race: Rejoin vs Tool Call

The `rejoin()` function is async — it connects to the daemon, waits for initial load (up to 120s), and then writes the session. During this window, a tool call can establish a different session:

```
Time    daemon_watch                    Tool call (agent)
─────   ────────────────                ──────────────────
t0      rejoin() starts for notebook A
t1      connect() + await initial_load  
t2                                      create_notebook(B) → session = Some(B)
t3      rejoin() completes for A
t4      session guard: B != A → drop A's connection, return true
```

Without the session-write guard at t4, rejoin would overwrite session B with session A — destroying the user's explicit choice. The guard (PR #2448) reads the session before writing: if a different notebook_id is present, the rejoin is dropped.

**Key invariant:** User-initiated session changes (tool calls) always win over background session changes (daemon_watch rejoin).

## Concurrency Model

### Tool Call Concurrency

The `require_handle!` macro pattern:
```rust
let handle = {
    let guard = session.read().await;
    match guard.as_ref() {
        Some(s) => s.handle.clone(),  // Clone DocHandle
        None => return no_session_error(),
    }
    // RwLock dropped here
};
// Now work with handle — no lock held
```

Multiple tool calls can be in flight simultaneously because they clone the DocHandle (which wraps `Arc<Mutex<SharedDocState>>`) and drop the session RwLock. The DocHandle's internal mutex serializes CRDT mutations (`with_doc`), while async RPC (`send_request`) goes through the shared command channel (capacity 32).

### daemon_watch Concurrency

The `watch()` loop is single-threaded — it processes one `DaemonEvent` at a time. But `rejoin()` is async and can take 120s+, during which tool calls run concurrently via the session RwLock.

### Proxy Concurrency

The proxy has a `restart_in_progress: Arc<Mutex<bool>>` flag to prevent concurrent restarts (monitor task vs. tool call racing). The `child_generation` counter detects stale forwarded calls after a restart.

## Disconnect → Reconnect Invariants

### What Resets on Disconnect

| State | Resets? | Why |
|-------|---------|-----|
| `NotebookSession` | Yes — set to None | Prevents tool calls from hanging on a dead DocHandle |
| `disconnect_target` | Set from old session | Enables auto-rejoin on reconnect |
| `was_disconnected` | Set to true | Gates rejoin — prevents heartbeat Connected from spurious rejoins |
| `initial_target` | Preserved if set | Proxy handoff survives disconnect |
| DocHandle | Dropped (with session) | Old sync task's socket is dead |
| Automerge document | Gone (was in DocHandle) | No local persistence of MCP peer state |
| RuntimeStateDoc | Gone (was in DocHandle) | Daemon is the authority; client rebuilds on rejoin |

### What Resets on Rejoin

| State | Resets? | Why |
|-------|---------|-----|
| DocHandle | New instance | Fresh socket, fresh sync state, fresh Automerge doc |
| `sync::State` (peer_state) | New (implicit) | New DocHandle = new `SharedDocState` = fresh `sync::State::new()` |
| `sync::State` (state_peer_state) | New (implicit) | Same — fresh RuntimeStateDoc sync handshake |
| `notebook_id` | Same (for continuation rejoin) | We're reconnecting to the same notebook |
| `was_disconnected` | Cleared to false | Successful rejoin means we're reconnected |
| `disconnect_target` | Cleared | No longer needed |

### What Persists Across Disconnect (Daemon Side)

| State | Persists? | Why |
|-------|-----------|-----|
| Notebook Automerge doc | Yes | Daemon owns the room |
| RuntimeStateDoc | Yes | Daemon-authoritative |
| Kernel state | Yes (during keep_alive) | Eviction timer hasn't fired yet |
| Room UUID | Yes | Stable room identity |
| File path mapping | Yes | `path_index` in daemon |

## Design Principles (Distilled from Upstream)

### From Automerge Sync State

Automerge's `sync::State` is **per-peer, per-session**. It tracks what the remote peer has (`their_heads`, `their_have`), what we've sent (`sent_hashes`, `in_flight`), and what we agree on (`shared_heads`). Key properties:

1. **Never share sync state across peers.** Each peer connection needs its own `sync::State`. Sharing causes suppressed or duplicate messages.
2. **Reset sync state on reconnect, preserve document.** When the transport dies, reset `sync::State::new()` to force a fresh handshake. The document itself is unchanged — the new handshake will reconcile any divergence.
3. **`in_flight` prevents duplicate messages.** `generate_sync_message` returns None while a message is unacknowledged. Don't try to work around this — it's correct behavior.
4. **`encode()`/`decode()` only preserves `shared_heads`.** The full sync state (in_flight, sent_hashes, etc.) is session-ephemeral. Only `shared_heads` survives serialization, and even that is only useful when reconnecting to the _same_ peer.

### From Automerge-Repo DocHandle

automerge-repo's `DocHandle` is the closest upstream analog to nteract's `DocHandle`. Key differences:

1. **DocHandle has an XState state machine** (idle → loading → requesting → ready → unavailable → unloaded → deleted). nteract's DocHandle is simpler — it's always "ready" once constructed, with no explicit state machine. Session state lives in `NteractMcp`, not in the handle.
2. **DocSynchronizer manages per-peer sync state.** On disconnect (`endSync`), it removes the peer from the active list but _keeps the sync state_ in `#syncStates`. On reconnect (`beginSync`), it round-trips encode/decode to clear in-flight state. nteract doesn't do this — it creates a fresh `SharedDocState` (and thus fresh `sync::State`) on every rejoin.
3. **Network adapters are pluggable.** automerge-repo separates transport from sync. nteract's sync_task is tightly coupled to a specific Unix socket connection. When the socket dies, the entire sync task dies.

### From samod's Subduction Engine

samod's strongest pattern for nteract is the **sans-IO state machine**: `SubductionEngine<C>` accepts `Input<C>` events and returns `EngineOutput<C>` with IO actions for the caller to execute. No async, no threads, no IO inside the engine.

Applied to MCP session lifecycle:
- The `classify()` function in daemon_watch is already a step toward sans-IO — it's a pure function that maps (event, state) → decision.
- The full `watch()` loop could be refactored as a state machine that accepts `WatchInput` events (DaemonEvent, ToolCallEstablishedSession, RejoinCompleted, RejoinFailed) and returns `WatchOutput` actions (DoRejoin, ClearSession, Exit, NoOp).
- This would make the concurrency races testable without async — unit test the state machine's transitions, integration test the async wiring.

### Connection Lost Pattern (samod)

samod's `handle_connection_lost`:
```rust
fn handle_connection_lost(&mut self, id: C) {
    if let Some(ConnectionState::Authenticated { peer_id, .. }) = self.connections.remove(&id) {
        self.incremental.peer_disconnected(peer_id);
    } else {
        self.connections.remove(&id);
    }
}
```

Clean, synchronous, no retries inside the handler. The retry logic lives outside — in `retry_pending_finds` which is called when a new connection authenticates. nteract's `MarkDisconnected` + `RejoinContinuation` on next `Connected` follows a similar pattern.

## Anti-Patterns

### Holding RwLock Across Await

Never hold a `tokio::sync::RwLock` guard across an `.await` point. The rejoin guard is correct:

```rust
// CORRECT: read lock in a block, dropped before write
{
    let guard = session.read().await;
    if let Some(existing) = guard.as_ref() {
        if existing.notebook_id != new_notebook_id {
            return true; // guard dropped here
        }
    }
} // guard dropped here
*session.write().await = Some(new_session); // separate lock acquisition
```

### Unconditional Session Write After Async Work

Always check the current session state before writing after an async operation. The async gap is where races happen. This is the exact bug the #2448 review caught.

### Sharing Automerge sync::State Across Connections

Each reconnection creates a new `SharedDocState` with `sync::State::new()`. Never try to carry over sync state from the old connection — the old peer is dead to the daemon.

### Blocking the daemon_watch Loop

`rejoin()` can take 120s (initial load timeout). During this time, the watch loop is blocked. If the daemon sends another event (e.g., another `Connected` heartbeat), it's queued in the broadcast channel. If events lag, the watch loop detects it and sets `was_disconnected = true`.

## Multi-Client Considerations

With N concurrent MCP clients connected to the same daemon:

1. **Each client has its own session.** `NteractMcp.session` is process-local. Client A's rejoin doesn't affect Client B.
2. **Each client is a separate peer in the daemon room.** The daemon tracks `active_peers` per room. Client A disconnecting doesn't affect Client B's sync.
3. **Kernel operations are shared.** If Client A interrupts, Client B's pending execution is also interrupted (daemon-side, kernel is shared per room).
4. **RuntimeStateDoc is shared.** All clients see the same kernel status, queue, execution state via their separate sync connections to the same RuntimeStateDoc.
5. **Room eviction waits for ALL peers.** The daemon only starts the eviction timer when `active_peers == 0`. As long as any client is connected, the room stays alive.
6. **File path conflicts.** Only one room can hold a given file path at a time (daemon's `path_index`). If Client A opens `/tmp/test.ipynb` and Client B tries to save a different notebook to the same path, the daemon returns `SaveError::PathAlreadyOpen`.

## Validation Checklist

Run the smallest tests that exercise the touched layer:

- Session state transitions: `cargo test -p runt-mcp` (116 tests, includes daemon_watch classify tests)
- Sync task resilience: `cargo test -p notebook-sync` (includes catch_unwind, rebuild_state_doc tests)
- Proxy session tracking: `cargo test -p runt-mcp-proxy` (includes track_session, reconnection message tests)
- Automerge sync convergence: `cargo test -p notebook-sync -- sync` or convergence tests
- Full lint: `cargo xtask lint --fix`

When in doubt about a race condition, write a test that:
1. Creates the session state
2. Starts an async operation (like rejoin)
3. Races a competing state change (like a tool call)
4. Asserts the expected winner

## Appendix: Key Structs and Their Lifetimes

```
McpProxy (process-level)
  └── child_generation: u64          (monotonic, per child restart)
  └── last_notebook_id: Option<String> (tracks for env var handoff)

NteractMcp (per MCP child process)
  └── session: Arc<RwLock<Option<NotebookSession>>>
  │     └── handle: DocHandle
  │     │     └── doc: Arc<Mutex<SharedDocState>>
  │     │     │     └── doc: AutoCommit               (Automerge document)
  │     │     │     └── peer_state: sync::State        (notebook sync with daemon)
  │     │     │     └── state_doc: RuntimeStateDoc     (runtime state)
  │     │     │     └── state_peer_state: sync::State  (runtime state sync with daemon)
  │     │     └── changed_tx: mpsc::Sender             (notifies sync task of local changes)
  │     │     └── cmd_tx: mpsc::Sender<SyncCommand>    (request/confirm_sync)
  │     └── notebook_id: String
  │     └── notebook_path: Option<String>
  │     └── broadcast_rx: BroadcastReceiver
  └── last_session_drop: Arc<RwLock<Option<SessionDropInfo>>>
  └── peer_label: Arc<RwLock<String>>

daemon_watch (background tokio task)
  └── was_disconnected: bool          (gates heartbeat rejoin)
  └── initial_target: Option<String>  (proxy handoff via env var)
  └── disconnect_target: Option<String> (stashed from cleared session)
```
