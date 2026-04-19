# Spawn panic handling audit — `crates/runtimed`

Date: 2026-04-19
Branch: `docs/spawn-panic-audit`
Status: Phase 1 — audit + design. No code changes in this PR.

## Diagnosis

We finished the "no panic in non-test code" pass in PRs #1916–#1918 and
#1923–#1925. `expect()`/`unwrap()` are largely gone from daemon source paths.
That work did **not** address what happens when a panic still reaches a
`tokio::spawn`ed task from another source: an `unreachable!()` we thought was
unreachable, a debug `assert!`, a third-party crate panicking, or an automerge
bug like the `MissingOps` collector panic we already catch explicitly.

When a spawned task panics, tokio's default behavior is:

1. The panic unwinds the task future.
2. Its `JoinHandle` resolves to `Err(JoinError::panic)`.
3. If nobody holds or awaits the `JoinHandle`, the panic payload is dropped.
4. The panic is surfaced only via the process-wide panic hook — a `tracing`
   log in our build. The worker is gone; anything waiting on it now hangs,
   and the daemon process keeps running in a degraded state.

"Crashing is just not cool. I don't even know if we catch when that happens in
all cases." — Kyle

The audit below shows: we don't catch it. 23 of 27 production spawn sites in
`crates/runtimed` are fire-and-forget or abort-only. None of them log a
JoinError on panic. One code path (`catch_automerge_panic`) exists for a
specific automerge bug but is not applied as a task-level guard.

## Scope

`crates/runtimed/src/` only, as instructed. 30 `tokio::spawn` call sites total
(27 production + 3 in tests). Other crates (`runt-mcp`, `notebook-sync`,
`runt-cli`, `mcp-supervisor`) are out of scope for this pass — brief
observations are at the end.

## Inventory

Legend for **Current**:
- **Orphan**: JoinHandle dropped immediately. Panic silent.
- **Abort-only**: JoinHandle stored, used only via `.abort()` on shutdown.
  Panic silent during normal operation.
- **Supervised**: already wraps body in a panic-catching helper.
- **Test**: `#[tokio::test]` block.

