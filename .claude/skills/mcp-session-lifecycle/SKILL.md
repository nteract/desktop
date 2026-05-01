---
name: mcp-session-lifecycle
description: >
  Understand the MCP server session lifecycle: proxy supervision, daemon
  watch loop, session state machine, rejoin/reconnect races, and room
  eviction. Use when working on runt-mcp, runt-mcp-proxy, daemon_watch.rs,
  or any code that reads/writes the session Arc<RwLock<Option<NotebookSession>>>.
---

# MCP Session Lifecycle

Use this skill when debugging session state, changing reconnection logic,
working on the proxy, or reasoning about races between background rejoin
and user-initiated tool calls.

## Three Layers

The MCP server has three layers, each with its own lifecycle:

```
MCP Client (Claude, etc.)
    |
    v
[runt-mcp-proxy] Process supervision layer
    |  - Tracks last_notebook_id from tool results
    |  - Restarts child on crash / daemon upgrade
    |  - Seeds NTERACT_MCP_REJOIN_NOTEBOOK env var
    v
[runt-mcp] Session state layer
    |  - Arc<RwLock<Option<NotebookSession>>>
    |  - daemon_watch loop (background rejoin)
    |  - Tool dispatch (user-initiated session changes)
    v
[runtimed] Room lifecycle layer
    - NotebookRoom per notebook UUID
    - Peer counting, delayed eviction (30s)
    - Automerge sync, RuntimeStateDoc
```

### Proxy Layer (`runt-mcp-proxy`)

The proxy is the outermost shell. It:
- Spawns the actual `runt mcp` child process
- Monitors stdout for MCP JSON-RPC and extracts `notebook_id` from
  `connect_notebook` / `create_notebook` results
- On child exit: if exit code is `EX_TEMPFAIL` (75), the daemon upgraded
  and we restart with the new binary; otherwise just restart
- Seeds `NTERACT_MCP_REJOIN_NOTEBOOK` env var so the child can rejoin
  the notebook the previous child was attached to
- Detects binary version changes (compares SHA of new binary path)

### Session State Layer (`runt-mcp`)

This is where most complexity lives. Key types:

```rust
// The shared session state
session: Arc<RwLock<Option<NotebookSession>>>

// What's in a session
struct NotebookSession {
    handle: DocHandle,        // Automerge sync handle
    broadcast_rx: Receiver,   // Daemon broadcast channel
    notebook_id: String,      // UUID
    notebook_path: Option<String>, // File path (None for untitled)
}
```

Two code paths compete for the session write lock:

1. **Tool calls** (`connect_notebook`, `create_notebook`): User-initiated,
   always win. Take write lock, install new session.
2. **daemon_watch loop** (`rejoin`): Background auto-rejoin. Must check
   that no tool call has changed the session during the async window.

### Daemon Room Layer (`runtimed`)

- Rooms are keyed by UUID, never change identity
- File paths are a secondary index (`PathIndex`)
- Peer counting: each connection is a peer. When all peers disconnect,
  a delayed eviction timer starts (default 30s, configurable via
  `keep_alive_secs`)
- If no peer reconnects within the eviction window: kernel shuts down,
  file watcher stops, room removed

## The Watch Loop State Machine

`daemon_watch.rs` runs a classify → act loop on `DaemonEvent`s:

```
classify(event, initial_target, has_session, was_disconnected, disconnect_target)
    -> WatchDecision { Exit | RejoinInitial | RejoinContinuation | MarkDisconnected | NoOp }
```

### State Tracking

| Variable | Purpose |
|----------|---------|
| `initial_target` | From `NTERACT_MCP_REJOIN_NOTEBOOK` env var. Cleared once consumed or once a tool call establishes a session. |
| `was_disconnected` | True after `Disconnected` event. Prevents heartbeat `Connected` events from triggering spurious rejoins. |
| `disconnect_target` | Stashed notebook_id/path when session cleared on disconnect. Used for rejoin when daemon comes back. |

### Event → Decision Matrix

| Event | initial_target? | has_session? | was_disconnected? | Decision |
|-------|----------------|-------------|-------------------|----------|
| `Upgraded` (version change) | any | any | any | Exit(75) |
| `Upgraded` (same version) | Some(t) | any | any | RejoinInitial(t) |
| `Upgraded` (same version) | None | true | any | RejoinContinuation |
| `Connected` | Some(t) | any | any | RejoinInitial(t) |
| `Connected` | None | true | true | RejoinContinuation |
| `Connected` | None | true | false | NoOp (heartbeat, not a real reconnect) |
| `Connected` | None | false | true | RejoinInitial(disconnect_target) |
| `Disconnected` | any | any | any | MarkDisconnected |

### The Heartbeat Problem (#2088)

`DaemonConnection` emits `Connected` every ~10s as a heartbeat. Without
`was_disconnected` gating, every heartbeat would trigger a rejoin, creating
a brief 2-peer spike that resets the room's eviction timer. The fix:
only rejoin after a real `Disconnected` event.

