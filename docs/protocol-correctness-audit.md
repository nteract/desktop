# Protocol Correctness Audit

**Date**: 2026-03-07
**Scope**: Wire protocol, sync protocol, and kernel execution protocol correctness — race conditions, ordering guarantees, error recovery, state machine gaps

This audit complements `protocol-audit.md` (security). This document focuses on protocol correctness: can the system lose messages, get stuck, diverge, or misbehave under concurrent or failure conditions?

---

## 1. Wire Protocol (Length-Prefixed Binary Framing)

**Files**: `crates/runtimed/src/connection.rs`

### Design

Every IPC connection uses `[4-byte big-endian length][payload]` framing. The first frame is a JSON handshake declaring the channel type. Notebook sync connections use typed frames where the first payload byte indicates the message type (0x00 = Automerge sync, 0x01 = request, 0x02 = response, 0x03 = broadcast).

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Medium** | **`send_frame` silently truncates payloads >4 GiB.** The length is cast via `data.len() as u32` (line 172). If `data.len()` exceeds `u32::MAX`, the length field silently wraps. The receive side enforces `MAX_FRAME_SIZE` (100 MiB), but the send side has no corresponding check. A bug in a caller that assembles a huge payload would produce a corrupt frame rather than an error. | `connection.rs:172` |
| **Medium** | **No request/response correlation IDs.** The notebook protocol multiplexes requests, responses, and broadcasts over a single connection. Responses are correlated to requests purely by ordering: the client sends a request and waits for the next Response frame. If the server ever sends two responses (bug) or a response is lost (connection error mid-frame), the client will misattribute the next response to the wrong request. Currently the protocol is single-request-at-a-time per connection, which avoids this in practice, but it's fragile if pipelining is ever added. | `connection.rs:82-96`, `notebook_sync_client.rs:1359-1413` |
| **Low** | **No keepalive or heartbeat.** The daemon and client have no way to detect a half-open connection (e.g., the peer's process was SIGKILL'd). The connection will appear alive until the next write fails. For the daemon, this means `active_peers` can be wrong: a crashed client still counts as a peer until the daemon tries to write to it. The eviction timer (30s default) provides eventual cleanup, but during that window the room state is inaccurate. | `notebook_sync_server.rs:710,784` |
| **Low** | **Partial frame writes are not atomic.** `send_frame` writes the 4-byte length then the payload in two `write_all` calls with a `flush` at the end. If the process crashes between the length write and payload write, the peer reads a valid length prefix followed by truncated data, which `read_exact` will block on (or error on EOF). This is acceptable for Unix sockets (local, reliable), but would be problematic over TCP. | `connection.rs:171-177` |
| **Low** | **~~No handshake timeout.~~** ~~The daemon's `route_connection` called `recv_control_frame` without a timeout, allowing stalled connections to hold resources indefinitely.~~ **Fixed**: Added a 10-second timeout on the handshake read. | `daemon.rs:853-858` |

### Recommendation

Add a send-side frame size assertion:
```rust
pub async fn send_frame<W: AsyncWrite + Unpin>(writer: &mut W, data: &[u8]) -> std::io::Result<()> {
    if data.len() > MAX_FRAME_SIZE {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("frame too large to send: {} bytes (max {})", data.len(), MAX_FRAME_SIZE),
        ));
    }
    let len = (data.len() as u32).to_be_bytes();
    // ...
}
```

---

## 2. Three-Peer Automerge Sync Protocol

**Files**: `crates/runtimed/src/notebook_sync_server.rs`, `crates/runtimed/src/notebook_sync_client.rs`, `apps/notebook/src/hooks/useAutomergeNotebook.ts`

### Design

Three Automerge peers participate in sync:

