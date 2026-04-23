# RuntimeLifecycle Enum Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace string-based `kernel.status` + `kernel.starting_phase` in `RuntimeStateDoc` with a single typed `RuntimeLifecycle` enum whose `Running(KernelActivity)` variant makes it impossible to represent a busy kernel when the runtime hasn't launched yet. Deliver a coordinated Rust + TypeScript + Python schema change in a single release.

**Architecture:** Introduce `RuntimeLifecycle` and `KernelActivity` enums in `crates/runtime-doc`, with `set_lifecycle`/`set_activity` writers on `RuntimeStateDoc` and CRDT storage using separate `kernel/lifecycle` + `kernel/activity` string keys. The migration runs **dual-shape**: both the old (`status` + `starting_phase`) and the new (`lifecycle` + `activity` + `error_reason`) keys + struct fields coexist for the duration of the migration, so every task-boundary commit compiles and passes tests. The final task removes the old shape in one atomic deletion. The schema change ships as one PR because the app bundles daemon + frontend + WASM; there is no on-disk migration because RuntimeStateDoc is ephemeral.

**Tech Stack:** Rust (serde, Automerge via `runtime-doc`), TypeScript (RxJS, React), Python (PyO3). No wire or Automerge schema version bump required — `RuntimeStateDoc` is ephemeral and recreated per room on daemon restart.

**Spec:** `docs/superpowers/specs/2026-04-23-runtime-lifecycle-enum-design.md`

---

## File Structure

| File | Role in this refactor |
|------|-----------------------|
| `crates/runtime-doc/src/types.rs` | New: `RuntimeLifecycle` and `KernelActivity` enums, `variant_str`, `as_str`, `parse` helpers, serde round-trip tests |
| `crates/runtime-doc/src/lib.rs` | Re-export the new enums (already wildcard) |
| `crates/runtime-doc/src/doc.rs` | Schema doc-comment, scaffold both old + new `kernel/*` keys, new `set_lifecycle` + `set_activity` writers (dual-shape — also maintain legacy `status`/`starting_phase` keys during the migration window), add `lifecycle` + `error_reason` fields on `KernelState`, update `read_state` to populate both, **Task 15** retires old keys + old setters + old struct fields + dual-shape legacy mirror writes in one atomic commit |
| `crates/runtime-doc/src/handle.rs` | Update handle unit tests to call the new writers |
| `crates/notebook-sync/src/tests.rs` | Replace `set_kernel_status("error")` in sync tests |
| `crates/notebook-sync/src/execution_wait.rs` | Replace `state.kernel.status == "error"/"shutdown"` reads with pattern matches on `state.kernel.lifecycle` |
| `crates/runtimed/src/jupyter_kernel.rs` | IOPub status handler: map `ExecutionState::Busy/Idle` to `set_activity`, `Starting/Restarting/Dead/Terminating` to `set_lifecycle` |
| `crates/runtimed/src/runtime_agent.rs` | `set_kernel_status("error")` → `set_lifecycle(RuntimeLifecycle::Error)` on kernel death; migrate the two `kernel.status == "error"` test asserts |
| `crates/runtimed/src/kernel_state.rs` | Stale doc comment referring to `set_kernel_status("error")` |
| `crates/runtimed/src/notebook_sync_server/peer.rs` | Auto-launch + trust-blocked + auto-launch-panic paths switch to `set_lifecycle` |
| `crates/runtimed/src/notebook_sync_server/metadata.rs` | `set_kernel_status("not_started")`, missing-ipykernel error, `preparing_env`/`launching`/`connecting` phases, post-launch `Running(Idle)`; **also** the `kernel.status != "idle"/"busy"` check-before-running read at line 673 |
| `crates/runtimed/src/notebook_sync_server/tests.rs` | Daemon tests calling `set_kernel_status("idle"/"starting")` |
| `crates/runtimed/src/notebook_sync_server/room.rs` | `state.kernel.status != "not_started"` read, add `lifecycle_to_status_string` helper |
| `crates/runtimed/src/requests/launch_kernel.rs` | Atomic claim, phase transitions, post-launch Running(Idle) writes |
| `crates/runtimed/src/requests/shutdown_kernel.rs` | `set_kernel_status("shutdown")` → `set_lifecycle(Shutdown)` |
| `crates/runtimed/src/requests/get_kernel_info.rs` | Map `lifecycle` back to a status string for the wire response |
| `crates/runtimed/src/requests/execute_cell.rs` | Rewrite `status == "shutdown"/"error"` precondition |
| `crates/runtimed/src/requests/run_all_cells.rs` | Same precondition rewrite |
| `crates/runt-mcp/src/tools/kernel.rs` | Rewrite the kernel-ready wait loop to inspect `lifecycle` + `activity` |
| `crates/runt-mcp/src/tools/session.rs` | `serde_json::json!(state.kernel.status)` → render `lifecycle`/`activity` via helper |
| `crates/runt-mcp/src/kernel_status.rs` | New: `lifecycle_to_status_string` helper module |
| `crates/runtimed-py/src/output.rs` | `PyKernelState` grows `lifecycle` + `activity` + `error_reason`, drops `status` |
| `crates/runtimed-py/src/session_core.rs` | Rewrite the 5 `rs.kernel.status` reads + the `hydrate_kernel_state` running check |
| `crates/runtimed-node/src/session.rs` | `r.kernel.status == "ready"/"busy"/"idle"` check switches to `lifecycle`-based |
| `packages/runtimed/src/runtime-state.ts` | New TS types mirroring the Rust enum, dual-shape `KernelState`, update `DEFAULT_RUNTIME_STATE`, expose a `lifecycleStatusString()` helper |
| `packages/runtimed/src/derived-state.ts` | `deriveEnvSyncState` + `kernelStatus$` rewritten in terms of `lifecycle` |
| `packages/runtimed/tests/sync-engine.test.ts` | Test fixtures updated to the new shape |
| `apps/notebook/src/lib/kernel-status.ts` | `getLifecycleLabel(lc)` added; `getKernelStatusLabel` + `KERNEL_STATUS_LABELS` deleted when last caller migrates |
| `apps/notebook/src/lib/__tests__/kernel-status.test.ts` | Rewritten around `getLifecycleLabel` |
| `apps/notebook/src/hooks/useDaemonKernel.ts` | Drive the busy-throttle off `lifecycle`; stop threading `starting_phase` |
| `apps/notebook/src/components/NotebookToolbar.tsx` | Replace `startingPhase` prop with `lifecycle` + `errorReason`, rewrite `missing_ipykernel` banner check |
| `apps/notebook/src/components/__tests__/notebook-toolbar.test.tsx` | Toolbar test fixtures follow the new prop shape |
| `apps/notebook/src/App.tsx` | Thread `lifecycle` + `errorReason` to the toolbar |
| `scripts/metrics/{kernel-reliability,execution-latency,sync-correctness}.py` | Python metrics scripts’ `kernel.status` reads |

---

## Migration order

> **On line numbers:** the plan cites line numbers from the snapshot of `main` at the time of writing (2026-04-23). Small drift from subsequent unrelated commits is expected. When in doubt, grep for the string or method name — the surrounding context in every step makes the target unambiguous.

The migration is **dual-shape**: both the old (`status` + `starting_phase`) and the new (`lifecycle` + `activity` + `error_reason`) CRDT keys and struct fields coexist from Task 2 through Task 14. The new writers (`set_lifecycle`, `set_activity`, `set_lifecycle_with_error`) **maintain both shapes** — they write the new keys *and* mirror the legacy `status` + `starting_phase` — so readers that haven't migrated yet still see consistent state. Each task ends with a green commit (`cargo check --workspace`, `cargo test -p <touched>`, and the relevant TS / Python test command pass). Task 15 removes the old shape atomically after a repo-wide grep confirms zero callers remain. The design intent:

1. **Task 1:** Add the enums. No behavior change. Green.
2. **Task 2:** Scaffold new CRDT keys **alongside** old ones in `new()` / `new_with_actor()`. Readers of either shape still work. Green.
3. **Task 3:** Add `lifecycle` + `error_reason` fields to `KernelState` alongside `status` + `starting_phase`. `read_state` populates all of them. Add `set_lifecycle` / `set_activity` / `set_lifecycle_with_error` — these write both the new AND the legacy keys. Keep `set_kernel_status` / `set_starting_phase` functional (they'll be removed in Task 15). Green.
4. **Tasks 4–7:** Migrate Rust callers in `runtimed` (IOPub, notebook_sync_server, request handlers, runtime_agent tests). Each task is green because both shapes are still written.
5. **Task 8:** Migrate `notebook-sync` consumers.
6. **Task 9:** Migrate `runt-mcp`.
7. **Task 10:** Migrate `runtimed-node`.
8. **Task 11:** Migrate Python bindings (`runtimed-py`) — add `lifecycle` / `activity` / `error_reason` attributes while keeping `status` for the metrics scripts and repo examples that still read it. Task 14 migrates those consumers; Task 15 drops the legacy attribute.
9. **Task 12:** Introduce the TS `RuntimeLifecycle` type + dual-shape `KernelState` in `packages/runtimed` (and export the new types from the package root). Green — legacy Rust `status` still flows through, TS reads whichever field it prefers.
10. **Task 13 (consolidated TS migration):** Move every TS caller (`derived-state`, `kernel-status`, `useDaemonKernel`, `NotebookToolbar`, `App.tsx`, toolbar test, `kernel-status.test.ts`, sync-engine test fixtures) in one green commit. Ends with deletion of `getKernelStatusLabel` / `KERNEL_STATUS_LABELS`.
11. **Task 14:** Migrate Python metrics scripts + examples in `python/runtimed/README.md`. Green — still reads `kernel.status` because Task 11 left it populated on `PyKernelState`; this task switches them to `kernel.lifecycle` so Task 15 can drop the attribute.
12. **Task 15 (atomic retire):** Migrate the `handle.rs` tests (the final in-crate callers of the legacy setters), delete `set_kernel_status` + `set_starting_phase`, drop `status` + `starting_phase` fields from `KernelState`, drop the dual-shape legacy-mirror writes from `set_lifecycle` / `set_activity`, drop `PyKernelState.status`, remove the old scaffold keys from both constructors, and simplify `read_state` + delete `legacy_status_to_lifecycle`. Verified green by a repo-wide grep that does NOT exclude `runtime-doc/**` before commit.
13. **Task 16:** Verification sweep + cold-launch smoke + **explicit restart-path smoke** (the "stuck on Shutdown" regression that motivated the refactor). Open the PR.

---

## Task 1: Add `KernelActivity` and `RuntimeLifecycle` enums

**Files:**
- Modify: `crates/runtime-doc/src/types.rs`
- Test: inline `#[cfg(test)] mod tests` in `types.rs`

- [ ] **Step 1: Write the failing tests**

At the bottom of `crates/runtime-doc/src/types.rs`, add:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activity_as_str_round_trips() {
        assert_eq!(KernelActivity::Unknown.as_str(), "Unknown");
        assert_eq!(KernelActivity::Idle.as_str(), "Idle");
        assert_eq!(KernelActivity::Busy.as_str(), "Busy");
    }

    #[test]
    fn activity_parse_valid() {
        assert_eq!(KernelActivity::parse("Unknown"), Some(KernelActivity::Unknown));
        assert_eq!(KernelActivity::parse("Idle"), Some(KernelActivity::Idle));
        assert_eq!(KernelActivity::parse("Busy"), Some(KernelActivity::Busy));
        assert_eq!(KernelActivity::parse("nope"), None);
        assert_eq!(KernelActivity::parse(""), None);
    }

    #[test]
    fn lifecycle_variant_str_round_trips() {
        use RuntimeLifecycle::*;
        assert_eq!(NotStarted.variant_str(), "NotStarted");
        assert_eq!(AwaitingTrust.variant_str(), "AwaitingTrust");
        assert_eq!(Resolving.variant_str(), "Resolving");
        assert_eq!(PreparingEnv.variant_str(), "PreparingEnv");
        assert_eq!(Launching.variant_str(), "Launching");
        assert_eq!(Connecting.variant_str(), "Connecting");
        assert_eq!(Running(KernelActivity::Idle).variant_str(), "Running");
        assert_eq!(Error.variant_str(), "Error");
        assert_eq!(Shutdown.variant_str(), "Shutdown");
    }

    #[test]
    fn lifecycle_parse_non_running_variants() {
        use RuntimeLifecycle::*;
        assert_eq!(RuntimeLifecycle::parse("NotStarted", ""), Some(NotStarted));
        assert_eq!(RuntimeLifecycle::parse("AwaitingTrust", ""), Some(AwaitingTrust));
        assert_eq!(RuntimeLifecycle::parse("Resolving", ""), Some(Resolving));
        assert_eq!(RuntimeLifecycle::parse("PreparingEnv", ""), Some(PreparingEnv));
        assert_eq!(RuntimeLifecycle::parse("Launching", ""), Some(Launching));
        assert_eq!(RuntimeLifecycle::parse("Connecting", ""), Some(Connecting));
        assert_eq!(RuntimeLifecycle::parse("Error", ""), Some(Error));
        assert_eq!(RuntimeLifecycle::parse("Shutdown", ""), Some(Shutdown));
        assert_eq!(RuntimeLifecycle::parse("bogus", ""), None);
    }

    #[test]
    fn lifecycle_parse_running_with_activity() {
        assert_eq!(
            RuntimeLifecycle::parse("Running", "Idle"),
            Some(RuntimeLifecycle::Running(KernelActivity::Idle)),
        );
        assert_eq!(
            RuntimeLifecycle::parse("Running", "Busy"),
            Some(RuntimeLifecycle::Running(KernelActivity::Busy)),
        );
        assert_eq!(
            RuntimeLifecycle::parse("Running", ""),
            Some(RuntimeLifecycle::Running(KernelActivity::Unknown)),
        );
    }

    #[test]
    fn lifecycle_serde_tag_content_round_trip() -> Result<(), serde_json::Error> {
        let running = RuntimeLifecycle::Running(KernelActivity::Busy);
        let json = serde_json::to_string(&running)?;
        assert_eq!(json, r#"{"lifecycle":"Running","activity":"Busy"}"#);
        let back: RuntimeLifecycle = serde_json::from_str(&json)?;
        assert_eq!(back, running);

        let not_started = RuntimeLifecycle::NotStarted;
        let json = serde_json::to_string(&not_started)?;
        assert_eq!(json, r#"{"lifecycle":"NotStarted"}"#);
        let back: RuntimeLifecycle = serde_json::from_str(&json)?;
        assert_eq!(back, not_started);
        Ok(())
    }

    #[test]
    fn lifecycle_default_is_not_started() {
        assert_eq!(RuntimeLifecycle::default(), RuntimeLifecycle::NotStarted);
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

```bash
cargo test -p runtime-doc --lib types::tests 2>&1 | tail -30
```

Expected: compile errors like `cannot find type 'RuntimeLifecycle' in this scope`.

- [ ] **Step 3: Implement the enums**

Replace the contents of `crates/runtime-doc/src/types.rs` with:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct StreamOutputState {
    pub index: usize,
    pub blob_hash: String,
}

/// Observable activity of a running kernel.
///
/// Only meaningful when the runtime lifecycle is `Running`. `Unknown` is the
/// transient state between runtime agent connect and the first IOPub status
/// from the kernel; it also covers non-Jupyter backends that do not report
/// idle/busy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelActivity {
    #[default]
    Unknown,
    Idle,
    Busy,
}

impl KernelActivity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "Unknown",
            Self::Idle => "Idle",
            Self::Busy => "Busy",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "Unknown" => Some(Self::Unknown),
            "Idle" => Some(Self::Idle),
            "Busy" => Some(Self::Busy),
            _ => None,
        }
    }
}

