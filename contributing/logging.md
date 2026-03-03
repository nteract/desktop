# Logging Guidelines

This guide covers logging conventions for contributors working on the nteract desktop codebase.

## Rust Logging

We use the `log` crate with `env_logger`. Import log macros at the top of your file:

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
- `[runtimed]` - Daemon core operations
- `[notebook-sync]` - Automerge sync server
- `[kernel-manager]` - Kernel lifecycle and execution
- `[comm_*]` - Widget communication

### Enabling Debug Logs

```bash
# All debug logs
RUST_LOG=debug cargo xtask dev-daemon

# Specific module
RUST_LOG=runtimed::notebook_sync_server=debug cargo xtask dev-daemon
```

## TypeScript Logging

Use the `logger` utility from `@/lib/logger` instead of raw `console.*`:

```typescript
import { logger } from "@/lib/logger";

logger.debug("[component] Internal detail");
logger.info("[component] Significant event");
logger.warn("[component] Recoverable issue");
logger.error("[component] Failure:", error);
```

### Log Level Behavior

- `logger.debug()` - Suppressed in production unless debug mode is enabled
- `logger.info()`, `logger.warn()`, `logger.error()` - Always enabled

### What NOT to Log at Info Level

- Per-cell execution, per-comm message details
- Retry attempts (only log final result)
- Internal state (blob port resolution, queue state)
- Success cases for routine operations (hot-sync succeeded)

### Enabling Debug Logs

In the browser console:
```javascript
localStorage.setItem('runt:debug', 'true');
// Reload the page
```

To disable:
```javascript
localStorage.removeItem('runt:debug');
// Reload the page
```

Debug mode is always enabled in development (`import.meta.env.DEV`).

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
