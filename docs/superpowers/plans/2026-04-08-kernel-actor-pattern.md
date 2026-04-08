# Kernel Actor Pattern Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate all 19 mutex-across-await violations in `runtime_agent.rs` and `kernel_manager.rs` by having the runtime agent's `select!` loop own the kernel directly — no `Arc<Mutex<Option<RoomKernel>>>`.

**Architecture:** Split `RoomKernel` into `KernelConnection` (trait for Jupyter ZeroMQ IO) and `KernelState` (execution queue/status state machine). Both are local variables in the agent's `select!` loop. The daemon-side `start_command_loop` is unchanged — this refactor only touches the runtime agent subprocess.

**Tech Stack:** Rust, tokio, async-trait, ZeroMQ (via runtimelib)

**Spec:** `docs/superpowers/specs/2026-04-08-kernel-actor-pattern-design.md`

**Branch:** `refactor/kernel-actor-pattern`

---

## File Map

| File | Action | Responsibility |
|------|--------|---------------|
| `crates/runtimed/src/kernel_connection.rs` | Create | `KernelConnection` trait + `JupyterKernel` impl (ZeroMQ IO extracted from `RoomKernel`) |
| `crates/runtimed/src/kernel_state.rs` | Create | `KernelState` struct (queue, executing, status — extracted from `RoomKernel`) |
| `crates/runtimed/src/runtime_agent.rs` | Modify | Own `Option<JupyterKernel>` + `KernelState` directly in select! loop, remove `RuntimeAgentContext.kernel` mutex |
| `crates/runtimed/src/kernel_manager.rs` | Modify | Keep `RoomKernel` for daemon-side `start_command_loop`, extract shared types |
| `crates/runtimed/src/lib.rs` | Modify | Add `kernel_connection` and `kernel_state` modules |
| `crates/runtimed/tests/tokio_mutex_lint.rs` | Modify | Gate `runtime_agent.rs` and `kernel_manager.rs` |

## Key Constraint

**`start_command_loop` in `kernel_manager.rs` stays unchanged.** It is used by the daemon-side code path (when no runtime agent subprocess is running). The `RoomKernel` struct remains in `kernel_manager.rs` for this purpose. We are only refactoring the runtime agent's internal ownership of the kernel.

This means `RoomKernel` continues to exist, but the agent subprocess no longer wraps it in `Arc<Mutex<Option<>>>`. Instead, the agent owns the kernel's IO connection and state machine as separate local variables.

---

### Task 1: Extract `KernelConnection` trait and `JupyterKernel` struct

**Files:**
- Create: `crates/runtimed/src/kernel_connection.rs`
- Modify: `crates/runtimed/src/lib.rs`

This task defines the trait and a struct that wraps the IO-bound parts of `RoomKernel`. The `JupyterKernel` struct holds ZeroMQ connections, task handles, and request/response infrastructure — everything that talks to the kernel process. It does NOT hold queue, executing, status, or other state machine fields.

- [ ] **Step 1: Create `kernel_connection.rs` with the `KernelConnection` trait**

