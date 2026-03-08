# Protocol Target Design

**Date**: 2026-03-07 (updated 2026-03-08)
**Status**: Design document — describes where the protocol should go, not where it is today.
**Companion**: See `protocol-correctness-audit.md` for the current-state assessment.

This document describes the target protocol architecture for stability. It is written for future implementers, not as a spec to implement all at once. Each section is independently actionable.

---

## 1. Two-Peer Sync (Relay Simplification)

### Current state

Three Automerge peers: Frontend WASM, Tauri relay, Daemon. The relay maintains its own `AutoCommit` and participates in two independent sync relationships (frontend↔relay, relay↔daemon). Every mutation goes through three merge operations.

### Target

Two Automerge peers: Frontend WASM and Daemon. The Tauri process becomes a **byte relay** — it forwards binary frames between the frontend's WebSocket/IPC channel and the daemon's Unix socket without deserializing or merging Automerge data.

```
Frontend WASM ──[automerge sync]──> Tauri relay (bytes only) ──> Daemon
Frontend WASM <──[automerge sync]── Tauri relay (bytes only) <── Daemon
```

The relay MUST NOT hold an `AutoCommit`. It sees opaque `[type_byte][payload]` frames and copies them between the two transports.

### Why this matters

- Eliminates an entire merge point and its divergence surface
- Removes the `frontend_peer_state` virtual handshake (the mirror-doc initialization workaround)
- Removes `sync_to_daemon()` ack waits where broadcasts can be silently dropped
- Halves the number of Automerge sync roundtrips for any mutation
- Makes the relay stateless and restartable without data loss

### Migration path

1. The WASM frontend already owns a full Automerge doc and generates sync messages. Today those go to the relay; redirect them to the daemon (via the relay as a passthrough).
2. The relay currently handles `SyncCommand` mutations (add cell, delete cell, etc). These commands already go through WASM first in the local-first flow. Remove the relay's command processing for cell mutations.
3. The relay still needs to forward `NotebookRequest`/`NotebookResponse` frames (kernel commands). These are JSON, not Automerge — the relay can forward them without holding state.
4. Retain `GetDocBytes` for initial bootstrap: the relay fetches the current doc bytes from the daemon (not from its own replica) and hands them to WASM for `AutoCommit::load()`.

### Invariant

After this change, exactly two Automerge documents exist per notebook: the frontend's WASM doc and the daemon's `NotebookDoc`. The relay holds zero Automerge state.

---

## 2. Connection Lifecycle

### Current state

No automatic reconnection. When the daemon restarts or the socket breaks, the frontend shows a banner and waits for the user to click reconnect. Each reconnect creates a new `NotebookSyncClient` and re-bootstraps the doc.

### Target

Automatic reconnection with exponential backoff. The connection lifecycle is a state machine:

```
                  ┌─────────────┐
        ┌────────>│  Connected  │<───────┐
        │         └──────┬──────┘        │
        │                │ EOF/error     │ handshake OK
        │                v               │
        │         ┌──────────────┐       │
        │         │ Reconnecting │───────┘
        │         │  (backoff)   │
        │         └──────┬──────┘
        │                │ max retries
        │                v
        │         ┌──────────────┐
        └─────────│ Disconnected │  (user action required)
                  └──────────────┘
```

**Backoff schedule**: 500ms, 1s, 2s, 4s, 8s. Max 5 attempts. Reset on successful handshake.

**Frontend behavior during reconnect**:
- Local WASM mutations continue working (cells are editable)
- Kernel status shows "reconnecting..."
- Execution requests queue locally, flush on reconnect
- The dirty flag is set to true (unsaved edits may exist)

**Reconnect handshake**:
1. Open new socket, send `Handshake::NotebookSync` with same `notebook_id`
2. Daemon returns `ProtocolCapabilities`
3. Exchange Automerge sync messages until converged — this naturally merges any edits made during disconnection
4. Resume kernel broadcast subscription

