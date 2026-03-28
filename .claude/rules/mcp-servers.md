---
description: MCP server selection — use the right server for the task
globs:
  - "**"
---

# MCP Server Selection

Three nteract MCP servers may be available. Always use the right one:

| Server | What it is | When to use |
|--------|-----------|-------------|
| `nteract-dev` | Dev MCP server with supervisor tools (`supervisor_*`). Manages a per-worktree dev daemon, hot-reloads on code changes. | **Default for all development work.** Use this for notebook interaction, daemon lifecycle, building, and testing. |
| `nteract-nightly` | System-installed nightly release daemon | Diagnostics and inspection of the installed nightly app. Do NOT use for development. |
| `nteract` | System-installed stable release daemon (nteract.app) | Diagnostics and inspection of the installed stable app. Do NOT use for development. |

**Rules:**

1. **Always prefer `nteract-dev`** (`mcp__nteract-dev__*` tools) for development work in this repo. It connects to the per-worktree dev daemon and includes supervisor tools for managing the build/daemon lifecycle.
2. **Never use `nteract-nightly` or `nteract` for development.** They connect to system-installed daemons and will not reflect your source changes.
3. If `nteract-dev` tools are not available, fall back to `cargo xtask` commands — not to the system MCP servers.
4. The supervisor tools (`supervisor_status`, `supervisor_restart`, `supervisor_rebuild`, `supervisor_logs`, `supervisor_start_vite`, `supervisor_stop`) are part of the `nteract-dev` server. They manage the dev daemon and build pipeline — prefer them over manual terminal commands.

## System daemon CLI (`runt` / `runt-nightly`)

When running CLI commands against system-installed daemons from a dev environment, **always use `env -i`** to strip dev env vars (`RUNTIMED_DEV`, `RUNTIMED_WORKSPACE_PATH`) that would otherwise redirect commands to the per-worktree dev daemon:

```bash
# Nightly system daemon
env -i PATH="$PATH" HOME="$HOME" runt-nightly diagnostics
env -i PATH="$PATH" HOME="$HOME" runt-nightly daemon status

# Stable system daemon
env -i PATH="$PATH" HOME="$HOME" runt diagnostics
env -i PATH="$PATH" HOME="$HOME" runt daemon status
```

For the dev daemon, use `./target/debug/runt` directly (no `env -i` needed — dev env vars are correct).
