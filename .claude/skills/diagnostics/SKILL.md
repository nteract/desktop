---
name: diagnostics
description: Collect and analyze nteract diagnostic logs. Use when debugging issues, investigating bugs, or gathering logs for a report.
---

# Diagnostics

## Why `env -i`?

In the dev environment, `RUNTIMED_DEV` and `RUNTIMED_WORKSPACE_PATH` are set (by direnv, xtask, or the supervisor). These cause `runt` to target the per-worktree dev daemon. System diagnostics need to target the **system-installed** daemon, so we use `env -i` to strip all env vars except `PATH` and `HOME`.

The repo-local `bin/runt` wrapper is first in `$PATH` — it runs `./target/debug/runt`, which is the dev build. That's fine for quick checks, but for true system diagnostics, call the system binary by its channel-specific name (`runt` for stable, `runt-nightly` for nightly) via `env -i` so dev env vars don't leak through.

## Collecting Diagnostics

**Nightly channel** (system-installed nteract Nightly.app):
```bash
env -i PATH="$PATH" HOME="$HOME" runt-nightly diagnostics
```

**Stable channel** (system-installed nteract.app):
```bash
env -i PATH="$PATH" HOME="$HOME" runt diagnostics
```

**Dev daemon** (per-worktree, no `env -i` needed):
```bash
./target/debug/runt diagnostics
```

The archive is written to the current directory (falls back to temp if not writable).

## Other Useful Commands

Same `env -i` pattern applies to any system daemon command:

```bash
# Check system daemon status
env -i PATH="$PATH" HOME="$HOME" runt-nightly daemon status
env -i PATH="$PATH" HOME="$HOME" runt daemon status

# List notebooks on system daemon
env -i PATH="$PATH" HOME="$HOME" runt-nightly notebooks
env -i PATH="$PATH" HOME="$HOME" runt ps

# Tail system daemon logs
env -i PATH="$PATH" HOME="$HOME" runt-nightly daemon logs -f
```

## Archive Contents

| File | Description |
|------|-------------|
| `runtimed.log` | Daemon log (current session) |
| `runtimed.log.1` | Daemon log (previous session) |
| `notebook.log` | Tauri app log — Rust + frontend entries (current session) |
| `notebook.log.1` | Tauri app log (previous session) |
| `daemon-status.json` | Daemon state, socket path, pool stats |
| `doctor.json` | Health checks — binary, plist, launchd, socket |
| `system-info.json` | OS version, architecture, channel |

## Extracting and Reading

```bash
mkdir -p /tmp/diag && tar xzf <archive>.tar.gz -C /tmp/diag
```

Then read the files from `/tmp/diag/`.

## What to Look For

- **Ghost windows:** `Context for '...' missing` in notebook.log
- **Daemon crashes:** `runtimed.log.1` often has the crash from the previous session
- **Upgrade failures:** Search for `[upgrade]` and `[runtimed upgrade]` in notebook.log
- **Kernel issues:** Search for `[daemon-kernel]` or `kernel_status` in notebook.log
- **Sync errors:** Search for `[notebook-sync]` or `daemon:disconnected`
- **Frontend errors:** Lines with `webview:error` or `webview:warn` in notebook.log (routed via tauri-plugin-log)
- **launchd issues:** Check `doctor.json` for `launchd_service` status and `diagnosis` field