```rust
// crates/runtimed/src/kernel_connection.rs

use anyhow::Result;
use notebook_protocol::protocol::{CompletionItem, HistoryEntry};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc, RwLock};

use crate::blob_store::BlobStore;
use crate::kernel_manager::{
    KernelStatus, LaunchedEnvConfig, QueueCommand,
};
use crate::notebook_doc::RuntimeStateDoc;
use crate::presence::{self, PresenceState};

/// Configuration for launching a kernel.
pub struct KernelLaunchConfig {
    pub kernel_type: String,
    pub env_source: String,
    pub notebook_path: Option<PathBuf>,
    pub launched_config: LaunchedEnvConfig,
    pub env_vars: std::collections::HashMap<String, String>,
    pub pooled_env: Option<crate::pool::PooledEnv>,
}

/// Shared references passed to the kernel connection for background task use.
/// These are `Arc`/`broadcast` types that the IOPub, shell reader, and other
/// spawned tasks need to write outputs and signal state changes.
pub struct KernelSharedRefs {
    pub state_doc: Arc<RwLock<RuntimeStateDoc>>,
    pub state_changed_tx: broadcast::Sender<()>,
    pub blob_store: Arc<BlobStore>,
    pub broadcast_tx: broadcast::Sender<notebook_protocol::protocol::NotebookBroadcast>,
    pub presence: Arc<RwLock<PresenceState>>,
    pub presence_tx: broadcast::Sender<(String, Vec<u8>)>,
}

/// The IO boundary for talking to a kernel process.
///
/// This is an internal abstraction for the Jupyter ZeroMQ layer within the
/// runtime agent. It is NOT a plugin interface for new runtime types — those
/// would speak the document protocol directly to the daemon.
#[async_trait::async_trait]
pub trait KernelConnection: Send + 'static {
    /// Launch the kernel process. Returns the command receiver for queue events.
    async fn launch(
        config: KernelLaunchConfig,
        shared: KernelSharedRefs,
    ) -> Result<(Self, mpsc::Receiver<QueueCommand>)>
    where
        Self: Sized;

    /// Send an execute_request for the given cell.
    async fn execute(
        &mut self,
        cell_id: &str,
        execution_id: &str,
        source: &str,
    ) -> Result<()>;

    /// Request interrupt (e.g., SIGINT via control channel).
    async fn interrupt(&mut self) -> Result<()>;

    /// Graceful shutdown. Joins background tasks.
    async fn shutdown(&mut self) -> Result<()>;

    /// Send a comm message to the kernel (widget interactions).
    async fn send_comm_message(&mut self, raw_message: serde_json::Value) -> Result<()>;

    /// Send a comm_update (frontend -> kernel widget state sync).
    async fn send_comm_update(
        &mut self,
        comm_id: &str,
        state: serde_json::Value,
    ) -> Result<()>;

    /// Request code completions (timeout handled internally).
    async fn complete(
        &self,
        code: &str,
        cursor_pos: usize,
    ) -> Result<(Vec<CompletionItem>, usize, usize)>;

    /// Request execution history (timeout handled internally).
    async fn get_history(
        &self,
        pattern: Option<String>,
        n: i32,
        unique: bool,
    ) -> Result<Vec<HistoryEntry>>;

    /// Read-only metadata.
    fn kernel_type(&self) -> &str;
    fn env_source(&self) -> &str;
    fn launched_config(&self) -> &LaunchedEnvConfig;
    fn env_path(&self) -> Option<&PathBuf>;

    /// Whether the kernel has an active shell connection.
    fn is_connected(&self) -> bool;

    /// Update launched config after hot-sync (UV deps changed).
    fn update_launched_uv_deps(&mut self, deps: Vec<String>);
}
```

- [ ] **Step 2: Add module to `lib.rs`**

Add to `crates/runtimed/src/lib.rs`:
```rust
pub mod kernel_connection;
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p runtimed`
Expected: Compiles (trait has no implementors yet, that's fine)

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed/src/kernel_connection.rs crates/runtimed/src/lib.rs
git commit -m "refactor(runtimed): define KernelConnection trait for Jupyter IO boundary"
```

---

### Task 2: Extract `KernelState` struct

**Files:**
- Create: `crates/runtimed/src/kernel_state.rs`
- Modify: `crates/runtimed/src/lib.rs`

This task extracts the execution state machine from `RoomKernel` into a standalone struct. `KernelState` owns the queue, executing cell, status, and error tracking. It writes to RuntimeStateDoc for CRDT state.

- [ ] **Step 1: Create `kernel_state.rs`**

```rust
// crates/runtimed/src/kernel_state.rs

use std::collections::VecDeque;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};

use crate::kernel_connection::KernelConnection;
use crate::kernel_manager::{KernelStatus, QueuedCell};
use crate::notebook_doc::RuntimeStateDoc;
use notebook_protocol::protocol::{NotebookBroadcast, QueueEntry};