| # | File:line | Task | Current | Proposed | Notes |
|---|-----------|------|---------|----------|-------|
| 1 | `runtime_agent_handle.rs:88` | Runtime agent child-process watcher (`child.wait().await` → set `alive=false`) | Orphan | `spawn_supervised` (log+continue). Body is small, but if panic, `alive` stays `true` forever → daemon thinks agent is live, never restarts. | Structural alt: stash JoinHandle on `RuntimeAgentHandle` and inspect on drop. Overkill. |
| 2 | `blob_server.rs:42` | Blob HTTP accept loop | Orphan | `spawn_supervised` with **shutdown on panic**. Accept-loop panic means the blob server is dead for the whole process lifetime — every image output breaks. | Long-lived, load-bearing. |
| 3 | `blob_server.rs:48` | Per-connection HTTP handler | Orphan | `spawn_best_effort` (log-only). One connection dying is fine. | High-cardinality; many of these. |
| 4 | `main.rs:439` | SIGTERM/SIGINT handler | Orphan | `spawn_supervised`. Panic here means no graceful shutdown. Low probability. | Body already uses `expect()` for signal registration; that's OS-fatal and correct. |
| 5 | `jupyter_kernel.rs:476` | Kernel stderr log pump | Orphan | `spawn_best_effort`. Diagnostic-only. | If it panics we lose kernel stderr until restart. |
| 6 | `jupyter_kernel.rs:550` | `process_watcher_task` — waits on kernel pid, sends `KernelDied` | Abort-only | `spawn_supervised`. On panic, also send `KernelDied` so the queue observer sees something. | Load-bearing: without it the kernel death signal is lost. |
| 7 | `jupyter_kernel.rs:588` | `iopub_task` — main kernel output pump | Abort-only | `spawn_supervised` with **kernel-died signal on panic**. | The largest and most-touched task. Any panic here silently halts all output delivery for that kernel. Q: should we auto-restart the kernel, or surface to frontend as "kernel unresponsive"? See design questions. |
| 8 | `jupyter_kernel.rs:1631` | `shell_reader_task` — kernel shell replies | Abort-only | `spawn_supervised` with kernel-died signal on panic. | Same rationale as iopub. |
| 9 | `jupyter_kernel.rs:1819` | `heartbeat_task` | Abort-only | `spawn_supervised`. Panic → KernelDied. | Already sends KernelDied on timeout, natural extension. |
| 10 | `jupyter_kernel.rs:1862` | `comm_coalesce_task` — widget state batcher | Abort-only | `spawn_supervised`. On panic, log and let widget updates stall for this kernel; don't take the whole daemon down. | Side-effect is degraded widgets, not lost data. |
| 11 | `daemon.rs:215` | `spawn_env_deletions` — rm -rf stale env dirs | Orphan | `spawn_best_effort`. Genuinely best-effort filesystem cleanup. | Document as such. |
| 12 | `daemon.rs:783` | `uv_warming_loop` | Orphan | `spawn_supervised` with **daemon shutdown on panic**. | Long-lived pool filler. If it dies the UV pool never refills, all UV kernel launches block until 120s timeout. |
| 13 | `daemon.rs:788` | `conda_warming_loop` | Orphan | `spawn_supervised` + shutdown. | Same as above. |
| 14 | `daemon.rs:793` | `pixi_warming_loop` | Orphan | `spawn_supervised` + shutdown. | Same. |
| 15 | `daemon.rs:799` | `env_gc_loop` | Orphan | `spawn_supervised` (log + restart? or just log). | Degradation, not data loss. Stale dirs accumulate. |
| 16 | `daemon.rs:805` | `watch_settings_json` | Orphan | `spawn_supervised`. Settings stop syncing if this dies. | User-visible degradation. |
| 17 | `daemon.rs:893` | Per-connection unix socket handler | Orphan | `spawn_best_effort`. One client's connection dying is fine. | High-cardinality. |
| 18 | `daemon.rs:958` | Per-connection windows named-pipe handler | Orphan | `spawn_best_effort`. | Same as 17. |
| 19 | `daemon.rs:2083` | `create_uv_env` post-take replenish | Orphan | `spawn_best_effort` with explicit warming-counter rollback on panic. | If body panics, `mark_warming` increments are never rolled back — pool accounting drifts. Q: safe to rely on `PoolEntry` Drop + `mark_warming` compensation, or do we need a scope guard? |
| 20 | `daemon.rs:2108` | `create_uv_env` retry spawn | Orphan | `spawn_best_effort` + rollback. | Same as 19. |
| 21 | `daemon.rs:2158` | `replenish_conda_env` | Orphan | `spawn_best_effort` + rollback. | Same as 19. |
| 22 | `daemon.rs:2183` | `create_conda_env` retry | Orphan | `spawn_best_effort` + rollback. | Same as 19. |
| 23 | `daemon.rs:2228` | `replenish_pixi_env` | Orphan | `spawn_best_effort` + rollback. | Same as 19. |
| 24 | `notebook_sync_server.rs:2034` | `auto_launch_kernel` on first connect | Orphan | `spawn_supervised`. If this panics the frontend is stuck in "Initializing" forever. | User-visible. Should flip `kernel_status` to `error` on panic. |
| 25 | `notebook_sync_server.rs:2130` | Room eviction delay + teardown | Orphan | `spawn_supervised`. On panic, room leaks — kernel, agent, blob handles all held. | Long delay (default 30s), memory and resource leak risk. |
| 26 | `notebook_sync_server.rs:5934` | Background cell formatter | Orphan | `spawn_best_effort`. | Body already does fork+merge with `catch_automerge_panic`. A formatter panic outside the merge is non-fatal. |
| 27 | `notebook_sync_server.rs:7906` | `spawn_persist_debouncer` — writes .automerge snapshot | Orphan | `spawn_supervised`. Panic here means untitled notebooks stop persisting → crash-recovery breaks silently. | Load-bearing. |
| 28 | `notebook_sync_server.rs:7996` | `spawn_autosave_debouncer` — writes .ipynb | Orphan | `spawn_supervised`. Panic means user edits stop saving to disk. | Critically load-bearing — user data. |
| 29 | `notebook_sync_server.rs:9450` | `spawn_notebook_file_watcher` | Orphan | `spawn_supervised`. Panic means external file changes silently stop merging. | Degradation, no data loss. |
| 30 | `notebook_sync_server.rs:12900` | Test helper | Test | Leave as-is. | Not production. |

