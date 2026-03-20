# Agent Instructions

<!-- This file is canonical. CLAUDE.md is a symlink to AGENTS.md. -->

This document provides essential guidance for AI agents working in this repository. Detailed guidance auto-loads via `.claude/rules/` (path-triggered) and `.claude/skills/` (on-demand) when relevant. Run `cargo xtask help` to discover all build commands.

## Dev Daemon (Required for All Dev Commands)

All commands that interact with the daemon require these env vars. Without them you'll hit the system daemon.

```bash
export RUNTIMED_DEV=1
export RUNTIMED_WORKSPACE_PATH="$(pwd)"
```

```bash
# Start dev daemon (Terminal 1)
cargo xtask dev-daemon

# Check status
./target/debug/runt daemon status

# Tail logs
./target/debug/runt daemon logs -f

# List running notebooks
./target/debug/runt ps

# Machine-readable status
./target/debug/runt daemon status --json | jq -r .socket_path
```

**NEVER** use `pkill runtimed` or `killall runtimed`. These kill all daemon processes system-wide. Use `./target/debug/runt daemon stop` instead.

**Important:** The daemon is a separate binary. Changes to `crates/runtimed/` require restarting `cargo xtask dev-daemon` (dev mode) or running `cargo xtask install-daemon` (production mode).

## Code Formatting (Required Before Committing)

```bash
cargo xtask lint --fix
```

Formats Rust, TypeScript/JavaScript (Biome), and Python (ruff). CI rejects PRs that fail. No pre-commit hooks — run manually.

For CI-style check-only: `cargo xtask lint`

## Commit and PR Title Standard

Use Conventional Commits for every commit message and PR title:

```text
<type>(<optional-scope>)!: <short imperative summary>
```

Types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `ci`, `build`, `perf`, `revert`

Examples:
- `feat(kernel): add environment source labels`
- `fix(runtimed): handle missing daemon socket`

## Workspace Description

```bash
mkdir -p .context
echo "Your description here" > .context/workspace-description
```

Appears in the debug banner. Keep short. The `.context/` directory is gitignored.

## Python Workspace

UV workspace root is the **repo root** — `pyproject.toml` and `.venv` live at the top level.

| Package | Path | Purpose |
|---------|------|---------|
| `runtimed` | `python/runtimed` | Python bindings (PyO3/maturin) |
| `nteract` | `python/nteract` | MCP server |
| `gremlin` | `python/gremlin` | Stress-test agent |

```bash
uv run nteract  # Run MCP server from repo root
```

## MCP Server

### Inkwell Supervisor

```bash
cargo xtask run-mcp          # Build and run (starts daemon if needed)
cargo xtask run-mcp --print-config  # Config JSON for your MCP client
```

### Supervisor Tools

| Tool | Purpose |
|------|---------|
| `supervisor_status` | Check child process, daemon, restart count |
| `supervisor_restart` | Restart child or daemon |
| `supervisor_rebuild` | Rebuild Python bindings + restart |
| `supervisor_logs` | Tail daemon logs |
| `supervisor_start_vite` | Start Vite dev server |
| `supervisor_stop` | Stop a managed process |

### Tool Availability

- **Inkwell active** → all supervisor + nteract tools
- **nteract MCP only** → nteract tools, no `supervisor_*`
- **No MCP server** → `cargo xtask run-mcp` to set one up
- **Dev daemon not running** → Inkwell starts it automatically

### Hot Reload

The supervisor watches `python/nteract/src/`, `python/runtimed/src/`, `crates/runtimed-py/src/`, and `crates/runtimed/src/`:
- **Python changes** → child restarts automatically
- **Rust changes** → `maturin develop` first, then restart

## Build System

All commands go through `cargo xtask`. **Run `cargo xtask help` at the start of each session** — it's the source of truth.

### Running the Notebook App

```bash
# Hot-reload dev mode
cargo xtask notebook

# One-shot setup + daemon + app
cargo xtask dev

# Debug build with bundled frontend
cargo xtask build && cargo xtask run
```

### Conductor Workspace Integration

| Conductor Variable | Translated To | Purpose |
|-------------------|---------------|---------|
| `CONDUCTOR_WORKSPACE_PATH` | `RUNTIMED_WORKSPACE_PATH` | Per-worktree daemon isolation |
| `CONDUCTOR_PORT` | (used directly) | Vite dev server port |

Translation happens in `cargo xtask dev`, `notebook`, and `dev-daemon` only.
