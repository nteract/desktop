# Logging and Debugging

This guide explains how to access and enable verbose logging in nteract Desktop.

## Daemon Logs

The daemon (`runtimed`) logs to a file that persists across sessions.

### Log File Location

| Platform | Path |
|----------|------|
| macOS | `~/Library/Caches/runt/runtimed.log` |
| Linux | `~/.cache/runt/runtimed.log` |
| Dev mode | `~/.cache/runt/worktrees/{hash}/runtimed.log` |

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

The notebook app logs to the browser console (View > Developer > Developer Tools).

### Enabling Debug Mode

By default, routine operations are not logged in production. To enable verbose logging:

1. Open the browser console (Cmd+Option+I)
2. Run: `localStorage.setItem('runt:debug', 'true')`
3. Reload the page

To disable:
```javascript
localStorage.removeItem('runt:debug');
// Reload the page
```

### Log Prefixes

Logs are prefixed by component:
- `[daemon-kernel]` - Kernel communication
- `[notebook-sync]` - Document sync
- `[manifest-resolver]` - Output blob resolution
- `[App]` - Main app lifecycle

## Troubleshooting

### Kernel Not Starting

Check daemon logs for errors:
```bash
runt daemon logs -n 50 | grep -i error
```

### Outputs Not Displaying

Enable debug mode and check for manifest resolution errors in the browser console.

### Environment Issues

Check daemon logs for UV/Conda errors:
```bash
runt daemon logs | grep -E "(uv|conda|env)"
```