/// Execution state machine for the runtime agent.
///
/// Owns the queue, executing cell, status — the parts of RoomKernel that
/// are pure state, not IO. Owned directly by the agent's select! loop
/// as a local variable (no mutex needed).
pub struct KernelState {
    queue: VecDeque<QueuedCell>,
    executing: Option<(String, String)>,
    execution_had_error: bool,
    status: KernelStatus,
    state_doc: Arc<RwLock<RuntimeStateDoc>>,
    state_changed_tx: broadcast::Sender<()>,
    broadcast_tx: broadcast::Sender<NotebookBroadcast>,
}

impl KernelState {
    pub fn new(
        state_doc: Arc<RwLock<RuntimeStateDoc>>,
        state_changed_tx: broadcast::Sender<()>,
        broadcast_tx: broadcast::Sender<NotebookBroadcast>,
    ) -> Self {
        Self {
            queue: VecDeque::new(),
            executing: None,
            execution_had_error: false,
            status: KernelStatus::Starting,
            state_doc,
            state_changed_tx,
            broadcast_tx,
        }
    }

    /// Reset state for a new kernel launch.
    pub fn reset(&mut self) {
        self.queue.clear();
        self.executing = None;
        self.execution_had_error = false;
        self.status = KernelStatus::Starting;
    }

    /// Queue a cell for execution. Writes queued status to RuntimeStateDoc.
    pub async fn queue_cell(
        &mut self,
        cell_id: String,
        execution_id: String,
        source: String,
        conn: &mut impl KernelConnection,
    ) {
        self.queue.push_back(QueuedCell {
            cell_id,
            execution_id,
            code: source,
        });
        self.write_queue_to_state_doc().await;
        self.process_next(conn).await;
    }

    /// Called when IOPub reports idle. Clears executing, processes next.
    pub async fn execution_done(
        &mut self,
        _cell_id: &str,
        _execution_id: &str,
        conn: &mut impl KernelConnection,
    ) {
        self.executing = None;
        self.execution_had_error = false;
        self.status = KernelStatus::Idle;
        self.write_queue_to_state_doc().await;
        let _ = self.broadcast_tx.send(NotebookBroadcast::ExecutionDone);
        self.process_next(conn).await;
    }

    /// Mark current execution as errored (for stop-on-error).
    pub fn mark_execution_error(&mut self) {
        self.execution_had_error = true;
    }

    /// Clear the queue. Returns cleared entries for broadcasting.
    pub fn clear_queue(&mut self) -> Vec<QueueEntry> {
        let cleared: Vec<QueueEntry> = self
            .queue
            .drain(..)
            .map(|q| QueueEntry {
                cell_id: q.cell_id,
                execution_id: q.execution_id,
            })
            .collect();
        cleared
    }

    /// Kernel process died. Set error state, clear queue.
    pub fn kernel_died(&mut self) -> (Option<(String, String)>, Vec<QueueEntry>) {
        self.status = KernelStatus::Error;
        let interrupted = self.executing.take();
        let cleared = self.clear_queue();
        (interrupted, cleared)
    }

    /// Set status to idle (after launch completes).
    pub fn set_idle(&mut self) {
        self.status = KernelStatus::Idle;
    }

    /// Pop next from queue and execute via the connection.
    pub async fn process_next(&mut self, conn: &mut impl KernelConnection) {
        if self.executing.is_some() {
            return; // Already executing
        }
        if self.execution_had_error {
            // Stop-on-error: don't process more cells
            return;
        }
        if let Some(queued) = self.queue.pop_front() {
            self.executing = Some((queued.cell_id.clone(), queued.execution_id.clone()));
            self.status = KernelStatus::Busy;
            self.write_queue_to_state_doc().await;
            if let Err(e) = conn
                .execute(&queued.cell_id, &queued.execution_id, &queued.code)
                .await
            {
                log::warn!(
                    "[kernel-state] Failed to execute cell {}: {}",
                    queued.cell_id,
                    e
                );
                self.executing = None;
                self.status = KernelStatus::Idle;
            }
        }
    }

    // -- Read-only accessors --

    pub fn status(&self) -> KernelStatus {
        self.status
    }