1. **Frontend (WASM)** — `NotebookHandle` in the webview. Cell mutations execute locally. Sync messages flow to the Tauri relay via `invoke("send_automerge_sync")`.
2. **Tauri relay** — `NotebookSyncClient`. Maintains its own Automerge doc. Forwards sync messages between frontend and daemon. Also processes `SyncCommand`s for local mutations.
3. **Daemon** — `NotebookDoc` in the room. Canonical doc for kernel execution and persistence.

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **High** | **Relay doc is a full participant, not a passthrough.** The Tauri relay maintains its own `AutoCommit` document and a `peer_state` for the daemon and a `frontend_peer_state` for the frontend WASM. This means every mutation goes through THREE merge operations (frontend→relay, relay→daemon, daemon→relay→frontend) instead of two. This adds merge complexity, latency, and a potential divergence surface. The CLAUDE.md calls this "transitional — will be simplified to pure relay." Until then, the relay's doc can theoretically diverge from both peers if a sync error is silently swallowed. | `notebook_sync_client.rs:1623-1647` |
| **High** | **`changed_rx` broadcast channel can drop document updates silently.** The daemon uses `broadcast::channel(16)` for `changed_tx`. When a peer can't keep up (e.g., slow network, large document), `broadcast::recv()` returns `Lagged(n)`. But in the server's sync loop (`run_sync_loop_v2`), lagged notifications are NOT handled — `changed_rx.recv()` in the `select!` simply yields whatever is available. If 16+ changes accumulate while the server is processing a frame, the oldest notifications are dropped. Since the handler generates a new sync message from current state regardless, the **data** is not lost (Automerge sync is convergent), but the sync will be delayed until the next trigger. | `notebook_sync_server.rs:978,522` |
| **Medium** | **`biased` select in client prefers commands over socket reads.** The sync task uses `tokio::select! { biased; ... }` with commands first (line 1678). Under sustained command load (rapid typing), incoming daemon sync messages will starve. The user's own edits always proceed, but daemon-sourced changes (kernel outputs) could be delayed. In practice this likely doesn't manifest because typing pauses give the socket a chance, but with automated bulk operations (run-all-cells with rapid outputs) it could introduce visible latency. | `notebook_sync_client.rs:1677-1683` |
| **Medium** | **`try_send` drops sync updates when channel is full.** When the daemon sends changes, the client's sync task uses `changes_tx.try_send(update)` (line 1878). If the channel is full (receiver not draining), the update is silently dropped. The comment says "skip this update" but doesn't log or count drops. This means the frontend's cell state can fall behind the Automerge doc with no notification. The next incoming sync message will catch up, but there's a window of stale UI. | `notebook_sync_client.rs:1878-1890` |
| **Medium** | **Virtual sync handshake loops at most 10 times.** When initializing `frontend_peer_state` via `GetDocBytes`, the code runs a sync exchange loop with `for _ in 0..10` (line 1778). If the documents are large enough that convergence takes more than 10 rounds, the state will be incomplete, causing the relay to send stale/duplicate data to the frontend. 10 rounds should be sufficient for any realistic document, but this is an unbounded-by-assumption invariant. | `notebook_sync_client.rs:1778` |
| **Medium** | **Frontend sync is fire-and-forget.** `syncToRelay` calls `invoke("send_automerge_sync")` with `.catch()` that only logs. If the Tauri command fails (e.g., relay disconnected), the frontend has made a local mutation that never reaches the daemon. The Automerge doc diverges. On reconnect, `reset_sync_state()` is called and bootstrap re-syncs, but any mutations during the disconnected window need the next sync exchange to propagate. If the frontend is closed before reconnecting, those mutations are lost. | `useAutomergeNotebook.ts:98-107` |
| **Low** | **Document save serializes under write lock.** When receiving a sync message, the server holds `room.doc.write()` while calling `doc.save()` (line 923). For large documents, serialization can be slow, blocking all other peers and requests for this room. | `notebook_sync_server.rs:920-939` |

### Recommendation

The most impactful improvement is simplifying the relay to a passthrough (as already planned). The relay doc should only hold bytes in transit, not a full Automerge replica. This eliminates the triple-merge path and reduces the divergence surface.

For the `changed_rx` channel, consider using `tokio::sync::watch` instead of `broadcast` for change notifications, since receivers only need the latest state (not every intermediate notification).

---

## 3. Room Lifecycle and Peer Counting

**Files**: `crates/runtimed/src/notebook_sync_server.rs`

### Design

