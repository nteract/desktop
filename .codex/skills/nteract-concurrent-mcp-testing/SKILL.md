---
name: nteract-concurrent-mcp-testing
description: Use when testing, debugging, or reasoning about multiple simultaneous MCP client connections to the nteract daemon — whether from gremlin harnesses, parallel Claude Code sessions, or multi-window desktop usage. Covers room/peer lifecycle, concurrent execution serialization, reconnection, eviction, and failure classification for daemon stress testing.
---

# nteract Concurrent MCP Testing

Use this skill when designing concurrent MCP test scenarios, diagnosing failures under multi-client load, or reasoning about what happens when two or more clients interact with the same daemon simultaneously. This skill answers architecture questions from source code and ties failure modes to specific code paths.

## Source Map

Daemon room/peer lifecycle (read these first for any concurrent behavior question):

- `crates/runtimed/src/notebook_sync_server/mod.rs` — module overview, room lifecycle documentation, `KERNEL_BROADCAST_CAPACITY` (256), `catch_automerge_panic` wrapper
- `crates/runtimed/src/notebook_sync_server/room.rs` — `NotebookRoom` struct, `RoomConnections`, `RoomBroadcasts` (channel capacities: changed 16, kernel 256, presence 64), `RoomPersistence` loading gate, `NotebookFileBinding`
- `crates/runtimed/src/notebook_sync_server/peer_connection.rs` — `handle_notebook_sync_connection`: `active_peers` increment (`fetch_add`), `had_peers` latch, auto-launch kernel on first peer, peer UUID assignment
- `crates/runtimed/src/notebook_sync_server/peer_eviction.rs` — `handle_peer_disconnect`: `active_peers` decrement (`fetch_sub`), delayed eviction scheduling, flush-retry logic, Arc pointer identity guard against double-eviction, teardown sequence
- `crates/runtimed/src/notebook_sync_server/peer_session.rs` — initial sync, streaming load, loading contention (`try_start_loading` returns false for second peer)
- `crates/runtimed/src/notebook_sync_server/peer_loop.rs` — `run_sync_loop_v2`: steady-state `select!` loop, biased polling order, broadcast lag recovery via `queue_doc_sync`, `FramedReader` actor for cancel-safe reads
- `crates/runtimed/src/notebook_sync_server/peer_writer.rs` — `PeerWriter` (outbound queue capacity 1024), `PeerRequestWorker` (request queue capacity 64, sequential processing per peer), `SLOW_PEER_REQUEST` (30s)
- `crates/runtimed/src/notebook_sync_server/peer_notebook_sync.rs` — `handle_notebook_doc_frame`: acquires `room.doc` write lock, applies sync message, broadcasts `changed_tx`, generates reply, releases lock before I/O

Client-side reconnection:

- `crates/runt-mcp/src/daemon_watch.rs` — `WatchDecision` enum, `classify()` decision function, `rejoin()` with retry logic, ephemeral room existence check via `list_rooms`, heartbeat filtering (#2088)

Execution path:

- `crates/runtimed/src/requests/execute_cell.rs` — `queue_cell_if_current`: acquires `room.doc` write lock, reads cell source, creates execution via `room.state.with_doc()`, `next_queue_seq` AtomicU64 for ordering
- `crates/runtime-doc/src/doc.rs` — `create_execution_with_source`: writes execution entry to RuntimeStateDoc CRDT with `seq` field; runtime agent processes in `seq` order

## How Concurrent Connections Work

### Connection Model

Each MCP client (gremlin, Claude Code session, desktop window) spawns its own `runt mcp` process connected to the daemon over a Unix socket. Every `runt mcp` process is a distinct peer:

1. `runt mcp` opens a socket to the shared daemon (`~/.cache/runt-nightly/runtimed.sock`)
2. Daemon runs a 5-byte magic/version preamble handshake, then a JSON handshake identifying the notebook
3. Daemon locates or creates a `NotebookRoom` for the notebook
4. Peer gets a `peer_id = Uuid::new_v4().to_string()` and its own `sync::State`
5. Daemon increments `room.connections.active_peers.fetch_add(1, Ordering::Relaxed)` and sets `had_peers = true`
6. First peer triggers kernel auto-launch; subsequent peers skip it (checked by `peers == 1`)
7. Peer enters `run_sync_loop_v2` — a `tokio::select!` loop subscribing to all room broadcast channels

Multiple peers connecting to the **same notebook path** join the **same room**. Multiple peers connecting to **different notebooks** get separate rooms. The daemon handles both patterns concurrently through Tokio tasks.

### Per-Peer vs Per-Room State

| State | Scope | Where |
|-------|-------|-------|
| `sync::State` (Automerge sync negotiation) | Per-peer | Local variable in `run_sync_loop_v2` |
| `PeerWriter` (outbound frame queue) | Per-peer | Spawned per connection in `run_sync_loop_v2` |
| `PeerRequestWorker` (request queue) | Per-peer | Spawned per connection, capacity 64, sequential processing |
| `NotebookDoc` (canonical CRDT document) | Per-room | `room.doc: Arc<RwLock<NotebookDoc>>` |
| `RuntimeStateDoc` (kernel, queue, outputs) | Per-room | `room.state: RuntimeStateHandle` |
| `RoomBroadcasts` (fan-out channels) | Per-room | All peers subscribe to same broadcast senders |
| `active_peers` / `had_peers` | Per-room | `room.connections` — AtomicUsize / AtomicBool |
| `next_queue_seq` | Per-room | `room.next_queue_seq: AtomicU64` |
| Kernel subprocess | Per-room | One kernel per notebook, shared by all peers |

### Concurrent Cell Execution

When two peers execute cells at the same time, both requests serialize through the same atomics and locks:

1. **Each peer's request enters its own `PeerRequestWorker`** — requests from the same peer are sequential (mpsc channel, capacity 64). Requests from different peers run on separate Tokio tasks.

2. **`queue_cell_if_current` acquires `room.doc.write().await`** — this is a Tokio RwLock, so concurrent execute requests serialize at the document lock. One peer's execution write completes before the other's begins.

3. **`next_queue_seq.fetch_add(1, Relaxed)` assigns ordering** — the AtomicU64 guarantees a unique, monotonically increasing sequence number per room. Two peers racing to execute get adjacent seq values.

4. **Execution entries land in RuntimeStateDoc** — `create_execution_with_source` writes `{cell_id, source, seq, status: "queued"}` to the CRDT. The runtime agent discovers entries via sync and processes them in `seq` order.

5. **Same-cell deduplication** — if a cell already has an active execution (`status == "queued"` or `"running"`), `queue_cell_if_current` returns `AlreadyActive` instead of creating a duplicate.

The kernel itself is single-threaded (Jupyter protocol), so even with two peers, cell executions run one at a time through the runtime agent's queue.

### Broadcast Fan-Out Under Load

When the kernel produces output, the daemon writes to RuntimeStateDoc and sends a `NotebookBroadcast` to `kernel_broadcast_tx` (capacity 256). Each peer's sync loop has a `kernel_broadcast_rx` subscriber:

- **Normal case:** Each peer receives the broadcast and forwards it to its client.
- **Lagged peer:** If a peer falls behind by 256+ messages, `RecvError::Lagged(n)` fires. The peer loop recovers by calling `queue_doc_sync` — generating an Automerge sync message that catches the peer up from the CRDT rather than replaying individual broadcasts.
- **Closed channel:** `RecvError::Closed` means the room is being evicted. The peer loop returns `Ok(())`.

### Document Lock Contention

The `room.doc` RwLock is the primary serialization point for concurrent peers in the same room:

- **Sync frames** (`handle_notebook_doc_frame`): Acquires write lock, applies incoming Automerge sync message, generates reply, releases lock, then does async I/O. The pattern is: lock → mutate → encode reply → drop lock → queue reply.
- **Broadcast forwarding** (`forward_notebook_doc_broadcast`): Acquires write lock to call `generate_sync_message`, which needs mutable access to the peer's sync negotiation state within the doc context.
- **Execute cell** (`queue_cell_if_current`): Acquires write lock to read cell source and write execution metadata.

Contention is bounded by design: no lock guard crosses an `.await` (enforced by CI lint), and the lock-hold duration covers only in-memory CRDT mutations plus encoding — no disk I/O or network calls.

## Peer Lifecycle Edge Cases

### Disconnect During Execution

When Peer A is executing a cell and Peer B disconnects:

1. Peer B's `run_sync_loop_v2` returns (clean EOF or error)
2. `cleanup_presence_on_disconnect` removes Peer B's cursor state
3. `handle_peer_disconnect` calls `active_peers.fetch_sub(1, Relaxed) - 1` → remaining = 1
4. remaining > 0 → **no eviction scheduled**
5. Peer A's execution continues uninterrupted; broadcasts still flow

When the **last** peer disconnects during execution:

1. `active_peers` → 0 with `had_peers = true` → delayed eviction scheduled
2. Eviction task sleeps, then checks `active_peers` — if still 0, begins teardown
3. Teardown sends `ShutdownKernel` RPC (5s timeout), kills the kernel, flushes notebook to disk
4. In-flight execution output is lost unless already committed to the CRDT before teardown

### Reconnection

Client-side reconnection (`daemon_watch.rs`) handles two scenarios:

**File-backed notebook reconnect:**
1. `classify()` returns `RejoinContinuation` when daemon reports `Connected` after a `Disconnected` event
2. `rejoin()` calls `connect_notebook(path)` — daemon finds or creates a room for the path
3. Up to `REJOIN_MAX_RETRIES` (3) attempts with `REJOIN_RETRY_DELAY` (1s)
4. `REJOIN_INITIAL_LOAD_TIMEOUT` (120s) for the notebook document to sync

**Ephemeral notebook reconnect:**
1. `rejoin()` first calls `list_rooms()` to check if the room UUID still exists
2. If the room was evicted (all peers disconnected, ephemeral = no file to reload), the room is gone — reconnect fails silently, session is cleared
3. If the room exists, `connect_notebook(uuid)` joins it
4. This prevents "phantom room" creation where reconnecting to a missing ephemeral UUID would silently create an empty room (#2088 fix)

**Version change:**
1. `classify()` detects `daemon_version != our_version` → `WatchDecision::Exit(75)`
2. Exit code 75 signals the MCP proxy to respawn with the new daemon version
3. This is NOT a failure — it's the upgrade path

### Eviction Safety

Eviction (`peer_eviction.rs`) is designed to never lose user edits:

1. **Delayed check:** Eviction task sleeps, then re-checks `active_peers`. If a peer reconnected, eviction is cancelled.
2. **Flush retry:** The persistence debouncer is flushed before room removal. On failure, it retries indefinitely — "we'd rather leak a room than silently lose user edits."
3. **Arc pointer identity:** Room removal from the map uses `Arc::ptr_eq` to verify the room being evicted is the same one that triggered eviction. This prevents a race where a new room was created for the same path between the eviction check and the removal.
4. **Teardown order:** Kernel shutdown → stop file/project watchers → flush deps to metadata → save to disk → cleanup env dir.

## Failure Taxonomy

### Layer 1: Daemon Process Failures

**Sync task panic** — `peer_notebook_sync.rs`
- *Trigger:* Automerge 0.7.4 `MissingOps` bug, or unexpected panic in doc mutation path
- *Mechanism:* Tokio task panic propagates as `JoinError` to the peer loop, which returns an error. If the panic occurs in code shared across peers, it can crash one peer's connection without affecting others (each peer has its own sync loop task).
- *Impact:* Affected peer loses connection. Other peers in the same room continue operating. Client sees "Connection closed" (MCP error -32000).
- *Mitigation:* `catch_automerge_panic` wrapper catches panics and falls back to `rebuild_from_save` + sync state reset.
- *Classification:* **Dev-actionable** if new panic path found; **expected** if known Automerge bug.
- *Evidence:* PR #2448 added panic guard; PR #2451 fixed interrupt+create_cell race.

**Daemon restart** — external
- *Trigger:* Nightly install, `runt-nightly daemon stop`, daemon crash
- *Mechanism:* Unix socket closes. All connected `runt mcp` processes lose their connections simultaneously.
- *Impact:* Every active gremlin/client gets "Connection closed" cascade. Ephemeral notebooks are lost. File-backed notebooks survive on disk.
- *Client recovery:* `daemon_watch.rs` detects `Disconnected` event, attempts `RejoinContinuation` when daemon comes back. For MCP clients without daemon-watch (raw `runt mcp` via stdio), the connection is permanently lost.
- *Classification:* **Expected** during nightly installs. **Dev-actionable** if daemon crashes spontaneously.

### Layer 2: Connection-Level Failures

**Broadcast lag** — `peer_loop.rs` kernel_broadcast_rx arm
- *Trigger:* Peer is slow to drain outbound frames while kernel produces >256 broadcasts
- *Mechanism:* `broadcast::RecvError::Lagged(n)` — peer missed N broadcasts
- *Impact:* Peer temporarily has stale output state. Recovery is automatic via `queue_doc_sync` which sends an Automerge sync message to catch up from the CRDT.
- *Classification:* **Expected** under heavy output load. Only dev-actionable if recovery sync fails.

**Peer writer backpressure** — `peer_writer.rs`
- *Trigger:* Client stops reading from its socket (stalled or slow)
- *Mechanism:* `PeerWriter` outbound queue (capacity 1024) fills. `try_send` returns `TrySendError::Full`. The peer loop returns an error.
- *Impact:* Stalled peer is disconnected. Other peers unaffected.
- *Classification:* **Expected** if client is genuinely stuck. **Emergent** if a protocol bug causes a read stall.

**Request queue full** — `peer_writer.rs` `PeerRequestWorker`
- *Trigger:* Client sends >64 requests without waiting for responses
- *Mechanism:* `enqueue()` returns `RequestEnqueueError::Full`. An error response is sent to the client for that request, but the peer loop continues.
- *Impact:* Individual request is rejected with "Peer request queue full". Connection stays alive.
- *Classification:* **Expected** for misbehaving clients. **Gremlin bug** if a gremlin floods without awaiting responses.

### Layer 3: Multi-Client Room Failures

**Loading race** — `peer_session.rs` `try_start_loading`
- *Trigger:* Two peers connect to the same file-backed notebook simultaneously before the first finishes loading
- *Mechanism:* `AtomicBool::compare_exchange` — first peer wins and performs streaming load. Second peer's `try_start_loading` returns false; it skips loading (no-op, not an error).
- *Impact:* Second peer gets the document via normal Automerge sync after the first peer finishes loading. No data loss, but the second peer's initial load may be delayed until sync catches up.
- *Classification:* **Expected** — the loading gate works correctly. Only dev-actionable if the second peer never converges.

**Execution ordering under contention** — `execute_cell.rs` `queue_cell_if_current`
- *Trigger:* Two peers execute different cells at nearly the same time
- *Mechanism:* Both acquire `room.doc.write()` sequentially (Tokio RwLock serializes). Each gets a unique `next_queue_seq` value. Runtime agent processes in seq order.
- *Impact:* Execution order is deterministic (whoever acquires the lock first gets the lower seq). No data loss.
- *Classification:* **Expected** — the design is correct. Worth testing to verify seq ordering is honored end-to-end.

**Same-cell double execution** — `execute_cell.rs`
- *Trigger:* Two peers execute the same cell simultaneously
- *Mechanism:* First peer creates the execution entry. Second peer finds `status == "queued"` or `"running"` and returns `AlreadyActive { execution_id }` — the existing execution ID, not a new one.
- *Impact:* Only one execution runs. Both peers get the same `execution_id`. Correct behavior.
- *Classification:* **Expected** — deduplication works. Worth testing to verify both peers see the output.

### Layer 4: Client/Gremlin-Level Failures

**Silent stall** — observed in gremlin runs
- *Pattern:* Gremlin stops making tool calls for 10+ minutes with no error logged
- *Possible causes:* (a) Agent SDK waiting for tool_result that was dropped, (b) MCP process hung on socket read, (c) model API timeout not propagated
- *Diagnosis:* Check daemon logs for the peer's last request. If the daemon shows a response was sent, the issue is client-side.
- *Classification:* **Harness bug** if SDK drops tool_result. **Dev-actionable** if daemon never sends response.

**Early exit** — observed in gremlin runs
- *Pattern:* Gremlin exits cleanly after very few turns (e.g., 7/50)
- *Possible causes:* (a) Model decides task is complete, (b) Tool call fails and model gives up, (c) Harness logging gap hides the actual interaction
- *Diagnosis:* Check if log contains tool_result entries. Zero tool_results with multiple tool_use entries indicates a logging bug.
- *Classification:* **Harness bug** if tool_results are dropped. **Gremlin prompt issue** if model genuinely exits early.

**Budget exhaustion**
- *Pattern:* Gremlin hits `max_turns` without completing its task
- *Classification:* **Expected** — optimize gremlin prompt or increase budget.

## Testing Strategies

### What the Current Harness Tests

The gremlin harness runs multiple agents concurrently (controlled by `--batch-size`), each with its own `runt mcp` process connecting to the shared daemon. This tests:

- Multiple simultaneous daemon socket connections
- Concurrent room creation for different notebooks
- Resource contention (env pools, kernel launches)
- Daemon stability under sustained multi-client load

### What It Does NOT Test

Current gremlins each create their own notebook. No two gremlins connect to the same room simultaneously. The collaborator-a/b pair is sequential (`depends_on`), not concurrent. This means:

- No testing of concurrent CRDT sync convergence within a room
- No testing of broadcast fan-out to multiple peers
- No testing of `active_peers` accounting accuracy
- No testing of execution interleaving from multiple peers
- No testing of eviction cancellation when peers reconnect

### Concurrent-Notebook Gremlin Design Patterns

**Pattern A: Shared fixture notebook (parallel connect)**
```
Both gremlins:
  - connect_notebook(path="/tmp/gremlins/shared/notebook.ipynb")
  - verify they can read existing cells
  - create cells with unique markers (e.g., "# PEER-A-{uuid}")
  - execute cells and verify outputs
  - read notebook to verify BOTH peers' cells appear
  - no depends_on — run in parallel
```
Fixture must pre-exist. Tests concurrent room join, CRDT merge, broadcast fan-out.

**Pattern B: Sequential handoff with overlap window**
```
Gremlin A:
  - create_notebook, add cells, execute
  - write notebook path to shared file
  - continue working (DON'T disconnect)

Gremlin B (depends_on A, but A is still running):
  - read path from shared file
  - connect_notebook(path)
  - verify A's cells and outputs are visible
  - add own cells, execute
  - verify interleaved cells
```
Requires harness change: `depends_on` currently means "wait for A to finish." Would need a "wait for A's signal" primitive instead.

**Pattern C: Stress interleaving**
```
Both gremlins (shared notebook):
  - Loop 10 times:
    - create_cell with unique source
    - execute_cell
    - verify output
  - Final: read all cells, verify 20 cells exist with correct outputs
```
Tests execution ordering (`next_queue_seq`), output broadcast completeness, CRDT cell list merge.

### Diagnosing Concurrent Failures

1. **Check daemon logs** (`runt-nightly daemon logs`): Look for `[notebook-sync]` entries. Each request is logged with peer ID, notebook ID, and request type.
2. **Check `active_peers`**: Use `list_active_notebooks` MCP tool — if `active_peers` doesn't match expected connected clients, there's a connection accounting bug.
3. **Check execution ordering**: Read RuntimeStateDoc executions. Each has a `seq` field. If seq values are non-sequential or duplicated, there's a race in `next_queue_seq`.
4. **Distinguish failure layers**: Connection errors (MCP -32000) = Layer 1-2. Missing cells/outputs = Layer 3. No errors but wrong behavior = check CRDT convergence.

### Invariants to Verify in Any Concurrent Test

- `active_peers` equals the number of connected clients (check via `list_active_notebooks`)
- All cells created by all peers appear in the notebook after sync convergence
- Execution seq values are unique and monotonically increasing per room
- A peer disconnecting does not interrupt another peer's in-flight execution
- Kernel broadcasts reach all connected peers (check output visibility)
- File-backed notebook survives all-peer-disconnect and is loadable by a new session

## Gremlin Harness Constraints

- Each gremlin gets its own `runt mcp` stdio process. The harness manages lifecycle.
- MCP processes share the daemon socket but have independent connections.
- `depends_on` is all-or-nothing: the dependency must fully complete before the dependent starts. There is no "signal" primitive for overlap-window testing.
- `daemon-recovery` gremlin has `run_last=True` because it stops/restarts the daemon, which kills all other MCP connections.
- Fixture directories are reset between runs by `init-fixtures.sh`. Gremlins with `skip_reset=True` (like `collaborator-b`) see the previous gremlin's output.
- Budget (max_turns) is the primary limiter. Concurrent notebook testing patterns should be designed to complete within 25-35 turns.

## Validation Checklist

Before claiming a concurrent MCP scenario works:

- Verify daemon version matches expectations (`runt-nightly daemon status`)
- Check that `active_peers` is correct throughout the test (not just at start/end)
- Confirm all peers' writes appear in the final notebook state
- Verify execution outputs are delivered to all connected peers (not just the requester)
- Check daemon logs for `Lagged` warnings (broadcast overflow) — acceptable but worth noting
- Confirm clean disconnect doesn't trigger premature eviction (check room still exists after one peer leaves)
- For ephemeral notebooks: verify eviction occurs only after ALL peers disconnect