    pub fn is_running(&self) -> bool {
        self.status != KernelStatus::Dead && self.status != KernelStatus::Error
    }

    pub fn executing_cell(&self) -> Option<&(String, String)> {
        self.executing.as_ref()
    }

    pub fn queued_entries(&self) -> Vec<QueueEntry> {
        self.queue
            .iter()
            .map(|q| QueueEntry {
                cell_id: q.cell_id.clone(),
                execution_id: q.execution_id.clone(),
            })
            .collect()
    }

    // -- Private helpers --

    async fn write_queue_to_state_doc(&self) {
        let queue_entries = self.queued_entries();
        let executing = self.executing.as_ref().map(|(cid, eid)| QueueEntry {
            cell_id: cid.clone(),
            execution_id: eid.clone(),
        });
        let mut sd = self.state_doc.write().await;
        sd.set_queue(executing, &queue_entries);
        let _ = self.state_changed_tx.send(());
    }
}
```

- [ ] **Step 2: Add module to `lib.rs`**

Add to `crates/runtimed/src/lib.rs`:
```rust
pub mod kernel_state;
```

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p runtimed`
Expected: Compiles

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed/src/kernel_state.rs crates/runtimed/src/lib.rs
git commit -m "refactor(runtimed): extract KernelState execution state machine"
```

---

### Task 3: Implement `JupyterKernel` (KernelConnection for ZeroMQ)

**Files:**
- Create: `crates/runtimed/src/jupyter_kernel.rs` (or extend `kernel_connection.rs`)
- Modify: `crates/runtimed/src/lib.rs`

This is the largest task. `JupyterKernel` wraps the IO-bound parts of `RoomKernel`: ZeroMQ connections, session ID, shell_writer, cell_id_map, pending_completions/history, stream_terminals, and all spawned task handles. The `launch()` method contains the bulk of `RoomKernel::launch()` (process spawning, ZeroMQ setup, task spawning).

- [ ] **Step 1: Create `jupyter_kernel.rs` with the struct definition**

Extract the IO-bound fields from `RoomKernel` into `JupyterKernel`:

```rust
// crates/runtimed/src/jupyter_kernel.rs

pub struct JupyterKernel {
    // Identity
    kernel_type: String,
    env_source: String,
    launched_config: LaunchedEnvConfig,
    pub env_path: Option<PathBuf>,
    session_id: String,
    kernel_actor_id: String,

    // ZeroMQ connections
    connection_info: Option<ConnectionInfo>,
    connection_file: Option<PathBuf>,
    shell_writer: Option<runtimelib::DealerSendConnection>,

    // Process management
    #[cfg(unix)]
    process_group_id: Option<i32>,
    kernel_id: Option<String>,

    // Background task handles
    iopub_task: Option<tokio::task::JoinHandle<()>>,
    shell_reader_task: Option<tokio::task::JoinHandle<()>>,
    process_watcher_task: Option<tokio::task::JoinHandle<()>>,
    heartbeat_task: Option<tokio::task::JoinHandle<()>>,
    comm_coalesce_tx: Option<mpsc::UnboundedSender<(String, serde_json::Value)>>,
    comm_coalesce_task: Option<tokio::task::JoinHandle<()>>,

