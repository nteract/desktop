# Protocol Correctness Audit

**Date**: 2026-03-07 (updated 2026-03-08)
**Scope**: Wire protocol, sync protocol, and kernel execution protocol correctness — race conditions, ordering guarantees, error recovery, state machine gaps

This audit complements `protocol-audit.md` (security). This document focuses on protocol correctness: can the system lose messages, get stuck, diverge, or misbehave under concurrent or failure conditions?

The initial audit identified six issues that have been fixed:
- Send-side frame size check added (prevents silent truncation at u32 boundary)
- Handshake timeout added (10-second timeout on `route_connection`)
- Kernel death detection added (`KernelDied` command unblocks execution queue)
- `Lagged` broadcast handling added (triggers full Automerge doc sync)
- Auto-launch `CellError` handler now clears execution queue (stop-on-error)
- WASM double-`save()` replaced with `get_heads()` for sync change detection

This document now tracks only remaining open items.

---

## 1. Wire Protocol (Length-Prefixed Binary Framing)

**Files**: `crates/runtimed/src/connection.rs`

### Strengths

- Frame size limits enforced on both send and receive sides
- 10-second handshake timeout prevents stalled connections
- Clean EOF handling

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Medium** | **No request/response correlation IDs.** Responses are correlated to requests purely by ordering. If the server ever sends two responses (bug) or a response is lost (connection error mid-frame), the client will misattribute the next response to the wrong request. Currently single-request-at-a-time per connection, which avoids this in practice, but it's fragile if pipelining is ever added. | `connection.rs:82-96`, `notebook_sync_client.rs:1359-1413` |
| **Low** | **No keepalive or heartbeat.** No way to detect half-open connections (e.g., peer SIGKILL'd). The eviction timer (30s default) provides eventual cleanup, but during that window `active_peers` can be wrong. | `notebook_sync_server.rs:710,784` |
| **Low** | **Partial frame writes are not atomic.** `send_frame` writes the 4-byte length then the payload in two `write_all` calls. If the process crashes between the length write and payload write, the peer reads a valid length prefix followed by truncated data. Acceptable for Unix sockets (local, reliable), but would be problematic over TCP. | `connection.rs:171-177` |

---

## 2. Three-Peer Automerge Sync Protocol

**Files**: `crates/runtimed/src/notebook_sync_server.rs`, `crates/runtimed/src/notebook_sync_client.rs`, `apps/notebook/src/hooks/useAutomergeNotebook.ts`

### Strengths

- Automerge CRDT guarantees eventual convergence
- WASM sync change detection uses efficient `get_heads()` comparison
- Lagged peers receive full doc sync to catch up

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **High** | **Relay doc is a full participant, not a passthrough.** The Tauri relay maintains its own `AutoCommit` document. Every mutation goes through THREE merge operations instead of two. The CLAUDE.md calls this "transitional — will be simplified to pure relay." Until then, the relay's doc can theoretically diverge from both peers if a sync error is silently swallowed. | `notebook_sync_client.rs:1623-1647` |
| **High** | **`changed_rx` broadcast channel can drop document updates silently.** The daemon uses `broadcast::channel(16)` for `changed_tx`. When a peer can't keep up, `Lagged(n)` notifications are dropped. Since the handler generates a new sync message from current state regardless, the **data** is not lost (Automerge sync is convergent), but sync will be delayed until the next trigger. | `notebook_sync_server.rs:978,522` |
| **Medium** | **`biased` select in client prefers commands over socket reads.** Under sustained command load (rapid typing), incoming daemon sync messages will starve. Kernel outputs could be delayed. In practice typing pauses give the socket a chance, but automated bulk operations could introduce visible latency. | `notebook_sync_client.rs:1677-1683` |
| **Medium** | **`try_send` drops sync updates when channel is full.** If the channel is full (receiver not draining), the update is silently dropped. The frontend's cell state can fall behind the Automerge doc with no notification. The next incoming sync message will catch up. | `notebook_sync_client.rs:1878-1890` |
| **Medium** | **Virtual sync handshake loops at most 10 times.** When initializing `frontend_peer_state` via `GetDocBytes`, the code runs a sync exchange loop with `for _ in 0..10`. If the documents are large enough that convergence takes more than 10 rounds, the state will be incomplete. 10 rounds should be sufficient for any realistic document, but this is an unbounded-by-assumption invariant. | `notebook_sync_client.rs:1778` |
| **Medium** | **Frontend sync is fire-and-forget.** `syncToRelay` calls `invoke("send_automerge_sync")` with `.catch()` that only logs. If the Tauri command fails, the frontend has made a local mutation that never reaches the daemon. On reconnect, `reset_sync_state()` re-syncs, but mutations during the disconnected window need the next sync exchange to propagate. If the frontend is closed before reconnecting, those mutations are lost. | `useAutomergeNotebook.ts:98-107` |
| **Medium** | **Broadcasts silently dropped during `sync_to_daemon` ack wait.** When the relay calls `sync_to_daemon()`, it waits up to 500ms for an AutomergeSync ack. If a Broadcast frame arrives instead, it's silently ignored — kernel outputs or status changes can be lost. | `notebook_sync_client.rs:1289-1303` |
| **Low** | **Document save serializes under write lock.** When receiving a sync message, the server holds `room.doc.write()` while calling `doc.save()`. For large documents, serialization can be slow, blocking all other peers. | `notebook_sync_server.rs:920-939` |
| **Low** | **`doc.save()` called on every sync message in the daemon.** After applying each incoming sync message, the daemon serializes the full doc for the persist debouncer. Mitigated by `watch` channel's "latest value" semantics, but serialization is expensive. | `notebook_sync_server.rs:923` |

### Recommendation

The most impactful improvement is simplifying the relay to a passthrough (as already planned). This eliminates the triple-merge path and reduces the divergence surface. See `protocol-target.md` Section 1.

---

## 3. Room Lifecycle and Peer Counting

**Files**: `crates/runtimed/src/notebook_sync_server.rs`

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Medium** | **Peer count can underflow if connection fails before increment.** `active_peers.fetch_add(1)` happens after the handshake. If code between increment and the sync loop errors without guarding the decrement, the count underflows. Currently safe, but fragile. Consider using a RAII guard. | `notebook_sync_server.rs:710,784` |
| **Medium** | **Eviction race with rapid reconnect.** When the last peer disconnects, a delayed eviction timer spawns. During the delay, rapid connect/disconnect can cancel the timer. The code handles this correctly (a new disconnect triggers a new timer), but cancelled timers are wasted tasks. | `notebook_sync_server.rs:801-847` |
| **Low** | **Multiple eviction timers can stack.** Rapid connect/disconnect spawns multiple timers. Each checks `active_peers == 0` before evicting, so only one evicts, but N-1 are wasted. Not a correctness issue. | `notebook_sync_server.rs:801` |

---

## 4. Kernel Execution Queue

**Files**: `crates/runtimed/src/kernel_manager.rs`, `crates/runtimed/src/notebook_sync_server.rs`

### Strengths

- Kernel death now detected via `KernelDied` command when iopub loop exits
- Stop-on-error clears queue for both manual and auto-launched kernels

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **High** | **`ExecutionDone` relies on iopub idle status matching the executing cell.** If a kernel sends idle without a parent_header (some kernels do for startup), `cell_id = None`, causing `ExecutionDone` not to fire. The queue would be stuck until the next cell's idle. | `kernel_manager.rs:776-782,1729-1753` |
| **Medium** | **`cell_id_map` grows unboundedly during a session.** Old msg_id→cell_id mappings are only cleaned up when a cell is re-executed. For long-running sessions with many unique cells it could accumulate. | `kernel_manager.rs:1711-1714` |
| **Medium** | **`try_send` for `QueueCommand` can fail silently.** The iopub handler uses `try_send` (capacity 100). Under extreme load (100+ concurrent status messages), signals could be lost, causing the queue to stall. | `kernel_manager.rs:780` |
| **Low** | **Interrupt clears queue but doesn't reset `executing`.** Assumes the kernel will eventually send idle for the interrupted cell. If the kernel is frozen, the queue is stuck (mitigated by `KernelDied` if the kernel process dies, but not if it hangs). | `kernel_manager.rs:1771-1780` |

### Recommendation

Add a kernel watchdog timer: if no iopub message is received within N seconds of an interrupt request, consider the kernel hung and transition to `Dead`.

---

## 5. Reconnection and State Recovery

**Files**: `crates/notebook/src/lib.rs`, `apps/notebook/src/hooks/useAutomergeNotebook.ts`, `apps/notebook/src/hooks/useDaemonKernel.ts`

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **High** | **Kernel state is lost on daemon restart.** When the daemon restarts, all kernel processes are orphaned. The kernel's connection info, HMAC keys, and session IDs are gone. The user sees "not started" and must re-launch. Running cells produce output that nobody receives. | `kernel_manager.rs` (entire lifecycle) |
| **Medium** | **Reconnection is not automatic.** When the sync task detects disconnection, the frontend shows a banner and waits for the user to click reconnect. No automatic retry with backoff. | `notebook_sync_client.rs:1920-1928`, `App.tsx:815,897` |
| **Medium** | **Notebook dirty state can be wrong after reconnect.** If the user has unsaved edits when the daemon disconnects, the dirty flag reflects pre-disconnect state. After reconnect, the doc is re-bootstrapped from the daemon's persisted state, which may not include the latest frontend edits. | `useAutomergeNotebook.ts:44,166` |
| **Low** | **`ReconnectInProgress` atomic prevents concurrent reconnects but allows a second reconnect to silently succeed without reconnecting.** | `lib.rs:1888-1896` |

---

## 6. Broadcast Channel Semantics

**Files**: `crates/runtimed/src/notebook_sync_server.rs`, `crates/runtimed/src/kernel_manager.rs`

### Strengths

- Lagged peers now receive a full Automerge doc sync to catch up on persisted data

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Medium** | **`broadcast::channel(64)` can lag under rapid output.** Kernels producing rapid output can generate hundreds of broadcasts per second. The `Lagged` recovery now triggers a doc sync for persisted data, but ephemeral broadcasts (status changes, queue state) that aren't persisted to the doc are still lost. | `notebook_sync_server.rs:991,523` |
| **Medium** | **Output broadcasts are ephemeral — no catch-up mechanism.** If a peer joins a room after execution has started, it receives no historical broadcasts. The Automerge doc contains persisted outputs, but a new peer won't see incremental updates until the next output arrives. | `notebook_sync_server.rs:887-903` |
| **Low** | **`send()` return value ignored for broadcasts.** All broadcast sends use `let _ = broadcast_tx.send(...)`. Intentional fire-and-forget, but means output produced while no peers are connected is only persisted to the Automerge doc, not streamed. | `kernel_manager.rs:770-773` |

---

## 7. Protocol Versioning and Evolution

**Files**: `crates/runtimed/src/connection.rs`

### Open Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Medium** | **Protocol version is not enforced.** The `protocol` field in the handshake is optional and defaults to `None`. A client sending `protocol: "v99"` would be served v2 without warning. | `connection.rs:48-52`, `notebook_sync_server.rs:776-779,885` |
| **Low** | **No wire format version marker.** The frame format has no version byte. If the framing needs to change, there's no way to negotiate it. | `connection.rs:1-7` |

---

## Open Action Items (Priority Order)

### Important (protocol robustness)

1. **Plan relay simplification.** The three-peer merge path is the largest correctness surface area. A passthrough relay eliminates an entire class of divergence bugs. See `protocol-target.md` Section 1.

2. **Add automatic reconnection with backoff.** Currently manual, causing friction on daemon upgrades. See `protocol-target.md` Section 2.

3. **Use `watch` channel for `changed_tx` instead of `broadcast`.** Receivers only need "something changed" signals, not every intermediate notification.

### Nice-to-have (hardening)

4. Add request/response correlation IDs for future pipelining support.
5. Add a keepalive/heartbeat mechanism for half-open connection detection.
6. Add a kernel watchdog timer — if no iopub message is received within N seconds of an interrupt, consider the kernel hung.
7. Enforce protocol version in handshake — reject unknown versions.
8. Add ephemeral state snapshots for lagged peers (status, queue state).
