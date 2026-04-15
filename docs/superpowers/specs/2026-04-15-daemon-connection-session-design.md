# Design: daemon metadata as a connection-session property

## Motivation

The `GetDaemonInfo` socket request introduced in #1803 and the `query_daemon_info` helper from #1804 treat daemon metadata (blob port, version, pid, started_at, worktree info) as something a client pulls **per-lookup**. Every `get_blob_port()` call, every version check, every mcpb-runt poll tick opens a socket, handshakes, sends one request, reads the reply, disconnects.

That was the right shape for the minimal first-pass migration, but it's backwards for most consumers. The real invariant is:

> Daemon metadata is a property of the current daemon connection session. It's learned on connect, stable for the life of the connection, and only changes when the connection resets (crash, restart, upgrade).

This document proposes treating it that way explicitly. The socket liveness IS the "is this info still valid?" signal. If the socket stays open, the blob port hasn't changed. If it drops, every field could be different on reconnect, so we re-fetch once.

## Goals

1. Frontend and mcpb-runt and runt-mcp-proxy hold a persistent daemon connection. They fetch `GetDaemonInfo` once on connect and cache the result for the life of that connection.
2. Reconnect events are the ONLY trigger for refetch. No per-lookup polling.
3. Upgrade detection becomes a side-effect of reconnect: if the post-reconnect `version` / `pid` differs from the cached value, that's an upgrade.
4. Short-lived CLI callers (`runt daemon status`, etc.) keep the one-shot `query_daemon_info` path — it's the right shape for them.
5. Once every in-process consumer is on the connection-session model, the daemon can stop writing `daemon.json` (closes #1812 phase 2).

## Non-goals

- Changing the wire protocol. `GetDaemonInfo` stays as-is.
- Removing `query_daemon_info`. One-shot callers keep using it.
- Centralizing all daemon communication through one connection. Clients that already have a connection for other reasons (notebook sync, pool RPC) MAY piggyback, but the design doesn't require it.

## Architecture

### New type: `DaemonConnection`

Lives in `runtimed-client`. Wraps a long-lived socket to the daemon plus the most recent `DaemonInfo` fetched over it.

```rust
pub struct DaemonConnection {
    /// The socket path we connect to. Pinned at construction.
    socket_path: PathBuf,

    /// Current state. RwLock so many callers can read `info()` concurrently
    /// without blocking each other, and the background reconnect task is
    /// the only writer.
    state: Arc<RwLock<ConnectionState>>,

    /// Notifies subscribers on any state transition (connected, disconnected,
    /// upgraded). Subscribers get the fresh `DaemonInfo` (or None on
    /// disconnect).
    events: broadcast::Sender<DaemonEvent>,

    /// Background task that owns the actual socket. Dropping the
    /// `DaemonConnection` cancels the task.
    _supervisor: JoinHandle<()>,
}

enum ConnectionState {
    Connected { info: DaemonInfo, connected_at: Instant },
    Reconnecting { last_info: Option<DaemonInfo>, since: Instant, attempt: u32 },
    Stopped,  // user called .close(); supervisor has exited
}

pub enum DaemonEvent {
    Connected { info: DaemonInfo },
    Upgraded { previous: DaemonInfo, current: DaemonInfo },
    Disconnected,
}
```

### Public API

```rust
impl DaemonConnection {
    /// Spin up a daemon connection supervisor. Returns immediately;
    /// the first GetDaemonInfo fetch happens in the background.
    pub fn spawn(socket_path: PathBuf) -> Self;

    /// Current cached info. Returns None while initial connect is in flight
    /// or while in reconnecting state and we have no prior info.
    pub async fn info(&self) -> Option<DaemonInfo>;

    /// Block until connected (with timeout). Useful for startup flows that
    /// want to wait for the daemon to appear.
    pub async fn wait_connected(&self, timeout: Duration) -> Option<DaemonInfo>;

    /// Subscribe to state transitions.
    pub fn subscribe(&self) -> broadcast::Receiver<DaemonEvent>;

    /// Shut down the supervisor. Idempotent.
    pub async fn close(self);
}
```

### Supervisor task

Owned by each `DaemonConnection`. Runs roughly:

```text
loop {
    connect to socket;
    if fail: transition to Reconnecting, backoff, continue;

    send GetDaemonInfo;
    receive DaemonInfo;

    if previous info exists and (new.pid, new.started_at) differ from previous:
        emit Upgraded { previous, current };
    else:
        emit Connected { info: current };

    transition to Connected;

    // Hold the connection open. We don't need to send anything.
    // The socket read side tells us when the daemon drops.
    read until EOF or error;

    emit Disconnected;
    transition to Reconnecting { last_info: Some(current), ... };
    // Loop back up.
}
```

### How existing consumers change

**Frontend (Tauri)**
- Today: `get_blob_port` Tauri command calls `query_daemon_info` each invocation (with a one-shot connection).
- After: a process-global `DaemonConnection` is spawned at app startup (stored in Tauri state). The Tauri command reads from its cache. On disconnect, the cache goes stale and the next read blocks briefly waiting for reconnect.

**mcpb-runt**
- Today: polls `~/Library/Caches/runt[-nightly]/daemon.json` on a timer, compares version, restarts child on change.
- After: spawns a `DaemonConnection` and subscribes to `DaemonEvent::Upgraded`. Drops the file-polling path entirely. Simpler AND avoids the "file-missing means daemon dead" false positive that caused the original "output void" bug.

**runt-mcp-proxy**
- Similar to mcpb-runt. Replaces `daemon_info_path` config + `read_daemon_version` polling with a `DaemonConnection`.

**runt-mcp (`runt mcp` command)**
- The existing `daemon_health_monitor` in `crates/runt-mcp/src/health.rs` already does a version of this — periodically pings, detects restart. Replace the ping-loop with a `DaemonConnection` subscription. Less code, same behavior.

**Short-lived CLI commands** (`runt daemon status`, `runt daemon info`, `runt daemon doctor`)
- Stay on `query_daemon_info(socket_path).await`. Spinning up a `DaemonConnection` for a 2-second command is overkill.

### Wire protocol: does it need changing?

**No**, initially. The supervisor just holds the socket open after sending `GetDaemonInfo` and reads until it errors. The daemon's existing `handle_pool_connection` loop keeps the connection alive while the client is idle (it's a request/response loop — no response is owed if no request arrives).

