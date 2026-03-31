# Logging and Debugging

This guide explains how to access and enable verbose logging in nteract Desktop.

## Daemon Logs

The daemon (`runtimed`) logs to a file that persists across sessions.

### Log File Location

| Platform | Path |
|----------|------|
| macOS | `~/Library/Caches/runt/runtimed.log` |
| Linux | `~/.cache/runt/runtimed.log` |
| Dev mode (macOS) | `~/Library/Caches/{cache_namespace}/worktrees/{hash}/runtimed.log` |
| Dev mode (Linux) | `~/.cache/{cache_namespace}/worktrees/{hash}/runtimed.log` |

Source builds default to the `runt-nightly` cache namespace. If you intentionally build with `RUNT_BUILD_CHANNEL=stable`, the namespace is `runt` instead.

### Viewing Logs

```bash
# Last 100 lines
runt daemon logs -n 100

# Follow/tail logs (live updates)
runt daemon logs -f

# Check daemon version and status
runt daemon status
```

### Enabling Verbose Logging

For more detailed output, restart the daemon with debug logging:

```bash
# Stop the daemon
runt daemon stop

# Start with debug logging (one-time)
RUST_LOG=debug runtimed

# Or set specific modules
RUST_LOG=runtimed::notebook_sync_server=debug runtimed
```

For persistent verbose logging, you can modify the launch agent/service configuration.

## Frontend Logs

The notebook app logs to the webview console (View > Developer > Developer Tools).

### Viewing Frontend Debug Logs

There is no `localStorage` debug toggle in the current app. In development,
frontend logs are mirrored into the webview console via `attachConsole()` in
`apps/notebook/src/lib/logger.ts`. In packaged builds, what you see is governed
by the app-side log level from `tauri-plugin-log`.

### Log Prefixes

Logs are prefixed by component:
- `[daemon-kernel]` - Kernel communication (`useDaemonKernel.ts`)
- `[automerge-notebook]` - Document sync and WASM lifecycle (`useAutomergeNotebook.ts`)
- `[manifest-resolver]` - Output blob resolution (`useManifestResolver.ts`)

## Troubleshooting

### Kernel Not Starting

Check daemon logs for errors:
```bash
runt daemon logs -n 50 | grep -i error
```

### Outputs Not Displaying

Check the webview console in a development build for manifest resolution errors.

### Environment Issues

Check daemon logs for UV/Conda errors:
```bash
runt daemon logs | grep -E "(uv|conda|env)"
```