Each notebook gets a "room" in the daemon. Rooms track `active_peers` (AtomicUsize) for eviction decisions. When peers drop to 0, a delayed eviction timer fires to clean up the room and shutdown its kernel.

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Medium** | **Peer count can underflow if connection fails before increment.** `active_peers.fetch_add(1)` happens at line 710, after the handshake, metadata seeding, and auto-launch check. If any of these steps panic or error, the function returns without incrementing, but the cleanup path (line 784) always decrements. In the current code, the function signature returns `anyhow::Result` and errors propagate cleanly before the increment, so this doesn't actually happen. But if someone adds fallible code between the increment and the sync loop without guarding the decrement, it would underflow. Consider using a RAII guard for peer count. | `notebook_sync_server.rs:710,784` |
| **Medium** | **Eviction race with rapid reconnect.** When the last peer disconnects, a delayed eviction timer spawns (line 801). During the delay, a new peer can connect and re-increment `active_peers`. The eviction task checks `active_peers > 0` before evicting. But there's a TOCTOU gap: the new peer connects and starts the sync loop, while the eviction task reads `active_peers == 1` and cancels. If the new peer disconnects during the eviction delay, the eviction task was already cancelled by the earlier check, and a new eviction timer must be spawned. The code handles this correctly (a new disconnect triggers a new eviction timer), but the cancelled timer is a wasted task. | `notebook_sync_server.rs:801-847` |
| **Low** | **Multiple eviction timers can stack.** If peers rapidly connect/disconnect (e.g., browser refreshing), each disconnect spawns a new eviction timer. Multiple timers can be pending simultaneously. Each checks `active_peers == 0` before evicting, so only one will actually evict, but N-1 timers are wasted tokio tasks. Not a correctness issue, but could be optimized with a single shared timer. | `notebook_sync_server.rs:801` |

---

## 4. Kernel Execution Queue

**Files**: `crates/runtimed/src/kernel_manager.rs`, `crates/runtimed/src/notebook_sync_server.rs`

### Design

Cells are queued for execution in a `VecDeque`. Only one cell executes at a time. The iopub channel signals `ExecutionDone` (via `QueueCommand`) when the kernel goes idle, which dequeues and sends the next cell.

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **High** | **`ExecutionDone` relies on iopub idle status matching the executing cell.** When a status=idle message arrives on iopub, the code looks up the `cell_id` from the `parent_header.msg_id` via `cell_id_map`. If the idle status's parent_header doesn't match the currently executing cell (e.g., a delayed idle from a previous interrupted execution), `execution_done` will be called with a stale `cell_id`. Since `execution_done` checks `self.executing == Some(cell_id)` (line 1730), the stale call is a no-op — the queue stays blocked. BUT: if the current cell's idle message was already processed, this is fine. The risk is if a kernel sends idle without a parent_header (some kernels do for startup), which would produce `cell_id = None`, causing the `try_send(QueueCommand::ExecutionDone)` to not fire. The queue would then be stuck until the next cell's idle. | `kernel_manager.rs:776-782,1729-1753` |
| **High** | **Queue can get permanently stuck if kernel crashes during execution.** If the kernel process dies while a cell is executing, the iopub connection breaks (the iopub read loop exits). The `executing` field remains `Some(cell_id)`, and `process_next()` checks `if self.executing.is_some() { return Ok(()); }`. No more cells will execute. The kernel's `is_running()` check depends on process status, but nothing checks for a dead kernel after execution starts. The frontend would see a permanent "busy" state with no error. | `kernel_manager.rs:1674-1678,741-744` |
| **Medium** | **`cell_id_map` grows unboundedly during a session.** Old msg_id→cell_id mappings are only cleaned up when a cell is re-executed (line 1712-1713: `map.retain(|_, v| v != &cell.cell_id)`). For notebooks where many cells are executed once but never re-executed, the map grows proportionally to the number of executed cells. This is bounded by notebook size in practice, but for long-running sessions with many unique cells it could accumulate. | `kernel_manager.rs:1711-1714` |
| **Medium** | **`try_send` for `QueueCommand` can fail silently.** The iopub handler uses `try_send` (line 780) to signal execution done. If the command channel is full (capacity 100), the signal is dropped. With rapid kernel outputs, the channel could theoretically fill, causing the queue to stall. In practice, the command processor drains quickly, but under extreme load (100+ concurrent status messages), signals could be lost. | `kernel_manager.rs:780` |
| **Low** | **Interrupt clears queue but doesn't reset `executing`.** `interrupt()` clears the queue (line 1772) and sends an interrupt request to the kernel, but `self.executing` remains `Some(cell_id)`. The assumption is that the kernel will eventually send an idle status for the interrupted cell, which triggers `execution_done`. If the kernel doesn't respond to the interrupt (frozen), the queue is stuck (same as the kernel crash case). | `kernel_manager.rs:1771-1780` |

