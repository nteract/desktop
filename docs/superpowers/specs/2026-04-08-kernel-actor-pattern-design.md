# Kernel Actor Pattern Design

## Goal

Eliminate the remaining 19 mutex-across-await violations in `runtime_agent.rs` (11) and `kernel_manager.rs` (8) by replacing `Arc<Mutex<Option<RoomKernel>>>` with an actor pattern where the kernel handle is owned directly by the runtime agent's `select!` loop — no mutex needed.

Simultaneously introduce a `KernelBackend` trait boundary to enable future runtime backends (remote kernels, native runtimes, sandboxed kernels).

## Context

- **Tracking issue:** #1641
- **Prior work:** #1637 (eviction deadlock fix), #1638 (cross-lock ordering), #1640 (lint upgrade), #1642 (Phase 1 burndown — 58 to 19 violations)
- **Reference patterns:** [tokio mini-redis](https://github.com/tokio-rs/mini-redis), [Alice Ryhl's "Actors with Tokio"](https://ryhl.io/blog/actors-with-tokio/)

## Architecture

Split the current `RoomKernel` monolith into two parts:

### `KernelBackend` (trait) — the IO boundary

Defines how to talk to a kernel. The current Jupyter/ZeroMQ implementation becomes `JupyterKernel`. Future backends implement the same trait.

Responsibilities:
- Launch the kernel process / connect to remote
- Send execute requests
- Interrupt / shut down
- Send comm messages (widget state)
- Handle completions and history requests

Does NOT own: execution queue, status state machine, CRDT queue writes.

### `KernelState` (struct) — the execution state machine

Extracted from `RoomKernel`. Owns the queue, executing cell, status, error tracking. Writes queue/execution state to RuntimeStateDoc.

Owned directly by the agent's `select!` loop — plain struct, no mutex. Only one task ever touches it.

### Agent `select!` loop — the actor

The runtime agent already acts as an actor (receives RPCs via `RuntimeAgentRequest`). Today it re-serializes kernel access via a mutex. After this refactor, the loop owns both `KernelState` and `KernelBackend` as local variables:

```
select! {
    frame from daemon => {
        match request {
            LaunchKernel    => backend = JupyterKernel::launch(...).await;
            Interrupt       => backend.interrupt().await;
            SendComm        => backend.send_comm(...).await;
            Complete        => backend.complete(...).await;
            GetHistory      => backend.get_history(...).await;
            ShutdownKernel  => backend.shutdown().await; state.clear();
        }
    }
    cmd from queue_rx => {
        match cmd {
            ExecutionDone   => state.execution_done(&backend).await;
            CellError       => state.mark_error(); state.clear_queue();
            KernelDied      => state.kernel_died();
            SendCommUpdate  => backend.send_comm_update(...).await;
        }
    }
    sync from state_changed_rx => { /* sync RuntimeStateDoc */ }
}
```

No mutex anywhere. The `select!` loop is the sole owner.

## `KernelBackend` Trait

```rust
#[async_trait]
pub trait KernelBackend: Send + 'static {
    async fn launch(
        config: KernelLaunchConfig,
        cmd_tx: mpsc::Sender<QueueCommand>,
        state_doc: Arc<RwLock<RuntimeStateDoc>>,
        blob_store: Arc<BlobStore>,
        broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    ) -> Result<Self> where Self: Sized;

    async fn execute(&mut self, cell_id: &str, execution_id: &str, source: &str) -> Result<()>;
    async fn interrupt(&mut self) -> Result<()>;
    async fn shutdown(&mut self) -> Result<()>;
    async fn send_comm_message(&mut self, msg: CommMessage) -> Result<()>;
    async fn send_comm_update(&mut self, target: &str, state: serde_json::Value) -> Result<()>;
    async fn complete(&self, source: &str, cursor_pos: usize) -> Result<CompletionReply>;
    async fn get_history(&self, max_items: u32) -> Result<Vec<HistoryEntry>>;

    fn kernel_type(&self) -> &str;
    fn env_source(&self) -> &str;
    fn launched_config(&self) -> &LaunchedEnvConfig;
}
```

Design choices:
- `launch` is an associated function (not `&mut self`) — constructs the backend. No un-launched backends.
- Background tasks (IOPub listener, process watcher, heartbeat) get `cmd_tx` to send events back to the agent's loop. They don't need the backend reference.
- `execute` takes cell metadata — queue management (what to execute next) stays in `KernelState`.
- No `execution_done` / `mark_error` / `clear_queue` — those are queue state, not backend operations.

## `KernelState` Struct

```rust
pub struct KernelState {
    queue: VecDeque<QueuedCell>,
    executing: Option<(String, String)>,  // (cell_id, execution_id)
    execution_had_error: bool,
    status: KernelStatus,
    state_doc: Arc<RwLock<RuntimeStateDoc>>,
    state_changed_tx: broadcast::Sender<()>,
}

impl KernelState {
    fn queue_cell(&mut self, cell_id: String, execution_id: String, source: String);
    async fn execution_done(&mut self, backend: &mut impl KernelBackend);
    fn mark_execution_error(&mut self);
    fn clear_queue(&mut self);
    fn kernel_died(&mut self);
    async fn process_next(&mut self, backend: &mut impl KernelBackend);

    fn status(&self) -> KernelStatus;
    fn is_running(&self) -> bool;
    fn executing_cell(&self) -> Option<&(String, String)>;
    fn queued_cells(&self) -> &VecDeque<QueuedCell>;
}
```

Only `process_next` and `execution_done` are async (they call `backend.execute()`). Everything else is pure state mutation. No locks needed.

## Data Flow

### Cell Execution (unchanged externally)

```
1. Daemon writes execution entry to RuntimeStateDoc (source, seq, status=queued)
2. Agent receives RuntimeStateSync frame
3. Agent calls state.queue_cell(...)
4. state.process_next(&backend) → backend.execute(cell_id, eid, source)
5. JupyterKernel sends execute_request via ZeroMQ shell
6. IOPub task reads kernel outputs → writes directly to RuntimeStateDoc/BlobStore
7. IOPub task sends QueueCommand::ExecutionDone via cmd_tx
8. Agent receives ExecutionDone → state.execution_done(&backend)
9. state.process_next(&backend) for next cell
```

### Output Hot Path (unchanged)

```
Kernel → ZeroMQ → IOPub task → writes outputs directly to CRDT/BlobStore
                             → sends ExecutionDone to actor loop (once per cell)
```

IOPub listener stays as a separate spawned task. It reads ZeroMQ at wire speed and writes outputs via `Arc` references. The actor loop is NOT in the output path — only the control path (launch, interrupt, queue transitions).

### Widget/Comm Path

```
Frontend CRDT → daemon → RuntimeStateSync → agent select! loop
    → backend.send_comm_update() → ZeroMQ shell → kernel
```

This goes through the actor loop but was already serialized by the mutex today. Channel dispatch is faster than `Mutex::lock().await`.

## What Changes

| Component | Before | After |
|-----------|--------|-------|
| `RoomKernel` | Monolith with queue + IO + state | Split into `KernelBackend` + `KernelState` |
| Kernel access | `Arc<Mutex<Option<RoomKernel>>>` | Local variables in `select!` loop |
| `RuntimeAgentContext` | Holds `kernel: Arc<Mutex<...>>` | Removed or holds `cmd_tx` only |
| `kernel_manager.rs` command loop | Locks kernel mutex, calls methods | Sends `QueueCommand` via channel (already does this for most operations) |
| IOPub task | Unchanged — writes directly to CRDT | Unchanged |
| Daemon | Unchanged — sends `RuntimeAgentRequest` | Unchanged |
| Frontend | Unchanged — reads CRDT | Unchanged |

## What Doesn't Change

- The daemon ↔ runtime agent protocol (RuntimeAgentRequest/Response)
- The CRDT-driven execution model (queue entries in RuntimeStateDoc)
- The IOPub task (output hot path)
- The blob store output resolution
- The `NotebookRoom` fields on the daemon side
- Any frontend code

## Scope

### In scope
- Extract `KernelBackend` trait with `JupyterKernel` as the sole implementation
- Extract `KernelState` from `RoomKernel`
- Restructure runtime agent's `select!` loop to own both directly
- Eliminate all 19 remaining mutex-across-await violations
- All existing tests must pass

### Out of scope
- Remote kernel backends
- Native runtime backends (non-Jupyter)
- Sandboxing (bubblewrap/seatbelt)
- Redesigning completions/history request/response pattern (stays as oneshot channels for now; see Future Work)
- Any changes to daemon or frontend

## Future Work

- **Completions/History simplification:** The current oneshot-channel pattern for `complete()` and `get_history()` works but is complex. Since we control the full stack, we could move to a simpler request-response model where the agent loop awaits the reply directly (no pending maps). This can be designed separately.
- **Remote kernels:** `RemoteKernel` implementing `KernelBackend` — connects via WebSocket/SSH instead of local ZeroMQ.
- **Native runtimes:** Runtimes that emit document protocol bits directly, no ZeroMQ wrapper. Implement `KernelBackend` without Jupyter protocol overhead.
- **Sandboxing:** `SandboxedKernel<T: KernelBackend>` — wraps any backend with cgroup/seatbelt/bubblewrap isolation.
- **Kernel actor pattern for `NotebookRoom` fields:** The daemon-side `runtime_agent_handle` / `runtime_agent_request_tx` fields on `NotebookRoom` could benefit from a similar actor treatment, but they were already cleaned up in #1642.

## Risks

- **IOPub task interaction:** The IOPub task currently reads `cell_id_map`, `pending_completions`, `pending_history`, and `stream_terminals` via `Arc<Mutex<...>>`. These are internal to `JupyterKernel` and stay behind their existing mutexes — they're accessed by the IOPub task (a separate spawned task within the backend) and are not the mutex we're eliminating. No change needed.
- **Blocking the select! loop:** If `backend.interrupt()` or `backend.shutdown()` takes a long time (ZeroMQ timeout), the agent can't process other requests. This is the same behavior as today (the mutex serialized everything). Mitigation: use timeouts on IO operations, same as we already do.
- **Execution ordering:** The `select!` loop processes events one at a time. If a burst of RuntimeStateSync frames arrive while `process_next` is awaiting `backend.execute()`, they'll queue up. This is the same as today (the mutex serialized access). The channel has backpressure.

## File Plan

| File | Action |
|------|--------|
| `crates/runtimed/src/kernel_backend.rs` | New — `KernelBackend` trait definition |
| `crates/runtimed/src/jupyter_kernel.rs` | New — `JupyterKernel` implementation (extracted from `RoomKernel`) |
| `crates/runtimed/src/kernel_state.rs` | New — `KernelState` struct (extracted from `RoomKernel`) |
| `crates/runtimed/src/kernel_manager.rs` | Modify — remove `RoomKernel`, update command loop to use `QueueCommand` channel |
| `crates/runtimed/src/runtime_agent.rs` | Modify — own `KernelBackend` + `KernelState` directly in `select!` loop, remove `RuntimeAgentContext.kernel` mutex |
| `crates/runtimed/src/lib.rs` | Modify — add new modules |
| `crates/runtimed/tests/tokio_mutex_lint.rs` | Modify — gate `runtime_agent.rs` and `kernel_manager.rs` |