/// Lifecycle of a runtime, from not-started through running to shutdown.
///
/// `Running` is the only variant that carries an activity — it is impossible
/// to represent a "busy kernel that hasn't launched yet" in the type system.
/// Error details are carried out-of-band via `KernelState::error_reason` so
/// this enum stays `Eq`-able.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "lifecycle", content = "activity")]
pub enum RuntimeLifecycle {
    #[default]
    NotStarted,
    AwaitingTrust,
    Resolving,
    PreparingEnv,
    Launching,
    Connecting,
    Running(KernelActivity),
    Error,
    Shutdown,
}

impl RuntimeLifecycle {
    /// Lifecycle variant name used as the CRDT `kernel/lifecycle` string.
    pub fn variant_str(&self) -> &'static str {
        match self {
            Self::NotStarted => "NotStarted",
            Self::AwaitingTrust => "AwaitingTrust",
            Self::Resolving => "Resolving",
            Self::PreparingEnv => "PreparingEnv",
            Self::Launching => "Launching",
            Self::Connecting => "Connecting",
            Self::Running(_) => "Running",
            Self::Error => "Error",
            Self::Shutdown => "Shutdown",
        }
    }

    /// Parse a `(lifecycle, activity)` pair from CRDT strings.
    ///
    /// `activity` is consulted only when `lifecycle == "Running"`.
    /// An empty or unknown activity on a `Running` read is treated as
    /// [`KernelActivity::Unknown`] so consumers never observe a broken doc.
    pub fn parse(lifecycle: &str, activity: &str) -> Option<Self> {
        match lifecycle {
            "NotStarted" => Some(Self::NotStarted),
            "AwaitingTrust" => Some(Self::AwaitingTrust),
            "Resolving" => Some(Self::Resolving),
            "PreparingEnv" => Some(Self::PreparingEnv),
            "Launching" => Some(Self::Launching),
            "Connecting" => Some(Self::Connecting),
            "Running" => {
                let act = if activity.is_empty() {
                    KernelActivity::Unknown
                } else {
                    KernelActivity::parse(activity).unwrap_or(KernelActivity::Unknown)
                };
                Some(Self::Running(act))
            }
            "Error" => Some(Self::Error),
            "Shutdown" => Some(Self::Shutdown),
            _ => None,
        }
    }
}
```

- [ ] **Step 4: Verify tests pass**

`lib.rs` already has `pub use types::*;`. Run:

```bash
cargo test -p runtime-doc --lib types::tests 2>&1 | tail -30
cargo check --workspace 2>&1 | tail -10
```

Expected: all `types::tests` tests pass; workspace still compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/runtime-doc/src/types.rs
git commit -m "feat(runtime-doc): add RuntimeLifecycle and KernelActivity enums"
```

---

## Task 2: Scaffold new CRDT keys alongside legacy ones

**Files:**
- Modify: `crates/runtime-doc/src/doc.rs` (schema comment + `new()` + `new_with_actor()`)

Both constructors currently scaffold `kernel/status` + `kernel/starting_phase`. This task **adds** `kernel/lifecycle` + `kernel/activity` + `kernel/error_reason` alongside them. Nothing is removed.

- [ ] **Step 1: Extend the schema doc comment**

In `crates/runtime-doc/src/doc.rs`, lines 10–16, add the new keys so the comment reads:

```text
//!   kernel/
//!     status: Str          ("idle" | "busy" | "starting" | "error" | "shutdown" | "not_started")
//!                           — DEPRECATED, retired in a follow-up commit (see Task 15)
//!     starting_phase: Str  ("" | "resolving" | "preparing_env" | "launching" | "connecting")
//!                           — DEPRECATED, retired in a follow-up commit (see Task 15)
//!     lifecycle: Str       ("NotStarted" | "AwaitingTrust" | "Resolving" | "PreparingEnv"
//!                           | "Launching" | "Connecting" | "Running" | "Error" | "Shutdown")
//!     activity: Str        ("" | "Unknown" | "Idle" | "Busy") — only meaningful when lifecycle == "Running"
//!     error_reason: Str    ("" unless lifecycle == "Error")
//!     name: Str            (e.g. "charming-toucan")
//!     language: Str        (e.g. "python", "typescript")
//!     env_source: Str      (e.g. "uv:prewarmed", "pixi:toml", "deno")
```

- [ ] **Step 2: Extend the `new()` scaffold**

In `crates/runtime-doc/src/doc.rs`, inside `pub fn new()`, after the existing `doc.put(&kernel, "starting_phase", "")` (around line 274) and before the `// queue/` section, append:

```rust
        doc.put(&kernel, "lifecycle", "NotStarted")
            .expect("scaffold kernel.lifecycle");
        doc.put(&kernel, "activity", "")
            .expect("scaffold kernel.activity");
        doc.put(&kernel, "error_reason", "")
            .expect("scaffold kernel.error_reason");
```

- [ ] **Step 3: Extend the `new_with_actor()` scaffold**

In the matching block inside `new_with_actor()` (around line 358, after the `starting_phase` scaffold), append the identical three `doc.put` calls.

- [ ] **Step 4: Verify the workspace is still green**

```bash
cargo test -p runtime-doc 2>&1 | tail -20
cargo check --workspace 2>&1 | tail -10
```

Expected: all runtime-doc tests still pass; workspace compiles.

- [ ] **Step 5: Commit**

```bash
git add crates/runtime-doc/src/doc.rs
git commit -m "refactor(runtime-doc): scaffold kernel/lifecycle+activity+error_reason alongside legacy keys"
```

---

## Task 3: Add `lifecycle` + `error_reason` fields on `KernelState` + `set_lifecycle` / `set_activity` writers

**Files:**
- Modify: `crates/runtime-doc/src/doc.rs`

`KernelState` grows `lifecycle` + `error_reason` fields alongside existing `status` + `starting_phase`. `read_state` populates all four from the CRDT (both from the legacy `status`/`starting_phase` keys and from the new `lifecycle`/`activity`/`error_reason` keys). The new writers (`set_lifecycle`, `set_activity`, `set_lifecycle_with_error`) write BOTH the new CRDT keys (`lifecycle`, `activity`, `error_reason`) AND mirror into the legacy `status` + `starting_phase` keys. This dual-shape write keeps readers that haven't migrated yet observing correct state. After Rust callers (Tasks 4–10), Python bindings (Task 11), the TS migration (Tasks 12–13), and Python metrics (Task 14) have all moved to the new API, Task 15 atomically retires the legacy shape.

- [ ] **Step 1: Write the failing tests**

Append to the existing `#[cfg(test)] mod tests` block in `crates/runtime-doc/src/doc.rs`:

```rust
    #[test]
    fn set_lifecycle_writes_variant_and_clears_activity() -> Result<(), RuntimeStateError> {
        use crate::{KernelActivity, RuntimeLifecycle};

        let mut doc = RuntimeStateDoc::new();

        doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Busy))?;
        assert_eq!(
            doc.read_state().kernel.lifecycle,
            RuntimeLifecycle::Running(KernelActivity::Busy)
        );

        doc.set_lifecycle(&RuntimeLifecycle::Shutdown)?;
        assert_eq!(doc.read_state().kernel.lifecycle, RuntimeLifecycle::Shutdown);

        // Activity is cleared when leaving Running so a future Running(Idle)
        // write is not conflated with stale Busy.
        let (_, kernel) = doc
            .doc()
            .get(&automerge::ROOT, "kernel")
            .expect("kernel key exists")
            .expect("kernel value present");
        let (activity, _) = doc
            .doc()
            .get(&kernel, "activity")
            .expect("activity key exists")
            .expect("activity value present");
        match activity {
            automerge::Value::Scalar(s) => match s.as_ref() {
                automerge::ScalarValue::Str(s) => assert_eq!(s.as_str(), ""),
                other => panic!("activity should be a string scalar, got {other:?}"),
            },
            other => panic!("activity should be a scalar, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn set_activity_is_noop_when_unchanged() -> Result<(), RuntimeStateError> {
        use crate::{KernelActivity, RuntimeLifecycle};

        let mut doc = RuntimeStateDoc::new();
        doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))?;
        let heads_before = doc.get_heads();
        doc.set_activity(KernelActivity::Idle)?;
        assert_eq!(
            heads_before,
            doc.get_heads(),
            "set_activity should not write when value is unchanged"
        );

        doc.set_activity(KernelActivity::Busy)?;
        assert_ne!(
            heads_before,
            doc.get_heads(),
            "set_activity should write when value changes"
        );
        assert_eq!(
            doc.read_state().kernel.lifecycle,
            RuntimeLifecycle::Running(KernelActivity::Busy)
        );
        Ok(())
    }

    #[test]
    fn set_lifecycle_with_error_populates_error_reason() -> Result<(), RuntimeStateError> {
        use crate::RuntimeLifecycle;

        let mut doc = RuntimeStateDoc::new();
        doc.set_lifecycle_with_error(
            &RuntimeLifecycle::Error,
            Some("missing_ipykernel"),
        )?;
        let state = doc.read_state();
        assert_eq!(state.kernel.lifecycle, RuntimeLifecycle::Error);
        assert_eq!(
            state.kernel.error_reason.as_deref(),
            Some("missing_ipykernel")
        );

        // Explicit clear: pass None to set_lifecycle_with_error.
        doc.set_lifecycle_with_error(&RuntimeLifecycle::NotStarted, None)?;
        let state = doc.read_state();
        assert_eq!(state.kernel.lifecycle, RuntimeLifecycle::NotStarted);
        assert_eq!(state.kernel.error_reason.as_deref(), Some(""));
        Ok(())
    }

    #[test]
    fn set_lifecycle_preserves_error_reason_when_reentering_error()
        -> Result<(), RuntimeStateError>
    {
        use crate::RuntimeLifecycle;

        let mut doc = RuntimeStateDoc::new();
        doc.set_lifecycle_with_error(
            &RuntimeLifecycle::Error,
            Some("missing_ipykernel"),
        )?;

        // Plain `set_lifecycle(Error)` must NOT clobber the existing reason —
        // otherwise a retry path that re-enters Error loses the original
        // diagnosis. Only `set_lifecycle_with_error(lc, None)` explicitly
        // clears the reason.
        doc.set_lifecycle(&RuntimeLifecycle::Error)?;
        assert_eq!(
            doc.read_state().kernel.error_reason.as_deref(),
            Some("missing_ipykernel"),
            "re-entering Error via set_lifecycle must preserve the existing reason"
        );
        Ok(())
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p runtime-doc --lib set_lifecycle set_activity_is_noop 2>&1 | tail -30
```

Expected: fails with "no method named `set_lifecycle`/`set_activity`/`set_lifecycle_with_error` found" and/or "no field `lifecycle` on `KernelState`".

- [ ] **Step 3: Extend the `KernelState` struct**

In `crates/runtime-doc/src/doc.rs`, update the `KernelState` struct (lines 74–90) to add the new fields:

```rust
/// Kernel state snapshot.
///
/// Dual-shape during the RuntimeLifecycle migration. The `status` and
/// `starting_phase` fields are deprecated and will be removed by Task 15
/// of the RuntimeLifecycle plan once every caller has migrated to
/// `lifecycle`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelState {
    /// Deprecated — reads from `kernel/status`, soon replaced by `lifecycle`.
    pub status: String,
    /// Deprecated — reads from `kernel/starting_phase`, soon replaced by
    /// pattern matching on `lifecycle`.
    #[serde(default)]
    pub starting_phase: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub env_source: String,
    /// ID of the runtime agent subprocess that owns this kernel.
    #[serde(default)]
    pub runtime_agent_id: String,
    /// Typed lifecycle state. Replaces `status` + `starting_phase`.
    #[serde(default)]
    pub lifecycle: RuntimeLifecycle,
    /// Human-readable reason when `lifecycle == Error`. `Some("")` when
    /// the `kernel/error_reason` key is scaffolded but empty; `None` when
    /// the kernel map is absent entirely (new-born, unscaffolded doc).
    #[serde(default)]
    pub error_reason: Option<String>,
}
```

