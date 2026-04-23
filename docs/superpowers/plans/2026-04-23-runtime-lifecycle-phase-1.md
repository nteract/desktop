# RuntimeLifecycle Phase 1 — Add Enums

**Goal:** Add `RuntimeLifecycle` and `KernelActivity` enums to `runtime-doc`. Read `lifecycle` into `KernelState` snapshots as a derived field. Do NOT touch any writer or caller.

**Why only this much:** the full refactor is a coordinated change across Rust, TS, and Python. Before we commit to that scope, we want to shake out the type shape (serde format, `Running(KernelActivity)` ergonomics) and see it compile end-to-end. Phase 2 adds the writers; Phase 3 migrates callers; Phase 4 retires the string fields.

**Spec:** `docs/superpowers/specs/2026-04-23-runtime-lifecycle-enum-design.md`

## Scope

- `crates/runtime-doc/src/types.rs`: define `KernelActivity` + `RuntimeLifecycle` with serde tag+content format.
- `crates/runtime-doc/src/doc.rs`: add a `lifecycle: RuntimeLifecycle` field to `KernelState` (new field, `#[serde(default)]`, derived from `status` + `starting_phase` during `read_state`).
- Unit tests in `types.rs` for serde round-trip.

## Out of scope

- `set_lifecycle` / `set_activity` writers. Phase 2.
- Daemon / TS / Python caller migration. Phase 3.
- Removing `KernelState.status` / `KernelState.starting_phase`. Phase 4.
- New CRDT keys (`kernel/lifecycle`, `kernel/activity`). Phase 2.

## Acceptance

- `cargo test -p runtime-doc` passes, including new serde round-trip tests.
- `cargo check --workspace` succeeds (no downstream breakage — we're only adding fields and types).
- Existing `KernelState.status` + `starting_phase` still populate as before; new `lifecycle` field shows the equivalent typed view derived from them.

## Sketch (not a script — just the target API)

```rust
// types.rs
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelActivity { #[default] Unknown, Idle, Busy }

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "lifecycle", content = "activity")]
pub enum RuntimeLifecycle {
    #[default] NotStarted, AwaitingTrust,
    Resolving, PreparingEnv, Launching, Connecting,
    Running(KernelActivity),
    Error, Shutdown,
}
```

`KernelState` gains:
```rust
#[serde(default)]
pub lifecycle: RuntimeLifecycle,
```

`read_state` fills `lifecycle` by mapping `(status, starting_phase)` — see spec. No CRDT schema change.

## Checkpoint for reviewer

Look at the type shape, the serde JSON (`{"lifecycle": "Running", "activity": "Idle"}` vs `{"lifecycle": "NotStarted"}`), and whether `Running(KernelActivity)` feels right. If those are good, Phase 2 writes into CRDT and adds setters.
