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

Automerge doesn't have native enums. Serde's `#[serde(tag = "lifecycle", content = "activity")]` produces:

```json
{ "lifecycle": "Running", "activity": "Idle", "name": "charming-toucan", ... }
{ "lifecycle": "PreparingEnv", "name": "", ... }
{ "lifecycle": "Error", "error_reason": "missing_ipykernel", ... }
```

The `activity` key is only present when `lifecycle == "Running"`. Other variants produce no `activity` field.

The setter method writes both `lifecycle` and `activity` (if applicable) in a single `with_doc` call. The old `set_kernel_status` + `set_starting_phase` two-call pattern is replaced by a single `set_lifecycle` call.

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

RuntimeStateDoc is ephemeral and daemon-authoritative. No on-disk migration needed. The frontend and Python bindings ship with the same release as the daemon, so there's no version skew. The only compatibility concern is the nightly MCP server, which reads RuntimeState - it updates in the same release.

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
