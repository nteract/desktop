---
paths:
  - crates/runtimed/src/**
  - apps/notebook/src/**
---

# Logging Guidelines

## Rust Logging

### runtimed daemon

Use the `tracing` crate. Import log macros at the top of your file:

```rust
use tracing::{debug, info, warn, error};
```

The daemon uses `tracing-subscriber` with layered subscribers (stderr + file).
Dependencies that use the `log` crate are automatically bridged into tracing
via `tracing-log` (set up by `.init()`).

### Tauri app (notebook crate)

The notebook app still uses `log` with `tauri-plugin-log`:

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
- `[runtimed]` -- Daemon core operations
- `[notebook-sync]` -- Automerge sync server
- `[kernel-manager]` -- Kernel lifecycle and execution
- `[doc-handle]` -- CRDT document mutations and requests
- `[comm_*]` -- Widget communication

### Default Log Levels by Channel

| Channel | Daemon default | Notebook app default |
|---------|---------------|---------------------|
| **Nightly** | `info` (with `debug` for sync modules) | `Debug` |
| **Stable** | `warn` | `Info` |

### Log File Rotation

Daemon logs rotate on startup — each daemon session gets a clean log file. Previous logs are preserved as `runtimed.log.1`. This makes `runt daemon logs -f` show only the current session.

### Enabling Debug Logs

```bash
# All debug logs (overrides channel default)
RUST_LOG=debug cargo xtask dev-daemon

# Specific module
RUST_LOG=runtimed::notebook_sync_server=debug cargo xtask dev-daemon
```

## TypeScript Logging

Use the `logger` utility from `apps/notebook/src/lib/logger.ts` instead of raw `console.*`:

```typescript
import { logger } from "../lib/logger";

logger.debug("[component] Internal detail");
logger.info("[component] Significant event");
logger.warn("[component] Recoverable issue");
logger.error("[component] Failure:", error);
```

### Log Level Behavior

- **Nightly**: All levels (`debug`, `info`, `warn`, `error`) enabled by default
- **Stable**: `logger.debug()` still goes through the logger, but the Rust-side filter usually drops it; `info`, `warn`, `error` remain visible
- Level filter applied server-side by `tauri-plugin-log`

### What NOT to Log at Info Level

- Per-cell execution, per-comm message details
- Retry attempts (only log final result)
- Internal state (blob port resolution, queue state)
- Success cases for routine operations (hot-sync succeeded)

### Seeing Frontend Debug Logs

There is no `localStorage` debug toggle in the current app. Frontend logs go
through `apps/notebook/src/lib/logger.ts`, and in development
(`import.meta.env.DEV`) `attachConsole()` mirrors them into browser devtools.
In packaged builds, visibility is controlled by the Rust-side app log level.

## Adding New Logging

Before adding a log statement, ask:

1. **Who needs this?** If only developers debugging, use `debug!`/`logger.debug()`
2. **How often does it fire?** High-frequency operations should be `debug` level
3. **Does it contain sensitive data?** Truncate or omit large JSON, file paths, etc.
4. **Is it actionable?** Errors should indicate what went wrong and suggest next steps

## Review Checklist

- Appropriate log level (not info for internal details)
- Consistent prefix format `[component-name]`
- No sensitive data (full file paths, large JSON)
- Uses `logger` utility in TypeScript, not raw `console.*`