    // Request/response infrastructure
    cell_id_map: Arc<StdMutex<HashMap<String, (String, String)>>>,
    cmd_tx: Option<mpsc::Sender<QueueCommand>>,
    comm_seq: Arc<AtomicU64>,
    pending_history: Arc<StdMutex<HashMap<String, oneshot::Sender<Vec<HistoryEntry>>>>>,
    pending_completions: PendingCompletions,
    stream_terminals: Arc<tokio::sync::Mutex<StreamTerminals>>,
}
```

Note: This does NOT include `queue`, `executing`, `execution_had_error`, `status`, or any of the broadcast/state_doc/presence fields that are in `KernelState` or `KernelSharedRefs`.

- [ ] **Step 2: Implement `KernelConnection` for `JupyterKernel`**

The implementation delegates to the existing `RoomKernel` methods. For this step, write the trait impl with method bodies that call into the existing code from `kernel_manager.rs`. The key methods:

- `launch()`: Adapts `RoomKernel::new()` + `RoomKernel::launch()`, returning `(Self, cmd_rx)`
- `execute()`: Adapts `RoomKernel::process_next()`'s inner ZeroMQ send logic
- `interrupt()`: Adapts `RoomKernel::interrupt()`
- `shutdown()`: Adapts `RoomKernel::shutdown()`
- `send_comm_message()`: Adapts `RoomKernel::send_comm_message()`
- `send_comm_update()`: Adapts `RoomKernel::send_comm_update()`
- `complete()`: Adapts `RoomKernel::complete()`
- `get_history()`: Adapts `RoomKernel::get_history()`

**Important:** Do NOT rewrite `launch()` from scratch. Extract the code from `RoomKernel::launch()` in `kernel_manager.rs` (lines 803-2674). This is ~1800 lines of kernel process spawning, ZeroMQ connection setup, and background task spawning. Move it into `JupyterKernel::launch()`, adjusting field references. The queue/state writes that were in `launch()` (broadcasting status, writing to state_doc) should be removed from `JupyterKernel::launch()` — those are the caller's responsibility (the agent loop via `KernelState`).

- [ ] **Step 3: Verify compilation**

Run: `cargo check -p runtimed`
Expected: Compiles (JupyterKernel may have unused warnings, that's expected)

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed/src/jupyter_kernel.rs crates/runtimed/src/lib.rs
git commit -m "refactor(runtimed): implement JupyterKernel as KernelConnection"
```

---

### Task 4: Refactor runtime agent to own kernel directly

**Files:**
- Modify: `crates/runtimed/src/runtime_agent.rs`

This is the core change. Replace `RuntimeAgentContext.kernel: Arc<Mutex<Option<RoomKernel>>>` with two local variables in the select! loop: `kernel: Option<JupyterKernel>` and `state: KernelState`.

- [ ] **Step 1: Update `RuntimeAgentContext` to remove the kernel mutex**

Remove the `kernel` field from `RuntimeAgentContext`. The context keeps non-kernel shared state:

```rust
struct RuntimeAgentContext {
    // kernel: Arc<tokio::sync::Mutex<Option<RoomKernel>>>,  // REMOVED
    state_doc: Arc<RwLock<RuntimeStateDoc>>,
    state_changed_tx: broadcast::Sender<()>,
    blob_store: Arc<BlobStore>,
    broadcast_tx: broadcast::Sender<notebook_protocol::protocol::NotebookBroadcast>,
    presence: Arc<RwLock<PresenceState>>,
    presence_tx: broadcast::Sender<(String, Vec<u8>)>,
    seen_execution_ids: HashSet<String>,  // No longer needs Mutex — owned by loop
}
```

- [ ] **Step 2: Add kernel and state as local variables in the agent function**

In the function that contains the select! loop, add:

```rust
let mut kernel: Option<JupyterKernel> = None;
let mut kernel_state = KernelState::new(
    ctx.state_doc.clone(),
    ctx.state_changed_tx.clone(),
    ctx.broadcast_tx.clone(),
);
let mut cmd_rx: Option<mpsc::Receiver<QueueCommand>> = None;
```

- [ ] **Step 3: Refactor `handle_runtime_agent_request` to take `&mut Option<JupyterKernel>` and `&mut KernelState`**

Change the signature from:
```rust
async fn handle_runtime_agent_request(
    request: RuntimeAgentRequest,
    ctx: &RuntimeAgentContext,
) -> RuntimeAgentResponse
```

To:
```rust
async fn handle_runtime_agent_request(
    request: RuntimeAgentRequest,
    kernel: &mut Option<JupyterKernel>,
    state: &mut KernelState,
    ctx: &RuntimeAgentContext,
) -> (RuntimeAgentResponse, Option<mpsc::Receiver<QueueCommand>>)
```

The return tuple includes an optional `cmd_rx` from launch (the caller stores it).

