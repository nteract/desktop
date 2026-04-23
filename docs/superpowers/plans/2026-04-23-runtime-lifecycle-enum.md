# RuntimeLifecycle Enum Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace string-based `kernel.status` + `kernel.starting_phase` in `RuntimeStateDoc` with a single typed `RuntimeLifecycle` enum whose `Running(KernelActivity)` variant makes it impossible to represent a busy kernel when the runtime hasn't launched yet. Deliver a coordinated Rust + TypeScript + Python schema change in a single release.

**Architecture:** Introduce `RuntimeLifecycle` and `KernelActivity` enums in `crates/runtime-doc`, with `set_lifecycle`/`set_activity` writers on `RuntimeStateDoc` and CRDT storage using separate `kernel/lifecycle` + `kernel/activity` string keys. Migrate every `set_kernel_status` / `set_starting_phase` call site to the new API, swap `KernelState.status` + `starting_phase` for `KernelState.lifecycle`, and update read-side consumers (TypeScript, Python, runt-mcp, runtimed-node, runt CLI) in lockstep. The schema change ships as one PR because the app bundles daemon + frontend + WASM; there is no on-disk migration because RuntimeStateDoc is ephemeral.

**Tech Stack:** Rust (serde, Automerge via `runtime-doc`), TypeScript (RxJS, React), Python (PyO3). No wire or Automerge schema version bump required — `RuntimeStateDoc` is ephemeral and recreated per room on daemon restart.

**Spec:** `docs/superpowers/specs/2026-04-23-runtime-lifecycle-enum-design.md`

---

## File Structure

| File | Role in this refactor |
|------|-----------------------|
| `crates/runtime-doc/src/types.rs` | New: `RuntimeLifecycle` and `KernelActivity` enums, `variant_str`, `as_str`, `parse` helpers, serde round-trip tests |
| `crates/runtime-doc/src/lib.rs` | Re-export the new enums |
| `crates/runtime-doc/src/doc.rs` | Schema doc-comment, scaffold `kernel/lifecycle` + `kernel/activity`, new `set_lifecycle` + `set_activity` writers, updated `read_state`, new `KernelState` shape (`lifecycle` + `error_reason`), retire `set_kernel_status` / `set_starting_phase`, update every in-crate test |
| `crates/runtime-doc/src/handle.rs` | Update handle unit tests to call the new writers |
| `crates/notebook-sync/src/tests.rs` | Replace `set_kernel_status("error")` in sync tests |
| `crates/notebook-sync/src/execution_wait.rs` | Replace `state.kernel.status == "error"/"shutdown"` reads with pattern matches on `state.kernel.lifecycle` |
| `crates/runtimed/src/jupyter_kernel.rs` | IOPub status handler: map `ExecutionState::Busy/Idle` to `set_activity`, `Starting/Restarting/Dead/Terminating` to `set_lifecycle` |
| `crates/runtimed/src/runtime_agent.rs` | `set_kernel_status("error")` → `set_lifecycle(RuntimeLifecycle::Error)` on kernel death |
| `crates/runtimed/src/kernel_state.rs` | Stale doc comment referring to `set_kernel_status("error")` |
| `crates/runtimed/src/notebook_sync_server/peer.rs` | Auto-launch + trust-blocked + auto-launch-panic paths switch to `set_lifecycle` |
| `crates/runtimed/src/notebook_sync_server/metadata.rs` | `set_kernel_status("not_started")`, missing-ipykernel error, `preparing_env`/`launching`/`connecting` phases, post-launch `Running(Idle)` |
| `crates/runtimed/src/notebook_sync_server/tests.rs` | Daemon tests calling `set_kernel_status("idle"/"starting")` |
| `crates/runtimed/src/notebook_sync_server/room.rs` | `state.kernel.status != "not_started"` read |
| `crates/runtimed/src/requests/launch_kernel.rs` | Atomic claim, phase transitions, post-launch Running(Idle) writes |
| `crates/runtimed/src/requests/shutdown_kernel.rs` | `set_kernel_status("shutdown")` → `set_lifecycle(Shutdown)` |
| `crates/runtimed/src/requests/get_kernel_info.rs` | Map `lifecycle` back to a status string for the wire response |
| `crates/runtimed/src/requests/execute_cell.rs` | Rewrite `status == "shutdown"/"error"` precondition |
| `crates/runtimed/src/requests/run_all_cells.rs` | Same precondition rewrite |
| `crates/runt-mcp/src/tools/kernel.rs` | Rewrite the kernel-ready wait loop to inspect `lifecycle` + `activity` |
| `crates/runt-mcp/src/tools/session.rs` | `serde_json::json!(state.kernel.status)` → render `lifecycle`/`activity` strings |
| `crates/runtimed-py/src/output.rs` | `PyKernelState` grows `lifecycle` + `activity` + `error_reason`, drops `status` |
| `crates/runtimed-py/src/session_core.rs` | Rewrite the 5 `rs.kernel.status` reads + the `hydrate_kernel_state` running check |
| `crates/runtimed-node/src/session.rs` | `r.kernel.status == "ready"/"busy"/"idle"` check switches to `lifecycle`-based |
| `crates/runt/src/main.rs` | Display the new status string in `kernels` command output |
| `packages/runtimed/src/runtime-state.ts` | New TS types mirroring the Rust enum, update `DEFAULT_RUNTIME_STATE`, expose a `getLifecycleStatus()` helper used by legacy consumers |
| `packages/runtimed/src/derived-state.ts` | `KERNEL_STATUS` + `deriveEnvSyncState` + `kernelStatus$` rewritten in terms of `lifecycle` (+ optional `activity`) |
| `packages/runtimed/tests/sync-engine.test.ts` | Test fixtures updated to the new shape |
| `apps/notebook/src/lib/kernel-status.ts` | `getLifecycleLabel(lc)` replaces `getKernelStatusLabel(status, phase)` |
| `apps/notebook/src/hooks/useDaemonKernel.ts` | Drive the busy-throttle off `lifecycle`; stop threading `starting_phase` |
| `apps/notebook/src/components/NotebookToolbar.tsx` | Replace `startingPhase` prop with a `lifecycle` prop, rewrite `missing_ipykernel` banner check |
| `apps/notebook/src/components/__tests__/notebook-toolbar.test.tsx` | Toolbar test fixtures follow the new prop shape |
| `apps/notebook/src/App.tsx` | Thread `lifecycle` to the toolbar instead of `startingPhase` |
| `scripts/metrics/kernel-reliability.py`, `scripts/metrics/execution-latency.py`, `scripts/metrics/sync-correctness.py` | Update the Python metrics scripts’ `kernel.status` reads |

---

## Migration order (why tasks are in this sequence)

The workspace must compile and tests must pass after every task. The order is:

1. **Task 1–3:** Add enums + new writers on `RuntimeStateDoc`. Keep the old `set_kernel_status` / `set_starting_phase` / `KernelState.status` / `KernelState.starting_phase` in place so the rest of the workspace still builds. Internal tests exercise both shapes.
2. **Task 4:** Switch `KernelState` snapshot to hold `RuntimeLifecycle` directly. Update the in-crate tests and `read_state`. This breaks every external reader — but only for one commit cycle; the following tasks fix all of them.
3. **Task 5–11:** Migrate Rust callers crate-by-crate.
4. **Task 12:** Delete `set_kernel_status` / `set_starting_phase`.
5. **Task 13–17:** TypeScript surface (`packages/runtimed`, `apps/notebook`).
6. **Task 18–20:** Python bindings + metrics scripts.
7. **Task 21:** Verification sweep + integration tests.

Each task ends with a commit.

---

## Task 1: Add `KernelActivity` and `RuntimeLifecycle` enums

