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
(29 production + 1 in a test block at `notebook_sync_server.rs:12900`). Other crates (`runt-mcp`, `notebook-sync`,
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
| 12 | `daemon.rs:783` | `uv_warming_loop` | Orphan | `spawn_supervised`. On-panic action: see Q2 — table resolves to **log-only** pending Kyle's call. | Long-lived pool filler. If it dies the UV pool never refills, all UV kernel launches block until 120s timeout. |
| 13 | `daemon.rs:788` | `conda_warming_loop` | Orphan | `spawn_supervised`. Same Q2. | Same as above. |
| 14 | `daemon.rs:793` | `pixi_warming_loop` | Orphan | `spawn_supervised`. Same Q2. | Same. |
| 15 | `daemon.rs:799` | `env_gc_loop` | Orphan | `spawn_supervised` (log + restart? or just log). | Degradation, not data loss. Stale dirs accumulate. |
| 16 | `daemon.rs:805` | `watch_settings_json` | Orphan | `spawn_supervised`. Settings stop syncing if this dies. | User-visible degradation. |
| 17 | `daemon.rs:893` | Per-connection unix socket handler | Orphan | `spawn_best_effort`. One client's connection dying is fine. | High-cardinality. |
| 18 | `daemon.rs:958` | Per-connection windows named-pipe handler | Orphan | `spawn_best_effort`. | Same as 17. |
| 19 | `daemon.rs:2083` | `create_uv_env` post-take replenish | Orphan | `spawn_best_effort`. No pre-spawn `mark_warming` here — the spawned body manages its own counter. | Corrected per codex review: this site does NOT pre-increment `warming`. Rollback concern applies only to #20, #22, and to the bodies of `replenish_*` (#21, #23). |
| 20 | `daemon.rs:2108` | `create_uv_env` retry after `mark_warming(1)` | Orphan | `spawn_best_effort` with warming-counter rollback on panic. | `mark_warming(1)` happens BEFORE the spawn at the call site — a panic inside the spawned body skips the rollback. Q3 applies. |
| 21 | `daemon.rs:2158` | `replenish_conda_env` | Orphan | `spawn_best_effort` with internal counter hygiene. | `mark_warming` happens inside the spawned body; ensure the helper drops it on panic. |
| 22 | `daemon.rs:2183` | `create_conda_env` retry after `mark_warming(1)` | Orphan | `spawn_best_effort` + rollback. | Same structure as #20. |
| 23 | `daemon.rs:2228` | `replenish_pixi_env` | Orphan | `spawn_best_effort` with internal counter hygiene. | Same as #21. |
| 24 | `notebook_sync_server.rs:2034` | `auto_launch_kernel` on first connect | Orphan | `spawn_supervised`. If this panics the frontend is stuck in "Initializing" forever. | User-visible. Should flip `kernel_status` to `error` on panic. |
| 25 | `notebook_sync_server.rs:2130` | Room eviction delay + teardown | Orphan | `spawn_supervised`. On panic, room leaks — kernel, agent, blob handles all held. | Long delay (default 30s), memory and resource leak risk. |
| 26 | `notebook_sync_server.rs:5934` | Background cell formatter | Orphan | `spawn_best_effort`. | Body already does fork+merge with `catch_automerge_panic`. A formatter panic outside the merge is non-fatal. |
| 27 | `notebook_sync_server.rs:7906` | `spawn_persist_debouncer` — writes .automerge snapshot | Orphan | `spawn_supervised`. Panic here means untitled notebooks stop persisting → crash-recovery breaks silently. | Load-bearing. |
| 28 | `notebook_sync_server.rs:7996` | `spawn_autosave_debouncer` — writes .ipynb | Orphan | `spawn_supervised`. Panic means user edits stop saving to disk. | Critically load-bearing — user data. |
| 29 | `notebook_sync_server.rs:9450` | `spawn_notebook_file_watcher` | Orphan | `spawn_supervised`. Panic means external file changes silently stop merging. | Degradation, no data loss. |
| 30 | `notebook_sync_server.rs:12900` | Test helper | Test | Leave as-is. | Not production. |

**Counts (production only, 29 sites):**
- Orphan: 24
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

**Phase 2 ordering and gating:**

1. Land `WarmingGuard` RAII + migrate the `create_uv_env`/`create_conda_env`
   pre-increment sites first, as its own PR. Unblocks Q3 without touching
   spawn wiring.
2. Land `spawn_supervised` + `spawn_best_effort` helpers in
   `crates/runtimed/src/task_supervisor.rs` (name TBD) with unit tests.
3. Migrate supervised sites in small, per-subsystem PRs (queue workers,
   kernel tasks, formatter, etc.) so each diff reads cleanly.
4. Migrate best-effort sites after supervised are done — lower-risk batch.
5. **Skip site #28 (autosave)** in this phase. Keep on `tokio::spawn` until
   Q4 research (task #70) decides the recovery story.
6. After all migrations land, grep for remaining `tokio::spawn` in
   `crates/runtimed/src/` — anything left is either #28 or a bug.

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

## Design decisions (answered by Kyle 2026-04-19)

- **Q1 (iopub/shell panic recovery) — DECIDED: `KernelDied` + explicit
  logging.** Same path existing kernel-exit failures take, so existing
  recovery UI and client messaging apply. `on_panic` for `iopub_task` and
  `shell_reader_task` sends `QueueCommand::KernelDied` via the held
  `cmd_tx` clone and calls `error!(task, panic=?, "iopub/shell task
  panicked")`.

- **Q2 (warming-loop panic) — DECIDED: log, mark in-flight as failed,
  single respawn with backoff; escalate to `trigger_shutdown` on second
  failure.** Log at minimum. A pool worker that dies mid-warm leaves a
  stuck counter (see Q3) and no further progress, so respawn is right.
  `on_panic` for sites #12–14: call the existing
  `warming_failed_with_error(Some(panic_marker))`, log, then schedule a
  single respawn via the daemon's existing spawn helper. Track respawn
  count per warming task (simple `AtomicU32`); if it panics twice,
  `trigger_shutdown()`. Don't introduce open-ended restart loops.

- **Q3 (pool-accounting rollback) — DECIDED: RAII `WarmingGuard`.**
  `register_warming_path` (daemon.rs:4069) pre-increments; every exit
  path in `create_uv_env` and siblings calls `warming_failed_*` or the
  success equivalent. A `WarmingGuard { path, pool }` with `impl Drop`
  that calls `warming_failed_for_path(panic_placeholder)` on drop, plus
  `fn commit(self)` that `mem::forget`s the guard on success, removes
  the class of bug. Works for panics, `?` returns, and future early
  returns we haven't written yet. `on_panic` for these sites becomes a
  no-op — the guard already ran during unwind. Separate commit/PR from
  the `spawn_supervised` migration since it's orthogonal; do it first so
  the migration doesn't have to work around half-landed state.

- **Q4 (autosave panic) — RESEARCH COMPLETE, DECISION PENDING.**
  Kyle's recollection of automerge-repo using a "refresh" pattern was
  off. Actual behavior: `Repo.ts:192-197` fires save through a throttled
  `void this.storageSubsystem.saveDoc(...)`; `StorageSubsystem.ts:327-333`
  wraps `loadSyncState` in try/catch but `saveDoc` / `#saveIncremental` /
  `#saveTotal` (lines 209-320) have **no error handling** — rejected
  promises are discarded, the doc stays in memory diverged from storage,
  no retry, no event. The Rust `automerge` core returns `Result` from
  save/load and has no panic-safety helpers; panic handling is a caller
  concern.

  That pattern is **unacceptable for our autosave task**. automerge-repo
  runs in a browser with sync peers that retransmit CRDT state; our
  daemon is often the only persistence path. Silent save failure means
  user edits vanish.

  Options for site #28 (`spawn_autosave_debouncer`):
  - **A. `trigger_shutdown()` on panic.** Daemon exits cleanly, frontend
    reconnects, user sees the failure. Matches how other load-bearing
    worker failures already propagate.
  - **B. Broadcast to frontend on panic.** Daemon stays up, surfaces an
    "autosave broken, please save-as / restart" banner. Keeps the
    session usable for read-only inspection. Needs a new broadcast kind.
  - **C. Single respawn with backoff.** Symmetric with Q2. Adds
    complexity around debouncer internal state (in-flight save, queued
    changes); the debouncer state is lost on respawn and we don't know
    whether the save-triggering change has been flushed.

  **Recommendation: A.** Cleanest; matches existing kernel-died and
  daemon-shutdown semantics the frontend already handles. B is a
  follow-up if we want finer-grained recovery; C is the wrong trade
  for a data-path worker.

  Kyle to confirm A or override to B. Tracked: task #70.

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

- 29 production spawn sites in `crates/runtimed/src/`. All 29 currently lose
  panic information.
- Propose one primitive: `spawn_supervised(label, fut, on_panic)`.
  `spawn_best_effort(label, fut)` is a thin wrapper that passes a no-op
  `on_panic`. Keeps the surface minimal per codex review.
- Migrate all 29 in Phase 2 after Kyle resolves Q1–Q4.
- No new dependencies; `futures::FutureExt::catch_unwind` is already in the
  tree via existing usage.