The CRDT handles conflict resolution. No special reconnection protocol is needed beyond re-establishing the sync relationship.

---

## 3. Protocol Versioning

### Current state

The `protocol` field in the handshake is optional and unvalidated. The server always responds with `v2` regardless of what the client requests.

### Target

Strict version negotiation in the handshake:

```json
// Client sends:
{"channel": "notebook_sync", "notebook_id": "...", "protocol": "v2"}

// Server responds:
{"protocol": "v2"}

// If client sends unknown version:
{"protocol": "v2", "error": "unsupported protocol: v3, downgrading to v2"}

// If server cannot serve any version the client supports:
// Server closes connection with error frame
```

**Rules**:
- The `protocol` field becomes required (not optional)
- Server always responds with the version it will use
- Server MAY downgrade to a supported version and indicate this in the response
- If no compatible version exists, server closes the connection
- The wire framing (`[4-byte length][payload]`) is version 0 and never changes — it's the substrate on which versioned protocols run

**When to bump the version**: Any change to frame types (new `0x04`+ type byte), changes to the handshake schema, or changes to the semantics of existing frame types. Adding new `NotebookRequest`/`NotebookResponse` variants does NOT require a version bump because they're JSON-tagged enums with forward-compatible serde.

---

## 4. Request/Response Correlation

### Current state

Single-request-at-a-time per connection. Responses are correlated to requests by ordering: send request, wait for next Response frame. Works because the protocol is serial, but breaks if pipelining is ever needed.

### Target

Add a `request_id` field to requests and responses:

```json
// Request
{"action": "execute_cell", "cell_id": "abc", "request_id": "r-001"}

// Response
{"result": "cell_queued", "cell_id": "abc", "request_id": "r-001"}
```

**Rules**:
- `request_id` is a client-generated opaque string (UUID or monotonic counter)
- Server echoes it in the response
- If a request arrives without `request_id`, server assigns one (backwards compatibility)
- Responses without matching requests are protocol errors

This doesn't require pipelining to be useful — it catches bugs where responses arrive out of order or are misattributed. It's a defensive correctness measure.

**Non-goal**: Full request pipelining. Single-request-at-a-time is fine for the current workload. Correlation IDs add safety without requiring concurrency.

---

## 5. Heartbeat and Connection Health

### Current state

No keepalive mechanism. Half-open connections (peer process killed) are only detected when the next write fails. The daemon's `active_peers` count can be wrong for the duration of the eviction delay.

### Target

Periodic heartbeat at the frame level:

```
Frame type 0x04: Heartbeat
Payload: [8-byte timestamp (millis since epoch)]
```

**Rules**:
- Both sides send a heartbeat every 15 seconds of idle time (no other frames sent)
- If no frame of any type is received within 45 seconds (3x interval), the connection is considered dead
- Heartbeats are NOT requests — they don't expect responses. Either side sends them independently.
- The timestamp is informational (for latency monitoring), not used for protocol decisions

**Why frame-level, not request-level**: Heartbeats must work during long operations (kernel execution can take minutes). A request-level heartbeat would require the server to handle it while processing execution — frame-level heartbeats are handled by the I/O layer before dispatch.

**Daemon cleanup**: On heartbeat timeout, the daemon decrements `active_peers` and runs the eviction check. This gives accurate peer counts without waiting for the OS to signal a broken pipe.

---

## 6. Execution Queue State Machine

### Current state