Each match arm changes from `ctx.kernel.lock().await` to direct access on `kernel`:

**LaunchKernel:**
```rust
let shared = KernelSharedRefs { /* from ctx */ };
match JupyterKernel::launch(config, shared).await {
    Ok((k, rx)) => {
        *kernel = Some(k);
        state.reset();
        state.set_idle();
        (RuntimeAgentResponse::KernelLaunched { env_source }, Some(rx))
    }
    Err(e) => (RuntimeAgentResponse::Error { error: e.to_string() }, None)
}
```

**InterruptExecution:**
```rust
if let Some(ref mut k) = kernel {
    match k.interrupt().await {
        Ok(()) => {
            let cleared = state.clear_queue();
            // ...
        }
        Err(e) => { /* ... */ }
    }
} else {
    RuntimeAgentResponse::Error { error: "No kernel running".into() }
}
```

**ShutdownKernel:**
```rust
if let Some(ref mut k) = kernel {
    k.shutdown().await.ok();
}
*kernel = None;
state.reset();
```

**SendComm, Complete, GetHistory** — same pattern: `if let Some(ref mut k) = kernel`.

- [ ] **Step 4: Refactor `handle_queue_command` to take `&mut Option<JupyterKernel>` and `&mut KernelState`**

Change from locking `ctx.kernel` to direct access:

**ExecutionDone:**
```rust
if let Some(ref mut k) = kernel {
    state.execution_done(&cell_id, &execution_id, k).await;
}
```

**CellError:**
```rust
state.mark_execution_error();
let cleared = state.clear_queue();
// write to state_doc...
```

**SendCommUpdate:**
```rust
if let Some(ref mut k) = kernel {
    k.send_comm_update(&comm_id, state_val).await.ok();
}
```

**KernelDied:**
```rust
let (interrupted, cleared) = state.kernel_died();
// write to state_doc, broadcast...
```

- [ ] **Step 5: Refactor RuntimeStateSync handler to use direct kernel access**

Replace:
```rust
let mut guard = ctx.kernel.lock().await;
if let Some(ref mut k) = *guard {
    k.send_comm_update(...).await;
}
```

With:
```rust
if let Some(ref mut k) = kernel {
    k.send_comm_update(...).await;
}
```

And replace:
```rust
let mut guard = ctx.kernel.lock().await;
if let Some(ref mut k) = *guard {
    k.queue_cell_with_id(...).await;
}
```

With:
```rust
if let Some(ref mut k) = kernel {
    state.queue_cell(cell_id, execution_id, source, k).await;
}
```

- [ ] **Step 6: Update the select! loop**

The select! loop structure stays the same but passes `&mut kernel` and `&mut kernel_state` to handlers:

```rust
loop {
    tokio::select! {
        frame = recv_typed_frame(&mut reader) => {
            // ... parse frame ...
            match typed_frame.frame_type {
                NotebookFrameType::Request => {
                    let (response, new_cmd_rx) = handle_runtime_agent_request(
                        request, &mut kernel, &mut kernel_state, &ctx
                    ).await;
                    if let Some(rx) = new_cmd_rx {
                        cmd_rx = Some(rx);
                    }
                    // send response...
                }
                NotebookFrameType::RuntimeStateSync => {
                    // ... sync handling with &mut kernel, &mut kernel_state ...
                }
                // ...
            }
        }
        Some(command) = async {
            match cmd_rx.as_mut() {
                Some(rx) => rx.recv().await,
                None => std::future::pending().await,
            }
        } => {
            handle_queue_command(command, &mut kernel, &mut kernel_state, &ctx).await;
        }
        _ = state_changed_rx.recv() => {
            // sync RuntimeStateDoc — no kernel access needed
        }
    }
}
```

- [ ] **Step 7: Update cleanup on disconnect**

The shutdown at the end of the agent function (after the loop breaks) changes from:
```rust
let mut guard = ctx.kernel.lock().await;
if let Some(ref mut k) = *guard {
    k.shutdown().await.ok();
}
```
To:
```rust
if let Some(ref mut k) = kernel {
    k.shutdown().await.ok();
}
```

