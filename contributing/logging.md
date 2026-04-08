# Logging Guidelines

This guide covers logging conventions for contributors working on the nteract desktop codebase.

## Rust Logging

### runtimed daemon

The daemon uses `tracing` with `tracing-subscriber` (layered subscribers for stderr + file output). Import tracing macros at the top of your file:

```rust
use tracing::{debug, info, warn, error};
```

Dependencies that use the `log` crate (runtimelib, automerge, rattler, etc.) are automatically bridged into the tracing subscriber via `tracing-log`.

### Tauri app (notebook crate)

The notebook app uses `log` with `tauri-plugin-log`:

```rust
use log::{debug, info, warn, error};
```

### Log Level Guidelines

| Level | Use For | Examples |
|-------|---------|----------|
| `error!` | Failures that affect functionality | Kernel crash, file write failure |
| `warn!` | Recoverable issues that may indicate problems | Trust verification failed, retry exhausted |
| `info!` | Significant user-visible events | Kernel launched, environment created, sync complete |
| `debug!` | Internal details useful for debugging | Pool operations, request handling, state transitions |

### What NOT to Log at Info Level

- Per-operation details (every cell execution, every pool take/return)
- Internal state transitions (metadata resolution, room creation)
- Expected conditions (kernel already running, no peers remaining)
- Large data structures (comm state, JSON payloads)

### Prefixes

Use consistent prefixes for filtering:

**Rust (daemon):**
- `[runtimed]` - Daemon core operations
- `[notebook-sync]` - Automerge sync server
- `[kernel-manager]` - Kernel lifecycle and execution
- `[doc-handle]` - CRDT document mutations and requests
- `[comm_*]` - Widget communication

**TypeScript (frontend):**
- `[automerge-notebook]` - WASM handle, bootstrap, materialization
- `[sync-engine]` - Frame processing, sync state, coalescing
- `[crdt-bridge]` - CodeMirror ↔ CRDT character-level sync
- `[frame-pipeline]` - Changeset materialization, cache behavior
- `[daemon-kernel]` - Kernel execution, broadcasts, comms
- `[flushSync]` - Outbound sync flush

### Default Log Levels by Channel

| Channel | Daemon default | Notebook app default |
|---------|---------------|---------------------|
| **Nightly** | `info` (with `debug` for `notebook_sync` and `notebook_sync_server`) | `Debug` |
| **Stable** | `warn` | `Info` |

Nightly builds are intentionally more verbose to aid debugging in the field. The defaults are channel-aware — no configuration needed.

### Overriding Log Levels

```bash
# All debug logs (overrides channel default)
RUST_LOG=debug cargo xtask dev-daemon

# Specific module
RUST_LOG=runtimed::notebook_sync_server=debug cargo xtask dev-daemon

# Daemon CLI flag (overrides channel default)
runtimed --log-level debug
```

## TypeScript Logging

Use the `logger` utility from `apps/notebook/src/lib/logger.ts` instead of raw `console.*`. The codebase uses relative imports from within the notebook app:

```typescript
import { logger } from "../lib/logger";

logger.debug("[component] Internal detail");
logger.info("[component] Significant event");
logger.warn("[component] Recoverable issue");
logger.error("[component] Failure:", error);
```

### Log Level Behavior

- **Nightly**: All levels (`debug`, `info`, `warn`, `error`) are enabled by default
- **Stable**: `logger.debug()` still sends through the logger, but the Rust-side log filter typically drops it; `info`, `warn`, `error` remain visible
- The level filter is applied server-side by `tauri-plugin-log`, not in JavaScript

### What NOT to Log at Info Level

- Per-cell execution, per-comm message details
- Retry attempts (only log final result)
- Internal state (blob port resolution, queue state)
- Success cases for routine operations (hot-sync succeeded)

### Seeing Frontend Debug Logs

There is no `localStorage` debug toggle in the current app. Frontend logs always
go through `apps/notebook/src/lib/logger.ts`, and in development
(`import.meta.env.DEV`) `attachConsole()` mirrors them into the browser devtools
console. In packaged builds, visibility is controlled by the Rust-side app log
level from `tauri-plugin-log`.

## Adding New Logging

Before adding a log statement, ask:

1. **Who needs this?** If only developers debugging, use `debug!`/`logger.debug()`
2. **How often does it fire?** High-frequency operations should be `debug` level
3. **Does it contain sensitive data?** Truncate or omit large JSON, file paths, etc.
4. **Is it actionable?** Errors should indicate what went wrong and suggest next steps

## Review Checklist

When reviewing PRs that add logging:

- [ ] Appropriate log level (not info for internal details)
- [ ] Consistent prefix format `[component-name]`
- [ ] No sensitive data (full file paths, large JSON)
- [ ] Uses `logger` utility in TypeScript, not raw `console.*`