### Recommendation

Add a dead-kernel watchdog: when the iopub read loop exits (kernel died), send a synthetic `ExecutionDone` or a new `KernelDied` command to unblock the queue and notify the frontend. This is the most impactful fix in this audit.

---

## 5. Reconnection and State Recovery

**Files**: `crates/notebook/src/lib.rs`, `apps/notebook/src/hooks/useAutomergeNotebook.ts`, `apps/notebook/src/hooks/useDaemonKernel.ts`

### Design

When the daemon disconnects (crash, restart, `install-daemon`), the frontend detects it via the sync task exiting. The user can trigger reconnection, which creates a new `NotebookSyncClient`. The frontend resets its WASM sync state and re-bootstraps from the new relay's doc.

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **High** | **Kernel state is lost on daemon restart.** When the daemon restarts, all kernel processes are orphaned (they're children of the old daemon process). The kernel processes may continue running (they listen on TCP ports), but the daemon has no way to re-adopt them — the kernel's connection info, HMAC keys, and session IDs are gone. The user sees "not started" and must re-launch. Running cells produce output that nobody receives. | `kernel_manager.rs` (entire lifecycle) |
| **Medium** | **Reconnection is not automatic.** When the sync task detects disconnection (EOF on socket), it logs and exits. The frontend shows a status banner prompting the user to reconnect. There's no automatic retry with backoff. For daemon upgrades (`install-daemon`), the user must manually reconnect each open notebook window. | `notebook_sync_client.rs:1920-1928`, `App.tsx:815,897` |
| **Medium** | **Notebook dirty state can be wrong after reconnect.** If the user has unsaved edits when the daemon disconnects, the `dirty` flag in React state reflects the pre-disconnect state. After reconnect, the doc is re-bootstrapped from the daemon's persisted state, which may not include the latest frontend edits (if they weren't synced before disconnect). The dirty flag isn't updated to reflect this potential data loss. | `useAutomergeNotebook.ts:44,166` |
| **Low** | **`ReconnectInProgress` atomic prevents concurrent reconnects but allows a second reconnect to silently fail.** If two reconnect attempts race (e.g., user clicks retry twice), the second returns `Ok(())` without reconnecting. This is fine behavior, but the user gets no feedback that the second attempt was skipped. | `lib.rs:1888-1896` |

### Recommendation

Consider adding automatic reconnection with exponential backoff (1s, 2s, 4s, ...) when the sync task detects EOF. This would handle daemon restarts transparently. For kernel re-adoption, consider persisting kernel connection info to disk so a restarted daemon can re-attach to running kernels.

---

## 6. Broadcast Channel Semantics

**Files**: `crates/runtimed/src/notebook_sync_server.rs`, `crates/runtimed/src/kernel_manager.rs`

### Design

Kernel events (status changes, outputs, execution progress) flow from the kernel's iopub channel through `kernel_broadcast_tx` to all connected peers. Each peer's sync loop `select!`s on `kernel_broadcast_rx.recv()`.

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Medium** | **`broadcast::channel(64)` can lag under rapid output.** Kernels producing rapid output (e.g., `for i in range(10000): print(i)`) can generate hundreds of broadcasts per second. With a channel capacity of 64, slow peers will experience `Lagged` errors. The server's sync loop doesn't handle `Lagged` for `kernel_broadcast_rx` — it only matches `Ok(broadcast)` (line 991). A `Lagged` error would cause `recv()` to return `Err(RecvError::Lagged(n))`, which is not matched by the `Ok()` pattern, so the `select!` arm doesn't fire. The broadcast is dropped and the peer misses those outputs. | `notebook_sync_server.rs:991,523` |
| **Medium** | **Output broadcasts are ephemeral — no catch-up mechanism.** If a peer joins a room after execution has started, it receives no historical broadcasts. The Automerge doc contains the persisted outputs, but a new peer won't see incremental updates until the next output arrives. For large outputs, the peer may see a partially-rendered cell until a new output triggers a full sync. | `notebook_sync_server.rs:887-903` |
| **Low** | **`send()` return value ignored for broadcasts.** All broadcast sends use `let _ = broadcast_tx.send(...)`, ignoring the receiver count. If no peers are connected, broadcasts are silently dropped. This is intentional (fire-and-forget), but means output produced while no peers are connected (e.g., during reconnect) is only persisted to the Automerge doc, not streamed. | `kernel_manager.rs:770-773` |

