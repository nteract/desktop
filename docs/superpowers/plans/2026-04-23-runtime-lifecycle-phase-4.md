# RuntimeLifecycle Phase 4 — Migrate Rust Callers

**Goal:** Replace every `set_kernel_status` / `set_starting_phase` call site in `runtimed` and `notebook-sync` with the typed writers from Phase 2/3. Replace every `kernel.status` / `kernel.starting_phase` snapshot read in those crates with pattern-matches on `kernel.lifecycle`. No wire-protocol or binding changes.

**Why only this much:** one PR per conceptual boundary. Phase 4 touches only the code that lives inside the same crates as the typed writers. Phase 5 takes the TS frontend; Phase 6 takes Python bindings. Retiring the string setters waits until all consumers move.

**Spec:** `docs/superpowers/specs/2026-04-23-runtime-lifecycle-enum-design.md`
**Prior phases:** phase 1 (enum), phase 2 (CRDT keys + writers), phase 3 (`KernelErrorReason` + `set_activity` fix)

## What lands

### Writers migrated (every `set_kernel_status` / `set_starting_phase` in runtimed/notebook-sync)

- `crates/runtimed/src/jupyter_kernel.rs` — IOPub hot path. Introduces a local `IoPubStateUpdate` enum (`Activity(KernelActivity)` vs `Lifecycle(RuntimeLifecycle)`) that makes the dispatch branching type-safe: Jupyter `ExecutionState::Busy/Idle` becomes an activity flip; `Starting/Restarting/Terminating/Dead` becomes a lifecycle transition. Retains the "transient busy/idle from non-execute messages" suppression; now it only applies to `IoPubStateUpdate::Activity`.
- `crates/runtimed/src/runtime_agent.rs` — kernel-died → `set_lifecycle(Error)`.
- `crates/runtimed/src/requests/launch_kernel.rs` — atomic claim, phase transitions (`Resolving → PreparingEnv → Launching → Connecting → Running(Idle)`), both the restart path and the fresh-spawn path. The wait-loop that previously spun on string status now matches on `RuntimeLifecycle`.
- `crates/runtimed/src/requests/shutdown_kernel.rs` — `set_lifecycle(Shutdown)`.
- `crates/runtimed/src/notebook_sync_server/peer.rs` — auto-launch claim, panic handler, trust-blocked path.
- `crates/runtimed/src/notebook_sync_server/metadata.rs` — `not_started` reset, `missing_ipykernel` via `set_lifecycle_with_error(Error, Some(MissingIpykernel))`, `PreparingEnv → Launching → Connecting → Running(Idle)` transitions.
- `crates/runtimed/src/notebook_sync_server/tests.rs` — 3 call sites migrated; `reset_starting_state` tests now pin `RuntimeLifecycle::Resolving` / `::NotStarted` directly.
- `crates/notebook-sync/src/tests.rs` — 2 call sites, `runtime_doc::RuntimeLifecycle::Error`.

### Readers migrated (every `kernel.status` snapshot read)

- `crates/runtimed/src/requests/execute_cell.rs`, `run_all_cells.rs` — kernel-dead gates (`Shutdown | Error`).
- `crates/runtimed/src/requests/get_kernel_info.rs` — still returns a legacy status string on the wire (NotebookResponse::KernelInfo.status); derives it from `RuntimeLifecycle::to_legacy()`.
- `crates/runtimed/src/notebook_sync_server/room.rs::kernel_info` — same wire contract, same `to_legacy` projection.
- `crates/runtimed/src/notebook_sync_server/metadata.rs:673` — "kernel is running" check now uses `matches!(lifecycle, Running(_))`.
- `crates/runtimed/src/runtime_agent.rs` — `kernel_died_*` test asserts migrated to `kernel.lifecycle == Error`.
- `crates/runtimed/src/notebook_sync_server/tests.rs` — 3 read sites in `reset_starting_state` tests.
- `crates/notebook-sync/src/execution_wait.rs` — kernel-fault fallback (`Error | Shutdown`).

### Stale comment cleanup