**Files:**
- Modify: `crates/runtime-doc/src/types.rs`
- Modify: `crates/runtime-doc/src/lib.rs`
- Test: `crates/runtime-doc/src/types.rs` (inline `#[cfg(test)] mod tests`)

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
        // Empty activity on a Running CRDT read is legal during scaffold → Running transitions;
        // treat it as Unknown so `read_state` is total.
        assert_eq!(
            RuntimeLifecycle::parse("Running", ""),
            Some(RuntimeLifecycle::Running(KernelActivity::Unknown)),
        );
    }

    #[test]
    fn lifecycle_serde_tag_content_round_trip() {
        let running = RuntimeLifecycle::Running(KernelActivity::Busy);
        let json = serde_json::to_string(&running).unwrap();
        assert_eq!(json, r#"{"lifecycle":"Running","activity":"Busy"}"#);
        let back: RuntimeLifecycle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, running);

        let idle = RuntimeLifecycle::NotStarted;
        let json = serde_json::to_string(&idle).unwrap();
        assert_eq!(json, r#"{"lifecycle":"NotStarted"}"#);
        let back: RuntimeLifecycle = serde_json::from_str(&json).unwrap();
        assert_eq!(back, idle);
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
    /// An empty or missing activity on a `Running` read is treated as
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

- [ ] **Step 4: Re-export from the crate root**

In `crates/runtime-doc/src/lib.rs`, `pub use types::*;` already re-exports everything in the module, so nothing to change. Verify with:

```bash
cargo test -p runtime-doc --lib types::tests 2>&1 | tail -30
```

Expected: all types::tests tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/runtime-doc/src/types.rs
git commit -m "feat(runtime-doc): add RuntimeLifecycle and KernelActivity enums"
```

---

## Task 2: Scaffold `kernel/lifecycle` + `kernel/activity` in RuntimeStateDoc

**Files:**
- Modify: `crates/runtime-doc/src/doc.rs` (schema doc comment + both `new()` + `new_with_actor()`)

Both constructors currently scaffold `kernel/status` and `kernel/starting_phase`. We'll scaffold the two new keys alongside them so readers of either layout see defined values. The old keys stay for now; Task 4 retires them.

- [ ] **Step 1: Update the schema comment at the top of doc.rs**

In `crates/runtime-doc/src/doc.rs`, lines 10–16, replace:

```text
//!   kernel/
//!     status: Str          ("idle" | "busy" | "starting" | "error" | "shutdown" | "not_started")
//!     starting_phase: Str  ("" | "resolving" | "preparing_env" | "launching" | "connecting")
//!     name: Str            (e.g. "charming-toucan")
//!     language: Str        (e.g. "python", "typescript")
//!     env_source: Str      (e.g. "uv:prewarmed", "pixi:toml", "deno")
```

with:

```text
//!   kernel/
//!     lifecycle: Str       ("NotStarted" | "AwaitingTrust" | "Resolving" | "PreparingEnv"
//!                           | "Launching" | "Connecting" | "Running" | "Error" | "Shutdown")
//!     activity: Str        ("" | "Unknown" | "Idle" | "Busy") — only meaningful when lifecycle == "Running"
//!     error_reason: Str    ("" unless lifecycle == "Error")
//!     name: Str            (e.g. "charming-toucan")
//!     language: Str        (e.g. "python", "typescript")
//!     env_source: Str      (e.g. "uv:prewarmed", "pixi:toml", "deno")
```

- [ ] **Step 2: Scaffold the new keys in `new()`**

In `crates/runtime-doc/src/doc.rs`, inside `pub fn new()`, find the block that scaffolds `kernel/status` + `kernel/starting_phase` (approximately lines 261–274). Replace it with:

```rust
        // kernel/
        let kernel = doc
            .put_object(&ROOT, "kernel", ObjType::Map)
            .expect("scaffold kernel");
        doc.put(&kernel, "lifecycle", "NotStarted")
            .expect("scaffold kernel.lifecycle");
        doc.put(&kernel, "activity", "")
            .expect("scaffold kernel.activity");
        doc.put(&kernel, "error_reason", "")
            .expect("scaffold kernel.error_reason");
        doc.put(&kernel, "name", "").expect("scaffold kernel.name");
        doc.put(&kernel, "language", "")
            .expect("scaffold kernel.language");
        doc.put(&kernel, "env_source", "")
            .expect("scaffold kernel.env_source");
        doc.put(&kernel, "runtime_agent_id", "")
            .expect("scaffold kernel.runtime_agent_id");
```

The `status` + `starting_phase` keys are gone. The `read_state` helper will be updated in Task 4 to produce a valid `KernelState` from the new keys.

- [ ] **Step 3: Scaffold the new keys in `new_with_actor()`**

Find the matching block in `pub fn new_with_actor()` (approximately lines 345–358) and apply the identical replacement — same keys, same values, same order.

- [ ] **Step 4: Run the crate tests (they will still fail until later tasks)**

```bash
cargo test -p runtime-doc 2>&1 | tail -20
```

Expected: the types::tests pass, but any existing test that reads `kernel.status` will fail. That is expected — we fix them in Task 4. The build itself must still succeed.

- [ ] **Step 5: Commit**

```bash
git add crates/runtime-doc/src/doc.rs
git commit -m "refactor(runtime-doc): scaffold kernel/lifecycle+activity alongside legacy keys"
```

---

## Task 3: Add `set_lifecycle` and `set_activity` writers

**Files:**
- Modify: `crates/runtime-doc/src/doc.rs`

- [ ] **Step 1: Write the failing tests**

Append near the existing `test_set_kernel_status` block in `crates/runtime-doc/src/doc.rs` (around line 2362), inside the same `#[cfg(test)] mod tests` block:

```rust
    #[test]
    fn set_lifecycle_writes_variant_and_clears_activity() {
        use crate::{KernelActivity, RuntimeLifecycle};

        let mut doc = RuntimeStateDoc::new();

        doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Busy))
            .unwrap();
        let state = doc.read_state();
        assert_eq!(
            state.kernel.lifecycle,
            RuntimeLifecycle::Running(KernelActivity::Busy)
        );

        doc.set_lifecycle(&RuntimeLifecycle::Shutdown).unwrap();
        let state = doc.read_state();
        assert_eq!(state.kernel.lifecycle, RuntimeLifecycle::Shutdown);
        // Leaving Running clears activity so a future Running(Idle) write is
        // not conflated with stale Busy.
        let kernel = doc.doc.get(&automerge::ROOT, "kernel").unwrap().unwrap().1;
        let (activity, _) = doc.doc.get(&kernel, "activity").unwrap().unwrap();
        match activity {
            automerge::Value::Scalar(s) => match s.as_ref() {
                automerge::ScalarValue::Str(s) => assert_eq!(s.as_str(), ""),
                _ => panic!("activity should be a string scalar"),
            },
            _ => panic!("activity should be a scalar"),
        }
    }

    #[test]
    fn set_activity_is_noop_when_unchanged() {
        use crate::{KernelActivity, RuntimeLifecycle};

        let mut doc = RuntimeStateDoc::new();
        doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))
            .unwrap();
        let heads_before = doc.get_heads();
        doc.set_activity(KernelActivity::Idle).unwrap();
        let heads_after = doc.get_heads();
        assert_eq!(
            heads_before, heads_after,
            "set_activity should not write when value is unchanged"
        );

        doc.set_activity(KernelActivity::Busy).unwrap();
        assert_ne!(
            heads_after,
            doc.get_heads(),
            "set_activity should write when value changes"
        );
        assert_eq!(
            doc.read_state().kernel.lifecycle,
            RuntimeLifecycle::Running(KernelActivity::Busy)
        );
    }

    #[test]
    fn set_lifecycle_populates_error_reason_for_error() {
        use crate::RuntimeLifecycle;

        let mut doc = RuntimeStateDoc::new();
        doc.set_lifecycle_with_error(
            &RuntimeLifecycle::Error,
            Some("missing_ipykernel"),
        )
        .unwrap();
        let state = doc.read_state();
        assert_eq!(state.kernel.lifecycle, RuntimeLifecycle::Error);
        assert_eq!(
            state.kernel.error_reason.as_deref(),
            Some("missing_ipykernel")
        );

        doc.set_lifecycle(&RuntimeLifecycle::NotStarted).unwrap();
        let state = doc.read_state();
        assert_eq!(state.kernel.lifecycle, RuntimeLifecycle::NotStarted);
        assert_eq!(state.kernel.error_reason.as_deref(), Some(""));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cargo test -p runtime-doc --lib set_lifecycle_writes_variant set_activity_is_noop set_lifecycle_populates_error 2>&1 | tail -20
```

Expected: fails with "no method named `set_lifecycle` / `set_activity` / `set_lifecycle_with_error` found".

- [ ] **Step 3: Implement the writers**

Leave `set_kernel_status` and `set_starting_phase` in place for now. Insert the new writers immediately above the `// ── Execution lifecycle ─────────────────────────────────────────` section in `crates/runtime-doc/src/doc.rs` (around line 875):

```rust
    // ── Lifecycle writers ───────────────────────────────────────────

    /// Write a runtime lifecycle transition.
    ///
    /// When the new lifecycle is `Running(activity)`, both the `lifecycle`
    /// variant and the `activity` key are written. When the new lifecycle is
    /// anything else, `activity` is cleared to `""`. `error_reason` is always
    /// cleared; use [`set_lifecycle_with_error`] to set it.
    pub fn set_lifecycle(
        &mut self,
        lifecycle: &RuntimeLifecycle,
    ) -> Result<(), RuntimeStateError> {
        self.set_lifecycle_with_error(lifecycle, None)
    }

    /// Write a runtime lifecycle transition with an optional error reason.
    ///
    /// Only meaningful when `lifecycle == RuntimeLifecycle::Error`. The
    /// `error_reason` is stored verbatim in `kernel/error_reason`.
    pub fn set_lifecycle_with_error(
        &mut self,
        lifecycle: &RuntimeLifecycle,
        error_reason: Option<&str>,
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
        let reason = error_reason.unwrap_or("");
        self.doc.put(&kernel, "error_reason", reason)?;
        Ok(())
    }

    /// Update just the kernel activity. Only meaningful when the lifecycle is
    /// already `Running`; callers are expected to ensure that invariant. This
    /// is the hot path for IOPub idle/busy status and is a no-op when the
    /// value has not changed.
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
        Ok(())
    }
```

You'll need `use crate::{KernelActivity, RuntimeLifecycle};` at the top of `doc.rs` (add it to the existing `use crate::StreamOutputState;` line → `use crate::{KernelActivity, RuntimeLifecycle, StreamOutputState};`).

Note: `read_state` still reads the legacy `kernel.status` field — these three tests will fail until Task 4 updates `read_state` and `KernelState`. Skip the verification step for now and commit; Task 4 ties it together.

- [ ] **Step 4: Compile-check**

```bash
cargo check -p runtime-doc 2>&1 | tail -10
```

Expected: compiles cleanly. Tests for the new writers still fail — that's fine.

- [ ] **Step 5: Commit**

```bash
git add crates/runtime-doc/src/doc.rs
git commit -m "feat(runtime-doc): add set_lifecycle and set_activity writers"
```

---

## Task 4: Swap `KernelState` to hold `RuntimeLifecycle`, update `read_state` + in-crate tests

**Files:**
- Modify: `crates/runtime-doc/src/doc.rs`
- Modify: `crates/runtime-doc/src/handle.rs`

This is the pivot task. After this, the `runtime-doc` crate is fully on the new shape; the rest of the workspace will not compile until subsequent tasks migrate callers.

- [ ] **Step 1: Replace the `KernelState` struct**

Replace the existing `KernelState` struct + its `Default` impl (lines 74–103 of `doc.rs`) with:

```rust
/// Kernel state snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelState {
    /// Current runtime lifecycle (replaces the old `status` + `starting_phase`
    /// string pair). `Running(KernelActivity)` is the only variant that
    /// carries activity — see the `RuntimeLifecycle` docs.
    #[serde(default)]
    pub lifecycle: RuntimeLifecycle,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub env_source: String,
    /// ID of the runtime agent subprocess that owns this kernel (e.g.,
    /// "runtime-agent:a1b2c3d4"). Used for provenance — identifying which
    /// runtime agent is running and detecting stale ones.
    #[serde(default)]
    pub runtime_agent_id: String,
    /// Human-readable reason populated when `lifecycle == Error`. Empty
    /// otherwise.
    #[serde(default)]
    pub error_reason: Option<String>,
}

impl Default for KernelState {
    fn default() -> Self {
        Self {
            lifecycle: RuntimeLifecycle::NotStarted,
            name: String::new(),
            language: String::new(),
            env_source: String::new(),
            runtime_agent_id: String::new(),
            error_reason: None,
        }
    }
}
```

Ensure `use crate::{KernelActivity, RuntimeLifecycle, StreamOutputState};` is at the top of the file (added in Task 3).

- [ ] **Step 2: Update `read_state` to reconstruct the lifecycle**

In `crates/runtime-doc/src/doc.rs`, locate the `read_state` method (around line 1849). Replace the `kernel_state = kernel.as_ref().map(...)` block (lines 1855–1865) with:

```rust
        let kernel_state = kernel
            .as_ref()
            .map(|k| {
                let lifecycle_str = self.read_str(k, "lifecycle");
                let activity_str = self.read_str(k, "activity");
                let lifecycle = RuntimeLifecycle::parse(&lifecycle_str, &activity_str)
                    .unwrap_or_default();
                let error_reason_raw = self.read_str(k, "error_reason");
                let error_reason = if error_reason_raw.is_empty() {
                    Some(String::new())
                } else {
                    Some(error_reason_raw)
                };
                KernelState {
                    lifecycle,
                    name: self.read_str(k, "name"),
                    language: self.read_str(k, "language"),
                    env_source: self.read_str(k, "env_source"),
                    runtime_agent_id: self.read_str(k, "runtime_agent_id"),
                    error_reason,
                }
            })
            .unwrap_or_default();
```

The slightly awkward `error_reason` handling (always `Some`, sometimes empty string) matches the contract the rest of the codebase expects: a `Some("")` when the CRDT field exists and is empty vs `None` when the kernel map hasn't been scaffolded.

- [ ] **Step 3: Update every remaining in-crate test that reads or writes the old fields**

All of these live in `crates/runtime-doc/src/doc.rs` (the `#[cfg(test)] mod tests` block) and `crates/runtime-doc/src/handle.rs`.

Apply the following transformations. This list is exhaustive — after this step, no `set_kernel_status` / `set_starting_phase` / `kernel.status` / `kernel.starting_phase` reference should remain in those two files.

In `crates/runtime-doc/src/doc.rs`:

- `test_set_kernel_status` (line 2362): replace `doc.set_kernel_status("busy")` with `doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Busy))`; replace `doc.set_kernel_status("idle")` with `doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))`; update assertions from `kernel.status` to `kernel.lifecycle`. Rename the test to `test_lifecycle_round_trip`.
- `test_set_starting_phase` (line 2458): this test exists to prove the `starting_phase` clear-on-transition rule. Replace its body entirely:

  ```rust
  #[test]
  fn test_lifecycle_transitions_clear_activity() {
      let mut doc = RuntimeStateDoc::new();

      doc.set_lifecycle(&RuntimeLifecycle::Resolving).unwrap();
      assert_eq!(doc.read_state().kernel.lifecycle, RuntimeLifecycle::Resolving);

      doc.set_lifecycle(&RuntimeLifecycle::Launching).unwrap();
      assert_eq!(doc.read_state().kernel.lifecycle, RuntimeLifecycle::Launching);

      doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))
          .unwrap();
      assert_eq!(
          doc.read_state().kernel.lifecycle,
          RuntimeLifecycle::Running(KernelActivity::Idle)
      );

      doc.set_lifecycle(&RuntimeLifecycle::Error).unwrap();
      // Activity is cleared when leaving Running.
      assert_eq!(doc.read_state().kernel.lifecycle, RuntimeLifecycle::Error);
  }
  ```
- Line 2494–2496 (`doc.set_kernel_status("busy")` twice): the surrounding test asserts idempotence. Replace both calls with `doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Busy))` and adjust the test name (`test_set_kernel_status_idempotent` → `test_set_lifecycle_idempotent`).
- Line 2527 (`daemon_doc.set_kernel_status("busy")`): replace with `daemon_doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Busy))`.
- Line 2666 (`doc.set_kernel_status("idle")`): replace with `doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))`.
- Line 2964 (`doc.set_kernel_status("busy")`): replace with `doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Busy))`.
- Line 2997 (`fork.set_kernel_status("error")`): replace with `fork.set_lifecycle(&RuntimeLifecycle::Error)`.
- Lines 4271, 4279 (`doc.set_kernel_status("idle")`): replace with `doc.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))`.

In `crates/runtime-doc/src/handle.rs`:

- Line 124: `handle.with_doc(|sd| sd.set_kernel_status("busy"))` → `handle.with_doc(|sd| sd.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Busy)))`.
- Lines 131, 133, 165 (same pattern, three occurrences): same replacement.
- Lines 143–144 (`sd.set_kernel_status("busy")?; sd.set_starting_phase("resolving")?;` inside a closure): replace the two calls with a single `sd.set_lifecycle(&RuntimeLifecycle::Resolving)?;`.
- Line 157 (`fork.set_kernel_status("idle")`): replace with `fork.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))`.

At the top of `handle.rs`, add `use crate::{KernelActivity, RuntimeLifecycle};` if not already in scope.

- [ ] **Step 4: Run `runtime-doc` tests and verify green**

```bash
cargo test -p runtime-doc 2>&1 | tail -40
```

Expected: all `runtime-doc` tests pass.

- [ ] **Step 5: Compile the workspace to see the downstream fallout**

```bash
cargo check --workspace 2>&1 | tail -40
```

Expected: errors in downstream crates referencing `kernel.status` / `kernel.starting_phase` / `set_kernel_status`. That's expected — Tasks 5–11 fix them.

- [ ] **Step 6: Commit**

```bash
git add crates/runtime-doc/src/doc.rs crates/runtime-doc/src/handle.rs
git commit -m "refactor(runtime-doc): swap KernelState.status+phase for RuntimeLifecycle"
```

---

## Task 5: Migrate `runtimed::jupyter_kernel` and `runtimed::runtime_agent`

**Files:**
- Modify: `crates/runtimed/src/jupyter_kernel.rs`
- Modify: `crates/runtimed/src/runtime_agent.rs`
- Modify: `crates/runtimed/src/kernel_state.rs` (comment only)

- [ ] **Step 1: Rewrite the IOPub status handler**

In `crates/runtimed/src/jupyter_kernel.rs`, locate the `JupyterMessageContent::Status` arm (around lines 740–766). Replace it with:

```rust
                                JupyterMessageContent::Status(status) => {
                                    use runtime_doc::{KernelActivity, RuntimeLifecycle};

                                    // Non-execute messages (kernel_info, completions) have a
                                    // parent_header.msg_id that isn't in our execute map.
                                    // `cell_id` is None for those — treat their busy/idle as transient.
                                    let is_transient = cell_id.is_none();

                                    match status.execution_state {
                                        jupyter_protocol::ExecutionState::Busy => {
                                            if !is_transient {
                                                if let Err(e) = state_for_iopub.with_doc(|sd| {
                                                    sd.set_activity(KernelActivity::Busy)
                                                }) {
                                                    warn!("[runtime-state] {}", e);
                                                }
                                            }
                                        }
                                        jupyter_protocol::ExecutionState::Idle => {
                                            if !is_transient {
                                                if let Err(e) = state_for_iopub.with_doc(|sd| {
                                                    sd.set_activity(KernelActivity::Idle)
                                                }) {
                                                    warn!("[runtime-state] {}", e);
                                                }
                                            }
                                        }
                                        jupyter_protocol::ExecutionState::Starting
                                        | jupyter_protocol::ExecutionState::Restarting => {
                                            if let Err(e) = state_for_iopub.with_doc(|sd| {
                                                sd.set_lifecycle(&RuntimeLifecycle::Connecting)
                                            }) {
                                                warn!("[runtime-state] {}", e);
                                            }
                                        }
                                        jupyter_protocol::ExecutionState::Terminating
                                        | jupyter_protocol::ExecutionState::Dead => {
                                            if let Err(e) = state_for_iopub.with_doc(|sd| {
                                                sd.set_lifecycle(&RuntimeLifecycle::Shutdown)
                                            }) {
                                                warn!("[runtime-state] {}", e);
                                            }
                                        }
                                        _ => {}
                                    }
```

Leave the `if status.execution_state == Idle` branch below it unchanged — it queues an `ExecutionDone` command, unrelated to this change.

- [ ] **Step 2: Rewrite the kernel-died write in `runtime_agent.rs`**

In `crates/runtimed/src/runtime_agent.rs`, locate the `set_kernel_status("error")` call inside the kernel-died handler (around line 988). Change the closure from:

```rust
            if let Err(e) = ctx.state.with_doc(|sd| {
                if let Some((_, ref eid)) = interrupted {
                    sd.set_execution_done(eid, false)?;
                }
                for entry in &cleared {
                    sd.set_execution_done(&entry.execution_id, false)?;
                }
                sd.set_kernel_status("error")?;
                sd.set_queue(None, &[])?;
                Ok(())
            }) {
```

to:

```rust
            if let Err(e) = ctx.state.with_doc(|sd| {
                if let Some((_, ref eid)) = interrupted {
                    sd.set_execution_done(eid, false)?;
                }
                for entry in &cleared {
                    sd.set_execution_done(&entry.execution_id, false)?;
                }
                sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Error)?;
                sd.set_queue(None, &[])?;
                Ok(())
            }) {
```

- [ ] **Step 3: Update the stale comment in `kernel_state.rs`**

In `crates/runtimed/src/kernel_state.rs` at line 268, change the comment:

```rust
// state_doc.set_kernel_status("error") + set_queue(None, &[])
```

to:

```rust
// state_doc.set_lifecycle(RuntimeLifecycle::Error) + set_queue(None, &[])
```

- [ ] **Step 4: Compile the crate**

```bash
cargo check -p runtimed 2>&1 | tail -20
```

Expected: fewer errors than before — but still some from `notebook_sync_server/*`, `requests/*`, which Task 6 handles.

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed/src/jupyter_kernel.rs crates/runtimed/src/runtime_agent.rs crates/runtimed/src/kernel_state.rs
git commit -m "refactor(runtimed): migrate IOPub + kernel-died paths to set_lifecycle/activity"
```

---

## Task 6: Migrate `notebook_sync_server::peer` + `metadata`

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server/peer.rs`
- Modify: `crates/runtimed/src/notebook_sync_server/metadata.rs`

- [ ] **Step 1: Rewrite the auto-launch claim in `peer.rs`**

In `crates/runtimed/src/notebook_sync_server/peer.rs`, around lines 457–463, replace:

```rust
            if let Err(e) = room.state.with_doc(|sd| {
                sd.set_kernel_status("starting")?;
                sd.set_starting_phase("resolving")?;
                Ok(())
            }) {
                warn!("[runtime-state] {}", e);
            }
```

with:

```rust
            if let Err(e) = room
                .state
                .with_doc(|sd| sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Resolving))
            {
                warn!("[runtime-state] {}", e);
            }
```

- [ ] **Step 2: Rewrite the auto-launch panic handler**

In the same file, around lines 487–494, replace:

```rust
                        if let Err(e) = r.state.with_doc(|sd| {
                            sd.set_kernel_status("error")?;
                            sd.set_starting_phase("")?;
                            Ok(())
                        }) {
                            tracing::warn!("[runtime-state] {}", e);
                        }
```

with:

```rust
                        if let Err(e) = r.state.with_doc(|sd| {
                            sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Error)
                        }) {
                            tracing::warn!("[runtime-state] {}", e);
                        }
```

- [ ] **Step 3: Rewrite the trust-blocked branch**

In the same file, around lines 509–515, replace:

```rust
            if let Err(e) = room.state.with_doc(|sd| {
                sd.set_kernel_status("awaiting_trust")?;
                sd.set_starting_phase("")?;
                Ok(())
            }) {
                warn!("[runtime-state] {}", e);
            }
```

with:

```rust
            if let Err(e) = room
                .state
                .with_doc(|sd| sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::AwaitingTrust))
            {
                warn!("[runtime-state] {}", e);
            }
```

- [ ] **Step 4: Rewrite the `not_started` write in metadata.rs**

In `crates/runtimed/src/notebook_sync_server/metadata.rs`, around lines 1731–1737, replace:

```rust
    if let Err(e) = room.state.with_doc(|sd| {
        sd.set_kernel_status("not_started")?;
        sd.set_prewarmed_packages(&[])?;
        Ok(())
    }) {
        warn!("[runtime-state] {}", e);
    }
```

with:

```rust
    if let Err(e) = room.state.with_doc(|sd| {
        sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::NotStarted)?;
        sd.set_prewarmed_packages(&[])?;
        Ok(())
    }) {
        warn!("[runtime-state] {}", e);
    }
```

- [ ] **Step 5: Rewrite the missing-ipykernel error**

In `metadata.rs`, around lines 2387–2394, replace:

```rust
                if let Err(e) = room.state.with_doc(|sd| {
                    sd.set_kernel_status("error")?;
                    sd.set_kernel_info("python", "python", env_source.as_str())?;
                    sd.set_starting_phase("missing_ipykernel")?;
                    Ok(())
                }) {
                    warn!("[runtime-state] {}", e);
                }
```

with:

```rust
                if let Err(e) = room.state.with_doc(|sd| {
                    sd.set_lifecycle_with_error(
                        &runtime_doc::RuntimeLifecycle::Error,
                        Some("missing_ipykernel"),
                    )?;
                    sd.set_kernel_info("python", "python", env_source.as_str())?;
                    Ok(())
                }) {
                    warn!("[runtime-state] {}", e);
                }
```

`error_reason = "missing_ipykernel"` preserves the existing contract the frontend uses to detect the pixi-missing-ipykernel case.

- [ ] **Step 6: Rewrite the phase transitions in metadata.rs**

- Around line 2403: `sd.set_starting_phase("preparing_env")` → `sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::PreparingEnv)`. The surrounding `with_doc(|sd| ...)` closure signature stays identical.
- Around line 2706: `sd.set_starting_phase("launching")` → `sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Launching)`.
- Around line 2760: `sd.set_starting_phase("connecting")` → `sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Connecting)`.
- Around line 2821: `sd.set_kernel_status("idle")?` → `sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Running(runtime_doc::KernelActivity::Idle))?`.

- [ ] **Step 7: Update the daemon test fixtures**

In `crates/runtimed/src/notebook_sync_server/tests.rs`:
- Line 3049 (`sd.set_kernel_status("idle")?`) → `sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Running(runtime_doc::KernelActivity::Idle))?`.
- Line 3101 (`with_doc(|sd| sd.set_kernel_status("idle"))`) → `with_doc(|sd| sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Running(runtime_doc::KernelActivity::Idle)))`.
- Lines 3531, 3581 (`with_doc(|sd| sd.set_kernel_status("starting"))`) → `with_doc(|sd| sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Resolving))`. (These fixtures simulate "kernel starting" so `Resolving` is the equivalent initial phase.)

- [ ] **Step 8: Update the `status != "not_started"` read in room.rs**

In `crates/runtimed/src/notebook_sync_server/room.rs`, around lines 388–395, replace:

```rust
                if state.kernel.status != "not_started" && !state.kernel.status.is_empty() {
                    ...
                    status: state.kernel.status.clone(),
                    ...
                }
```

with:

```rust
                if !matches!(state.kernel.lifecycle, runtime_doc::RuntimeLifecycle::NotStarted) {
                    ...
                    status: lifecycle_to_status_string(&state.kernel.lifecycle),
                    ...
                }
```

Add a free-function helper at the bottom of `room.rs` (before any test module):

```rust
/// Render `RuntimeLifecycle` as the legacy status string used by the
/// presence channel and external wire consumers (runt-mcp, runtimed-node,
/// metrics scripts). Kept simple and total — `Running` collapses to either
/// "idle" or "busy" depending on activity.
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

We keep this helper because presence uses the legacy status strings on the wire (see `crates/notebook-doc/src/presence.rs`) — changing presence is out of scope for this refactor.

- [ ] **Step 9: Compile**

```bash
cargo check -p runtimed 2>&1 | tail -20
```

Expected: remaining errors only in `requests/*.rs` (handled by Task 7).

- [ ] **Step 10: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server/peer.rs \
        crates/runtimed/src/notebook_sync_server/metadata.rs \
        crates/runtimed/src/notebook_sync_server/tests.rs \
        crates/runtimed/src/notebook_sync_server/room.rs
git commit -m "refactor(runtimed): migrate notebook_sync_server to set_lifecycle"
```

---

## Task 7: Migrate `runtimed::requests`

**Files:**
- Modify: `crates/runtimed/src/requests/launch_kernel.rs`
- Modify: `crates/runtimed/src/requests/shutdown_kernel.rs`
- Modify: `crates/runtimed/src/requests/execute_cell.rs`
- Modify: `crates/runtimed/src/requests/run_all_cells.rs`
- Modify: `crates/runtimed/src/requests/get_kernel_info.rs`

- [ ] **Step 1: Rewrite the atomic claim in `launch_kernel.rs`**

In `crates/runtimed/src/requests/launch_kernel.rs` (around lines 55–75), replace:

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

Below, replace the `"busy"` / `"idle"` / other match arms with the equivalents. Near the end of the `match`, replace the catch-all that returns `NotStarted` behavior to `_ => { /* continue launching */ }`. Here is the full `match` skeleton you should land on:

```rust
    match prior_lifecycle {
        RuntimeLifecycle::Running(KernelActivity::Idle | KernelActivity::Busy) => {
            // Agent already has a running kernel — check for restart path below
        }
        _ => {
            // NotStarted / Error / Shutdown / AwaitingTrust / Resolving/… — proceed with launch.
        }
    }
```

Keep the downstream early-return paths as-is unless they rely on the string — we'll audit them in the next step.

- [ ] **Step 2: Rewrite the in-flight phase transitions in `launch_kernel.rs`**

- Around line 465: `sd.set_starting_phase("preparing_env")` → `sd.set_lifecycle(&RuntimeLifecycle::PreparingEnv)`.
- Around line 1080: `sd.set_starting_phase("launching")` → `sd.set_lifecycle(&RuntimeLifecycle::Launching)`.
- Around line 1111 (inside the `KernelRestarted` arm): `sd.set_kernel_status("idle")?` → `sd.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))?`.
- Around line 1197: `with_doc(|sd| sd.set_starting_phase("connecting"))` → `with_doc(|sd| sd.set_lifecycle(&RuntimeLifecycle::Connecting))`.
- Around line 1257 (inside the `KernelLaunched` arm): `sd.set_kernel_status("idle")?` → `sd.set_lifecycle(&RuntimeLifecycle::Running(KernelActivity::Idle))?`.

Add `use runtime_doc::{KernelActivity, RuntimeLifecycle};` at the top of `launch_kernel.rs` if it isn't already imported (the snippets above bring it into scope block-locally; top-level `use` is cleaner — prefer that).

- [ ] **Step 3: Rewrite `shutdown_kernel.rs`**

In `crates/runtimed/src/requests/shutdown_kernel.rs`, line 24:

```rust
            sd.set_kernel_status("shutdown")?;