## The Session-Write Guard

The critical race: rejoin is async (socket connect + initial sync load,
potentially 120s). During that window, a tool call might establish a
completely different session. Without a guard, rejoin would overwrite it.

The guard in `rejoin()`:
```rust
// After connect + initial load succeed...
{
    let guard = session.read().await;
    if let Some(existing) = guard.as_ref() {
        if existing.notebook_id != new_notebook_id {
            info!("Rejoin superseded by active session; dropping");
            return true;
        }
    }
}
// Only now install the rejoined session
*session.write().await = Some(new_session);
```

**Invariant:** User-initiated session changes always win over background
rejoin. The guard ensures this by checking the session state *after* the
async work completes.

## Session Access Pattern

All tool handlers use the `require_handle!` macro:

```rust
// Read lock → clone DocHandle → drop lock → work
let handle = {
    let guard = session.read().await;
    match guard.as_ref() {
        Some(s) => s.handle.clone(),
        None => return no_session_error(...),
    }
};
// handle is now owned; lock is dropped
handle.with_doc(|doc| { ... });
```

This prevents lock contention: the read lock is held only for the clone,
not for the entire tool execution. `DocHandle` is cheaply cloneable
(`Arc<Mutex<SharedDocState>>`).

## Rejoin: File-Backed vs Ephemeral

| Notebook type | Identified by | Rejoin method | Eviction check |
|--------------|---------------|---------------|----------------|
| File-backed | Has file path | `connect_open(path)` | File exists on disk |
| Ephemeral (untitled) | UUID only | `connect(uuid)` | `list_rooms` check |

**Ephemeral notebooks** get an explicit `list_rooms` check before rejoin.
If the room was evicted during disconnect, we clear the session
immediately instead of creating a phantom empty room.

**File-backed notebooks** use `connect_open(path)` which lets the daemon
reload from disk. The `.automerge` persist files for file-backed rooms
are deleted, so UUID-only connect would yield an empty document.

## Session Drop Tracking

When a session ends, `last_session_drop` records why:

```rust
enum SessionDropReason {
    Evicted,       // Room evicted (all peers left, timer expired)
    Switched,      // User connected to a different notebook
    Disconnected,  // Daemon connection lost
}

struct SessionDropInfo {
    reason: SessionDropReason,
    notebook_id: String,
    notebook_path: Option<String>,
}
```

The `no_session_error()` function uses this to give the MCP client
actionable guidance: "notebook X was evicted, call connect_notebook"
vs "daemon disconnected, retry in a moment."

## Common Mistakes

### 1. Taking write lock during tool execution

Tool handlers should clone the `DocHandle` under a read lock, then drop
the lock before doing work. Holding a write lock during async work blocks
all other tool calls and the rejoin loop.

### 2. Not checking session after async work in rejoin

Any async gap in rejoin (socket connect, initial load) is a window where
a tool call can change the session. Always re-check before installing.

### 3. Treating Connected events as reconnection signals

`Connected` fires on every heartbeat (~10s). Only rejoin after a real
`Disconnected` event (`was_disconnected` flag).

### 4. UUID-only connect for file-backed notebooks

File-backed rooms don't have persistent `.automerge` files after the room
is closed. Use `connect_open(path)` to let the daemon reload from disk.

### 5. Creating phantom rooms on rejoin

If an ephemeral room was evicted, `connect(uuid)` creates a new empty room
with no cells and no kernel. Check `list_rooms` first.

## Key Source Files

| File | What it owns |
|------|-------------|
| `crates/runt-mcp/src/daemon_watch.rs` | `classify()` pure function, `watch()` loop, `rejoin()` |
| `crates/runt-mcp/src/session.rs` | `NotebookSession`, `SessionDropReason`, `SessionDropInfo` |
| `crates/runt-mcp/src/lib.rs` | `NteractMcp` server, `require_handle!` pattern |
| `crates/runt-mcp/src/tools/session.rs` | `connect_notebook`, `create_notebook`, `disconnect_previous_session` |
| `crates/runt-mcp-proxy/src/proxy.rs` | `McpProxy`, `restart_child()`, `track_session()` |
| `crates/runtimed/src/notebook_sync_server/` | Room lifecycle, peer counting, eviction |

## North Star: Concurrent MCP Clients

The current architecture assumes a single MCP client per daemon session.
The north star is supporting multiple concurrent MCP clients against the
same daemon. Key tension points:

- **Session state is per-process:** Each `runt mcp` process has one
  `Arc<RwLock<Option<NotebookSession>>>`. Multiple clients would need
  either multiple processes or per-client session tracking.
- **Peer identity:** Currently one peer label per MCP process. Multiple
  clients would need distinct peer identities for presence and
  conflict resolution.
- **Tool dispatch:** `require_handle!` assumes one active session. With
  multiple notebooks open, tools would need notebook_id routing.
- **Proxy supervision:** The proxy tracks one `last_notebook_id`. Multiple
  concurrent notebooks would need a session registry.