### Recommendation

Handle `Lagged` in the server sync loop by sending a full state sync when the peer falls behind:
```rust
result = kernel_broadcast_rx.recv() => {
    match result {
        Ok(broadcast) => { /* send to peer */ }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            warn!("Peer lagged {} broadcasts, sending full sync", n);
            // Generate a full Automerge sync message to catch up
            let mut doc = room.doc.write().await;
            if let Some(msg) = doc.generate_sync_message(&mut peer_state) {
                send_typed_frame(writer, AutomergeSync, &msg.encode()).await?;
            }
        }
        Err(broadcast::error::RecvError::Closed) => break,
    }
}
```

---

## 7. Protocol Versioning and Evolution

**Files**: `crates/runtimed/src/connection.rs`

### Findings

| Severity | Finding | Location |
|----------|---------|----------|
| **Medium** | **Protocol version is not enforced.** The `protocol` field in the `NotebookSync` handshake is optional and defaults to `None`. The server logs it (line 885) and always responds with `ProtocolCapabilities { protocol: "v2" }`, but doesn't reject unknown versions. A client sending `protocol: "v99"` would be served v2 without warning. There's no way for the client to detect version mismatch. | `connection.rs:48-52`, `notebook_sync_server.rs:776-779,885` |
| **Low** | **No wire format version marker.** The frame format has no version byte. If the framing needs to change (e.g., larger length field, compression), there's no way to negotiate it. The handshake JSON could carry this, but the handshake itself uses the current framing, creating a chicken-and-egg problem. | `connection.rs:1-7` |

---

## Summary: Priority-Ordered Action Items

### Fixed in This Audit

1. **Add kernel death detection to unblock execution queue.** When iopub read loop exits, send a `KernelDied` command that resets `executing` to `None` and broadcasts an error status. Added `QueueCommand::KernelDied`, `KernelStatus::Dead`, and `kernel_died()` method.

2. **Handle `Lagged` on `kernel_broadcast_rx` in the server sync loop.** Previously dropped broadcasts silently. Now triggers a full Automerge doc sync to catch the peer up.

3. **Add send-side frame size check in `send_frame`.** Prevents silent truncation at the u32 boundary. Returns `InvalidInput` error for oversized payloads.

4. **Fix auto-launch `CellError` handler not clearing queue.** The auto-launch command processor only logged cell errors but didn't clear the execution queue, breaking stop-on-error for auto-launched kernels. Now matches the manual-launch handler behavior.

5. **Add handshake timeout.** `route_connection` called `recv_control_frame` without a timeout, allowing stalled/idle connections to hold daemon resources indefinitely. Added a 10-second timeout.

### Important (protocol robustness)

5. **Plan relay simplification.** The three-peer merge path is the largest correctness surface area. A passthrough relay eliminates an entire class of divergence bugs.

6. **Add automatic reconnection with backoff.** Currently manual, causing friction on daemon upgrades.

7. **Use `watch` channel for `changed_tx` instead of `broadcast`.** Receivers only need "something changed" signals, not every intermediate notification.

### Nice-to-have (hardening)

8. Add request/response correlation IDs for future pipelining support.
9. Add a keepalive/heartbeat mechanism for half-open connection detection.
10. Add a kernel watchdog timer — if no iopub message is received within N seconds of an execute request, consider the kernel hung.
11. Enforce protocol version in handshake — reject unknown versions.
