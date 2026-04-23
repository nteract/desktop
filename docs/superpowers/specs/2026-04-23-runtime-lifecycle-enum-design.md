# RuntimeLifecycle Enum

## Summary

Replace the string-based `kernel.status` + `kernel.starting_phase` fields in RuntimeStateDoc with a single `RuntimeLifecycle` enum. `Running` carries a `KernelActivity` payload, making it impossible to represent a busy kernel when the runtime hasn't launched yet.

## Problem

`KernelState` in RuntimeStateDoc uses two string fields:
- `status`: "not_started", "starting", "idle", "busy", "error", "shutdown", "awaiting_trust"
- `starting_phase`: "", "resolving", "preparing_env", "launching", "connecting"

"idle" and "busy" are overloaded onto `status` alongside lifecycle states. The frontend stitches them together in `getKernelStatusLabel`. The Python bindings match on string literals. Nobody gets compile-time exhaustiveness checks.

`starting_phase` is only meaningful when `status == "starting"`. Nothing prevents setting `starting_phase = "launching"` while `status == "idle"`.

## Design

### RuntimeLifecycle enum

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "lifecycle", content = "activity")]
pub enum RuntimeLifecycle {
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
```

### KernelActivity enum

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum KernelActivity {
    Unknown,
    Idle,
    Busy,
}
```

`Unknown` is the initial state when the runtime agent has connected but the kernel hasn't reported its first status yet. Also used for non-Jupyter backends that might not have the idle/busy concept.

### Why Running holds KernelActivity

Only a running runtime has a kernel. A `Resolving` runtime is installing packages, not running code. Encoding this in the type means:

- You can't set `Busy` while `Resolving`. It won't compile.
- Pattern matching is exhaustive. Add a new lifecycle state, the compiler tells you every place that needs updating.
- The idle/busy throttle only applies inside `Running`. Lifecycle transitions are never throttled.

### KernelState struct changes

```rust
pub struct KernelState {
    pub lifecycle: RuntimeLifecycle,
    pub name: String,
    pub language: String,
    pub env_source: String,
    pub runtime_agent_id: String,
    pub error_reason: Option<String>,
}
```

`status` and `starting_phase` are gone. `lifecycle` replaces both. `error_reason` is populated when `lifecycle == Error` (kept separate so the enum stays `Eq`-able).

### CRDT storage

RuntimeStateDoc writes Automerge keys manually (not through serde). `lifecycle` and `activity` are two separate string keys in the `kernel` map:

```
kernel/
  lifecycle: "Running"     (or "PreparingEnv", "Error", etc.)
  activity: "Idle"         (or "Busy", "Unknown", "" when not Running)
  error_reason: ""         (populated when lifecycle == "Error")
  name: "charming-toucan"
  ...
```

`read_state()` reconstructs `RuntimeLifecycle` from these two keys:
- If `lifecycle == "Running"`, parse `activity` into `KernelActivity` and return `Running(activity)`
- Otherwise, parse `lifecycle` into the variant directly, ignore `activity`

`set_lifecycle()` writes `lifecycle` and clears `activity` to `""` when leaving `Running`. `set_activity()` writes only `activity` (hot path for IOPub idle/busy).

### RuntimeStateDoc API changes

```rust
impl RuntimeStateDoc {
    pub fn set_lifecycle(&mut self, lifecycle: &RuntimeLifecycle) -> Result<(), RuntimeStateError> {
        let kernel = self.scaffold_map("kernel")?;
        // Write lifecycle variant as string
        self.doc.put(&kernel, "lifecycle", lifecycle.variant_str())?;
        // Write or clear activity
        match lifecycle {
            RuntimeLifecycle::Running(activity) => {
                self.doc.put(&kernel, "activity", activity.as_str())?;
            }
            _ => {
                self.doc.put(&kernel, "activity", "")?;
            }
        }
        Ok(())
    }
}
```

The old `set_kernel_status` and `set_starting_phase` are removed. All callers switch to `set_lifecycle`.

### Idle/busy throttle

Today, the IOPub handler suppresses redundant `set_kernel_status("busy")` / `set_kernel_status("idle")` writes. With the enum:

```rust
// Only write if activity actually changed
pub fn set_activity(&mut self, activity: KernelActivity) -> Result<(), RuntimeStateError> {
    let kernel = self.scaffold_map("kernel")?;
    let current = self.read_str(&kernel, "activity");
    if current == activity.as_str() {
        return Ok(());
    }
    self.doc.put(&kernel, "activity", activity.as_str())?;
    Ok(())
}
```

`set_activity` only writes the `activity` field, not the full lifecycle. This is the hot path (every IOPub status message). `set_lifecycle` is for lifecycle transitions (infrequent).

### IOPub status handler

The IOPub handler maps Jupyter `ExecutionState` to both lifecycle transitions and activity changes. Today this is one `set_kernel_status` call. With the enum it branches:

```rust
match status.execution_state {
    ExecutionState::Busy => set_activity(KernelActivity::Busy),
    ExecutionState::Idle => set_activity(KernelActivity::Idle),
    ExecutionState::Starting | ExecutionState::Restarting => set_lifecycle(RuntimeLifecycle::Connecting),
    ExecutionState::Terminating | ExecutionState::Dead => set_lifecycle(RuntimeLifecycle::Shutdown),
}
```

`Busy`/`Idle` are activity changes (hot path, throttled). `Starting`/`Restarting`/`Dead`/`Terminating` are lifecycle transitions (infrequent, not throttled).

### Running(Unknown) in practice

Current launch paths eagerly write `Running(Idle)` on successful kernel_info handshake. `Running(Unknown)` will not appear in the Jupyter backend. It exists for future non-Jupyter backends that may not report idle/busy, and as a brief transient state if a backend connects before reporting its first status.

### Caller migration (complete list)

All current `set_kernel_status` call sites, mapped to the new API:

| File | Current | New |
|------|---------|-----|
| `jupyter_kernel.rs` | `set_kernel_status("busy"/"idle")` | `set_activity(Busy/Idle)` |
| `jupyter_kernel.rs` | `set_kernel_status("starting"/"shutdown")` | `set_lifecycle(Connecting/Shutdown)` |
| `runtime_agent.rs` | `set_kernel_status("error")` | `set_lifecycle(Error)` |
| `peer.rs` | `set_kernel_status("starting"/"error"/"awaiting_trust")` | `set_lifecycle(Connecting/Error/AwaitingTrust)` |
| `metadata.rs` | `set_kernel_status("not_started"/"error"/"idle")` | `set_lifecycle(NotStarted/Error/Running(Idle))` |
| `launch_kernel.rs` | `set_kernel_status("starting")` + `set_starting_phase(...)` | `set_lifecycle(Resolving/PreparingEnv/etc.)` |
| `launch_kernel.rs` | `set_kernel_status("idle")` | `set_lifecycle(Running(Idle))` |
| `shutdown_kernel.rs` | `set_kernel_status("shutdown")` | `set_lifecycle(Shutdown)` |

### Frontend changes

TypeScript types mirror the Rust enum:

```typescript
type RuntimeLifecycle =
  | { lifecycle: "NotStarted" }
  | { lifecycle: "AwaitingTrust" }
  | { lifecycle: "Resolving" }
  | { lifecycle: "PreparingEnv" }
  | { lifecycle: "Launching" }
  | { lifecycle: "Connecting" }
  | { lifecycle: "Running"; activity: KernelActivity }
  | { lifecycle: "Error" }
  | { lifecycle: "Shutdown" };

type KernelActivity = "Unknown" | "Idle" | "Busy";
```

`getKernelStatusLabel` simplifies to a single switch on `lifecycle`:

```typescript
function getLifecycleLabel(lc: RuntimeLifecycle): string {
  switch (lc.lifecycle) {
    case "NotStarted": return "initializing";
    case "AwaitingTrust": return "awaiting approval";
    case "Resolving": return "resolving environment";
    case "PreparingEnv": return "preparing environment";
    case "Launching": return "launching kernel";
    case "Connecting": return "connecting to kernel";
    case "Running": return lc.activity === "Busy" ? "busy" : "idle";
    case "Error": return "error";
    case "Shutdown": return "shutdown";
  }
}
```

No more `KERNEL_STATUS` constants or `STARTING_PHASE_LABELS` lookup table.

### Python changes

The `runtimed-py` bindings read `RuntimeState.kernel.lifecycle` instead of `kernel.status`:

```python
rs = notebook.runtime
if rs.kernel.lifecycle == "Running":
    print(f"Kernel is {rs.kernel.activity}")
```

`wait_for_ready` becomes: wait for `lifecycle == "Running"` and `activity == "Idle"`.

### Migration path

1. Add `RuntimeLifecycle` and `KernelActivity` enums to `runtime-doc`
2. Add `set_lifecycle` and `set_activity` methods to RuntimeStateDoc
3. Migrate daemon callers from `set_kernel_status`/`set_starting_phase` to `set_lifecycle`/`set_activity`
4. Update `read_state` snapshot to populate `lifecycle` from the CRDT fields
5. Update frontend TypeScript types and `getKernelStatusLabel`
6. Update Python bindings
7. Remove old `status` and `starting_phase` fields from KernelState snapshot type

The CRDT field names change from `status`/`starting_phase` to `lifecycle`/`activity`. The scaffold in `RuntimeStateDoc::new()` updates accordingly. Since RuntimeStateDoc is ephemeral (recreated on daemon restart), there's no migration concern for existing documents.

### Backward compatibility

No on-disk migration needed. RuntimeStateDoc is ephemeral (in-memory, recreated per room). Clients start empty and receive state via CRDT sync.

The app bundles daemon + frontend + WASM together, so there's no version skew within a release. The MCP server (`runt mcp`) reads `RuntimeState` and ships in the same release.

Live consumers that read `kernel.status` directly (packages/runtimed TypeScript, runtimed-py Python, runt-mcp Rust) all need updating in the same release. This is a coordinated schema change across Rust, TypeScript, and Python - but since the app ships as one artifact, it's safe.

## Testing

- Unit tests for `RuntimeLifecycle` serde round-trip (tag + content format)
- Unit tests for `set_lifecycle` / `set_activity` in RuntimeStateDoc
- Verify `set_activity` is a no-op when value unchanged (throttle behavior)
- Verify `read_state` correctly populates the `lifecycle` field from CRDT
- Frontend: verify `getLifecycleLabel` covers all variants

## Future

- **Runtime that isn't Jupyter**: `KernelActivity` works for any REPL-like backend. `Unknown` covers backends that don't report idle/busy.
- **Richer error states**: `Error` could eventually carry structured error info (missing package, launch timeout, crash) beyond a string reason.
- **wait_for_ready cleanup** (#74): becomes trivial - poll for `Running(Idle)` or subscribe to lifecycle changes.
- **Actor-pattern runtime fields** (#70): the actor manages `RuntimeLifecycle` transitions as its core state machine.