Update the `Default` impl (lines 92–103):

```rust
impl Default for KernelState {
    fn default() -> Self {
        Self {
            status: "not_started".to_string(),
            starting_phase: String::new(),
            name: String::new(),
            language: String::new(),
            env_source: String::new(),
            runtime_agent_id: String::new(),
            lifecycle: RuntimeLifecycle::NotStarted,
            error_reason: None,
        }
    }
}
```

At the top of `doc.rs`, update the `use crate::StreamOutputState;` line to:

```rust
use crate::{KernelActivity, RuntimeLifecycle, StreamOutputState};
```

- [ ] **Step 4: Populate both shapes in `read_state`**

Locate `read_state` (around line 1849). Replace the `kernel_state = kernel.as_ref().map(|k| KernelState { ... })` block with:

```rust
        let kernel_state = kernel
            .as_ref()
            .map(|k| {
                let lifecycle_str = self.read_str(k, "lifecycle");
                let activity_str = self.read_str(k, "activity");
                let lifecycle = if lifecycle_str.is_empty() {
                    // Older docs without the new scaffold — derive a best-effort
                    // lifecycle from the legacy status string. Task 15 removes
                    // this fallback.
                    legacy_status_to_lifecycle(
                        &self.read_str(k, "status"),
                        &self.read_str(k, "starting_phase"),
                    )
                } else {
                    RuntimeLifecycle::parse(&lifecycle_str, &activity_str).unwrap_or_default()
                };
                let error_reason_raw = self.read_str(k, "error_reason");
                KernelState {
                    status: self.read_str(k, "status"),
                    starting_phase: self.read_str(k, "starting_phase"),
                    name: self.read_str(k, "name"),
                    language: self.read_str(k, "language"),
                    env_source: self.read_str(k, "env_source"),
                    runtime_agent_id: self.read_str(k, "runtime_agent_id"),
                    lifecycle,
                    error_reason: Some(error_reason_raw),
                }
            })
            .unwrap_or_default();
```

Add a free-function helper at the bottom of the `impl RuntimeStateDoc` block (or anywhere in the file, after `impl RuntimeStateDoc`):

```rust
fn legacy_status_to_lifecycle(status: &str, starting_phase: &str) -> RuntimeLifecycle {
    match status {
        "idle" => RuntimeLifecycle::Running(KernelActivity::Idle),
        "busy" => RuntimeLifecycle::Running(KernelActivity::Busy),
        "starting" => match starting_phase {
            "resolving" => RuntimeLifecycle::Resolving,
            "preparing_env" => RuntimeLifecycle::PreparingEnv,
            "launching" => RuntimeLifecycle::Launching,
            "connecting" => RuntimeLifecycle::Connecting,
            _ => RuntimeLifecycle::Resolving,
        },
        "error" => RuntimeLifecycle::Error,
        "shutdown" => RuntimeLifecycle::Shutdown,
        "awaiting_trust" => RuntimeLifecycle::AwaitingTrust,
        _ => RuntimeLifecycle::NotStarted,
    }
}
```

This helper only runs on docs that lack the new scaffold (forked-before-Task-2 docs received via sync). Task 15 deletes it.

- [ ] **Step 5: Implement the new writers (dual-shape)**

During the migration window (Tasks 3–14), the new writers ALSO maintain the legacy `kernel.status` + `kernel.starting_phase` keys. Every reader — whether it looks at `kernel.lifecycle` (new) or `kernel.status` (legacy) — sees consistent state. Task 15 removes the legacy writes along with the keys.

Insert immediately above the `// ── Execution lifecycle ──` section (around line 875):

```rust
    // ── Lifecycle writers ───────────────────────────────────────────

    /// Write a runtime lifecycle transition without touching `error_reason`.
    ///
    /// When the new lifecycle is `Running(activity)`, both the `lifecycle`
    /// variant and the `activity` key are written. When the new lifecycle is
    /// anything else, `activity` is cleared to `""`. `error_reason` is left
    /// as-is — callers that need to set or clear it should use
    /// [`set_lifecycle_with_error`].
    ///
    /// Also updates the legacy `kernel.status` and `kernel.starting_phase`
    /// keys so that readers still on the old shape (during the
    /// RuntimeLifecycle migration) see consistent state. This legacy
    /// maintenance is removed together with the old keys in the atomic
    /// retire commit (plan Task 15).
    pub fn set_lifecycle(
        &mut self,
        lifecycle: &RuntimeLifecycle,
    ) -> Result<(), RuntimeStateError> {
        let kernel = self.scaffold_map("kernel")?;
        self.doc.put(&kernel, "lifecycle", lifecycle.variant_str())?;
        match lifecycle {
            RuntimeLifecycle::Running(activity) => {
                self.doc.put(&kernel, "activity", activity.as_str())?;
            }
            _ => {
                self.doc.put(&kernel, "activity", "")?;
            }
        }
        // Dual-shape: maintain legacy kernel.status + kernel.starting_phase
        // for readers not yet migrated. Removed in plan Task 15.
        let (legacy_status, legacy_phase) = legacy_shape_for(lifecycle);
        self.doc.put(&kernel, "status", legacy_status)?;
        self.doc.put(&kernel, "starting_phase", legacy_phase)?;
        Ok(())
    }

    /// Write a runtime lifecycle transition and set or clear `error_reason`.
    ///
    /// Pass `Some("reason")` to record a diagnosis when transitioning into
    /// `Error`. Pass `None` to explicitly clear the reason. `set_lifecycle`
    /// alone does NOT touch `error_reason`, so callers can call
    /// `set_lifecycle(Error)` a second time on retry without losing the
    /// original diagnosis.
    pub fn set_lifecycle_with_error(
        &mut self,
        lifecycle: &RuntimeLifecycle,
        error_reason: Option<&str>,
    ) -> Result<(), RuntimeStateError> {
        self.set_lifecycle(lifecycle)?;
        let kernel = self.scaffold_map("kernel")?;
        let reason = error_reason.unwrap_or("");
        self.doc.put(&kernel, "error_reason", reason)?;
        Ok(())
    }

    /// Update just the kernel activity. Only meaningful when the lifecycle is
    /// already `Running`; callers are expected to ensure that invariant. This
    /// is the hot path for IOPub idle/busy status and is a no-op when the
    /// value has not changed.
    ///
    /// Also updates the legacy `kernel.status` key (to `"busy"`/`"idle"`)
    /// during the migration window. Removed in plan Task 15.
    pub fn set_activity(
        &mut self,
        activity: KernelActivity,
    ) -> Result<(), RuntimeStateError> {
        let kernel = self.scaffold_map("kernel")?;
        let current = self.read_str(&kernel, "activity");
        if current == activity.as_str() {
            return Ok(());
        }
        self.doc.put(&kernel, "activity", activity.as_str())?;
        // Dual-shape: mirror into legacy kernel.status. Removed in Task 15.
        let legacy_status = match activity {
            KernelActivity::Busy => "busy",
            KernelActivity::Idle => "idle",
            KernelActivity::Unknown => "idle",
        };
        self.doc.put(&kernel, "status", legacy_status)?;
        Ok(())
    }
```

Add the helper right after the new writers (or anywhere in the `impl` block):

```rust
/// Legacy (status, starting_phase) projection used during the migration
/// window so readers still on the old shape see consistent state. Removed
/// in plan Task 15 along with the legacy CRDT keys.
fn legacy_shape_for(lc: &RuntimeLifecycle) -> (&'static str, &'static str) {
    use RuntimeLifecycle::*;
    match lc {
        NotStarted => ("not_started", ""),
        AwaitingTrust => ("awaiting_trust", ""),
        Resolving => ("starting", "resolving"),
        PreparingEnv => ("starting", "preparing_env"),
        Launching => ("starting", "launching"),
        Connecting => ("starting", "connecting"),
        Running(KernelActivity::Busy) => ("busy", ""),
        Running(_) => ("idle", ""),
        Error => ("error", ""),
        Shutdown => ("shutdown", ""),
    }
}
```

Append a test that pins the dual-shape invariant:

```rust
    #[test]
    fn set_lifecycle_maintains_legacy_shape() -> Result<(), RuntimeStateError> {
        use crate::{KernelActivity, RuntimeLifecycle};

        let mut doc = RuntimeStateDoc::new();

        doc.set_lifecycle(&RuntimeLifecycle::Resolving)?;
        let s = doc.read_state();
        assert_eq!(s.kernel.status, "starting");
        assert_eq!(s.kernel.starting_phase, "resolving");

        doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))?;
        let s = doc.read_state();
        assert_eq!(s.kernel.status, "idle");
        assert_eq!(s.kernel.starting_phase, "");

        doc.set_activity(KernelActivity::Busy)?;
        let s = doc.read_state();
        assert_eq!(s.kernel.status, "busy");
        assert_eq!(
            s.kernel.lifecycle,
            RuntimeLifecycle::Running(KernelActivity::Busy)
        );
        Ok(())
    }
```

- [ ] **Step 6: Run tests**

```bash
cargo test -p runtime-doc 2>&1 | tail -30
cargo check --workspace 2>&1 | tail -10
```

Expected: all runtime-doc tests pass (including the four new ones); workspace still compiles.

- [ ] **Step 7: Commit**

```bash
git add crates/runtime-doc/src/doc.rs
git commit -m "feat(runtime-doc): add lifecycle/activity/error_reason to KernelState + writers"
```

---

## Task 4: Migrate IOPub + kernel-died paths (`runtimed::jupyter_kernel` + `runtime_agent`)

**Files:**
- Modify: `crates/runtimed/src/jupyter_kernel.rs`
- Modify: `crates/runtimed/src/runtime_agent.rs`
- Modify: `crates/runtimed/src/kernel_state.rs` (comment only)

These sites migrate first because they are the hot path producers.

- [ ] **Step 1: Rewrite the IOPub status handler**

In `crates/runtimed/src/jupyter_kernel.rs`, locate the `JupyterMessageContent::Status` arm (around lines 740–766). Replace it with:

```rust
                                JupyterMessageContent::Status(status) => {
                                    use runtime_doc::{KernelActivity, RuntimeLifecycle};

                                    // Non-execute messages (kernel_info, completions) have a
                                    // parent_header.msg_id that isn't in our execute map.
                                    // `cell_id` is None for those — treat their busy/idle as transient.
                                    let is_transient = cell_id.is_none();

                                    let update = match status.execution_state {
                                        jupyter_protocol::ExecutionState::Busy if !is_transient => {
                                            Some(Ok(KernelActivity::Busy))
                                        }
                                        jupyter_protocol::ExecutionState::Idle if !is_transient => {
                                            Some(Ok(KernelActivity::Idle))
                                        }
                                        jupyter_protocol::ExecutionState::Starting
                                        | jupyter_protocol::ExecutionState::Restarting => {
                                            Some(Err(RuntimeLifecycle::Connecting))
                                        }
                                        jupyter_protocol::ExecutionState::Terminating
                                        | jupyter_protocol::ExecutionState::Dead => {
                                            Some(Err(RuntimeLifecycle::Shutdown))
                                        }
                                        _ => None,
                                    };

                                    if let Some(update) = update {
                                        let result = state_for_iopub.with_doc(|sd| match update {
                                            Ok(activity) => sd.set_activity(activity),
                                            Err(lifecycle) => sd.set_lifecycle(&lifecycle),
                                        });
                                        if let Err(e) = result {
                                            warn!("[runtime-state] {}", e);
                                        }
                                    }
```

Leave the `if status.execution_state == Idle { … ExecutionDone … }` block below untouched.

- [ ] **Step 2: Rewrite the kernel-died write in `runtime_agent.rs`**

In `crates/runtimed/src/runtime_agent.rs`, at the `set_kernel_status("error")` call inside the kernel-died handler (around line 988), replace:

```rust
                sd.set_kernel_status("error")?;
```

with:

```rust
                sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Error)?;
```

- [ ] **Step 3: Update the stale comment in `kernel_state.rs`**

Around line 268, change:

```rust
// state_doc.set_kernel_status("error") + set_queue(None, &[])
```

to:

```rust
// state_doc.set_lifecycle(RuntimeLifecycle::Error) + set_queue(None, &[])
```

- [ ] **Step 4: Compile + test**

```bash
cargo check -p runtimed 2>&1 | tail -10
cargo test -p runtimed --lib 2>&1 | tail -30
```

Expected: `runtimed` still compiles; the two `runtime_agent.rs` tests at lines 1181 and 1200 (which read `kernel.status == "error"`) still pass because Task 3 kept populating `kernel.status`; Task 7 migrates those asserts to `kernel.lifecycle`.

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed/src/jupyter_kernel.rs crates/runtimed/src/runtime_agent.rs crates/runtimed/src/kernel_state.rs
git commit -m "refactor(runtimed): migrate IOPub + kernel-died paths to set_lifecycle/activity"
```

---

## Task 5: Migrate `notebook_sync_server` (peer + metadata + tests + room)

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server/peer.rs`
- Modify: `crates/runtimed/src/notebook_sync_server/metadata.rs`
- Modify: `crates/runtimed/src/notebook_sync_server/tests.rs`
- Modify: `crates/runtimed/src/notebook_sync_server/room.rs`

- [ ] **Step 1: Rewrite the auto-launch claim in `peer.rs`**

Around lines 457–463, replace:

```rust
            if let Err(e) = room.state.with_doc(|sd| {
                sd.set_kernel_status("starting")?;
                sd.set_starting_phase("resolving")?;
                Ok(())
            }) {
```

with:

```rust
            if let Err(e) = room
                .state
                .with_doc(|sd| sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Resolving))
            {
```

