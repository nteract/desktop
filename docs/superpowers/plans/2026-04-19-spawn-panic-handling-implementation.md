# Implementation Plan: Spawn Panic Handling for `crates/runtimed`

Date: 2026-04-19
Design doc: PR #1926 (`docs/superpowers/plans/2026-04-19-spawn-panic-handling.md`)
Status: Implementation plan — approved with revisions (see § Review Feedback).

## Context

PR #1926 audits all 30 `tokio::spawn` sites in `crates/runtimed/src/` and finds
that **none** catch panics at the task boundary. When a spawned task panics,
tokio logs via `tracing` and silently drops the task. The daemon stays up in a
degraded state: blob server gone, autosave stopped, warming loops dead, pool
accounting drifted. All 4 design questions (Q1-Q4) are answered in the design
doc PR.

This plan implements Phase 2: the `WarmingGuard` RAII type, two spawn helpers
(`spawn_supervised` / `spawn_best_effort`), and migration of all 29 production
spawn sites.

## PR Sequence (6 PRs)

### PR 1: `WarmingGuard` RAII (Q3)

**Why first:** Orthogonal to spawn wiring. Fixes the pool-accounting-on-panic
bug independently. Unblocks cleaner spawn migration later since the warming
counter is already protected.

**New code:**

Add a `WarmingGuard` struct in `crates/runtimed/src/daemon.rs` (private, near
the `EnvPool` impl). The guard holds an `Option<WarmingGuardInner>` where inner
has `Arc<tokio::sync::Mutex<EnvPool>>`, `PathBuf`, `Arc<Notify>`, and
`Arc<Daemon>`. On `Drop`, if inner is `Some` (not committed), it spawns a
background task that: locks the pool, calls `warming_failed_for_path(path,
None)`, drops the lock, then calls `daemon.update_pool_doc().await`.

`fn commit(mut self)` takes the inner via `self.inner.take()`, preventing Drop
from firing the rollback. No `mem::forget` needed — the struct drops cleanly
with `inner = None`.

```rust
struct WarmingGuardInner {
    pool: Arc<Mutex<EnvPool>>,
    path: PathBuf,
    pool_ready: Arc<Notify>,
    daemon: Arc<Daemon>,
}

struct WarmingGuard {
    inner: Option<WarmingGuardInner>,
}

impl WarmingGuard {
    fn commit(&mut self) {
        self.inner.take();
    }
}

impl Drop for WarmingGuard {
    fn drop(&mut self) {
        if let Some(inner) = self.inner.take() {
            tokio::spawn(async move {
                inner.pool.lock().await.warming_failed_for_path(&inner.path, None);
                inner.pool_ready.notify_waiters();
                inner.daemon.update_pool_doc().await;
            });
        }
    }
}
```