**Counts (production only, 27 sites):**
- Orphan: 23
- Abort-only: 5 (the jupyter_kernel tasks, with a stored JoinHandle whose
  only use is `.abort()`)
- Supervised (body-level wrapper): 0 at the task boundary. A handful of
  *internal* calls already use `catch_automerge_panic` for CRDT mutations,
  but that's per-operation, not per-task.

No existing task boundary catches panics today.

## Proposed helper API

One module `crates/runtimed/src/task_supervisor.rs`. Two functions:

```rust
/// Spawn a task that logs panics and triggers a daemon action on panic.
///
/// Use for long-lived workers whose death should not be silent. On panic,
/// the panic is caught, logged with `label`, and `on_panic` runs (typical:
/// flip a status, send a signal, or trigger shutdown).
///
/// Returns a `JoinHandle` so callers that want to store and `abort()` still
/// can — mirrors the `tokio::spawn` surface.
pub fn spawn_supervised<F, P>(
    label: &'static str,
    fut: F,
    on_panic: P,
) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
    P: FnOnce(&PanicInfo) + Send + 'static;

/// Spawn a task that logs panics but does nothing else.
///
/// Use for genuinely best-effort work (filesystem cleanup, per-connection
/// handlers, diagnostic pumps) where the panic is worth knowing about but
/// no action is needed. Still better than default tokio behavior because
/// we downcast the payload to a string and emit a `warn!` with `label`.
pub fn spawn_best_effort<F>(
    label: &'static str,
    fut: F,
) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static;
```

Implementation sketch:

```rust
use std::future::Future;
use std::panic::AssertUnwindSafe;
use futures::FutureExt; // for catch_unwind

pub struct PanicInfo {
    pub label: &'static str,
    pub message: String,
}

pub fn spawn_best_effort<F>(label: &'static str, fut: F) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(payload) = AssertUnwindSafe(fut).catch_unwind().await {
            let msg = panic_payload_to_string(payload);
            tracing::error!(
                "[task-supervisor] '{label}' panicked (best-effort, logged only): {msg}"
            );
        }
    })
}

pub fn spawn_supervised<F, P>(
    label: &'static str,
    fut: F,
    on_panic: P,
) -> tokio::task::JoinHandle<()>
where
    F: Future<Output = ()> + Send + 'static,
    P: FnOnce(&PanicInfo) + Send + 'static,
{
    tokio::spawn(async move {
        if let Err(payload) = AssertUnwindSafe(fut).catch_unwind().await {
            let message = panic_payload_to_string(payload);
            let info = PanicInfo { label, message };
            tracing::error!(
                "[task-supervisor] '{label}' panicked: {}",
                info.message
            );
            on_panic(&info);
        }
    })
}
```

`panic_payload_to_string` is the same downcast chain already in
`catch_automerge_panic`. Good opportunity to extract and share.

### When to use which

- **`spawn_supervised`** when a panic should cause *something else to happen*:
  flip kernel status to error, send `KernelDied`, trigger `shutdown_notify`,
  mark the notebook dirty. Long-lived workers (warming loops, autosave,
  persist debouncer, room eviction).

- **`spawn_best_effort`** when a panic is survivable and only needs a log:
  filesystem cleanup, per-connection handlers, diagnostic log pumps, background
  formatters. High-cardinality spawns (accept loops spawn many of these).

### What we explicitly do **not** propose

- A `spawn_fatal` that aborts the process. We have `trigger_shutdown()` on
  the daemon; use that as `on_panic` where appropriate. `std::process::abort`
  loses logs and runs no destructors.
- A `spawn_restart` with automatic retry. That's a future phase if we decide
  we want self-healing workers; it's a real design call, not a mechanical
  migration.

## Migration plan (Phase 2)

23 orphan sites + 5 abort-only sites = 28 call sites to touch. Breakdown by
proposed helper:

| Helper | Count | Sites |
|--------|-------|-------|
| `spawn_supervised` | 16 | 1, 2, 4, 6, 7, 8, 9, 10, 12, 13, 14, 15, 16, 24, 25, 27, 28, 29 (minus a few that move to best-effort) |
| `spawn_best_effort` | 12 | 3, 5, 11, 17, 18, 19, 20, 21, 22, 23, 26 |
| Leave as-is | 1 | 30 (test) |

Rough count stated; actual totals finalized during migration.

The 5 abort-only tasks keep their JoinHandle for `.abort()` — `spawn_supervised`
returns a `JoinHandle<()>` that's `.abort()`-compatible, so no struct changes.

## What breaks if we get this wrong

- Classifying a load-bearing task as `spawn_best_effort` is a regression:
  panic becomes silent again, same as today. **Audit gate:** everything
  labelled `spawn_supervised` in the table above must stay supervised.
- Wrapping a task that holds a `tokio::sync::Mutex` guard across `.await`
  in `catch_unwind` is subtle: the guard would be dropped during unwind, so
  it's actually *safer* than letting the panic propagate. But `AssertUnwindSafe`
  is a hint to the compiler — in practice our tasks all own their state
  through channels, so this is not a concern.
- `on_panic` callbacks must not themselves panic. Keep them trivial:
  `daemon.trigger_shutdown()`, `tx.try_send(KernelDied)`, `status.store(...)`.

## Design questions for Kyle

- **Q1 (iopub/shell panic recovery):** Should a panic in `iopub_task` or
  `shell_reader_task` send `KernelDied` (so the queue observer transitions
  the kernel to `error` and the frontend offers a restart), or should it
  trigger a silent kernel restart? Current proposal is `KernelDied` —
  explicit, user-visible, symmetric with other kernel failures.
- **Q2 (warming-loop panic):** Warming loops (#12-14) — `spawn_supervised`
  with what `on_panic`? Options: (a) `trigger_shutdown()` — brutal but
  honest; without warming, the daemon is useless. (b) Log only and accept
  that env launches will time out at 120s. (c) Attempt a single respawn.
  My vote: (b) — matches current behavior (silent today, logged tomorrow)
  without introducing restart semantics this pass.
- **Q3 (pool-accounting rollback):** Sites #19-23 (`create_uv_env` spawns)
  increment `mark_warming` before the spawn and rely on the spawned body
  to roll it back on failure. A panic would skip the rollback. Do we want
  a `PoolCounterGuard` RAII that decrements on drop, or handle it in
  `on_panic`? Slight preference for RAII — it's defensive against any
  early return, not just panics.
- **Q4 (autosave panic):** Site #28 is user data. Currently a panic means
  the user loses subsequent edits silently. Is it enough to log and let
  the next save-triggering broadcast re-subscribe? Probably not — there's
  no re-spawn logic. Options: (a) trigger_shutdown so the user sees the
  daemon die and reconnects, (b) surface to frontend as a broadcast, (c)
  respawn the task. Needs explicit decision.

## Notes on other crates (out of scope, follow-up)

Quick `grep` for `tokio::spawn` outside runtimed, not audited in detail:

- `crates/runt-mcp/src/` — MCP server. Spawns for tool-call handling and
  notification streams. Similar orphan pattern. Same helper would apply.
- `crates/notebook-sync/src/` — client-side sync. Spawns for the sync loop
  and RPC wait-loops. Panic here is visible to Python users.
- `crates/mcp-supervisor/src/` — already has a supervisor pattern for
  child processes; check whether its `tokio::spawn` sites get the same
  panic protection.

Leave these for a follow-up PR once the runtimed pattern is agreed.

## Summary

- 27 production spawn sites in `crates/runtimed/src/`. All 27 currently lose
  panic information.
- Propose two helpers: `spawn_supervised` (long-lived workers, custom
  on-panic action) and `spawn_best_effort` (side-effect-only, log-and-drop).
- Migrate all 27 in Phase 2 after Kyle resolves Q1–Q4.
- No new dependencies; `futures::FutureExt::catch_unwind` is already in the
  tree via existing usage.