- [ ] **Step 2: Rewrite the auto-launch panic handler**

Around lines 487–494, replace:

```rust
                        if let Err(e) = r.state.with_doc(|sd| {
                            sd.set_kernel_status("error")?;
                            sd.set_starting_phase("")?;
                            Ok(())
                        }) {
```

with:

```rust
                        if let Err(e) = r.state.with_doc(|sd| {
                            sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Error)
                        }) {
```

- [ ] **Step 3: Rewrite the trust-blocked branch**

Around lines 509–515, replace:

```rust
            if let Err(e) = room.state.with_doc(|sd| {
                sd.set_kernel_status("awaiting_trust")?;
                sd.set_starting_phase("")?;
                Ok(())
            }) {
```

with:

```rust
            if let Err(e) = room
                .state
                .with_doc(|sd| sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::AwaitingTrust))
            {
```

- [ ] **Step 4: Migrate the `metadata.rs` writers**

In `crates/runtimed/src/notebook_sync_server/metadata.rs`:

- Around lines 1731–1737 (`not_started` reset):
  ```rust
  sd.set_kernel_status("not_started")?;
  sd.set_prewarmed_packages(&[])?;
  ```
  →
  ```rust
  sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::NotStarted)?;
  sd.set_prewarmed_packages(&[])?;
  ```

- Around lines 2387–2394 (missing-ipykernel):
  ```rust
  sd.set_kernel_status("error")?;
  sd.set_kernel_info("python", "python", env_source.as_str())?;
  sd.set_starting_phase("missing_ipykernel")?;
  ```
  →
  ```rust
  sd.set_lifecycle_with_error(
      &runtime_doc::RuntimeLifecycle::Error,
      Some("missing_ipykernel"),
  )?;
  sd.set_kernel_info("python", "python", env_source.as_str())?;
  ```

- Around line 2403 (`preparing_env`): `sd.set_starting_phase("preparing_env")` → `sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::PreparingEnv)`.

- Around line 2706 (`launching`): `sd.set_starting_phase("launching")` → `sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Launching)`.

- Around line 2760 (`connecting`): `sd.set_starting_phase("connecting")` → `sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Connecting)`.

- Around line 2821 (`idle` on launch success): `sd.set_kernel_status("idle")?` → `sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Running(runtime_doc::KernelActivity::Idle))?`.

- [ ] **Step 5: Migrate the `metadata.rs` reader at line 673**

Around lines 670–678, replace:

```rust
    // Check kernel is actually running via RuntimeStateDoc
    {
        let status = room
            .state
            .read(|sd| sd.read_state().kernel.status.clone())
            .unwrap_or_default();
        if status != "idle" && status != "busy" {
            return;
        }
    }
```

with:

```rust
    // Check kernel is actually running via RuntimeStateDoc
    {
        let lifecycle = room
            .state
            .read(|sd| sd.read_state().kernel.lifecycle)
            .unwrap_or(runtime_doc::RuntimeLifecycle::NotStarted);
        if !matches!(lifecycle, runtime_doc::RuntimeLifecycle::Running(_)) {
            return;
        }
    }
```

- [ ] **Step 6: Migrate the daemon tests**