```

→

```rust
            sd.set_lifecycle(&runtime_doc::RuntimeLifecycle::Shutdown)?;
```

- [ ] **Step 4: Rewrite `execute_cell.rs` precondition**

In `crates/runtimed/src/requests/execute_cell.rs`, around lines 58–63, replace:

```rust
                    .read(|sd| sd.read_state().kernel.status.clone())
                    .unwrap_or_default();
                if status == "shutdown" || status == "error" {
```

with:

```rust
                    .read(|sd| sd.read_state().kernel.lifecycle)
                    .unwrap_or(runtime_doc::RuntimeLifecycle::NotStarted);
                if matches!(
                    status,
                    runtime_doc::RuntimeLifecycle::Shutdown | runtime_doc::RuntimeLifecycle::Error
                ) {
```

You may need to rename the local `status` binding to `lifecycle` for readability.

- [ ] **Step 5: Rewrite `run_all_cells.rs` precondition**

In `crates/runtimed/src/requests/run_all_cells.rs`, around lines 16–20, apply the same transformation as step 4.

- [ ] **Step 6: Rewrite `get_kernel_info.rs`**

In `crates/runtimed/src/requests/get_kernel_info.rs`, replace the whole `handle` body with:

```rust
pub(crate) async fn handle(room: &NotebookRoom) -> NotebookResponse {
    use runtime_doc::RuntimeLifecycle;
    // Read from RuntimeStateDoc (source of truth for runtime agent).
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

(The helper was added in Task 6 Step 8. Adjust visibility on `lifecycle_to_status_string` to `pub(crate)` if it wasn't already.)

- [ ] **Step 7: Compile + test the daemon crate**

```bash
cargo check -p runtimed 2>&1 | tail -20
cargo test -p runtimed --lib 2>&1 | tail -40
```

Expected: compiles; any unit tests remaining in the crate pass.

- [ ] **Step 8: Commit**

```bash
git add crates/runtimed/src/requests/launch_kernel.rs \
        crates/runtimed/src/requests/shutdown_kernel.rs \
        crates/runtimed/src/requests/execute_cell.rs \
        crates/runtimed/src/requests/run_all_cells.rs \
        crates/runtimed/src/requests/get_kernel_info.rs
git commit -m "refactor(runtimed): migrate request handlers to RuntimeLifecycle"
```

---

## Task 8: Migrate `notebook-sync` consumers

**Files:**
- Modify: `crates/notebook-sync/src/execution_wait.rs`
- Modify: `crates/notebook-sync/src/tests.rs`

- [ ] **Step 1: Rewrite `execution_wait.rs` reads**

In `crates/notebook-sync/src/execution_wait.rs`, around lines 116–122, replace:

```rust
            if state.kernel.status == "error" {
                ...
            }
            if state.kernel.status == "shutdown" {
```

with pattern-based checks:

```rust
            if matches!(state.kernel.lifecycle, runtime_doc::RuntimeLifecycle::Error) {
                ...
            }
            if matches!(state.kernel.lifecycle, runtime_doc::RuntimeLifecycle::Shutdown) {
```

Update any nearby doc comments that mention `kernel.status == "error"` to refer to `lifecycle`.

- [ ] **Step 2: Rewrite the `notebook-sync` tests**

In `crates/notebook-sync/src/tests.rs`:

- Line 815: `st.state_doc.set_kernel_status("error").unwrap();` → `st.state_doc.set_lifecycle(&runtime_doc::RuntimeLifecycle::Error).unwrap();`
- Line 845: same replacement.

- [ ] **Step 3: Compile + test**

```bash
cargo check -p notebook-sync 2>&1 | tail -20
cargo test -p notebook-sync --lib 2>&1 | tail -20
```

Expected: compiles; sync tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/notebook-sync/src/execution_wait.rs crates/notebook-sync/src/tests.rs
git commit -m "refactor(notebook-sync): consume RuntimeLifecycle instead of status string"
```

---

## Task 9: Migrate `runt-mcp`

**Files:**
- Modify: `crates/runt-mcp/src/tools/kernel.rs`
- Modify: `crates/runt-mcp/src/tools/session.rs`

- [ ] **Step 1: Rewrite the kernel-ready wait loop**

In `crates/runt-mcp/src/tools/kernel.rs` (around lines 175–188), replace:

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

with:

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

- [ ] **Step 2: Rewrite the session-level status emission**

In `crates/runt-mcp/src/tools/session.rs`, around line 106, replace:

```rust
                serde_json::json!(state.kernel.status),
```

with:

```rust
                serde_json::json!(
                    crate::kernel_status::lifecycle_to_status_string(&state.kernel.lifecycle)
                ),
```

Create a tiny helper module at `crates/runt-mcp/src/kernel_status.rs`:

```rust
use runtime_doc::{KernelActivity, RuntimeLifecycle};

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

Wire it up in `crates/runt-mcp/src/lib.rs` (or `crates/runt-mcp/src/main.rs`, whichever already declares the sibling modules):

```rust
mod kernel_status;
```

- [ ] **Step 3: Compile runt-mcp**

```bash
cargo check -p runt-mcp 2>&1 | tail -20
cargo test -p runt-mcp --lib 2>&1 | tail -20
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/runt-mcp/src/tools/kernel.rs crates/runt-mcp/src/tools/session.rs crates/runt-mcp/src/kernel_status.rs crates/runt-mcp/src/lib.rs
git commit -m "refactor(runt-mcp): read RuntimeLifecycle via helper"
```

---

## Task 10: Migrate `runtimed-node` + `runt` CLI

**Files:**
- Modify: `crates/runtimed-node/src/session.rs`
- Modify: `crates/runt/src/main.rs`

- [ ] **Step 1: Rewrite the readiness check in `runtimed-node::session`**

In `crates/runtimed-node/src/session.rs` (around line 343), replace:

```rust
                r.kernel.status == "ready" || r.kernel.status == "busy" || r.kernel.status == "idle"
```

with:

```rust
                matches!(r.kernel.lifecycle, runtime_doc::RuntimeLifecycle::Running(_))
```

There was no `"ready"` variant in the enum — `Running(_)` covers both live states. If the `"ready"` check was reachable in the old shape, map it to `Running(Unknown)` via `matches!`. The rewrite above is a proper equivalent.

- [ ] **Step 2: Rewrite the `runt` CLI kernel-list print**

In `crates/runt/src/main.rs` (around line 5182), replace:

```rust
            kernel.kernel_type, kernel.env_source, kernel.status
```

with:

```rust
            kernel.kernel_type,
            kernel.env_source,
            kernel.status  // Already a string from GetKernelInfo response — no change needed
```

This one is actually fine — `kernel.status` here refers to `NotebookResponse::KernelInfo::status`, which is the legacy status *string* on the wire. No migration needed.

(The grep at line 5182 is a false positive. Verify by running `cargo check -p runt`:

```bash
cargo check -p runt 2>&1 | tail -10
```

If it's clean, move on.)

- [ ] **Step 3: Compile + test**

```bash
cargo check -p runtimed-node -p runt 2>&1 | tail -20
cargo test -p runtimed-node --lib 2>&1 | tail -20
```

Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed-node/src/session.rs
git commit -m "refactor(runtimed-node): readiness check via RuntimeLifecycle::Running"
```

(If `runt/src/main.rs` didn't actually need changes, skip it.)

---

## Task 11: Delete `set_kernel_status` + `set_starting_phase`

**Files:**
- Modify: `crates/runtime-doc/src/doc.rs`

Sanity-check first.

- [ ] **Step 1: Verify no remaining callers**

```bash
rg -n 'set_kernel_status|set_starting_phase' --glob '*.rs'
```

Expected output: only the definitions in `crates/runtime-doc/src/doc.rs`.

If any callers remain, migrate them following the Task 5–7 pattern before proceeding.

- [ ] **Step 2: Delete the methods**

In `crates/runtime-doc/src/doc.rs`, delete lines 747–775 (the `set_kernel_status` and `set_starting_phase` method bodies and their doc comments). The comment block `// ── Granular setters (daemon calls these individually) ──────────` stays.

- [ ] **Step 3: Compile the workspace**

```bash
cargo check --workspace 2>&1 | tail -20
cargo test -p runtime-doc --lib 2>&1 | tail -40
```

Expected: clean build; all runtime-doc tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/runtime-doc/src/doc.rs
git commit -m "refactor(runtime-doc): remove set_kernel_status + set_starting_phase"
```

---

## Task 12: Update TypeScript `runtime-state.ts` types

**Files:**
- Modify: `packages/runtimed/src/runtime-state.ts`

- [ ] **Step 1: Write failing tests against the TS package**

Append to `packages/runtimed/tests/sync-engine.test.ts` (at the end of the file):

```typescript
describe("RuntimeLifecycle TS types", () => {
  it("DEFAULT_RUNTIME_STATE.kernel.lifecycle is NotStarted", () => {
    expect(DEFAULT_RUNTIME_STATE.kernel.lifecycle).toEqual({ lifecycle: "NotStarted" });
  });

  it("a Running lifecycle can carry activity", () => {
    const k: KernelState = {
      lifecycle: { lifecycle: "Running", activity: "Idle" },
      name: "",
      language: "",
      env_source: "",
    };
    expect(k.lifecycle.lifecycle).toBe("Running");
  });
});
```

Add `KernelState` to the existing `import { DEFAULT_RUNTIME_STATE, ... } from "../src/runtime-state";` line.

- [ ] **Step 2: Run tests to verify they fail**

```bash
cd packages/runtimed && pnpm test sync-engine.test.ts 2>&1 | tail -20
```

Expected: type errors around the `lifecycle` field.

- [ ] **Step 3: Rewrite `runtime-state.ts` types + default**

Replace the `KernelState` interface (lines 10–16) + the `DEFAULT_RUNTIME_STATE.kernel` block (lines 90–96) in `packages/runtimed/src/runtime-state.ts`.

New types to add near the top:

```typescript
export type KernelActivity = "Unknown" | "Idle" | "Busy";

/**
 * Runtime lifecycle enum (serde tag+content mirror of the Rust
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

export interface KernelState {
  lifecycle: RuntimeLifecycle;
  name: string;
  language: string;
  env_source: string;
  error_reason?: string;
}
```

Replace the default block:

```typescript
  kernel: {
    lifecycle: { lifecycle: "NotStarted" },
    name: "",
    language: "",
    env_source: "",
  },
```

Append a helper at the bottom of the file:

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

- [ ] **Step 4: Update `packages/runtimed/tests/sync-engine.test.ts` fixtures**

The file has four fixtures that still set `starting_phase` / `status` strings on the kernel (lines 106, 601, 645, 1997). Rewrite each of them:

Before:
```typescript
kernel: { status: "idle", starting_phase: "", name: "", language: "", env_source: "" },
```

After:
```typescript
kernel: {
  lifecycle: { lifecycle: "Running", activity: "Idle" },
  name: "",
  language: "",
  env_source: "",
},
```

The assertion on line 632 (`expect(received[0].kernel.status).toBe("busy")`) becomes:
```typescript
expect(received[0].kernel.lifecycle).toEqual({ lifecycle: "Running", activity: "Busy" });
```

- [ ] **Step 5: Run the TypeScript tests**

```bash
cd packages/runtimed && pnpm test 2>&1 | tail -30
```

Expected: green.

- [ ] **Step 6: Commit**

```bash
git add packages/runtimed/src/runtime-state.ts packages/runtimed/tests/sync-engine.test.ts
git commit -m "feat(runtimed-ts): add RuntimeLifecycle type to runtime-state"
```

---

## Task 13: Update TypeScript `derived-state.ts` + `kernel-status.ts`

**Files:**
- Modify: `packages/runtimed/src/derived-state.ts`
- Modify: `apps/notebook/src/lib/kernel-status.ts`

- [ ] **Step 1: Rewrite `derived-state.ts`**

Replace the entire `// ── Kernel status ───` block and `deriveEnvSyncState` + `kernelStatus$` in `packages/runtimed/src/derived-state.ts` with:

```typescript
import type { RuntimeLifecycle } from "./runtime-state";
import { lifecycleStatusString } from "./runtime-state";

// KERNEL_STATUS is retained as the legacy wire-level enum used by the
// busy-throttle and the toolbar label code. New code should pattern-match
// on `RuntimeLifecycle` directly.
export const KERNEL_STATUS = {
  NOT_STARTED: "not_started",
  STARTING: "starting",
  IDLE: "idle",
  BUSY: "busy",
  ERROR: "error",
  SHUTDOWN: "shutdown",
  AWAITING_TRUST: "awaiting_trust",
} as const;

export type KernelStatus = (typeof KERNEL_STATUS)[keyof typeof KERNEL_STATUS];

const KERNEL_STATUS_SET: ReadonlySet<KernelStatus> = new Set(Object.values(KERNEL_STATUS));

export function isKernelStatus(value: string): value is KernelStatus {
  return KERNEL_STATUS_SET.has(value as KernelStatus);
}
```

Rewrite `deriveEnvSyncState`:

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

Rewrite `kernelStatus$` to use the new shape:

```typescript
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

- [ ] **Step 2: Rewrite `apps/notebook/src/lib/kernel-status.ts`**

Replace the whole file with:

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

The old `getKernelStatusLabel(status, startingPhase)` helper is gone. Tasks 14–15 migrate the two call sites (`NotebookToolbar.tsx` and its test).

- [ ] **Step 3: Run the tests**

```bash
cd packages/runtimed && pnpm test 2>&1 | tail -20
cd ../../apps/notebook && pnpm -w -F @nteract/notebook run typecheck 2>&1 | tail -20
```

Expected: `packages/runtimed` tests pass. Typecheck in the notebook app will report errors in `NotebookToolbar.tsx` (Task 14) and `App.tsx`/`useDaemonKernel.ts` (Task 15); that's expected.

- [ ] **Step 4: Commit**

```bash
git add packages/runtimed/src/derived-state.ts apps/notebook/src/lib/kernel-status.ts
git commit -m "refactor(runtimed-ts): derive UI state from RuntimeLifecycle"
```

---

## Task 14: Migrate `NotebookToolbar` + its tests

**Files:**
- Modify: `apps/notebook/src/components/NotebookToolbar.tsx`
- Modify: `apps/notebook/src/components/__tests__/notebook-toolbar.test.tsx`

- [ ] **Step 1: Rewrite the toolbar props**

In `apps/notebook/src/components/NotebookToolbar.tsx`, replace the `startingPhase?: string;` prop (line 25) with `lifecycle: RuntimeLifecycle;`. Import:

```typescript
import type { RuntimeLifecycle } from "runtimed";
```

Replace `getKernelStatusLabel(kernelStatus, startingPhase)` (line 99) with `getLifecycleLabel(lifecycle)` (and update the import near the top from `getKernelStatusLabel` → `getLifecycleLabel`).

Replace the `startingPhase === "missing_ipykernel"` check (line 378) with a lifecycle-based check. Because `error_reason` flows through the `KernelState` snapshot but not through this component's props directly, add an optional `errorReason?: string;` prop and thread it from `App.tsx` (Task 15 handles the `App.tsx` side). The guard becomes:

```tsx
        {lifecycle.lifecycle === "Error" &&
        errorReason === "missing_ipykernel" && (
```

- [ ] **Step 2: Rewrite the toolbar tests**

In `apps/notebook/src/components/__tests__/notebook-toolbar.test.tsx`, lines 310–368, every place that currently passes `startingPhase="missing_ipykernel"` now needs both `lifecycle` and `errorReason`:

```tsx
lifecycle={{ lifecycle: "Error" }}
errorReason="missing_ipykernel"
```

The `kernelStatus="error"` prop can stay — it drives the `kernelStatus` display separately. If the test fixtures previously relied on `kernelStatus="error"` alone, verify that the `kernelStatusText` assertion still passes under `getLifecycleLabel({ lifecycle: "Error" })` (`"error"`).

- [ ] **Step 3: Run typecheck + unit tests**

```bash
cd apps/notebook && pnpm run typecheck 2>&1 | tail -20
cd apps/notebook && pnpm vitest run components/__tests__/notebook-toolbar.test.tsx 2>&1 | tail -20
```

Expected: errors left in `App.tsx` / `useDaemonKernel.ts` only; toolbar tests pass.

- [ ] **Step 4: Commit**

```bash
git add apps/notebook/src/components/NotebookToolbar.tsx apps/notebook/src/components/__tests__/notebook-toolbar.test.tsx
git commit -m "refactor(notebook-toolbar): consume RuntimeLifecycle directly"
```

---

## Task 15: Migrate `useDaemonKernel` + `App.tsx`

**Files:**
- Modify: `apps/notebook/src/hooks/useDaemonKernel.ts`
- Modify: `apps/notebook/src/App.tsx`

- [ ] **Step 1: Rewrite the busy-throttle in `useDaemonKernel.ts`**

The existing throttle reads `runtimeState.kernel.status` as a string. Rewrite it to project `lifecycle` into a throttle-friendly `KernelStatus`.

In `apps/notebook/src/hooks/useDaemonKernel.ts`, replace lines 95–140 (from `const rawStatus = runtimeState.kernel.status;` through the `useEffect` that closes around line 138) with:

```typescript
  // Derive a string-level status for the busy-throttle. Running(Busy) →
  // "busy", Running(_) → "idle", Connecting/Launching/etc. → "starting".
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

Remove the now-unused `isKernelStatus(rawStatus)` import.

- [ ] **Step 2: Stop returning `startingPhase`; return `lifecycle` instead**

In the same file, around line 394, replace:

```typescript
    startingPhase: runtimeState.kernel.starting_phase,
```

with:

```typescript
    lifecycle: runtimeState.kernel.lifecycle,
    errorReason: runtimeState.kernel.error_reason,
```

- [ ] **Step 3: Rewrite `App.tsx`**

In `apps/notebook/src/App.tsx`, around lines 312–315 and the toolbar render at line 1168–1170, replace:

```tsx
    startingPhase,
    ...
          startingPhase={startingPhase}
```

with:

```tsx
    lifecycle,
    errorReason,
    ...
          lifecycle={lifecycle}
          errorReason={errorReason}
```

- [ ] **Step 4: Run the notebook typecheck + tests**

```bash
cd apps/notebook && pnpm run typecheck 2>&1 | tail -20
cd apps/notebook && pnpm vitest run 2>&1 | tail -30
```

Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add apps/notebook/src/hooks/useDaemonKernel.ts apps/notebook/src/App.tsx
git commit -m "refactor(notebook-app): thread RuntimeLifecycle through hooks + App"
```

---

## Task 16: Migrate Python bindings (`runtimed-py`)

**Files:**
- Modify: `crates/runtimed-py/src/output.rs`
- Modify: `crates/runtimed-py/src/session_core.rs`

- [ ] **Step 1: Update `PyKernelState`**

In `crates/runtimed-py/src/output.rs`, replace the `PyKernelState` struct (lines 836–847) with:

```rust
/// Kernel state from the RuntimeStateDoc.
///
/// `lifecycle` is the typed lifecycle enum variant name
/// (`"NotStarted"`, `"AwaitingTrust"`, `"Resolving"`, `"PreparingEnv"`,
/// `"Launching"`, `"Connecting"`, `"Running"`, `"Error"`, `"Shutdown"`).
///
/// When `lifecycle == "Running"`, `activity` carries the kernel's reported
/// activity (`"Idle"`, `"Busy"`, or `"Unknown"`). Otherwise `activity` is `""`.
#[pyclass(name = "KernelState", get_all, skip_from_py_object)]
#[derive(Clone, Debug)]
pub struct PyKernelState {
    /// Lifecycle variant (e.g. "Running", "Resolving", "Error").
    pub lifecycle: String,
    /// Kernel activity when lifecycle == "Running"; empty string otherwise.
    pub activity: String,
    /// Human-readable reason when lifecycle == "Error". Empty otherwise.
    pub error_reason: String,
    /// Kernel display name (e.g. "charming-toucan").
    pub name: String,
    /// Kernel language (e.g. "python", "typescript").
    pub language: String,
    /// Environment source label (e.g. "uv:prewarmed", "pixi:toml").
    pub env_source: String,
}
```

Update the `__repr__` method (lines 850–856) to include `lifecycle`:

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

Update the `From<runtime_doc::KernelState>` conversion (line 1033 area). Replace the inner `PyKernelState { status: rs.kernel.status, ... }` with:

```rust
            kernel: PyKernelState {
                lifecycle: rs.kernel.lifecycle.variant_str().to_string(),
                activity: match rs.kernel.lifecycle {
                    runtime_doc::RuntimeLifecycle::Running(act) => act.as_str().to_string(),
                    _ => String::new(),
                },
                error_reason: rs.kernel.error_reason.unwrap_or_default(),
                name: rs.kernel.name,
                language: rs.kernel.language,
                env_source: rs.kernel.env_source,
            },
```

Also update `PyRuntimeState::__repr__` (line 1007) — the `self.kernel.status` reference needs to become `self.kernel.lifecycle`.

- [ ] **Step 2: Rewrite the 5 `rs.kernel.status` reads in `session_core.rs`**

Create a small private helper at the top of the file (right after the imports) so the rewrites stay short:

```rust
use runtime_doc::{KernelActivity, RuntimeLifecycle};

fn lifecycle_status_string(lc: &RuntimeLifecycle) -> &'static str {
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

Now apply these five rewrites:

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

- Line 316 (`ensure_create_runtime_ready`):
  ```rust
  .map(|rs| rs.kernel.status)
  .unwrap_or_else(|| "not_started".to_string());
  ```
  →
  ```rust
  .map(|rs| lifecycle_status_string(&rs.kernel.lifecycle).to_string())
  .unwrap_or_else(|| "not_started".to_string());
  ```

- Line 737 (`if rs.kernel.status != "idle" { saw_non_idle = true; }`):
  ```rust
  if !matches!(
      rs.kernel.lifecycle,
      RuntimeLifecycle::Running(KernelActivity::Idle)
  ) {
      saw_non_idle = true;
  } else if saw_non_idle {
      return Ok(progress_messages);
  }
  ```

- Line 1465 (`if rs.kernel.status == "error"`):
  ```rust
  if matches!(rs.kernel.lifecycle, RuntimeLifecycle::Error) {
      kernel_error = Some("Kernel error".to_string());
      done = true;
  } else if matches!(rs.kernel.lifecycle, RuntimeLifecycle::Shutdown) {
      kernel_error = Some("Kernel shut down".to_string());
      done = true;
  }
  ```

- [ ] **Step 3: Rebuild Python bindings and run Python tests**

Follow the project's nteract-dev + maturin workflow. If `up` is available:

```bash
# Rebuild runtimed-py into the workspace venv (.venv)
cargo xtask run-mcp --print-config >/dev/null # sanity, not required
cd crates/runtimed-py && VIRTUAL_ENV=../../.venv uv run --directory ../../python/runtimed maturin develop
```

Then run unit tests:

```bash
python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_session_unit.py -v 2>&1 | tail -30
```

Expected: green.

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed-py/src/output.rs crates/runtimed-py/src/session_core.rs
git commit -m "refactor(runtimed-py): expose lifecycle/activity instead of status string"
```

---

## Task 17: Migrate Python metrics scripts

**Files:**
- Modify: `scripts/metrics/kernel-reliability.py`
- Modify: `scripts/metrics/execution-latency.py`
- Modify: `scripts/metrics/sync-correctness.py`

These are lightweight CLI scripts — they read `notebook.runtime.kernel.*` on a live daemon. Align them with the new Python binding shape.

- [ ] **Step 1: Rewrite `kernel-reliability.py`**

Replace:

```python
while notebook.runtime.kernel.status not in ("idle", "busy"):
    ...
    status = notebook.runtime.kernel.status
```

with:

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

while _kernel_status(notebook.runtime) not in ("idle", "busy"):
    ...
    status = _kernel_status(notebook.runtime)
```

Apply the same helper + call-site substitution to the other two scripts (`execution-latency.py` reads the status identically; `sync-correctness.py` has a slightly different shape — `kernel_ready = _kernel_status(notebook.runtime) in ("idle", "busy")`).

- [ ] **Step 2: Quick smoke test**

Skip — these scripts require a live daemon with running notebooks. Instead, Python syntax-check:

```bash
python3 -m py_compile scripts/metrics/kernel-reliability.py scripts/metrics/execution-latency.py scripts/metrics/sync-correctness.py
```

Expected: no output (files compile).

- [ ] **Step 3: Commit**

```bash
git add scripts/metrics/kernel-reliability.py scripts/metrics/execution-latency.py scripts/metrics/sync-correctness.py
git commit -m "chore(metrics): read lifecycle+activity instead of kernel.status"
```

---

## Task 18: Workspace verification sweep

**Files:** None — verification only.

- [ ] **Step 1: Exhaustive grep**

```bash
rg -n 'set_kernel_status|set_starting_phase' --glob '*.rs'
rg -n 'kernel\.status|kernel\.starting_phase' --glob '*.rs' --glob '*.ts' --glob '*.tsx' --glob '*.py'
```

Expected first command: empty.
Expected second command: only a handful of results you've already reasoned about — specifically `crates/notebook-doc/src/presence.rs` (legacy wire presence status — intentionally unchanged), `crates/runtimed/src/notebook_sync_server/room.rs` (the `lifecycle_to_status_string` helper and its callers), and the `NotebookResponse::KernelInfo::status` wire field (also intentionally unchanged). Verify each remaining hit is intentional.

- [ ] **Step 2: Full workspace build + test**

```bash
cargo xtask lint
cargo check --workspace
cargo test --workspace 2>&1 | tail -50
```

Expected: all green.

- [ ] **Step 3: Frontend typecheck + tests**

```bash
cd packages/runtimed && pnpm test 2>&1 | tail -20
cd ../../apps/notebook && pnpm run typecheck 2>&1 | tail -20
cd apps/notebook && pnpm vitest run 2>&1 | tail -30
```

Expected: all green.

- [ ] **Step 4: End-to-end smoke via nteract-dev (if available)**

Use the `verify-changes` skill, or manually:

1. `up rebuild=true` — rebuilds daemon + runtimed-py into the workspace venv + restarts the MCP child.
2. `connect_notebook` on a small test fixture (e.g., `fixtures/pep723.ipynb`).
3. `execute_cell` to run the first cell.
4. Inspect the notebook's runtime state (`mcp__nteract-dev__status` or a quick Python REPL snippet using the bindings).

Expected behavior:
- During resolve/env-prep/launch, `lifecycle` cycles through `Resolving`/`PreparingEnv`/`Launching`/`Connecting`.
- Once the kernel is up, `lifecycle == "Running"` with `activity == "Idle"`.
- Running a cell flips `activity` to `"Busy"` then back to `"Idle"`.
- The toolbar label matches `getLifecycleLabel` output.

If `nteract-dev` is not available, perform the same sequence by hand against `cargo xtask dev-daemon` + the MCP inspector.

- [ ] **Step 5: Commit any incidental test fixture touch-ups + push**

If the full sweep produced no further changes, nothing to commit.

- [ ] **Step 6: Open the PR**

```bash
git push -u origin refactor/kernel-lifecycle-enum
gh pr create --title "refactor: RuntimeLifecycle enum replaces kernel.status+starting_phase" \
  --body "$(cat <<'EOF'
## Summary

- Introduces `RuntimeLifecycle` + `KernelActivity` enums in `runtime-doc`, with `Running(KernelActivity)` making "busy kernel before launch" unrepresentable.
- Replaces `KernelState.status` + `KernelState.starting_phase` strings with `KernelState.lifecycle` across Rust, TypeScript, and Python.
- Coordinated schema change across the app, daemon, and bindings — ships together because the desktop app bundles everything.

## Test plan
- [ ] `cargo test --workspace` green.
- [ ] `packages/runtimed` + `apps/notebook` `pnpm test` + `pnpm run typecheck` green.
- [ ] Python unit tests green.
- [ ] Manual smoke via `nteract-dev`: resolve → prep → launch → running(idle) → running(busy) → running(idle).
EOF
)"
```

---

## Self-review checklist (applied inline; fix-ups folded in above)

- **Spec coverage:** Each spec bullet maps to a task:
  - `RuntimeLifecycle` / `KernelActivity` enum + tag/content serde → Task 1.
  - CRDT `kernel/lifecycle` + `kernel/activity` + `error_reason` scaffold → Task 2.
  - `set_lifecycle` + `set_activity` + `set_lifecycle_with_error` writers + throttle → Task 3.
  - `KernelState` struct swap + `read_state` reconstruction → Task 4.
  - IOPub status handler branching (Busy/Idle vs Starting/Restarting/Dead/Terminating) → Task 5.
  - Every caller in the migration table → Tasks 5–7.
  - Frontend TS types + `getLifecycleLabel` → Tasks 12–15.
  - Python bindings → Task 16.
  - Removal of `set_kernel_status` / `set_starting_phase` → Task 11.
- **Placeholder scan:** No `TODO` / `fill in` / `handle edge cases` / "similar to Task N" left.
- **Type consistency:** `set_lifecycle` / `set_activity` / `set_lifecycle_with_error` / `RuntimeLifecycle` / `KernelActivity` spellings match across Rust, TS, and Python. `lifecycle_to_status_string` exists in `runtimed::notebook_sync_server::room`, `runt-mcp::kernel_status`, `runtimed-py::session_core`, and `packages/runtimed::runtime-state::lifecycleStatusString` — deliberately duplicated (Rust crate-locality + TS helper) because each consumer has its own call sites and no shared crate exists to hang a single helper off of. If a future task adds a shared "status presentation" crate, consolidate then.
