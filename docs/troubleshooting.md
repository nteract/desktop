# Troubleshooting

If you're having issues with nteract Desktop, start here.

## Step 1: Run the Doctor Command

The `runt doctor` command diagnoses common issues and can often fix them automatically.

**If you have `runt` installed in your PATH:**

```bash
runt doctor
```

**If not, run it directly from the app bundle:**

```bash
# macOS
/Applications/nteract.app/Contents/MacOS/runt doctor

# If you installed to ~/Applications
~/Applications/nteract.app/Contents/MacOS/runt doctor
```

### Understanding the Output

The doctor checks:
- **Installed binary** - Is the daemon binary present?
- **Quarantine** (macOS) - Is Gatekeeper blocking the binary?
- **Service config** - Is the launchd/systemd service configured?
- **Socket file** - Can the app communicate with the daemon?
- **Daemon state** - Is the daemon actually running?

If issues are found, run with `--fix` to attempt automatic repair:

```bash
/Applications/nteract.app/Contents/MacOS/runt doctor --fix
```

## Common Issues

### "Runtime unavailable - Runtime daemon not available"

This means the app can't connect to the background daemon that manages kernels and environments.

**Fix:** Run `runt doctor --fix`. This typically resolves the issue by:
- Cleaning up stale state files from a crashed daemon
- Reinstalling the daemon binary if missing
- Resetting the launchd service registration

### Gatekeeper Quarantine (macOS)

If you downloaded nteract from the web, macOS may quarantine the daemon binary, preventing it from running.

**Symptoms:** Doctor shows "quarantine: blocked"

**Fix:** `runt doctor --fix` removes the quarantine attribute, or manually:

```bash
xattr -d com.apple.quarantine ~/Library/Application\ Support/runt/bin/runtimed
```

### Stale Daemon State

If the daemon crashed, it may have left behind state files that prevent restart.

**Symptoms:** Doctor shows "daemon_state: stale"

**Fix:** `runt doctor --fix` cleans up stale files, or manually:

```bash
rm ~/Library/Caches/runt/daemon.json
rm ~/Library/Caches/runt/runtimed.sock
```

### Service Not Loading (macOS)

The launchd service may fail to load if the plist is corrupted or not registered.

**Symptoms:** Doctor shows "launchd_service: not_loaded"

**Fix:** `runt doctor --fix` re-registers the service, or manually:

```bash
# Unload any stale registration
launchctl bootout gui/$(id -u)/io.nteract.runtimed 2>/dev/null

# Re-register the service
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.nteract.runtimed.plist
```

## Manual Recovery

If `doctor --fix` doesn't resolve your issue, try these manual steps.

### macOS

**Key file locations:**

| File | Path |
|------|------|
| Daemon binary | `~/Library/Application Support/runt/bin/runtimed` |
| Service config | `~/Library/LaunchAgents/io.nteract.runtimed.plist` |
| Socket | `~/Library/Caches/runt/runtimed.sock` |
| State file | `~/Library/Caches/runt/daemon.json` |
| Logs | `~/Library/Caches/runt/runtimed.log` |

**Full reset:**

```bash
# Stop the service
launchctl bootout gui/$(id -u)/io.nteract.runtimed 2>/dev/null

# Remove state files
rm -f ~/Library/Caches/runt/daemon.json
rm -f ~/Library/Caches/runt/runtimed.sock
rm -f ~/Library/Caches/runt/daemon.lock

# Re-register and start
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.nteract.runtimed.plist
```

### Linux

**Key file locations:**

| File | Path |
|------|------|
| Daemon binary | `~/.local/share/runt/bin/runtimed` |
| Service config | `~/.config/systemd/user/runtimed.service` |
| Socket | `~/.cache/runt/runtimed.sock` |
| State file | `~/.cache/runt/daemon.json` |
| Logs | `~/.cache/runt/runtimed.log` |

**Full reset:**

```bash
# Stop the service
systemctl --user stop runtimed.service

# Remove state files
rm -f ~/.cache/runt/daemon.json
rm -f ~/.cache/runt/runtimed.sock
rm -f ~/.cache/runt/daemon.lock

# Restart the service
systemctl --user start runtimed.service
```

## Viewing Logs

To see what the daemon is doing:

```bash
# If runt is in PATH
runt daemon logs -f

# Or directly
/Applications/nteract.app/Contents/MacOS/runt daemon logs -f

# Or read the log file directly
tail -f ~/Library/Caches/runt/runtimed.log  # macOS
tail -f ~/.cache/runt/runtimed.log          # Linux
```

## Installing the CLI (Optional)

For easier access to `runt` commands, you can install it to your PATH:

1. Open nteract
2. Go to the **nteract** menu (macOS) or **File** menu (Linux/Windows)
3. Click **"Install 'runt' Command in PATH..."**

This creates a symlink to `/usr/local/bin/runt`, so you can run `runt doctor` directly.

## Getting Help

If you're still stuck:

1. Check the logs for error messages: `runt daemon logs -n 50`
2. [Open an issue on GitHub](https://github.com/nteract/desktop/issues) with:
   - The output of `runt doctor`
   - Any relevant log messages
   - Your macOS/Linux version