- `crates/runtimed/src/kernel_state.rs:268` — historical note updated to reference `set_lifecycle(Error)`.

## Out of scope

- `runt-mcp` (wire status field) — Phase 5 or later.
- `runtimed-py`, `runtimed-node` (language bindings) — Phase 5 or later.
- `runt/src/main.rs` (CLI consumer of wire KernelInfo) — stays on the wire field.
- TS frontend / `NotebookToolbar` — Phase 5.
- Retiring `set_kernel_status` / `set_starting_phase` — waits for bindings + frontend to migrate.
- Moving `read_str_if_present` to `automunge` — standing follow-up from Phase 2.

## Invariants preserved

- `with_doc` discipline: every mutation still routes through `RuntimeStateHandle::with_doc(|sd| ...)` — no direct `doc.set_*` calls on the handle surface.
- Fork+merge for async CRDT mutations: untouched. The migration only swaps method names inside the same closures.
- Dual-shape correctness: the typed setters continue to mirror into the string CRDT keys (Phase 2 behavior). Phase 4 removes Rust-side string consumers but leaves the mirror writes in place for the frontend and language bindings still on the string shape.
- Error reason → legacy `starting_phase` mirror: Phase 3 added this; Phase 4 exercises it via `metadata.rs`'s `missing_ipykernel` path.

## Acceptance

- `cargo test -p runtime-doc -p notebook-sync -p runtimed --lib` passes.
  - `runtime-doc`: 145 tests (+0 new; Phase 3 covered the writer changes).
  - `notebook-sync`: 41 tests.
  - `runtimed`: 389 tests.
- `cargo check --workspace` clean.
- `cargo xtask lint` clean.
- `rg -n 'set_kernel_status|set_starting_phase' --glob '*.rs' --glob '!crates/runtime-doc/**'` returns empty.

## Smoke scenarios the tests pin

- IOPub `Idle → Busy → Idle` flip during execution — activity-throttled writes don't advance heads on redundant calls.
- Launch path: `Resolving → PreparingEnv → Launching → Connecting → Running(Idle)`. The full sequence is exercised by `test_launch_kernel_auto_python` and friends.
- Restart path via `RestartKernel` RPC goes to `Running(Idle)` without re-entering `Resolving` (the atomic claim sees `Running(_)` as "already progressing" and bypasses the reset).
- `missing_ipykernel` error: `set_lifecycle_with_error(Error, Some(MissingIpykernel))` writes `error_reason = "missing_ipykernel"` AND mirrors into `starting_phase` for the frontend's pixi-install prompt gate.
- Kernel died: `set_lifecycle(Error)` clears the queue, leaves `error_reason` untouched (retry paths preserve any prior reason).
- Shutdown flow: `set_lifecycle(Shutdown)` clears the queue, kernel stays connected for a subsequent RestartKernel.

## Review checkpoints

- `jupyter_kernel.rs` IOPub handler — the `IoPubStateUpdate` local enum is the cleanest expression of "which CRDT channel does this status go to?" I could find. Alternative: inline the match. I prefer the named enum because it makes the transient-suppression logic trivially type-correct (`matches!(update, IoPubStateUpdate::Activity(_))`) and documents that only activity writes are transient.
- `launch_kernel.rs` wait-loop — the terminal-state guard is `matches!(lc, Running(_) | Error | Shutdown | NotStarted)`. Pre-migration this was five string comparisons. Worth checking that `AwaitingTrust` isn't a terminal state here (it isn't — a doc in AwaitingTrust is waiting for the user, not the launch).
- `room.rs::kernel_info` and `get_kernel_info.rs` — both still emit a legacy status string because the wire contract hasn't changed. The `to_legacy()` projection lives on `RuntimeLifecycle` (added in Phase 2) so neither file repeats the string table.
- `set_lifecycle_with_error(Error, Some(MissingIpykernel))` in `metadata.rs` — replaces the Phase 1–3 pattern of `set_kernel_status("error") + set_kernel_info(...) + set_starting_phase("missing_ipykernel")` with a single typed call. The Phase 3 mirror handles the legacy `starting_phase` side.