The execution queue is a `VecDeque` guarded by implicit state: `executing: Option<String>` tracks the current cell, and `KernelStatus` is a flat enum. The iopub loop signals `ExecutionDone` when it sees an idle status, and `KernelDied` when the loop exits. Several edge cases can stall the queue (kernel hung, interrupted cell that doesn't idle, race between kernel death and execution done).

### Target

Make the execution state machine explicit:

```
     ┌──────────┐
     │   Idle   │ ── ExecuteCell ──> Busy(cell_id)
     └──────────┘                         │
          ^                               │
          │                               ├── iopub idle ──> dequeue next ──> Busy | Idle
          │                               │
          │                               ├── Interrupt ──> Interrupting(cell_id)
          │                               │                        │
          │                               │                        ├── iopub idle ──> Idle
          │                               │                        └── watchdog ──> Dead
          │                               │
          │                               └── KernelDied ──> Dead
          │
     ┌──────────┐
     │   Dead   │ ── LaunchKernel ──> Idle (new kernel)
     └──────────┘
```

**Watchdog timer**: When a cell starts executing, arm a watchdog timer. The timeout should be configurable (default: no timeout — kernels can run for hours legitimately). When interrupting, arm a shorter watchdog (default: 10s). If the watchdog fires, transition to `Dead`.

**Key invariants**:
- `Idle` means `executing.is_none()` AND `queue.is_empty()`. If the queue is non-empty, `process_next()` runs immediately.
- `Busy` means `executing.is_some()`. Only iopub idle or kernel death exits this state.
- `Dead` means the kernel process is gone. The queue is cleared. New execute requests return an error until a new kernel is launched.
- `Interrupting` means an interrupt was sent but we haven't seen idle yet. If the kernel responds, go to `Idle`. If the kernel doesn't respond within the interrupt watchdog, go to `Dead`.

**No implicit transitions**: Today, the queue relies on iopub messages that may or may not arrive. The target adds the watchdog as a backstop so no state is terminal-by-omission.

---

## 7. Broadcast Delivery Guarantees

### Current state

Kernel broadcasts use `tokio::broadcast(64)`. Slow peers get `Lagged` errors. After the audit fix, lagged peers receive a full Automerge doc sync to catch up on persisted data (outputs in the CRDT). But ephemeral broadcasts (status changes, queue state) that aren't persisted to the doc are lost.

### Target

Accept lossy delivery for ephemeral state. Strengthen delivery for persistent state.

**Ephemeral broadcasts** (KernelStatus, QueueChanged, ExecutionStarted, ExecutionDone):
- These are point-in-time signals. A lagged peer doesn't need the missed signals — it needs the current state.
- On `Lagged`, send a **state snapshot** frame instead of replaying missed broadcasts:
  ```json
  {"event": "state_snapshot", "kernel_status": "idle", "executing": null, "queued": []}
  ```
- The frontend treats a snapshot the same as the individual broadcasts — it just updates to the latest state.

**Persistent broadcasts** (Output, Comm, DisplayUpdate):
- These are already persisted to the Automerge doc or blob store.
- On `Lagged`, the existing doc sync catch-up is sufficient.
- The broadcast is an optimization (streaming incremental output) — the doc is the source of truth.

**Channel sizing**: Keep `broadcast(64)` for now. The `Lagged` recovery path makes the capacity a performance tuning knob, not a correctness boundary. If a peer consistently lags, it gets snapshot+doc-sync which is more expensive but correct.

---

## 8. Peer Count Accuracy (RAII Guard)

### Current state

`active_peers` is incremented with `fetch_add(1)` after the handshake and decremented with `fetch_sub(1)` at function exit. If a panic occurs between increment and decrement, the count leaks. If an error occurs before increment but the cleanup path decrements, the count underflows.

### Target

Use an RAII guard for peer count:

```rust
struct PeerGuard<'a> {
    room: &'a NotebookRoom,
}

impl<'a> PeerGuard<'a> {
    fn new(room: &'a NotebookRoom) -> Self {
        room.active_peers.fetch_add(1, Ordering::Relaxed);
        Self { room }
    }
}

impl Drop for PeerGuard<'_> {
    fn drop(&mut self) {
        self.room.active_peers.fetch_sub(1, Ordering::Relaxed);
    }
}
```

Create the guard immediately before entering the sync loop. The guard lives on the stack. Panic, error return, or normal exit all drop it, decrementing the count exactly once.

The eviction timer should be triggered by the guard's drop, not by checking the count after the sync function returns. This co-locates the trigger with the state change.

---

## 9. Persistence Decoupling

### Current state

`doc.save()` runs inside the `room.doc.write()` lock on every incoming sync message. Serialization time grows with document size, blocking all other peers during serialization.

### Target

Decouple serialization from the write lock:

1. After applying a sync message, **clone the doc heads** (cheap — just change hashes) and release the write lock.
2. Serialize outside the lock using `doc.save_after(&previous_heads)` for incremental saves, or snapshot from a read lock.
3. Send the bytes to the persist channel.

Alternatively, use Automerge's incremental save: `save_incremental()` produces only the changes since the last save, which is much smaller and faster than a full `save()`. The persist task can maintain the full file by appending incremental chunks.

**Trade-off**: Incremental saves are faster but require periodic compaction (full save) to prevent the file from growing unboundedly. A reasonable schedule: incremental saves on every change, full compaction every 60 seconds or on room eviction.

---

## 10. Kernel Reattachment After Daemon Restart

### Current state

When the daemon restarts, all kernel processes become orphaned. The daemon has no way to re-adopt them — connection info, HMAC keys, and session IDs are gone. The user must re-launch.

### Target

Persist kernel connection info to disk so a restarted daemon can re-attach:

```
~/.cache/runt/kernel-sessions/
  {notebook_id_hash}.json
```

```json
{
  "kernel_type": "python",
  "env_source": "uv:inline",
  "connection_info": {
    "transport": "tcp",
    "ip": "127.0.0.1",
    "shell_port": 52341,
    "iopub_port": 52342,
    "stdin_port": 52343,
    "control_port": 52344,
    "hb_port": 52345,
    "key": "...",
    "signature_scheme": "hmac-sha256"
  },
  "pid": 12345,
  "launched_at": "2026-03-07T10:00:00Z",
  "launched_config": { ... }
}
```

**Reattachment flow**:
1. On startup, daemon scans `kernel-sessions/` for session files
2. For each session, check if the PID is still alive (`kill(pid, 0)`)
3. If alive, create ZMQ sockets with the persisted connection info and HMAC key
4. Send a `kernel_info_request` on the shell channel to verify the kernel responds
5. If it responds, re-create the `RoomKernel` with the existing process
6. If it doesn't respond within 5 seconds, consider the kernel dead and clean up

**Cleanup**: Session files are deleted when a kernel is intentionally shut down. Stale files (dead PID) are cleaned up on daemon startup.

**Scope**: This is the hardest item in this document. It requires careful handling of ZMQ socket reconnection and iopub subscription replay. It's worth doing for daemon upgrades (the most common restart scenario) but not essential for initial stability.

---

## Priority Order

Listed by impact-to-effort ratio:

1. **Two-peer sync** (Section 1) — Highest impact. Eliminates the largest correctness surface area. The relay's triple-merge path is the root cause of multiple audit findings.

2. **RAII peer guard** (Section 8) — Small, surgical. Eliminates a class of peer count bugs permanently.

3. **Automatic reconnection** (Section 2) — High user impact. Daemon upgrades currently interrupt every open notebook.

4. **Heartbeat** (Section 5) — Enables accurate peer counts and timely cleanup. Prerequisite for trusting `active_peers` in eviction decisions.

5. **Broadcast state snapshots** (Section 7) — Completes the `Lagged` recovery story. The audit fixed persistent data recovery; this adds ephemeral state recovery.

6. **Execution state machine** (Section 6) — Formalizes the implicit state that exists today. The audit added `KernelDied` as a patch; this makes it part of the design.

7. **Protocol versioning** (Section 3) — Low urgency today (one version), high value when the second version arrives.

8. **Request/response correlation** (Section 4) — Defensive measure. Catches bugs, doesn't fix them.

9. **Persistence decoupling** (Section 9) — Performance, not correctness. Only matters for large notebooks.

10. **Kernel reattachment** (Section 10) — Highest effort. Valuable for daemon upgrades but complex to implement correctly.
