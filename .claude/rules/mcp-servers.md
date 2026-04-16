---
description: MCP server selection — use the right server for the task
globs:
  - "**"
---

# MCP Server Selection

Three nteract MCP servers may be available. Always use the right one:

| Server | What it is | When to use |
|--------|-----------|-------------|
| `nteract-dev` | Dev MCP server. Adds dev tools (`up`, `down`, `status`, `logs`, `vite_logs`) on top of the proxied `runt mcp` toolset. Manages a per-worktree dev daemon, hot-reloads on code changes. | **Default for all development work.** Use this for notebook interaction, daemon lifecycle, building, and testing. |
| `nteract-nightly` | System-installed nightly release daemon | Diagnostics and inspection of the installed nightly app. Do NOT use for development. |
| `nteract` | System-installed stable release daemon (nteract.app) | Diagnostics and inspection of the installed stable app. Do NOT use for development. |

**Rules:**

1. **Always prefer `nteract-dev`** (`mcp__nteract-dev__*` tools) for development work in this repo. It connects to the per-worktree dev daemon and includes the dev tools for managing the build/daemon lifecycle.
2. **Never use `nteract-nightly` or `nteract` for development.** They connect to system-installed daemons and will not reflect your source changes.
3. If `nteract-dev` tools are not available, fall back to `cargo xtask` commands — not to the system MCP servers.
4. The dev tools (`up`, `down`, `status`, `logs`, `vite_logs`) live on the `nteract-dev` server. They manage the dev daemon and build pipeline — prefer them over manual terminal commands.

## nteract-dev tool surface

Two verbs plus three read-only tools, layered on top of the proxied `runt mcp` toolset:

| Tool | Purpose |
|------|---------|
| `up` | Idempotent "bring the dev environment to a working state." Sweeps zombie Vite processes, ensures the daemon is running, ensures the MCP child is healthy. Optional args: `vite=true` to also start Vite, `rebuild=true` to rebuild daemon + Python bindings first, `mode='debug'\|'release'` to switch build mode. Safe to call repeatedly — this is the first thing to reach for when things feel off. |
| `down` | Stop the managed Vite dev server. Leaves the daemon running by default (launchd / the installed app may own it). Pass `daemon=true` to also stop the managed daemon process. |
| `status` | Read-only report of `nteract-dev`, child, daemon, and managed-process state. |
| `logs` | Tail the daemon log. Arg: `lines` (default 50). |
| `vite_logs` | Tail the Vite dev server log. Arg: `lines` (default 50). |

## MCP Server

`nteract-dev` proxies `runt mcp` (Rust-native, direct Automerge access, no Python overhead). It auto-builds `runt-cli` on startup and watches `crates/runt-mcp/src/` for hot reload. For the installed app, `runt mcp` ships as a sidecar binary — no Python or uv required.

## System daemon CLI (`runt` / `runt-nightly`)

When running CLI commands against system-installed daemons from a dev environment, **always use `env -i`** to strip dev env vars (`RUNTIMED_DEV`, `RUNTIMED_WORKSPACE_PATH`) that would otherwise redirect commands to the per-worktree dev daemon:

**Important:** The repo's `bin/runt` (added to PATH by direnv) shadows `/usr/local/bin/runt` and always resolves to the dev build (nightly channel). When targeting system-installed daemons, use absolute paths:

```bash
# Nightly system daemon
env -i HOME=$HOME /usr/local/bin/runt-nightly diagnostics
env -i HOME=$HOME /usr/local/bin/runt-nightly daemon status

# Stable system daemon
env -i HOME=$HOME /usr/local/bin/runt diagnostics
env -i HOME=$HOME /usr/local/bin/runt daemon status
```

For the dev daemon, use `./target/debug/runt` directly (no `env -i` needed — dev env vars are correct).

## Verifying Daemon Isolation

After setting up direnv, verify that the three MCP servers connect to the correct daemons:

```bash
# 1. Check nteract-dev status (should show worktrees/ socket)
status
# Expected socket: ~/.cache/runt-nightly/worktrees/{hash}/runtimed.sock

# 2. List active notebooks on nteract-nightly (should show user's notebooks)
mcp__nteract-nightly__list_active_notebooks
# Should list real notebooks like coordination.ipynb

# 3. List active notebooks on nteract-dev (should be empty in fresh dev env)
mcp__nteract-dev__list_active_notebooks
# Should return []

# 4. Verify nteract-nightly MCP processes have NO dev env vars
ps aux | grep "runt.*mcp" | grep -v grep
# For each nteract-nightly PID:
cat /proc/{PID}/environ | tr '\0' '\n' | grep RUNTIMED
# Should return nothing — no RUNTIMED_DEV or RUNTIMED_WORKSPACE_PATH

# 5. Verify nteract-nightly daemon socket (should be system socket)
env -i HOME=$HOME /usr/local/bin/runt-nightly daemon status --json | jq -r '.socket_path'
# Expected: ~/.cache/runt-nightly/runtimed.sock (NOT worktrees/)
```

**Red flags:**
- nteract-dev socket path doesn't contain `worktrees/` → direnv not active, using system daemon
- nteract-nightly shows empty notebook list → connecting to dev daemon instead of system daemon
- nteract-nightly MCP process has `RUNTIMED_DEV=1` in environment → env var stripping failed

**Fix:** If direnv is not active, install it and run `direnv allow` in the repo root. See CLAUDE.md § Development Setup.
