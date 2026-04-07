---
name: frontend-dev
description: Run the notebook app in dev mode with hot reload. Use when starting the app, setting up Vite, or debugging frontend development workflow.
---

# Frontend Development

## Quick Reference

| Task | Command |
|------|---------|
| Hot reload dev | `cargo xtask notebook` |
| Standalone Vite | `cargo xtask vite` |
| Attach to Vite | `cargo xtask notebook --attach` |
| Full debug build | `cargo xtask build` |
| Rust-only rebuild | `cargo xtask build --rust-only` |
| Run bundled binary | `cargo xtask run` |
| One-shot setup | `cargo xtask dev` |
| Lint/format | `cargo xtask lint --fix` |
| MCP supervisor | `cargo xtask run-mcp` |

## `cargo xtask notebook` — Hot Reload

Best for UI/React development. Uses Vite dev server on port 5174.

```bash
cargo xtask notebook
```

Changes to React components hot-reload instantly. The Tauri window loads from `localhost:5174` instead of bundled assets.

**Requires a dev daemon running.** Start one in another terminal first:

```bash
# Terminal 1: Start dev daemon
cargo xtask dev-daemon

# Terminal 2: Start the app
cargo xtask notebook
```

## `cargo xtask vite` + `notebook --attach` — Multi-Window Testing

When testing multiple notebook windows, closing the first Tauri window kills Vite. To avoid this:

```bash
# Terminal 1: Start Vite standalone (stays running)
cargo xtask vite

# Terminal 2+: Attach Tauri to existing Vite
cargo xtask notebook --attach
```

Close and reopen Tauri windows without losing Vite. Useful for:
- Realtime collaboration testing
- Widget testing across windows
- Avoiding breakage when one window closes

## `cargo xtask build` + `run` — Debug Build

Builds a debug binary with frontend assets bundled in. No Vite dev server needed.

```bash
cargo xtask build           # Full build (frontend + rust)
cargo xtask run             # Run the bundled binary
cargo xtask run path/to/notebook.ipynb
```

Emits JavaScript source maps for native webview devtools (including inline maps for iframe bundle).

For fast Rust-only iteration after initial build:

```bash
cargo xtask build --rust-only   # Skip frontend rebuild
cargo xtask run
```

## Dev Daemon Setup

The notebook app connects to a background daemon. In dev mode, each worktree gets its own isolated daemon.

### Two-Terminal Workflow

```bash
# Terminal 1: Start dev daemon (stays running)
cargo xtask dev-daemon

# Terminal 2: Run the notebook app
cargo xtask notebook         # Hot-reload mode
```

### Or One-Shot

```bash
cargo xtask dev              # Installs deps + builds + starts daemon + launches app
cargo xtask dev --skip-install --skip-build  # Fast repeat
```

### Conductor vs Non-Conductor

**Conductor users:** Dev mode is automatic. `CONDUCTOR_WORKSPACE_PATH` is translated to `RUNTIMED_WORKSPACE_PATH` by xtask commands.

**Non-Conductor users:** Set `RUNTIMED_DEV=1` explicitly:

```bash
RUNTIMED_DEV=1 cargo xtask dev-daemon    # Terminal 1
RUNTIMED_DEV=1 cargo xtask notebook      # Terminal 2
```

### Useful Daemon Commands

```bash
./target/debug/runt daemon status       # Check daemon state
./target/debug/runt daemon logs -f      # Tail logs
./target/debug/runt ps                  # List running kernels
./target/debug/runt notebooks           # List open notebooks
```

## MCP Server Development

### nteract-dev Supervisor (recommended)

```bash
cargo xtask run-mcp
```

Starts the dev daemon, launches the dev-only `nteract-dev` supervisor, spawns a child `runt mcp`, proxies notebook tool calls, watches for file changes, and hot-reloads. Python bindings are rebuilt when the watched Rust paths require it.

For editor config:

```bash
cargo xtask run-mcp --print-config
```

Use `nteract-dev` as the repo-local MCP server name so it stays distinct from any global/system `nteract` entry.

### Zed Integration

`.zed/settings.json` (gitignored):

```json
{
  "context_servers": {
    "nteract-dev": {
      "command": "cargo",
      "args": ["run", "-p", "mcp-supervisor"],
      "env": { "RUNTIMED_DEV": "1" }
    }
  }
}
```

### Direct Mode (no supervisor)

```bash
# Terminal 1: start dev daemon
cargo xtask dev-daemon

# Terminal 2: build bindings + launch MCP server
cargo xtask dev-mcp
```

### Supervisor Tools

| Tool | Purpose |
|------|---------|
| `supervisor_status` | Child process, daemon, restart count, last error |
| `supervisor_restart` | Restart child or daemon |
| `supervisor_rebuild` | Rebuild the daemon binary plus Rust Python bindings, then restart the daemon and MCP child |
| `supervisor_logs` | Tail daemon log file |
| `supervisor_vite_logs` | Tail Vite dev server log file |
| `supervisor_start_vite` | Start Vite dev server for hot-reload frontend dev |
| `supervisor_stop` | Stop a managed process by name |
| `supervisor_set_mode` | Switch the managed daemon between `debug` and `release` builds |

### Hot Reload

Watches `python/nteract/src/`, `python/runtimed/src/`, `crates/runtimed-py/src/`, `crates/runtimed/src/`:
- **Python changes** — child restarts automatically
- **Rust changes** — `maturin develop` runs first, then child restarts

## Zed Editor Tasks

The repo includes `.zed/tasks.json` with pre-configured tasks (use cmd-shift-t):

| Task | What it does |
|------|-------------|
| Dev Daemon | `cargo xtask dev-daemon` with dev env vars |
| Dev App | `cargo xtask notebook` with dev env vars and auto Vite port |
| Daemon Status | `./target/debug/runt daemon status` |
| Daemon Logs | `./target/debug/runt daemon logs -f` |
| Format | `cargo fmt` + biome |
| Setup | `pnpm install && cargo xtask build` |

## Common Gotchas

**Daemon code changes not taking effect:**
1. In dev mode: restart `cargo xtask dev-daemon`
2. In production mode: run `cargo xtask install-daemon`
3. Check which daemon: `./target/debug/runt daemon status`

**App says "Dev daemon not running":**
- You're in dev mode but haven't started the dev daemon
- Run `cargo xtask dev-daemon` in another terminal

**Port conflicts with Vite:**
- Default port 5174 may conflict across worktrees
- Use `cargo xtask build` + `run` to avoid Vite entirely
- Or use `CONDUCTOR_PORT` for automatic port assignment

**Frontend changes not showing:**
- With `cargo xtask notebook`: should hot-reload automatically
- With `cargo xtask run`: need to rebuild (`cargo xtask build`)
- With `--rust-only`: frontend is NOT rebuilt (that's the point)