In `crates/runtimed/src/notebook_sync_server/tests.rs`:
- Line 3049 (`sd.set_kernel_status("idle")?`) → `sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Running(runtime_doc::KernelActivity::Idle))?`.
- Line 3101 (`with_doc(|sd| sd.set_kernel_status("idle"))`) → `with_doc(|sd| sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Running(runtime_doc::KernelActivity::Idle)))`.
- Lines 3531, 3581 (`with_doc(|sd| sd.set_kernel_status("starting"))`) → `with_doc(|sd| sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Resolving))`.
- Lines 3540, 3564, 3588 (three asserts on `state.kernel.status`). The file's `reset_starting_state` tests read `state.kernel.status.clone()` and assert either `"starting"` or `"not_started"`. Rewrite each read + assert pair:

  ```rust
  let lifecycle = room
      .state
      .read(|sd| sd.read_state().kernel.lifecycle)
      .unwrap_or(runtime_doc::RuntimeLifecycle::NotStarted);
  assert!(
      matches!(lifecycle, runtime_doc::RuntimeLifecycle::Resolving),
      "expected Resolving (was `starting`), got {lifecycle:?}",
  );
  ```

  And for the `not_started` assertions:

  ```rust
  assert!(
      matches!(lifecycle, runtime_doc::RuntimeLifecycle::NotStarted),
      "expected NotStarted, got {lifecycle:?}",
  );
  ```

  Replace the `.unwrap()` on the `read` call if the surrounding test already returns a `Result`, or keep `.expect("read runtime state")` if not.

  Also, line 3585 in the same block has a second `room.state.with_doc(|sd| sd.set_kernel_status("starting")).unwrap();` — convert to `with_doc(|sd| sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Resolving))?` (or `.expect("reset lifecycle")` if the test doesn't return `Result`).

- [ ] **Step 7: Rewrite `room.rs` read + add helper**

In `crates/runtimed/src/notebook_sync_server/room.rs` around lines 490–496 (grep `rg -n 'state.kernel.status' crates/runtimed/src/notebook_sync_server/room.rs` to locate), replace:

```rust
                if state.kernel.status != "not_started" && !state.kernel.status.is_empty() {
                    ...
                    state.kernel.status.clone(),
                    ...
                }
```

with:

```rust
                if !matches!(state.kernel.lifecycle, runtime_doc::RuntimeLifecycle::NotStarted) {
                    ...
                    lifecycle_to_status_string(&state.kernel.lifecycle),
                    ...
                }
```

Append the helper at the bottom of `room.rs` (before any test module):

```rust
/// Render `RuntimeLifecycle` as the legacy status string used by the
/// presence channel and external wire consumers (runt-mcp, runtimed-node,
/// metrics scripts). `Running` collapses to either "idle" or "busy"
/// depending on activity.
pub(crate) fn lifecycle_to_status_string(
    lc: &runtime_doc::RuntimeLifecycle,
) -> String {
    use runtime_doc::{KernelActivity, RuntimeLifecycle};
    match lc {
        RuntimeLifecycle::NotStarted => "not_started".to_string(),
        RuntimeLifecycle::AwaitingTrust => "awaiting_trust".to_string(),
        RuntimeLifecycle::Resolving
        | RuntimeLifecycle::PreparingEnv
        | RuntimeLifecycle::Launching
        | RuntimeLifecycle::Connecting => "starting".to_string(),
        RuntimeLifecycle::Running(KernelActivity::Busy) => "busy".to_string(),
        RuntimeLifecycle::Running(_) => "idle".to_string(),
        RuntimeLifecycle::Error => "error".to_string(),
        RuntimeLifecycle::Shutdown => "shutdown".to_string(),
    }
}
```

Presence (`crates/notebook-doc/src/presence.rs`) and `NotebookResponse::KernelInfo::status` keep their legacy wire strings on purpose — changing them is out of scope.

- [ ] **Step 8: Compile + test**

```bash
cargo check -p runtimed 2>&1 | tail -10
cargo test -p runtimed --lib 2>&1 | tail -30
```

Expected: green.

- [ ] **Step 9: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server/peer.rs \
        crates/runtimed/src/notebook_sync_server/metadata.rs \
        crates/runtimed/src/notebook_sync_server/tests.rs \
        crates/runtimed/src/notebook_sync_server/room.rs
git commit -m "refactor(runtimed): migrate notebook_sync_server to set_lifecycle"
```

---

## Task 6: Migrate `runtimed::requests`

**Files:**
- Modify: `crates/runtimed/src/requests/launch_kernel.rs`
- Modify: `crates/runtimed/src/requests/shutdown_kernel.rs`
- Modify: `crates/runtimed/src/requests/execute_cell.rs`
- Modify: `crates/runtimed/src/requests/run_all_cells.rs`
- Modify: `crates/runtimed/src/requests/get_kernel_info.rs`

- [ ] **Step 1: Rewrite the atomic claim in `launch_kernel.rs`**

Around lines 55–75, replace:

```rust
    let kernel_status = room
        .state
        .with_doc(|sd| {
            let status = sd.read_state().kernel.status.clone();
            if status != "idle" && status != "busy" && status != "starting" {
                sd.clear_comms().ok();
                sd.set_trust("trusted", false).ok();
                sd.set_kernel_status("starting").ok();
                sd.set_starting_phase("resolving").ok();
            }
            Ok(status)
        })
        .unwrap_or_else(|e| {
            warn!("[runtime-state] {}", e);
            "not_started".to_string()
        });
    match kernel_status.as_str() {
        "idle" | "busy" => {
```

with:

```rust
    use runtime_doc::{KernelActivity, RuntimeLifecycle};

    let prior_lifecycle = room
        .state
        .with_doc(|sd| {
            let lifecycle = sd.read_state().kernel.lifecycle;
            let already_progressing = matches!(
                lifecycle,
                RuntimeLifecycle::Running(_)
                    | RuntimeLifecycle::Resolving
                    | RuntimeLifecycle::PreparingEnv
                    | RuntimeLifecycle::Launching
                    | RuntimeLifecycle::Connecting
            );
            if !already_progressing {
                sd.clear_comms().ok();
                sd.set_trust("trusted", false).ok();
                sd.set_lifecycle(&RuntimeLifecycle::Resolving).ok();
            }
            Ok(lifecycle)
        })
        .unwrap_or(RuntimeLifecycle::NotStarted);

    match prior_lifecycle {
        RuntimeLifecycle::Running(KernelActivity::Idle | KernelActivity::Busy) => {
```

Rewrite the subsequent match arms so the "else" arm is `_ => { /* continue launching */ }`. The caller-visible flow is identical.

- [ ] **Step 2: Rewrite the "starting in progress — wait" branch in `launch_kernel.rs`**

The `match prior_lifecycle` block has a second arm (around lines 85–99 in the pre-migration file) that waits up to 60s for an in-flight launch to finish. It currently polls `kernel.status.clone()` and returns when the status becomes one of `"idle"`, `"busy"`, `"error"`, `"shutdown"`, or `"not_started"`. Rewrite to poll `kernel.lifecycle` and return on terminal variants:

```rust
        // Old shape, for reference:
        // let s = room
        //     .state
        //     .read(|sd| sd.read_state().kernel.status.clone())
        //     .unwrap_or_default();
        // if s == "idle" || s == "busy" || s == "error" || s == "shutdown" || s == "not_started" {
        //     return s;
        // }

        // New shape — match any terminal lifecycle variant.
        use runtime_doc::RuntimeLifecycle;
        let wait_result = tokio::time::timeout(std::time::Duration::from_secs(60), async {
            loop {
                let lc = room
                    .state
                    .read(|sd| sd.read_state().kernel.lifecycle)
                    .unwrap_or(RuntimeLifecycle::NotStarted);
                if matches!(
                    lc,
                    RuntimeLifecycle::Running(_)
                        | RuntimeLifecycle::Error
                        | RuntimeLifecycle::Shutdown
                        | RuntimeLifecycle::NotStarted
                ) {
                    return lc;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await;
```

The match arm that consumes this block's return value should also be updated from `kernel_status.as_str()` pattern matching to `matches!(wait_result, RuntimeLifecycle::Running(_))` for the "kernel ready" path; audit the surrounding code after rewriting and update downstream branches that consume the wait result to match on `RuntimeLifecycle` variants instead of strings.

- [ ] **Step 3: Rewrite in-flight phase transitions in `launch_kernel.rs`**

- Around line 465: `sd.set_starting_phase("preparing_env")` → `sd.set_lifecycle(&RuntimeLifecycle::PreparingEnv)`.
- Around line 1080: `sd.set_starting_phase("launching")` → `sd.set_lifecycle(&RuntimeLifecycle::Launching)`.
- Around line 1111 (inside the `KernelRestarted` arm): `sd.set_kernel_status("idle")?` → `sd.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))?`.
- Around line 1197: `with_doc(|sd| sd.set_starting_phase("connecting"))` → `with_doc(|sd| sd.set_lifecycle(&RuntimeLifecycle::Connecting))`.
- Around line 1257 (inside the `KernelLaunched` arm): `sd.set_kernel_status("idle")?` → `sd.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))?`.

- [ ] **Step 4: Rewrite `shutdown_kernel.rs`**

Line 24: `sd.set_kernel_status("shutdown")?` → `sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Shutdown)?`.

- [ ] **Step 5: Rewrite `execute_cell.rs` precondition**

Around lines 58–63:

```rust
                    .read(|sd| sd.read_state().kernel.status.clone())
                    .unwrap_or_default();
                if status == "shutdown" || status == "error" {
```

→

```rust
                    .read(|sd| sd.read_state().kernel.lifecycle)
                    .unwrap_or(runtime_doc::RuntimeLifecycle::NotStarted);
                if matches!(
                    lifecycle,
                    runtime_doc::RuntimeLifecycle::Shutdown | runtime_doc::RuntimeLifecycle::Error
                ) {
```

Rename the local `status` binding to `lifecycle`.

- [ ] **Step 6: Rewrite `run_all_cells.rs` precondition**

Around lines 16–20, apply the same transformation as Step 4.

- [ ] **Step 7: Rewrite `get_kernel_info.rs`**

Replace the `handle` body with:

```rust
pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    use runtime_doc::RuntimeLifecycle;
    let state = room.state.read(|sd| sd.read_state());
    match state {
        Ok(state)
            if !matches!(state.kernel.lifecycle, RuntimeLifecycle::NotStarted) =>
        {
            NotebookResponse::KernelInfo {
                kernel_type: if state.kernel.name.is_empty() {
                    None
                } else {
                    Some(state.kernel.name)
                },
                env_source: if state.kernel.env_source.is_empty() {
                    None
                } else {
                    Some(state.kernel.env_source)
                },
                status: crate::notebook_sync_server::room::lifecycle_to_status_string(
                    &state.kernel.lifecycle,
                ),
            }
        }
        _ => NotebookResponse::KernelInfo {
            kernel_type: None,
            env_source: None,
            status: "not_started".to_string(),
        },
    }
}
```

- [ ] **Step 8: Compile + test**

```bash
cargo check -p runtimed 2>&1 | tail -10
cargo test -p runtimed --lib 2>&1 | tail -30
```

Expected: green.

- [ ] **Step 9: Commit**

```bash
git add crates/runtimed/src/requests/launch_kernel.rs \
        crates/runtimed/src/requests/shutdown_kernel.rs \
        crates/runtimed/src/requests/execute_cell.rs \
        crates/runtimed/src/requests/run_all_cells.rs \
        crates/runtimed/src/requests/get_kernel_info.rs
git commit -m "refactor(runtimed): migrate request handlers to RuntimeLifecycle"
```

---

## Task 7: Migrate `runtimed::runtime_agent` test asserts

**Files:**
- Modify: `crates/runtimed/src/runtime_agent.rs`

The two tests inside `runtime_agent.rs`'s `#[cfg(test)] mod tests` assert on `kernel.status == "error"`. With `KernelState` now carrying `lifecycle` in parallel, swap those asserts to the typed form.

- [ ] **Step 1: Rewrite line 1181**

Replace:

```rust
        // Kernel status should be error
        assert_eq!(queue.kernel.status, "error");
```

with:

```rust
        // Kernel lifecycle should be Error
        assert_eq!(queue.kernel.lifecycle, runtime_doc::RuntimeLifecycle::Error);
```

- [ ] **Step 2: Rewrite line 1200**

Replace:

```rust
        assert_eq!(rs.kernel.status, "error");
```

with:

```rust
        assert_eq!(rs.kernel.lifecycle, runtime_doc::RuntimeLifecycle::Error);
```

- [ ] **Step 3: Run the tests**

```bash
cargo test -p runtimed --lib runtime_agent 2>&1 | tail -20
```

Expected: both `kernel_died_*` tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed/src/runtime_agent.rs
git commit -m "test(runtime-agent): assert kernel.lifecycle instead of kernel.status"
```

---

## Task 8: Migrate `notebook-sync` consumers

**Files:**
- Modify: `crates/notebook-sync/src/execution_wait.rs`
- Modify: `crates/notebook-sync/src/tests.rs`

- [ ] **Step 1: Rewrite `execution_wait.rs` reads**

Around lines 116–122:

```rust
            if state.kernel.status == "error" {
                ...
            }
            if state.kernel.status == "shutdown" {
```

→

```rust
            if matches!(state.kernel.lifecycle, runtime_doc::RuntimeLifecycle::Error) {
                ...
            }
            if matches!(state.kernel.lifecycle, runtime_doc::RuntimeLifecycle::Shutdown) {
```

Update the nearby doc comments that mention `kernel.status == "error"` to refer to `lifecycle`.

- [ ] **Step 2: Rewrite the `notebook-sync` tests**

In `crates/notebook-sync/src/tests.rs`:
- Line 815: `st.state_doc.set_kernel_status("error").unwrap();` → `st.state_doc.set_lifecycle(&runtime_doc::RuntimeLifecycle::Error)?;` (and add `-> Result<(), runtime_doc::RuntimeStateError>` to the containing test fn if it's still plain `fn`; if the file's existing tests return `()`, match whichever style the surrounding test uses and either way use `?` — not `.unwrap()`)
- Line 845: same replacement.

- [ ] **Step 3: Compile + test**

```bash
cargo check -p notebook-sync 2>&1 | tail -10
cargo test -p notebook-sync --lib 2>&1 | tail -20
```

Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/notebook-sync/src/execution_wait.rs crates/notebook-sync/src/tests.rs
git commit -m "refactor(notebook-sync): consume RuntimeLifecycle instead of status string"
```

---

## Task 9: Migrate `runt-mcp`

**Files:**
- Create: `crates/runt-mcp/src/kernel_status.rs`
- Modify: `crates/runt-mcp/src/lib.rs` (or `main.rs`, whichever declares sibling modules)
- Modify: `crates/runt-mcp/src/tools/kernel.rs`
- Modify: `crates/runt-mcp/src/tools/session.rs`

- [ ] **Step 1: Create the helper module**

Create `crates/runt-mcp/src/kernel_status.rs`:

```rust
use runtime_doc::{KernelActivity, RuntimeLifecycle};

/// Render a `RuntimeLifecycle` as the legacy status string the MCP wire
/// format exposes.
pub(crate) fn lifecycle_to_status_string(lc: &RuntimeLifecycle) -> String {
    match lc {
        RuntimeLifecycle::NotStarted => "not_started",
        RuntimeLifecycle::AwaitingTrust => "awaiting_trust",
        RuntimeLifecycle::Resolving
        | RuntimeLifecycle::PreparingEnv
        | RuntimeLifecycle::Launching
        | RuntimeLifecycle::Connecting => "starting",
        RuntimeLifecycle::Running(KernelActivity::Busy) => "busy",
        RuntimeLifecycle::Running(_) => "idle",
        RuntimeLifecycle::Error => "error",
        RuntimeLifecycle::Shutdown => "shutdown",
    }
    .to_string()
}
```

Add `mod kernel_status;` to the appropriate file (check `crates/runt-mcp/src/lib.rs` or `main.rs`).

- [ ] **Step 2: Rewrite the kernel-ready wait loop**

In `crates/runt-mcp/src/tools/kernel.rs` around lines 175–188:

```rust
                    if state.kernel.status == "idle" || state.kernel.status == "busy" {
                        return Ok(serde_json::json!({ "ok": true }));
                    }
                    if state.kernel.status == "error" {
                        return Ok(serde_json::json!({
                            "ok": false,
                            "error": format!("kernel failed to launch: {}", state.kernel.env_source),
                        }));
                    }
```

→

```rust
                    use runtime_doc::RuntimeLifecycle;
                    if matches!(state.kernel.lifecycle, RuntimeLifecycle::Running(_)) {
                        return Ok(serde_json::json!({ "ok": true }));
                    }
                    if matches!(state.kernel.lifecycle, RuntimeLifecycle::Error) {
                        let reason = state
                            .kernel
                            .error_reason
                            .as_deref()
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| state.kernel.env_source.as_str());
                        return Ok(serde_json::json!({
                            "ok": false,
                            "error": format!("kernel failed to launch: {}", reason),
                        }));
                    }
```

- [ ] **Step 3: Rewrite the session JSON emission**

In `crates/runt-mcp/src/tools/session.rs` around line 106:

```rust
                serde_json::json!(state.kernel.status),
```

→

```rust
                serde_json::json!(
                    crate::kernel_status::lifecycle_to_status_string(&state.kernel.lifecycle)
                ),
```

- [ ] **Step 4: Compile + test**

```bash
cargo check -p runt-mcp 2>&1 | tail -10
cargo test -p runt-mcp --lib 2>&1 | tail -20
```

Expected: green.

- [ ] **Step 5: Commit**

```bash
git add crates/runt-mcp/src/kernel_status.rs crates/runt-mcp/src/lib.rs \
        crates/runt-mcp/src/tools/kernel.rs crates/runt-mcp/src/tools/session.rs
git commit -m "refactor(runt-mcp): read RuntimeLifecycle via helper"
```

---

## Task 10: Migrate `runtimed-node`

**Files:**
- Modify: `crates/runtimed-node/src/session.rs`

- [ ] **Step 1: Rewrite the readiness check**

Around line 343:

```rust
                r.kernel.status == "ready" || r.kernel.status == "busy" || r.kernel.status == "idle"
```

→

```rust
                matches!(r.kernel.lifecycle, runtime_doc::RuntimeLifecycle::Running(_))
```

The legacy `"ready"` case had no `Running(_)` equivalent that differs from `Idle/Busy`; `Running(_)` subsumes it.

- [ ] **Step 2: Compile + test**

```bash
cargo check -p runtimed-node 2>&1 | tail -10
cargo test -p runtimed-node --lib 2>&1 | tail -20
```

Expected: green. `runt/src/main.rs` line 5182 refers to `NotebookResponse::KernelInfo::status` (the wire field that stays), not `KernelState::status` — no change needed. Confirm:

```bash
rg -n 'kernel\.status|kernel\.starting_phase' crates/runt/src/ crates/runtimed-node/src/
```

Expected: the only remaining hit is `runt/src/main.rs:5182` (wire field).

- [ ] **Step 3: Commit**

```bash
git add crates/runtimed-node/src/session.rs
git commit -m "refactor(runtimed-node): readiness check via RuntimeLifecycle::Running"
```

---

## Task 11: Migrate Python bindings (`runtimed-py`)

**Files:**
- Modify: `crates/runtimed-py/src/output.rs`
- Modify: `crates/runtimed-py/src/session_core.rs`

- [ ] **Step 1: Update `PyKernelState` (dual-shape — keep `status`)**

Replace the struct (lines 836–847) with:

```rust
#[pyclass(name = "KernelState", get_all, skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct PyKernelState {
    /// Lifecycle variant name (`"NotStarted"`, `"AwaitingTrust"`, `"Resolving"`,
    /// `"PreparingEnv"`, `"Launching"`, `"Connecting"`, `"Running"`, `"Error"`,
    /// `"Shutdown"`).
    pub lifecycle: String,
    /// Activity when lifecycle == "Running"; empty string otherwise.
    pub activity: String,
    /// Human-readable reason when lifecycle == "Error". Empty otherwise.
    pub error_reason: String,
    /// DEPRECATED. Legacy string-status view (`"idle"`, `"busy"`, `"starting"`,
    /// ...). Retained during the RuntimeLifecycle migration so the in-repo
    /// metrics scripts and README examples keep working; Task 15 removes it
    /// after Task 14 migrates those callers.
    pub status: String,
    pub name: String,
    pub language: String,
    pub env_source: String,
}
```

Update `__repr__` (lines 850–856):

```rust
fn __repr__(&self) -> String {
    let activity = if self.activity.is_empty() {
        String::new()
    } else {
        format!(", activity={}", self.activity)
    };
    format!(
        "KernelState(lifecycle={}{}, env_source={})",
        self.lifecycle, activity, self.env_source
    )
}
```

Update the `From<runtime_doc::RuntimeState>` conversion (around line 1033). Use the local `lifecycle_status_string` helper added in Step 2 to fill the deprecated `status` field:

```rust
            kernel: PyKernelState {
                lifecycle: rs.kernel.lifecycle.variant_str().to_string(),
                activity: match &rs.kernel.lifecycle {
                    runtime_doc::RuntimeLifecycle::Running(act) => act.as_str().to_string(),
                    _ => String::new(),
                },
                error_reason: rs.kernel.error_reason.clone().unwrap_or_default(),
                status: crate::session_core::lifecycle_status_string(&rs.kernel.lifecycle)
                    .to_string(),
                name: rs.kernel.name,
                language: rs.kernel.language,
                env_source: rs.kernel.env_source,
            },
```

If `From<runtime_doc::RuntimeState>` lives in `output.rs` and `lifecycle_status_string` is private to `session_core.rs`, either make the helper `pub(crate)` or move it to a shared module (e.g. a new `crates/runtimed-py/src/lifecycle_status.rs`). The plan chooses the first option for minimal churn — in Step 2, declare it `pub(crate) fn` instead of `fn`.

Update `PyRuntimeState::__repr__` (line 1007) to reference `self.kernel.lifecycle` instead of `self.kernel.status`.

- [ ] **Step 2: Rewrite `session_core.rs`**

Add near the top of the file:

```rust
use runtime_doc::{KernelActivity, RuntimeLifecycle};

pub(crate) fn lifecycle_status_string(lc: &RuntimeLifecycle) -> &'static str {
    match lc {
        RuntimeLifecycle::NotStarted => "not_started",
        RuntimeLifecycle::AwaitingTrust => "awaiting_trust",
        RuntimeLifecycle::Resolving
        | RuntimeLifecycle::PreparingEnv
        | RuntimeLifecycle::Launching
        | RuntimeLifecycle::Connecting => "starting",
        RuntimeLifecycle::Running(KernelActivity::Busy) => "busy",
        RuntimeLifecycle::Running(_) => "idle",
        RuntimeLifecycle::Error => "error",
        RuntimeLifecycle::Shutdown => "shutdown",
    }
}
```

Apply the five rewrites:

- Line 285 (`hydrate_kernel_state`):
  ```rust
  let running = matches!(rs.kernel.status.as_str(), "idle" | "busy" | "starting");
  ```
  →
  ```rust
  let running = matches!(
      rs.kernel.lifecycle,
      RuntimeLifecycle::Running(_)
          | RuntimeLifecycle::Resolving
          | RuntimeLifecycle::PreparingEnv
          | RuntimeLifecycle::Launching
          | RuntimeLifecycle::Connecting
  );
  ```

- Line 316:
  ```rust
  .map(|rs| rs.kernel.status)
  .unwrap_or_else(|| "not_started".to_string());
  ```
  →
  ```rust
  .map(|rs| lifecycle_status_string(&rs.kernel.lifecycle).to_string())
  .unwrap_or_else(|| "not_started".to_string());
  ```

- Line 737:
  ```rust
  if rs.kernel.status != "idle" { saw_non_idle = true; }
  else if saw_non_idle { return Ok(progress_messages); }
  ```
  →
  ```rust
  if !matches!(rs.kernel.lifecycle, RuntimeLifecycle::Running(KernelActivity::Idle)) {
      saw_non_idle = true;
  } else if saw_non_idle {
      return Ok(progress_messages);
  }
  ```

- Lines 1465–1470:
  ```rust
  if rs.kernel.status == "error" { kernel_error = Some("Kernel error".to_string()); done = true; }
  else if rs.kernel.status == "shutdown" { kernel_error = Some("Kernel shut down".to_string()); done = true; }
  ```
  →
  ```rust
  if matches!(rs.kernel.lifecycle, RuntimeLifecycle::Error) {
      kernel_error = Some("Kernel error".to_string());
      done = true;
  } else if matches!(rs.kernel.lifecycle, RuntimeLifecycle::Shutdown) {
      kernel_error = Some("Kernel shut down".to_string());
      done = true;
  }
  ```

- [ ] **Step 3: Rebuild Python bindings + run tests**

Two venvs matter for this crate (see CLAUDE.md § Rebuilding Python bindings):
- `.venv` (repo root) — used by the MCP server and `uv run nteract`.
- `python/runtimed/.venv` — used by `pytest` integration tests.

Native changes here affect both, so build into both. Run each block from the repo root.

```bash
# Build into the workspace venv (nteract MCP + uv run).
(cd crates/runtimed-py && VIRTUAL_ENV=../../.venv \
  uv run --directory ../../python/runtimed maturin develop) 2>&1 | tail -20

# Build into the test venv (pytest).
(cd crates/runtimed-py && VIRTUAL_ENV=../../python/runtimed/.venv \
  uv run --directory ../../python/runtimed maturin develop) 2>&1 | tail -20

# Run unit tests against the freshly-built extension.
python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_session_unit.py -v 2>&1 | tail -30
```

If `nteract-dev` MCP is available, `up rebuild=true` handles the first of the two builds (workspace venv). The pytest build is not automated — run it explicitly as shown.

Expected: green. The test suite exercises both the new `lifecycle`/`activity`/`error_reason` attributes and the preserved `status` field — confirm both paths pass before committing.

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed-py/src/output.rs crates/runtimed-py/src/session_core.rs
git commit -m "refactor(runtimed-py): expose lifecycle/activity instead of status string"
```

---
## Task 12: Introduce TS `RuntimeLifecycle` types (dual-shape)

**Files:**
- Modify: `packages/runtimed/src/runtime-state.ts`
- Modify: `packages/runtimed/src/index.ts`

Extend the TS `KernelState` interface with the new fields alongside the existing `status` + `starting_phase`, and re-export the new types from the package root. Once every TS consumer has moved (next task), we delete the old fields. The Rust→TS flow goes through the WASM runtime-state snapshot built by serde, so the Rust-side changes from earlier tasks (especially Task 3's `KernelState` dual-shape) are what populate these fields; this task just teaches TS to accept and use them.

- [ ] **Step 1: Write failing tests**

Append to `packages/runtimed/tests/sync-engine.test.ts`:

```typescript
describe("RuntimeLifecycle TS types", () => {
  it("DEFAULT_RUNTIME_STATE.kernel.lifecycle is NotStarted", () => {
    expect(DEFAULT_RUNTIME_STATE.kernel.lifecycle).toEqual({ lifecycle: "NotStarted" });
  });

  it("a Running lifecycle can carry activity", () => {
    const k: KernelState = {
      lifecycle: { lifecycle: "Running", activity: "Idle" },
      status: "idle",
      starting_phase: "",
      name: "",
      language: "",
      env_source: "",
    };
    expect(k.lifecycle.lifecycle).toBe("Running");
  });
});
```

Add `KernelState` to the `import { DEFAULT_RUNTIME_STATE, ... } from "../src/runtime-state"` line.

- [ ] **Step 2: Extend `runtime-state.ts`**

In `packages/runtimed/src/runtime-state.ts`, add near the top:

```typescript
export type KernelActivity = "Unknown" | "Idle" | "Busy";

/**
 * Runtime lifecycle (serde tag+content mirror of the Rust
 * `runtime_doc::RuntimeLifecycle`). Only `Running` carries `activity`.
 */
export type RuntimeLifecycle =
  | { lifecycle: "NotStarted" }
  | { lifecycle: "AwaitingTrust" }
  | { lifecycle: "Resolving" }
  | { lifecycle: "PreparingEnv" }
  | { lifecycle: "Launching" }
  | { lifecycle: "Connecting" }
  | { lifecycle: "Running"; activity: KernelActivity }
  | { lifecycle: "Error" }
  | { lifecycle: "Shutdown" };
```

Update the `KernelState` interface to dual-shape:

```typescript
export interface KernelState {
  /** Typed lifecycle. Task 13 makes this the preferred read. */
  lifecycle: RuntimeLifecycle;
  /** @deprecated Legacy status string, replaced by `lifecycle`. */
  status: string;
  /** @deprecated Legacy sub-status string, replaced by `lifecycle`. */
  starting_phase: string;
  name: string;
  language: string;
  env_source: string;
  /** Populated when `lifecycle.lifecycle === "Error"`. */
  error_reason?: string;
}
```

Update `DEFAULT_RUNTIME_STATE.kernel`:

```typescript
  kernel: {
    lifecycle: { lifecycle: "NotStarted" },
    status: "not_started",
    starting_phase: "",
    name: "",
    language: "",
    env_source: "",
  },
```

Append the helper at the bottom of the file:

```typescript
/**
 * Legacy status-string view of a lifecycle. Callers migrating off the old
 * `kernel.status` string should prefer pattern-matching on `lifecycle`
 * directly, but this helper exists for one-line compatibility.
 */
export function lifecycleStatusString(lc: RuntimeLifecycle): string {
  switch (lc.lifecycle) {
    case "NotStarted":
      return "not_started";
    case "AwaitingTrust":
      return "awaiting_trust";
    case "Resolving":
    case "PreparingEnv":
    case "Launching":
    case "Connecting":
      return "starting";
    case "Running":
      return lc.activity === "Busy" ? "busy" : "idle";
    case "Error":
      return "error";
    case "Shutdown":
      return "shutdown";
  }
}
```

- [ ] **Step 3: Export the new types from the package root**

In `packages/runtimed/src/index.ts`, extend the existing `// Runtime state` re-export block (around lines 35–49):

```typescript
export {
  type CommDocEntry,
  DEFAULT_RUNTIME_STATE,
  type EnvState,
  type ExecutionState,
  type ExecutionTransition,
  type KernelActivity,
  type KernelState,
  type QueueEntry,
  type QueueState,
  type RuntimeLifecycle,
  type RuntimeState,
  type TrustState,
  diffExecutions,
  getExecutionCountForCell,
  lifecycleStatusString,
} from "./runtime-state";
```

Task 13 imports `RuntimeLifecycle` from the package root (`from "runtimed"`); without this re-export the typecheck fails immediately.

- [ ] **Step 4: Run tests**

```bash
cd packages/runtimed && pnpm test 2>&1 | tail -20
cd packages/runtimed && pnpm run typecheck 2>&1 | tail -10
```

Expected: all tests pass (including the two new type tests); typecheck green.

- [ ] **Step 5: Commit**

```bash
git add packages/runtimed/src/runtime-state.ts packages/runtimed/src/index.ts \
        packages/runtimed/tests/sync-engine.test.ts
git commit -m "feat(runtimed-ts): dual-shape KernelState with RuntimeLifecycle"
```

---

## Task 13: Migrate TS surface in one green commit

**Files:**
- Modify: `packages/runtimed/src/derived-state.ts`
- Modify: `packages/runtimed/tests/sync-engine.test.ts`
- Modify: `apps/notebook/src/lib/kernel-status.ts`
- Modify: `apps/notebook/src/lib/__tests__/kernel-status.test.ts`
- Modify: `apps/notebook/src/hooks/useDaemonKernel.ts`
- Modify: `apps/notebook/src/components/NotebookToolbar.tsx`
- Modify: `apps/notebook/src/components/__tests__/notebook-toolbar.test.tsx`
- Modify: `apps/notebook/src/App.tsx`

Everything below happens in a single commit because deleting `getKernelStatusLabel` + `KERNEL_STATUS_LABELS` must happen the moment the last caller migrates. Smaller commits would leave stale imports.

- [ ] **Step 1: Rewrite `derived-state.ts`**

In `packages/runtimed/src/derived-state.ts`:

- Update `deriveEnvSyncState` to gate on `lifecycle`:
  ```typescript
  export function deriveEnvSyncState(state: RuntimeState): EnvSyncState | null {
    const lc = state.kernel.lifecycle;
    if (lc.lifecycle === "NotStarted" && !state.kernel.env_source) return null;
    if (lc.lifecycle === "Shutdown" || lc.lifecycle === "Error" || lc.lifecycle === "AwaitingTrust") {
      return null;
    }
    return {
      inSync: state.env.in_sync,
      diff: state.env.in_sync
        ? undefined
        : {
            added: state.env.added,
            removed: state.env.removed,
            channelsChanged: state.env.channels_changed,
            denoChanged: state.env.deno_changed,
          },
    };
  }
  ```

- Replace `kernelStatus$`:
  ```typescript
  import { lifecycleStatusString } from "./runtime-state";

  export function kernelStatus$(
    runtimeState$: Observable<RuntimeState>,
    threshold?: number,
  ): Observable<KernelStatus> {
    return runtimeState$.pipe(
      map((s) => lifecycleStatusString(s.kernel.lifecycle)),
      throttleBusyStatus(threshold),
    );
  }
  ```

- [ ] **Step 2: Rewrite `kernel-status.ts`**

Replace `apps/notebook/src/lib/kernel-status.ts` with:

```typescript
/**
 * Kernel lifecycle labels for the toolbar.
 *
 * Takes a `RuntimeLifecycle` and returns a user-facing string.
 */

import type { RuntimeLifecycle } from "runtimed";

export { KERNEL_STATUS, isKernelStatus, type KernelStatus } from "runtimed";

export function getLifecycleLabel(lc: RuntimeLifecycle): string {
  switch (lc.lifecycle) {
    case "NotStarted":
      return "initializing";
    case "AwaitingTrust":
      return "awaiting approval";
    case "Resolving":
      return "resolving environment";
    case "PreparingEnv":
      return "preparing environment";
    case "Launching":
      return "launching kernel";
    case "Connecting":
      return "connecting to kernel";
    case "Running":
      return lc.activity === "Busy" ? "busy" : "idle";
    case "Error":
      return "error";
    case "Shutdown":
      return "shutdown";
  }
}
```

`getKernelStatusLabel` and `KERNEL_STATUS_LABELS` are gone.

- [ ] **Step 3: Rewrite `kernel-status.test.ts`**

Replace `apps/notebook/src/lib/__tests__/kernel-status.test.ts` with:

```typescript
import { describe, expect, it } from "vite-plus/test";
import type { RuntimeLifecycle } from "runtimed";
import { getLifecycleLabel, isKernelStatus, KERNEL_STATUS } from "../kernel-status";

describe("isKernelStatus", () => {
  it.each(Object.values(KERNEL_STATUS))(
    "returns true for valid status '%s'",
    (status) => {
      expect(isKernelStatus(status)).toBe(true);
    },
  );

  it("returns false for unknown strings", () => {
    expect(isKernelStatus("running")).toBe(false);
    expect(isKernelStatus("stopped")).toBe(false);
    expect(isKernelStatus("")).toBe(false);
    expect(isKernelStatus("IDLE")).toBe(false);
    expect(isKernelStatus("Busy")).toBe(false);
  });
});

describe("getLifecycleLabel", () => {
  const cases: Array<[RuntimeLifecycle, string]> = [
    [{ lifecycle: "NotStarted" }, "initializing"],
    [{ lifecycle: "AwaitingTrust" }, "awaiting approval"],
    [{ lifecycle: "Resolving" }, "resolving environment"],
    [{ lifecycle: "PreparingEnv" }, "preparing environment"],
    [{ lifecycle: "Launching" }, "launching kernel"],
    [{ lifecycle: "Connecting" }, "connecting to kernel"],
    [{ lifecycle: "Running", activity: "Idle" }, "idle"],
    [{ lifecycle: "Running", activity: "Busy" }, "busy"],
    [{ lifecycle: "Running", activity: "Unknown" }, "idle"],
    [{ lifecycle: "Error" }, "error"],
    [{ lifecycle: "Shutdown" }, "shutdown"],
  ];
  it.each(cases)("labels %o as '%s'", (lc, expected) => {
    expect(getLifecycleLabel(lc)).toBe(expected);
  });
});

describe("KERNEL_STATUS", () => {
  it("contains exactly seven statuses", () => {
    expect(Object.keys(KERNEL_STATUS)).toHaveLength(7);
  });
});
```

- [ ] **Step 4: Rewrite `useDaemonKernel.ts` throttle + return shape**

In `apps/notebook/src/hooks/useDaemonKernel.ts`, replace lines 95–140 (from `const rawStatus = runtimeState.kernel.status;` through the closing `useEffect`) with:

```typescript
  const lifecycle = runtimeState.kernel.lifecycle;
  const rawStatus: KernelStatus = useMemo(() => {
    switch (lifecycle.lifecycle) {
      case "NotStarted":
        return KERNEL_STATUS.NOT_STARTED;
      case "AwaitingTrust":
        return KERNEL_STATUS.AWAITING_TRUST;
      case "Resolving":
      case "PreparingEnv":
      case "Launching":
      case "Connecting":
        return KERNEL_STATUS.STARTING;
      case "Running":
        return lifecycle.activity === "Busy" ? KERNEL_STATUS.BUSY : KERNEL_STATUS.IDLE;
      case "Error":
        return KERNEL_STATUS.ERROR;
      case "Shutdown":
        return KERNEL_STATUS.SHUTDOWN;
    }
  }, [lifecycle]);

  const [throttledStatus, setThrottledStatus] = useState<KernelStatus>(rawStatus);
  const busyTimerRef = useRef<number | null>(null);
  const prevRawStatusRef = useRef(rawStatus);

  useEffect(() => {
    const prev = prevRawStatusRef.current;
    prevRawStatusRef.current = rawStatus;
    if (rawStatus === prev) return;

    if (rawStatus === KERNEL_STATUS.BUSY) {
      if (busyTimerRef.current === null) {
        busyTimerRef.current = window.setTimeout(() => {
          busyTimerRef.current = null;
          setThrottledStatus(KERNEL_STATUS.BUSY);
        }, 60);
      }
    } else if (rawStatus === KERNEL_STATUS.IDLE) {
      if (busyTimerRef.current !== null) {
        clearTimeout(busyTimerRef.current);
        busyTimerRef.current = null;
      } else {
        setThrottledStatus(rawStatus);
      }
    } else {
      if (busyTimerRef.current !== null) {
        clearTimeout(busyTimerRef.current);
        busyTimerRef.current = null;
      }
      setThrottledStatus(rawStatus);
    }

    return () => {
      if (busyTimerRef.current !== null) {
        clearTimeout(busyTimerRef.current);
        busyTimerRef.current = null;
      }
    };
  }, [rawStatus]);

  const kernelStatus = throttledStatus;
```

**Stale-closure check:** The throttle is driven by `rawStatus` (a primitive string) rather than `lifecycle` (an object reference). `useMemo` recomputes `rawStatus` on every `lifecycle` change, and `distinctUntilChanged`-equivalent behavior is enforced by the `if (rawStatus === prev) return;` guard. On a `Running(Idle) → Shutdown → Running(Idle)` transition, the `rawStatus` sequence is `"idle" → "shutdown" → "idle"`; the effect fires on each change, `busyTimerRef` is cleared when leaving `Running`, and returning to `"idle"` commits immediately. No stale closures because callbacks read through `callbacksRef.current`.

Remove the now-unused `isKernelStatus` import.

Around line 394, replace:
```typescript
    startingPhase: runtimeState.kernel.starting_phase,
```
with:
```typescript
    lifecycle: runtimeState.kernel.lifecycle,
    errorReason: runtimeState.kernel.error_reason,
```

- [ ] **Step 5: Rewrite `NotebookToolbar.tsx`**

- Replace the `startingPhase?: string;` prop (around line 25) with:
  ```typescript
  lifecycle: RuntimeLifecycle;
  errorReason?: string;
  ```
  Add `import type { RuntimeLifecycle } from "runtimed";` near the top.
- Update the destructuring at line 51.
- Replace `getKernelStatusLabel(kernelStatus, startingPhase)` (line 99) with `getLifecycleLabel(lifecycle)` (and change the import from `getKernelStatusLabel` to `getLifecycleLabel`).
- Replace the `startingPhase === "missing_ipykernel"` check (line 378) with:
  ```tsx
  {lifecycle.lifecycle === "Error" && errorReason === "missing_ipykernel" && (
  ```

- [ ] **Step 6: Rewrite toolbar tests**

In `apps/notebook/src/components/__tests__/notebook-toolbar.test.tsx`, every test fixture that currently passes `startingPhase="missing_ipykernel"` now passes:

```tsx
lifecycle={{ lifecycle: "Error" }}
errorReason="missing_ipykernel"
```

(and drops the `startingPhase` prop). If the test fixture only passes `startingPhase` but not a matching `kernelStatus="error"`, add both.

- [ ] **Step 7: Rewrite `App.tsx`**

At lines 312–315 and 1168–1170:

```tsx
    startingPhase,
    ...
          startingPhase={startingPhase}
```

→

```tsx
    lifecycle,
    errorReason,
    ...
          lifecycle={lifecycle}
          errorReason={errorReason}
```

- [ ] **Step 8: Update `sync-engine.test.ts` fixtures**

Lines 106, 601, 645, 1997 — replace:

```typescript
kernel: { status: "idle", starting_phase: "", name: "", language: "", env_source: "" },
```

with:

```typescript
kernel: {
  lifecycle: { lifecycle: "Running", activity: "Idle" },
  status: "idle",
  starting_phase: "",
  name: "",
  language: "",
  env_source: "",
},
```

Line 632 assertion — change:
```typescript
expect(received[0].kernel.status).toBe("busy");
```
to:
```typescript
expect(received[0].kernel.lifecycle).toEqual({ lifecycle: "Running", activity: "Busy" });
```

- [ ] **Step 9: Run the full frontend test suite + typecheck**

Run each command from the repo root (new shell for each — the chain below is NOT a `cd` sequence):

```bash
(cd packages/runtimed && pnpm test) 2>&1 | tail -30
(cd apps/notebook && pnpm run typecheck) 2>&1 | tail -20
(cd apps/notebook && pnpm vitest run) 2>&1 | tail -40
```

Expected: all green.

- [ ] **Step 10: Commit**

```bash
git add packages/runtimed/src/derived-state.ts packages/runtimed/tests/sync-engine.test.ts \
        apps/notebook/src/lib/kernel-status.ts \
        apps/notebook/src/lib/__tests__/kernel-status.test.ts \
        apps/notebook/src/hooks/useDaemonKernel.ts \
        apps/notebook/src/components/NotebookToolbar.tsx \
        apps/notebook/src/components/__tests__/notebook-toolbar.test.tsx \
        apps/notebook/src/App.tsx
git commit -m "refactor(notebook-app): thread RuntimeLifecycle through TS surface"
```

---


## Task 14: Migrate Python metrics scripts + README examples

**Files:**
- Modify: `scripts/metrics/kernel-reliability.py`
- Modify: `scripts/metrics/execution-latency.py`
- Modify: `scripts/metrics/sync-correctness.py`
- Modify: `python/runtimed/README.md`

- [ ] **Step 1: Rewrite each script**

Replace every `notebook.runtime.kernel.status` read with a helper that maps `lifecycle` + `activity` back to the legacy status string. Add this helper once at the top of each script:

```python
def _kernel_status(rs):
    lc = rs.kernel.lifecycle
    if lc == "Running":
        return "busy" if rs.kernel.activity == "Busy" else "idle"
    if lc in ("Resolving", "PreparingEnv", "Launching", "Connecting"):
        return "starting"
    return {
        "NotStarted": "not_started",
        "AwaitingTrust": "awaiting_trust",
        "Error": "error",
        "Shutdown": "shutdown",
    }.get(lc, lc.lower())
```

Substitute `notebook.runtime.kernel.status` with `_kernel_status(notebook.runtime)` at the five call sites in `kernel-reliability.py` (lines 71, 73, 82, 118, 122) and the two in `execution-latency.py` (lines 43, 45, 54) and the two in `sync-correctness.py` (lines 190, 194).

- [ ] **Step 2: Update the README example**

`python/runtimed/README.md` around line 97 currently shows:

```python
rs = notebook.runtime
if rs.kernel.status == "idle":
    ...
```

Replace with the typed form:

```python
rs = notebook.runtime
if rs.kernel.lifecycle == "Running" and rs.kernel.activity == "Idle":
    ...
```

Grep the whole README for any other `kernel.status` references and update them consistently.

- [ ] **Step 3: Syntax-check**

```bash
python3 -m py_compile scripts/metrics/kernel-reliability.py scripts/metrics/execution-latency.py scripts/metrics/sync-correctness.py
```

Expected: no output.

- [ ] **Step 4: Commit**

```bash
git add scripts/metrics/kernel-reliability.py scripts/metrics/execution-latency.py scripts/metrics/sync-correctness.py python/runtimed/README.md
git commit -m "chore(metrics): read lifecycle+activity instead of kernel.status"
```

---

## Task 15: Retire the legacy shape atomically

**Files:**
- Modify: `crates/runtime-doc/src/doc.rs`
- Modify: `crates/runtime-doc/src/handle.rs`
- Modify: `crates/runtimed-py/src/output.rs`
- Modify: `packages/runtimed/src/runtime-state.ts`
- Modify: `packages/runtimed/tests/sync-engine.test.ts`

Every reader has migrated by now (Rust via Tasks 4–10, Python bindings via Task 11, TS via Tasks 12–13, Python metrics via Task 14). This task deletes the old setters, fields, scaffold keys, dual-shape mirror writes (both CRDT-side in Rust and attribute-side in `PyKernelState`), the legacy `status` + `starting_phase` on the TS `KernelState` interface, and the `read_state` fallback — all in one atomic commit.

- [ ] **Step 1: Verify no remaining callers**

Run these greps from the repo root. Each should return only the intentional wire/presence hits listed below. Any unexpected hit must be migrated before proceeding.

```bash
rg -n 'set_kernel_status|set_starting_phase' crates/
rg -n 'kernel\.status|kernel\.starting_phase' crates/ \
   --glob '!runtime-doc/src/doc.rs' --glob '!runtime-doc/src/handle.rs' \
   --glob '!runtimed-py/src/output.rs'
rg -n 'kernel\.status|kernel\.starting_phase' python/ scripts/
rg -n 'kernel\.status|kernel\.starting_phase' packages/ apps/ --glob '*.ts' --glob '*.tsx'
```

Expected hits (everything else should be empty):

- First command: only the `pub fn set_kernel_status` / `pub fn set_starting_phase` definitions in `crates/runtime-doc/src/doc.rs` and the legacy-mirror `doc.put(&kernel, "status", …)` / `… "starting_phase", …` calls inside `set_lifecycle` / `set_activity` / `legacy_shape_for`. All deleted in Steps 4–5 below. Plus the legacy puts in `RuntimeStateDoc::new()` / `new_with_actor()` scaffolds, deleted in Step 8.
- Second command: only `crates/runt/src/main.rs:5182` (the wire field on `NotebookResponse::KernelInfo::status`, unchanged) and `crates/notebook-doc/src/presence.rs` (legacy wire presence status, unchanged).
- Third command: only `python/runtimed/tests/…` test fixtures asserting on the deprecated `status` attribute. If Task 14 migrated those, empty; otherwise migrate them as part of this task.
- Fourth command: only TS `KernelState` field definitions and `DEFAULT_RUNTIME_STATE` defaults in `packages/runtimed/src/runtime-state.ts`, plus their two uses in `sync-engine.test.ts` fixtures (lines 106, 601, 645, 1997). All deleted in Step 7.

If anything outside this list shows up, migrate it first (following the Task 5 pattern for Rust, Task 13 for TS, Task 14 for Python) before continuing.

- [ ] **Step 2: Migrate `handle.rs` tests**

In `crates/runtime-doc/src/handle.rs`, replace every `sd.set_kernel_status(...)` / `sd.set_starting_phase(...)` / `kernel.status` reference with the new API. Use `?` in tests — add `-> Result<(), crate::RuntimeStateError>` to `fn` signatures or switch to `.expect()`-free patterns. Exact rewrites:

- Line 124 (`handle.with_doc(|sd| sd.set_kernel_status("busy")).unwrap();`) → use the new writer and return a `Result`:
  ```rust
  #[test]
  fn with_doc_notifies_on_change() -> Result<(), crate::RuntimeStateError> {
      let handle = make_handle();
      let mut rx = handle.subscribe();
      handle.with_doc(|sd| sd.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Busy)))?;
      assert!(rx.try_recv().is_ok());
      Ok(())
  }
  ```

- Line 131–134 (two-call idempotence test): same pattern — `sd.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Busy))` twice.

- Lines 143–146 (`sd.set_kernel_status("busy")?; sd.set_starting_phase("resolving")?;` inside a closure): collapse to one call:
  ```rust
  handle.with_doc(|sd| sd.set_lifecycle(&RuntimeLifecycle::Resolving))?;
  ```

- Line 157 (`fork.set_kernel_status("idle").unwrap();`): `fork.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))?;`.

- Lines 162–170 (`read_does_not_notify`): the `read` closure currently reads `.kernel.status` — change it to `.kernel.lifecycle`, and update the `assert_eq!` to compare against `RuntimeLifecycle::Running(KernelActivity::Busy)` instead of the string `"busy"`.

Add `use crate::{KernelActivity, RuntimeLifecycle};` at the top of `handle.rs` if it isn't already in scope.

- [ ] **Step 3: Remove old setters**

In `crates/runtime-doc/src/doc.rs`, delete `pub fn set_kernel_status` and `pub fn set_starting_phase` (locate with `rg -n "set_kernel_status|set_starting_phase" crates/runtime-doc/src/doc.rs`).

- [ ] **Step 4: Drop the dual-shape legacy-mirror writes from `set_lifecycle` / `set_activity`**

In `set_lifecycle`, delete the block that writes `status` + `starting_phase`:

```rust
        // Dual-shape: maintain legacy kernel.status + kernel.starting_phase
        // for readers not yet migrated. Removed in plan Task 15.
        let (legacy_status, legacy_phase) = legacy_shape_for(lifecycle);
        self.doc.put(&kernel, "status", legacy_status)?;
        self.doc.put(&kernel, "starting_phase", legacy_phase)?;
```

In `set_activity`, delete the legacy-status mirror:

```rust
        // Dual-shape: mirror into legacy kernel.status. Removed in Task 15.
        let legacy_status = match activity { ... };
        self.doc.put(&kernel, "status", legacy_status)?;
```

Delete the `fn legacy_shape_for(...)` helper (no longer reachable).

Delete the `set_lifecycle_maintains_legacy_shape` test — it's pinning a behavior we just removed.

- [ ] **Step 5: Drop the legacy fields from `KernelState`**

Replace the `KernelState` struct with the lean version:

```rust
/// Kernel state snapshot.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelState {
    #[serde(default)]
    pub lifecycle: RuntimeLifecycle,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub env_source: String,
    #[serde(default)]
    pub runtime_agent_id: String,
    #[serde(default)]
    pub error_reason: Option<String>,
}
```

Delete the old `impl Default for KernelState` block (the derive now provides it — all fields are `Default`).

- [ ] **Step 6: Simplify `read_state`**

Replace the kernel-state projection with:

```rust
        let kernel_state = kernel
            .as_ref()
            .map(|k| {
                let lifecycle_str = self.read_str(k, "lifecycle");
                let activity_str = self.read_str(k, "activity");
                let lifecycle = RuntimeLifecycle::parse(&lifecycle_str, &activity_str)
                    .unwrap_or_default();
                let error_reason_raw = self.read_str(k, "error_reason");
                KernelState {
                    lifecycle,
                    name: self.read_str(k, "name"),
                    language: self.read_str(k, "language"),
                    env_source: self.read_str(k, "env_source"),
                    runtime_agent_id: self.read_str(k, "runtime_agent_id"),
                    error_reason: Some(error_reason_raw),
                }
            })
            .unwrap_or_default();
```

Delete `fn legacy_status_to_lifecycle(...)` — it's no longer reachable.

- [ ] **Step 7: Drop `PyKernelState.status` and its mirror writes**

In `crates/runtimed-py/src/output.rs`:

- Remove the `pub status: String` field from `PyKernelState` (the one marked DEPRECATED in Task 11).
- In the `From<runtime_doc::RuntimeState>` conversion, delete the `status: crate::session_core::lifecycle_status_string(...)` line.
- If any pytest fixture in `python/runtimed/tests/` still references `kernel.status`, update it to `kernel.lifecycle` / `kernel.activity` (grep: `rg -n "\.kernel\.status" python/runtimed/tests/`).

- [ ] **Step 8: Drop legacy fields from the TS `KernelState` interface**

In `packages/runtimed/src/runtime-state.ts`:

- Remove `status: string;` and `starting_phase: string;` from the `KernelState` interface.
- Remove `status: "not_started",` and `starting_phase: "",` from `DEFAULT_RUNTIME_STATE.kernel`.

In `packages/runtimed/tests/sync-engine.test.ts`, the fixtures rewritten in Task 13 still carry `status: "idle", starting_phase: ""`. Remove those two keys from each fixture (lines 106, 601, 645, 1997 before the Task 13 edits; re-grep after to confirm `rg -n "starting_phase|status:" packages/runtimed/tests/`).

- [ ] **Step 9: Drop the legacy CRDT scaffold keys**

In both `new()` and `new_with_actor()`, delete the two lines that scaffold `kernel.status` and `kernel.starting_phase`:

```rust
doc.put(&kernel, "status", "not_started").expect("…");
...
doc.put(&kernel, "starting_phase", "").expect("…");
```

- [ ] **Step 10: Update the schema doc comment**

Remove the two `DEPRECATED` lines for `status` and `starting_phase` added in Task 2.

- [ ] **Step 11: Run the full workspace + frontend + Python tests**

Each command runs from the repo root. Rebuild the Python bindings into the test venv before running pytest so the native extension matches the rebuilt `PyKernelState`.

```bash
cargo xtask lint
cargo check --workspace
cargo test --workspace 2>&1 | tail -50
(cd packages/runtimed && pnpm test) 2>&1 | tail -20
(cd apps/notebook && pnpm run typecheck) 2>&1 | tail -20
(cd apps/notebook && pnpm vitest run) 2>&1 | tail -30

(cd crates/runtimed-py && VIRTUAL_ENV=../../.venv \
  uv run --directory ../../python/runtimed maturin develop) 2>&1 | tail -10
(cd crates/runtimed-py && VIRTUAL_ENV=../../python/runtimed/.venv \
  uv run --directory ../../python/runtimed maturin develop) 2>&1 | tail -10
python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/ -v 2>&1 | tail -30
```

Expected: all green.

- [ ] **Step 12: Commit**

```bash
git add crates/runtime-doc/src/doc.rs crates/runtime-doc/src/handle.rs \
        crates/runtimed-py/src/output.rs \
        packages/runtimed/src/runtime-state.ts \
        packages/runtimed/tests/sync-engine.test.ts
git commit -m "refactor(runtime-doc): retire legacy kernel.status+starting_phase shape"
```

---

## Task 16: Verification sweep + restart-path smoke

**Files:** None — verification only.

- [ ] **Step 1: Exhaustive grep**

```bash
rg -n 'set_kernel_status|set_starting_phase' --glob '*.rs'
rg -n 'kernel\.status|kernel\.starting_phase' --glob '*.rs' --glob '*.ts' --glob '*.tsx' --glob '*.py'
```

Expected:
- First command: empty.
- Second command: only intentional hits in `crates/notebook-doc/src/presence.rs` (legacy wire presence — unchanged), `crates/runt/src/main.rs:5182` (wire field on `NotebookResponse::KernelInfo`), and anywhere `lifecycle_to_status_string` reconstructs it. Each remaining hit must be deliberate.

- [ ] **Step 2: Full workspace build + test**

```bash
cargo xtask lint
cargo check --workspace
cargo test --workspace 2>&1 | tail -50
```

Expected: all green.

- [ ] **Step 3: Frontend typecheck + tests**

Run each command from the repo root (new shell for each):

```bash
(cd packages/runtimed && pnpm test) 2>&1 | tail -20
(cd apps/notebook && pnpm run typecheck) 2>&1 | tail -20
(cd apps/notebook && pnpm vitest run) 2>&1 | tail -30
```

Expected: all green.

- [ ] **Step 4: Cold-launch smoke via `nteract-dev`**

If `nteract-dev` is available: `up rebuild=true`, `connect_notebook` on `fixtures/pep723.ipynb`, `execute_cell` on the first cell.

Expected state sequence (observe via `status` / a Python REPL reading `notebook.runtime.kernel.lifecycle`):

```
NotStarted → Resolving → PreparingEnv → Launching → Connecting → Running(Idle)
then on cell execute: Running(Idle) → Running(Busy) → Running(Idle)
```

Toolbar label at each stage must match `getLifecycleLabel`.

- [ ] **Step 5: Restart-path smoke — THE motivating regression**

This is the scenario that currently leaves the UI stuck on "Shutdown". After the plan is applied, the daemon writes relevant lifecycle transitions through:

- `launch_kernel.rs` `RestartKernel` arm: on `KernelRestarted` response, writes `set_lifecycle(Running(KernelActivity::Idle))` (after Task 6).
- `jupyter_kernel.rs` IOPub: if the Jupyter kernel emits `ExecutionState::Restarting`, writes `set_lifecycle(Connecting)`; if it emits `Dead`, writes `set_lifecycle(Shutdown)` (after Task 4).

Whether the `Connecting`/`Shutdown` intermediate states appear depends on the Jupyter kernel implementation — ipykernel generally does emit `restarting` during a restart. What the plan guarantees is that **the daemon always writes `Running(Idle)` on a successful `KernelRestarted` response**. The "stuck on Shutdown" regression is expected to clear because the final `Running(Idle)` write is guaranteed.

Steps:

1. With the kernel in `Running(Idle)` from Step 4, restart the kernel via MCP:
   ```
   mcp__nteract-dev__restart_kernel
   ```
   (`runt` has no `restart` subcommand — `cargo run -p runt-cli -- help` in the dev daemon only exposes `open`, `daemon`, `ps`, `stop`, `status`, `doctor`, `logs`, `diagnostics`, `mcp`, `config`, `env`. Restart flows via the `LaunchKernel` RPC handled by `launch_kernel.rs`, which the MCP tool invokes.)

2. Poll `notebook.runtime.kernel.lifecycle` throughout the restart (Python bindings, or read the CRDT directly via `./target/debug/runt daemon status --json`). Record what you observe. The **required** final state is `Running(KernelActivity::Idle)`. Intermediate states may include:
   - `Shutdown` (if kernel IOPub emits `Dead`).
   - `Connecting` (if kernel IOPub emits `Restarting`).
   - `PreparingEnv` / `Launching` (if the restart falls through to a fresh spawn — i.e., the `KernelRestarted` RPC returns an error and the code falls through to the subprocess spawn path at `launch_kernel.rs:1155`).
   - No intermediate at all (if the restart is fast enough that the polling loop misses it).

3. **Regression verification:** the lifecycle must NOT remain stuck on `Shutdown` after the restart settles. If `lifecycle == Shutdown` persists for more than 5 seconds, the regression is still present — investigate the RestartKernel response handler in `launch_kernel.rs:1099–1118` (did the RPC return `KernelRestarted` or fall through to error/fresh-spawn?).

4. After the restart settles at `Running(Idle)`, execute a cell. Expected: `Running(Idle) → Running(Busy) → Running(Idle)`. Toolbar label should display "idle" → "busy" → "idle" (or "connecting to kernel" if observed during step 2, per `getLifecycleLabel`).

Record the observed sequence in the PR description.

- [ ] **Step 6: Push + open PR**

```bash
git push -u origin refactor/kernel-lifecycle-enum
gh pr create --title "refactor: RuntimeLifecycle enum replaces kernel.status+starting_phase" \
  --body "$(cat <<'EOF'
## Summary

- Introduces `RuntimeLifecycle` + `KernelActivity` enums in `runtime-doc`, with `Running(KernelActivity)` making "busy kernel before launch" unrepresentable.
- Replaces `KernelState.status` + `KernelState.starting_phase` strings with `KernelState.lifecycle` across Rust, TypeScript, and Python.
- Coordinated schema change across the app, daemon, and bindings — ships together because the desktop app bundles everything.
- Migration ran dual-shape so every intermediate commit is bisectable; the "retire legacy…" commit atomically removed the old fields after every caller migrated.

## Test plan
- [x] `cargo test --workspace` green.
- [x] `packages/runtimed` + `apps/notebook` `pnpm test` + `pnpm run typecheck` green.
- [x] Python unit tests green.
- [x] Cold-launch smoke: lifecycle traverses `NotStarted → Resolving → PreparingEnv → Launching → Connecting → Running(Idle)`.
- [x] **Restart smoke (the motivating regression): after `mcp__nteract-dev__restart_kernel`, the lifecycle settles back at `Running(Idle)` — it no longer sticks on `Shutdown`. Observed intermediate sequence: `<record actual sequence here>`.**
EOF
)"
```

---

## Self-review

- **Spec coverage:** every bullet in the spec maps to a task — enums (1), CRDT scaffold (2), struct + dual-shape writers + throttle (3), IOPub branching (4), Rust caller migration including the wait-loop and reset_starting_state asserts (5–10), Python bindings dual-shape with `status` retained (11), TS types + barrel exports (12), consolidated TS migration (13), Python metrics + README examples (14), atomic retire covering Rust + PyO3 + TS in one commit (15), verification + restart smoke (16).
- **Commit boundaries honored.** Tasks 2 and 3 populate both shapes (CRDT keys + struct fields). The new writers in Task 3 also maintain the legacy `status` + `starting_phase` keys so readers not yet migrated keep observing correct state. Tasks 4–10 migrate Rust callers one crate at a time; Task 11 adds `lifecycle`/`activity`/`error_reason` to the Python bindings while keeping `status` dual-shape; Tasks 12–13 split the TS migration into "add types + export from barrel" (green) and "migrate every caller in one commit" (green); Task 14 migrates Python metrics + repo examples. Task 15 atomically retires every piece of the legacy shape after a repo-wide grep (including `runtime-doc/**`) confirms zero callers remain. No task knowingly produces a red intermediate.
- **Restart path:** Task 16 Step 5 documents the actual state transitions the plan's writes guarantee (`Running(Idle)` settle after successful `KernelRestarted`), notes that intermediate `Shutdown`/`Connecting` depend on Jupyter kernel IOPub behavior, and pins the regression check to "lifecycle must not remain stuck on `Shutdown`."
- **Missed Rust callers folded in:** `metadata.rs:673` (reader) → Task 5; `launch_kernel.rs:85-99` (starting-in-progress wait loop) → Task 6; `runtime_agent.rs:1181`/`:1200` (test asserts) → Task 7; `tests.rs:3540/3564/3588` (reset_starting_state asserts) → Task 5; `handle.rs` tests migrated in Task 15 alongside the atomic retire.
- **Missed frontend test:** `apps/notebook/src/lib/__tests__/kernel-status.test.ts` → Task 13.
- **TS barrel export:** Task 12 updates `packages/runtimed/src/index.ts` to re-export `RuntimeLifecycle` + `KernelActivity` + `lifecycleStatusString` so Task 13's imports from `"runtimed"` resolve.
- **error_reason semantics:** `set_lifecycle` does NOT touch `error_reason`. Only `set_lifecycle_with_error(lc, Some("reason"))` sets it; `set_lifecycle_with_error(lc, None)` clears it. Both the populate and preserve-on-reentry tests are in Task 3 Step 1.
- **Dual-shape writer invariant:** `set_lifecycle` and `set_activity` mirror every write into the legacy `kernel.status` / `kernel.starting_phase` keys during Tasks 3–14. Task 15 removes those mirror writes along with the keys. Pinned by a dedicated test in Task 3 Step 5.
- **No `.unwrap()` in test snippets:** plan test snippets use `-> Result<(), crate::RuntimeStateError>` with `?` (or `.expect("msg")` for non-`Result` boundaries), matching the project's `clippy::unwrap_used` policy.
- **`with_doc` discipline:** every daemon snippet in Tasks 4–11 routes mutations through `room.state.with_doc(|sd| ...)` — never direct `doc.set_*` calls — so subscribers receive change notifications.
- **Chained `cd` typo fixed:** Tasks 13 and 16 use `(cd <path> && ...)` subshells instead of a running `cd` chain, so each command executes from the repo root.
- **TS throttle safety:** `useDaemonKernel` drives the throttle off a primitive-string `rawStatus` derived by `useMemo`, not directly off the lifecycle object. `Running(Idle) → Shutdown → Running(Idle)` produces the `"idle" → "shutdown" → "idle"` sequence; the existing `if (rawStatus === prev) return;` guard + `busyTimerRef` cleanup handle it.