**Possible future optimization**: add a `DaemonStateSync` frame (0x08?) so the daemon pushes state updates proactively (new blob port after restart, new version after hot-reload). Defer until we actually have a use case — reconnect-on-drop handles version upgrades correctly today.

### Lock hygiene

`DaemonConnection::info()` reads under a `tokio::RwLock`. The supervisor writes under the same lock. Critical: the supervisor must NOT hold the write guard across any `.await` point (memory rule, see CLAUDE.md). Pattern:

```rust
let new_info = fetch_daemon_info().await?;  // no lock held here
{
    let mut state = self.state.write().await;
    *state = ConnectionState::Connected { info: new_info.clone(), ... };
}  // guard dropped before the broadcast send
let _ = self.events.send(DaemonEvent::Connected { info: new_info });
```

Same rule for reads: never hold `info()`'s guard across an await.

### Backpressure on the event channel

`broadcast::channel` has a capacity. If a subscriber is slow, they see `RecvError::Lagged` and may miss events. Design: cap at 64, document that subscribers should treat Lagged by querying `info()` directly to re-sync, since it's always current.

## Migration phases

### Phase 1 — introduce the type (this spec)
- Add `DaemonConnection` in `runtimed-client`, with tests
- No callers switch yet. Pure additive.

### Phase 2 — switch in-process consumers
- Tauri app: spawn at startup, route `get_blob_port` through it
- `runt-mcp` health monitor: replace ping-loop with DaemonConnection subscription
- Keep `query_daemon_info` as-is for one-shot CLI use

### Phase 3 — switch MCPB/proxy
- `mcpb-runt`: drop file polling, use DaemonConnection
- `runt-mcp-proxy`: drop `daemon_info_path` config + `read_daemon_version`

### Phase 4 — retire `daemon.json`
- Daemon stops writing the file (`DaemonLock::write_info` gone, Drop-time cleanup gone)
- One-shot CLI callers switch to `query_daemon_info` (which just does a one-shot `DaemonConnection::spawn` + `wait_connected` + drop)
- Closes #1812

## Out of scope

- Making `notebook-sync` share a connection with `DaemonConnection`. Different protocol discriminator at handshake (`Pool` vs `NotebookSync`). Can revisit if we feel connection-per-purpose is wasteful, but it's not currently a bottleneck.
- Windows named-pipe reconnect specifics. The supervisor handles both Unix sockets and named pipes, but the retry/backoff details may want platform-specific tuning. Defer.

## Open questions

1. **Backoff tuning.** Current `reconnect_with_backoff` in the runtime agent uses 100ms → 6.4s capped at 10 attempts. Reasonable for DaemonConnection? The runtime agent wants to eventually give up and let the process exit; DaemonConnection is more like a daemon-shaped dependency — probably wants unbounded reconnect with a longer cap (e.g. 30s).
2. **First-connect timing.** Should `DaemonConnection::spawn` block until the first fetch succeeds, or return eagerly and let callers use `wait_connected`? Proposal: return eagerly; expose `wait_connected(timeout)` for callers that need it.
3. **Connection sharing.** If two parts of the same process both want the daemon, should they share one `DaemonConnection`? Proposal: put it behind a lazy-init cell (`OnceCell` or similar) so the first caller wins and subsequent callers reuse.

## References

- #1803 (merged) — `GetDaemonInfo` request
- #1804 (merged) — `query_daemon_info` helper
- #1805 (merged) — reconnect-on-framing-error, close cousin architecture in the runtime agent
- #1820 (merged) — Tauri get_daemon_info using socket
- #1812 — daemon.json migration tracking issue (closes at phase 4)
- CLAUDE.md "No Tokio Mutex Guards Across `.await` Points"