- [ ] **Step 8: Verify compilation**

Run: `cargo check -p runtimed`
Expected: Compiles with no mutex-across-await in runtime_agent.rs

- [ ] **Step 9: Run the lint test**

Run: `cargo test -p runtimed --test tokio_mutex_lint -- --nocapture 2>&1 | grep runtime_agent`
Expected: Zero violations for `runtime_agent.rs`

- [ ] **Step 10: Commit**

```bash
git add crates/runtimed/src/runtime_agent.rs
git commit -m "refactor(runtimed): agent select! loop owns kernel directly, no mutex"
```

---

### Task 5: Gate files and verify

**Files:**
- Modify: `crates/runtimed/tests/tokio_mutex_lint.rs`

- [ ] **Step 1: Add `runtime_agent.rs` to GATED_FILES**

```rust
const GATED_FILES: &[&str] = &[
    "daemon.rs",
    "notebook_sync_server.rs",
    "runtime_agent.rs",
    "sync_server.rs",
];
```

Note: `kernel_manager.rs` stays ungated because `start_command_loop` still uses the mutex pattern (daemon-side, different code path). The 8 `kernel_manager.rs` violations are daemon-side and tracked separately in #1641.

- [ ] **Step 2: Run full lint**

Run: `cargo xtask lint --fix`
Expected: All checks passed

- [ ] **Step 3: Run lint test**

Run: `cargo test -p runtimed --test tokio_mutex_lint`
Expected: PASS (runtime_agent.rs is gated, 0 violations in gated files)

- [ ] **Step 4: Run full test suite**

Run: `cargo test -p runtimed`
Expected: All tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed/tests/tokio_mutex_lint.rs
git commit -m "test(runtimed): gate runtime_agent.rs in tokio mutex lint"
```

---

### Task 6: Push and create draft PR

- [ ] **Step 1: Push branch**

```bash
git push -u origin refactor/kernel-actor-pattern
```

- [ ] **Step 2: Create draft PR**

```bash
gh pr create --draft \
  --title "refactor(runtimed): kernel actor pattern — agent owns kernel directly" \
  --body "..."
```

Reference #1641 in the body. Mention that this eliminates 11 of the remaining 19 violations (the runtime_agent.rs ones). The 8 kernel_manager.rs violations are daemon-side and tracked separately.

---

## Implementation Notes

### What to extract vs what to delegate

The `JupyterKernel::launch()` method is ~1800 lines in `RoomKernel::launch()`. Rather than rewriting it, **move the code** — extract the function body, adjust field references from `self.field` to `self.field` on the new struct. Remove queue/status writes from within `launch()` (those are now the caller's job via `KernelState`).

The IOPub task, shell reader task, process watcher task, heartbeat task, and comm coalesce task all stay as spawned tasks inside `JupyterKernel`. They communicate back via `cmd_tx` (which becomes `cmd_rx` in the agent's select! loop). They reference `cell_id_map`, `pending_completions`, `pending_history`, `stream_terminals` via `Arc` — these stay as fields on `JupyterKernel`.

### What stays in `RoomKernel`

`RoomKernel` in `kernel_manager.rs` stays intact. It is used by the daemon-side `start_command_loop()` which runs when no runtime agent subprocess is available. That code path still uses `Arc<Mutex<Option<RoomKernel>>>` — those 8 violations in `kernel_manager.rs` are a separate concern (tracked in #1641 as Phase 2 daemon-side work, or addressed when the runtime agent becomes the sole kernel owner).

### Testing strategy

This is a refactor — behavior should be identical. The primary verification is:
1. `cargo check -p runtimed` compiles
2. `cargo test -p runtimed --test tokio_mutex_lint` shows 0 violations for `runtime_agent.rs`
3. `cargo test -p runtimed` all existing tests pass
4. Manual: launch notebook, execute cells, interrupt, restart kernel, widget interaction
5. Integration tests: `cargo xtask integration` (if daemon is running)
