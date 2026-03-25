---
name: diagnostics
description: Collect and analyze nteract diagnostic logs. Use when debugging issues, investigating bugs, or gathering logs for a report.
---

# Diagnostics

## Collecting Diagnostics

The system nightly daemon runs outside the dev environment, so use `env -i` to avoid picking up dev-mode env vars:

```bash
env -i PATH="/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin" HOME="$HOME" runt-nightly diagnostics
```

For the stable channel:
```bash
env -i PATH="/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin" HOME="$HOME" runt diagnostics
```

The archive is written to the current directory (falls back to temp if not writable).

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