Using `tokio::spawn` in Drop is safe on the multi-threaded runtime. During
shutdown, pool accounting is irrelevant — the guard checks
`daemon.is_shutting_down()` and skips the spawn if the runtime is winding
down (review feedback item #1).

**Files modified:**
- `crates/runtimed/src/daemon.rs`
  - Add `WarmingGuard` struct + `impl Drop` (~30 lines)
  - `create_uv_env`: Replace `register_warming_path` + all
    `warming_failed_for_path` exit paths with guard. ~6 early-return cleanup
    blocks become guard Drop. Call `guard.commit()` before `pool.add()` on
    success.
  - `create_conda_env`: Same pattern. ~8 early-return cleanup blocks.
  - `create_pixi_env`: Same pattern. ~2 early-return cleanup blocks.

**What not to change:** `replenish_conda_env` and `replenish_pixi_env` call
`mark_warming(1)` then delegate to `create_*`. The guard lives inside
`create_*`, so the `mark_warming` accounting is already covered —
`create_*` calls `register_warming_path` which the guard protects.

**Verification:**
- `cargo xtask lint --fix`
- `cargo test -p runtimed` (existing pool tests cover success + error paths)

---

### PR 2: `task_supervisor` module with helpers

**New file:** `crates/runtimed/src/task_supervisor.rs` (~80 lines)

**Contents:**
1. `PanicInfo { label: &'static str, message: String }`
2. `panic_payload_to_string(Box<dyn Any + Send>) -> String` — extracted from
   the identical downcast chain in `catch_automerge_panic`
   (notebook_sync_server.rs:74-80). After extraction, update
   `catch_automerge_panic` to call this shared helper.
3. `spawn_supervised<F, P>(label, fut, on_panic) -> JoinHandle<()>` — wraps
   `fut` in `AssertUnwindSafe(fut).catch_unwind()`, logs at `error!`, calls
   `on_panic(&PanicInfo)`.
4. `spawn_best_effort<F>(label, fut) -> JoinHandle<()>` — same but `on_panic`
   is a no-op (log only).

**Dependencies:** `futures::FutureExt` (already in `Cargo.toml`),
`std::panic::AssertUnwindSafe`.

**Unit tests** (in `#[cfg(test)] mod tests` at bottom of `task_supervisor.rs`):
- `test_spawn_best_effort_normal` — future completes normally.
- `test_spawn_best_effort_panic` — future panics, verify JoinHandle resolves
  to `Ok(())` (panic caught internally).
- `test_spawn_supervised_calls_on_panic` — future panics, verify callback
  fires with correct label and message.
- `test_spawn_supervised_abort` — store JoinHandle, call `.abort()`, verify
  task cancelled cleanly.

**Files modified:**
- `crates/runtimed/src/task_supervisor.rs` (new)
- `crates/runtimed/src/lib.rs` — add `pub(crate) mod task_supervisor;`
- `crates/runtimed/src/notebook_sync_server.rs` — update
  `catch_automerge_panic` to use shared `panic_payload_to_string`.

**Verification:**
- `cargo test -p runtimed --lib task_supervisor`
- `cargo xtask lint --fix`

---

### PR 3: Migrate kernel tasks — `jupyter_kernel.rs`

Sites #5-10. The abort-only tasks (#6-10) get `spawn_supervised` with
`on_panic` sending `QueueCommand::KernelDied`. Site #5 (stderr pump) gets
`spawn_best_effort`.

| Site | Line | Task | `on_panic` action |
|------|------|------|-------------------|
| #5 | 476 | Kernel stderr pump | Best-effort (log only) |
| #6 | 550 | `process_watcher_task` | `cmd_tx.try_send(KernelDied)` |
| #7 | 588 | `iopub_task` | `cmd_tx.try_send(KernelDied)` |
| #8 | 1631 | `shell_reader_task` | `cmd_tx.try_send(KernelDied)` |
| #9 | 1819 | `heartbeat_task` | `cmd_tx.try_send(KernelDied)` |
| #10 | 1862 | `comm_coalesce_task` | `cmd_tx.try_send(KernelDied)` |

JoinHandle usage for `.abort()` is preserved since `spawn_supervised` returns
`JoinHandle<()>`.

**Files modified:**
- `crates/runtimed/src/jupyter_kernel.rs` — 6 spawn sites

**Verification:**
- `cargo test -p runtimed`
- `cargo xtask lint --fix`

---

### PR 4: Migrate daemon long-lived workers — `daemon.rs`, `blob_server.rs`, `main.rs`, `runtime_agent_handle.rs`

| Site | File:Line | Task | `on_panic` action |
|------|-----------|------|-------------------|
| #1 | `runtime_agent_handle.rs:88` | Child process watcher | Log + set `alive = false` |
| #2 | `blob_server.rs:42` | Blob accept loop | Respawn with counter (see below) |
| #4 | `main.rs:439` | Signal handler | Log only |
| #12 | `daemon.rs:783` | `uv_warming_loop` | Respawn (see below) |
| #13 | `daemon.rs:788` | `conda_warming_loop` | Respawn (see below) |
| #14 | `daemon.rs:793` | `pixi_warming_loop` | Respawn (see below) |
| #15 | `daemon.rs:799` | `env_gc_loop` | Log only |
| #16 | `daemon.rs:805` | `watch_settings_json` | Log only |

**Blob server respawn:** Site #2 uses respawn-with-counter (same pattern as
warming loops). The blob server is stateless and content-addressed, so
restarting it is safe. Second panic escalates to `trigger_shutdown()`.
Add `blob_server_respawns: AtomicU32` to `Daemon`. The `on_panic` handler
uses `compare_exchange` to atomically check-and-increment the counter
(review feedback item #2 — avoids race with concurrent panics).

**Warming loop respawn (Q2):** For sites #12-14, the `on_panic` handler:
1. Calls `warming_failed_with_error(Some(panic_marker))` on the pool
2. Logs `error!`
3. Uses `compare_exchange(0, 1, SeqCst, Relaxed)` on a per-task `AtomicU32`
   respawn counter. If CAS succeeds (was 0), spawns a single replacement.
   If CAS fails (already >= 1), calls `daemon.trigger_shutdown()`.
   (Review feedback item #2 — CAS prevents double-respawn race.)

Add four `AtomicU32` fields to `Daemon`: `blob_server_respawns`,
`uv_warming_respawns`, `conda_warming_respawns`, `pixi_warming_respawns`.

**Files modified:**
- `crates/runtimed/src/daemon.rs` — 10 spawn sites + 3 new `AtomicU32` fields
- `crates/runtimed/src/blob_server.rs` — 1 spawn site
- `crates/runtimed/src/main.rs` — 1 spawn site
- `crates/runtimed/src/runtime_agent_handle.rs` — 1 spawn site

**Verification:**
- `cargo test -p runtimed`
- `cargo xtask lint --fix`
- `cargo xtask build --release` + `cargo xtask install-daemon --channel nightly`

---

### PR 5: Migrate notebook-sync supervised sites + best-effort batch

**Supervised sites:**

| Site | Line | Task | `on_panic` action |
|------|------|------|-------------------|
| #24 | 2034 | `auto_launch_kernel` | Set `kernel_status` to `"error"` |
| #25 | 2130 | Room eviction delay | Log only |
| #27 | 7906 | `spawn_persist_debouncer` | `trigger_shutdown()` |
| #29 | 9450 | `spawn_notebook_file_watcher` | Log only |

**Best-effort sites (all files):**

| Site | File:Line | Task |
|------|-----------|------|
| #3 | `blob_server.rs:48` | Per-connection HTTP handler |
| #11 | `daemon.rs:215` | `spawn_env_deletions` |
| #17 | `daemon.rs:893` | Per-connection unix socket |
| #18 | `daemon.rs:958` | Per-connection windows pipe |
| #19 | `daemon.rs:2083` | UV post-take replenish |
| #20 | `daemon.rs:2108` | UV retry after `mark_warming` |
| #21 | `daemon.rs:2158` | `replenish_conda_env` |
| #22 | `daemon.rs:2183` | Conda retry after `mark_warming` |
| #23 | `daemon.rs:2228` | `replenish_pixi_env` |
| #26 | `notebook_sync_server.rs:5934` | Background cell formatter |

**Files modified:**
- `crates/runtimed/src/notebook_sync_server.rs` — 4 supervised + 1 best-effort
- `crates/runtimed/src/blob_server.rs` — 1 best-effort
- `crates/runtimed/src/daemon.rs` — 7 best-effort

**Verification:**
- `cargo test -p runtimed`
- `cargo xtask lint --fix`

---

### PR 6: Autosave (site #28) + final gate

**Site #28** (`notebook_sync_server.rs:7996`): `spawn_autosave_debouncer`. Per
Q4 decision: `on_panic` calls `trigger_shutdown()`. The daemon exits cleanly,
frontend reconnects, user sees the failure.

**Refactor:** `spawn_autosave_debouncer` needs an `Arc<Daemon>` parameter to
call `trigger_shutdown()` in the `on_panic` handler.

**Final gate:** `grep -n "tokio::spawn" crates/runtimed/src/` — only test site
#30 should remain.

**Files modified:**
- `crates/runtimed/src/notebook_sync_server.rs` — 1 spawn site + signature change

**Verification:**
- `cargo test -p runtimed`
- `cargo xtask lint --fix`
- `cargo xtask build --release`
- `cargo xtask install-daemon --channel nightly`
- `runt-nightly daemon status`
- Final grep confirms only test site #30 remains

## Key Design Decisions

1. **`catch_unwind` is sound:** Tasks communicate via channels (owned state),
   no `tokio::sync::Mutex` guards held across `.await`. `AssertUnwindSafe` is
   correct.
2. **`on_panic` must not panic:** Callbacks are trivial: `tx.try_send()`,
   `store()`, `trigger_shutdown()`. Wrap each `on_panic` body in a secondary
   `catch_unwind` for defense-in-depth.
3. **`spawn_best_effort` is `spawn_supervised` with no-op `on_panic`.**
4. **WarmingGuard checks shutdown before spawning in Drop:** If the daemon is
   shutting down, the guard skips the async cleanup spawn (pool accounting is
   irrelevant during shutdown and the runtime may be winding down).
5. **Respawn counters use `compare_exchange`:** Atomic CAS prevents
   double-respawn races when two panics occur concurrently.
6. **Blob server respawns instead of shutting down:** The blob server is
   stateless and content-addressed — safe to restart. Second panic escalates.

## Review Feedback (incorporated)

Independent review by DeepSeek V3.2, 2026-04-19. Three items accepted:

1. **WarmingGuard::drop shutdown race** — guard checks `daemon.is_shutting_down()`
   before `tokio::spawn`. Prevents spawning into a winding-down runtime.
2. **Warming respawn counter race** — use `compare_exchange` (CAS) instead of
   relaxed load-then-increment. Prevents double-respawn if two panics race.
3. **Blob server (site #2)** — changed from `trigger_shutdown()` to
   respawn-with-counter. Blob server is stateless, safe to restart.

## Risks

- **`AssertUnwindSafe` on futures with interior mutability:** All tasks use
  channels or `Arc<AtomicX>`. No `RefCell` or non-`UnwindSafe` types cross the
  catch boundary.
- **Automerge panics in autosave:** Triggers `trigger_shutdown()`.
  Intentionally aggressive — better to restart cleanly than silently lose data.
- **Performance:** `catch_unwind` adds negligible overhead on the non-panic
  path (just a landing pad).
