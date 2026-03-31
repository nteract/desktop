# Development Guide

## Quick Reference

| Task | Command |
|------|---------|
| One-shot notebook setup | `cargo xtask dev` |
| Start dev server | `cargo xtask notebook` |
| Standalone Vite | `cargo xtask vite` |
| Attach to Vite | `cargo xtask notebook --attach` |
| Full debug build | `cargo xtask build` |
| Rust-only rebuild | `cargo xtask build --rust-only` |
| Run bundled binary | `cargo xtask run` |
| Run with notebook | `cargo xtask run path/to/notebook.ipynb` |
| Build release .app | `cargo xtask build-app` |
| Build release DMG | `cargo xtask build-dmg` |
| MCP supervisor (nteract-dev) | `cargo xtask run-mcp` |
| MCP config JSON | `cargo xtask run-mcp --print-config` |
| MCP server (no supervisor) | `cargo xtask dev-mcp` |
| Lint (check mode) | `cargo xtask lint` |
| Lint (auto-fix) | `cargo xtask lint --fix` |

## Build Cache (sccache)

Install [sccache](https://github.com/mozilla/sccache) to share compiled
artifacts across worktrees. Without it, each worktree rebuilds ~788 crates from
scratch.

```bash
brew install sccache   # macOS
```

The xtask commands auto-detect sccache and set `RUSTC_WRAPPER` when it's
available — no configuration needed. You'll see "Using sccache for compilation
cache" in the build output when it's active.

## Choosing a Workflow

### `cargo xtask dev` — One Command Setup + Dev

Best for first-time local setup or when you want the daemon and notebook app to
come up together.

```bash
cargo xtask dev
```

This command:
- runs `pnpm install` when your workspace dependencies are missing or stale
- runs `cargo xtask build` unless you pass `--skip-build`
- starts the per-worktree dev daemon
- waits for the daemon to be reachable
- launches the notebook app in dev mode

For faster repeat launches:

```bash
cargo xtask dev --skip-install --skip-build
```

### `cargo xtask notebook` — Hot Reload

Best for UI/React development. Uses Vite dev server on port 5174. Changes to React components hot-reload instantly.

```bash
cargo xtask notebook
```

### `cargo xtask vite` + `notebook --attach` — Multi-Window Testing

When testing with multiple notebook windows, closing the first Tauri window normally kills the Vite server. To avoid this:

```bash
# Terminal 1: Start Vite standalone (stays running)
cargo xtask vite

# Terminal 2+: Attach Tauri to existing Vite
cargo xtask notebook --attach
```

Now you can close and reopen Tauri windows without losing Vite. This is useful for:
- Testing realtime collaboration
- Testing widgets across windows
- Avoiding confusion when one window close breaks others

### `cargo xtask build` / `run` — Debug Build

Best for:
- Testing Rust changes
- Multiple worktrees (avoids port 5174 conflicts)
- Running the standalone binary

Builds a debug binary with frontend assets bundled in.

```bash
# Full build (frontend + rust)
cargo xtask build

# Run the bundled binary
cargo xtask run

# Run with a specific notebook
cargo xtask run path/to/notebook.ipynb
```

`cargo xtask build` also emits JavaScript source maps for the bundled debug UI,
including inline maps for the isolated renderer iframe bundle, so native webview
devtools can step through `.tsx` sources.

### `cargo xtask build --rust-only` — Fast Rust Iteration

When you're only changing Rust code (not the frontend), skip the frontend rebuild:

```bash
# First time: full build
cargo xtask build

# Subsequent rebuilds: rust only (much faster)
cargo xtask build --rust-only
cargo xtask run
```

This is ideal for daemon development — build the frontend once, then iterate on Rust with fast rebuilds.

### `cargo xtask build-app` / `build-dmg` — Release Builds

Mostly handled by CI for preview releases. Use locally only when testing:
- App bundle structure
- File associations
- Icons

## Build Order

The UI must be built before Rust because:
- `crates/notebook` embeds assets from `apps/notebook/dist/` via Tauri

The xtask commands handle this automatically. If building manually:

```bash
pnpm build          # Build notebook UI (isolated-renderer built inline)
cargo build         # Build Rust
```

> **Note:** If you've changed `crates/runtimed-wasm/`, you need to run
> `wasm-pack build crates/runtimed-wasm --target web --out-dir ../../apps/notebook/src/wasm/runtimed-wasm`
> before `pnpm build`. `cargo xtask build` handles this automatically.

## Test Notebooks

Test notebooks live in `crates/notebook/fixtures/audit-test/` and sample notebooks in `crates/notebook/resources/sample-notebooks/`.

```bash
cargo xtask build
./target/debug/notebook crates/notebook/fixtures/audit-test/1-vanilla.ipynb
```

## Daemon Development

The notebook app connects to a background daemon (`runtimed`) that manages prewarmed environments and notebook document sync. **Important:** The daemon is a separate process. When you change code in `crates/runtimed/`, the running daemon still uses the old binary until you reinstall it.

### Development Mode (Per-Worktree Isolation)

In production, the Tauri app auto-installs and manages the system daemon. In development, you control the daemon yourself, which gives you:

- Isolated state per worktree (no conflicts when testing across branches)
- Your code changes take effect immediately on daemon restart
- No interference with the system daemon

**With nteract-dev (preferred for agents):**

If you have the repo-local `nteract-dev` MCP entry configured, the daemon is managed for you through the `supervisor_*` tools:

- `supervisor_restart(target="daemon")` — start or restart the dev daemon
- `supervisor_status` — check daemon status (includes `daemon_managed: true/false`)
- `supervisor_rebuild` — rebuild Python bindings + restart
- `supervisor_logs` — tail daemon logs

No env vars or extra terminals needed. nteract-dev handles per-worktree isolation automatically.

**Two-terminal workflow (without supervisor):**

```bash
# Terminal 1: Start the dev daemon (stays running)
cargo xtask dev-daemon

# Terminal 2: Run the notebook app
cargo xtask notebook         # Hot-reload mode
# or
cargo xtask dev              # One-shot setup + daemon + app
# or
cargo xtask build            # Full build once
cargo xtask build --rust-only && cargo xtask run  # Fast iteration
```

The app detects dev mode and connects to the per-worktree daemon instead of installing/starting the system daemon.

**Conductor users:** When using `cargo xtask dev`, `cargo xtask notebook`, or `cargo xtask dev-daemon`, the xtask commands automatically translate `CONDUCTOR_WORKSPACE_PATH` to `RUNTIMED_WORKSPACE_PATH`, enabling dev mode.

**Non-Conductor users:** Set `RUNTIMED_DEV=1`:

```bash
# Terminal 1
RUNTIMED_DEV=1 cargo xtask dev-daemon

# Terminal 2
RUNTIMED_DEV=1 cargo xtask notebook
```

**Useful commands:**

```bash
./target/debug/runt daemon status           # Shows dev mode, worktree path, version
./target/debug/runt dev worktrees           # List all running dev daemons (requires RUNTIMED_DEV=1)
./target/debug/runt daemon logs -f          # Tail logs (uses correct log path in dev mode)
```

Per-worktree state is stored in `<cache>/{cache_namespace}/worktrees/{hash}/` (macOS: `~/Library/Caches/`, Linux: `~/.cache/`). Source builds default to `runt-nightly`; set `RUNT_BUILD_CHANNEL=stable` only when you intentionally need the stable flow.

**For AI agents:** If the repo-local `nteract-dev` MCP entry is available, prefer its `supervisor_*` tools — they handle env vars and daemon lifecycle automatically. Keep `nteract` as the name for the global/system-installed MCP server. When using a raw terminal (not Zed tasks), set the env vars manually:

```bash
export RUNTIMED_DEV=1
export RUNTIMED_WORKSPACE_PATH="$(pwd)"
./target/debug/runt daemon status
```

### Testing Against System Daemon (Production Mode)

When you need to test the full production flow (daemon auto-install, upgrades, etc.):

```bash
# Make sure dev mode is NOT set
unset RUNTIMED_DEV
unset RUNTIMED_WORKSPACE_PATH

# Rebuild and reinstall system daemon
cargo xtask install-daemon

# Run the app (it will connect to system daemon)
cargo xtask notebook
```

### Daemon logs

```bash
# View recent logs
runt daemon logs -n 100

# Watch logs in real-time
runt daemon logs -f

# Filter for specific topics
runt daemon logs -f | grep -i "kernel\|auto-detect"
```

### Common gotchas

If your daemon code changes aren't taking effect:
1. **In dev mode:** Did you restart `cargo xtask dev-daemon`?
2. **In production mode:** Did you run `cargo xtask install-daemon`?
3. Check which daemon is running: `runt daemon status`

If the app says "Dev daemon not running":
- You're in dev mode but haven't started the dev daemon
- Run `cargo xtask dev-daemon` in another terminal first

See [contributing/runtimed.md](./runtimed.md) for full daemon development docs.

## MCP Server Development

The [nteract MCP server](../python/nteract/) lets AI agents
(Claude, Zed, etc.) interact with notebooks via the daemon. There are two ways
to run it locally.

### Python environment

The Python workspace root is the **repo root** — `pyproject.toml` and `.venv`
both live there, not inside `python/`.

| Venv | Location | Purpose |
|------|----------|---------|
| Workspace venv | `.venv` (repo root) | MCP server, `uv run nteract`, `maturin develop` target |
| Test venv | `python/runtimed/.venv` | Isolated `pytest` runs for the `runtimed` bindings |

**Sync the workspace** (installs nteract + runtimed + deps into `.venv`):

```bash
uv sync            # from repo root
```

**Build the native runtimed extension** (from `crates/runtimed-py`):

```bash
cd crates/runtimed-py
VIRTUAL_ENV=../../.venv maturin develop
```

The `VIRTUAL_ENV` override ensures the `.so` is installed into the workspace
venv (where the MCP server runs), not into `python/runtimed/.venv` (the
test-only venv).

**Run the MCP server manually:**

```bash
# Using the built runt binary (preferred)
./target/debug/runt mcp

# Or via the Python wrapper (finds and launches runt mcp)
uv run nteract
```

### nteract-dev supervisor (recommended)

The **nteract-dev** MCP supervisor (`crates/mcp-supervisor/`) is a stable Rust
process that proxies to `runt mcp`. It handles daemon
lifecycle, auto-restart on crash, and hot-reload on file changes — one command,
everything works:

```bash
cargo xtask run-mcp
```

This:
1. Starts the dev daemon if not running
2. Builds `runt-cli` and spawns `runt mcp` as a child process
3. Proxies all tool calls + injects `supervisor_*` management tools
4. Watches source files and hot-reloads on changes

For your MCP client config (Zed, Claude Desktop, Codex, etc.):

```bash
cargo xtask run-mcp --print-config
```

Use `nteract-dev` as the server name for this source tree so it stays distinct from any global/system `nteract` MCP entry.

Codex app/CLI can read a project-scoped `.codex/config.toml`. This repo includes one that mirrors the same supervisor setup:

```toml
[mcp_servers.nteract-dev]
command = "cargo"
args = ["run", "-p", "mcp-supervisor"]

[mcp_servers.nteract-dev.env]
RUNTIMED_DEV = "1"
```

Or configure `.zed/settings.json` directly (gitignored):

```json
{
  "context_servers": {
    "nteract-dev": {
      "command": "./target/debug/mcp-supervisor",
      "args": [],
      "env": { "RUNTIMED_DEV": "1" }
    }
  }
}
```

In clients that namespace tools by server name, this keeps repo-local notebook tools separate from the global install while still exposing the same notebook APIs plus the extra `supervisor_*` tools.

#### Supervisor tools

These tools are always available, even when the Python child is down:

| Tool | Purpose |
|------|---------|
| `supervisor_status` | Child process, daemon, restart count, last error |
| `supervisor_restart` | Restart child or daemon |
| `supervisor_rebuild` | `maturin develop` into `.venv` + restart (after Rust changes) |
| `supervisor_logs` | Tail the daemon log file |
| `supervisor_start_vite` | Start the Vite dev server for hot-reload frontend development |
| `supervisor_stop` | Stop a managed process by name (e.g. `"vite"`) |

#### Hot reload

The supervisor watches `python/nteract/src/`, `python/runtimed/src/`,
`crates/runtimed-py/src/`, and `crates/runtimed/src/`:

- **Python changes** → child restarts automatically
- **Rust changes** → `maturin develop` runs first, then child restarts
- **Behavior changes** take effect immediately on the next tool call
- **New/removed tools** may take a moment for the client to discover

### Direct mode (no supervisor)

If you don't need auto-restart or file watching, `dev-mcp` runs the nteract
server directly:

```bash
# Terminal 1: start the dev daemon
cargo xtask dev-daemon

# Terminal 2: build bindings + launch MCP server
cargo xtask dev-mcp
```

### How it works

The MCP server is a pure Python package (`python/nteract/`) that depends on
`runtimed` (PyO3 bindings in `python/runtimed/`, built from
`crates/runtimed-py/`). Both are workspace members of the repo-root
`pyproject.toml`, so `uv sync` installs them into `.venv` at the repo root.

The nteract-dev supervisor (`crates/mcp-supervisor/`) uses the `rmcp` Rust SDK to
act as both an MCP server (facing the client) and an MCP client (facing the
Python child process). It spawns the child via `uv run --directory . nteract`
from the repo root.

## Before You Commit

CI rejects PRs that fail formatting. Run this before every commit:

```bash
cargo xtask lint --fix
```

This formats Rust, lints/formats TypeScript/JavaScript with Biome, and lints/formats Python with ruff.

Use [conventional commits](https://www.conventionalcommits.org/) for commit messages and PR titles:

```
feat(kernel): add environment source labels
fix(runtimed): handle missing daemon socket
docs(agents): enforce conventional commit format
```

## Zed Editor Integration

The repo includes `.zed/tasks.json` with pre-configured tasks that set the correct environment variables for dev mode. Use `task: spawn` (cmd-shift-t) to run them:

| Task | What it does |
|------|-------------|
| **Dev Daemon** | `cargo xtask dev-daemon` with `RUNTIMED_DEV=1` and `RUNTIMED_WORKSPACE_PATH` |
| **Dev App** | `cargo xtask notebook` with dev env vars and auto-assigned Vite port |
| **Daemon Status** | `./target/debug/runt daemon status` pointed at the worktree daemon |
| **Daemon Logs** | `./target/debug/runt daemon logs -f` with live tail |
| **Format** | `cargo fmt` + biome in one step |
| **Setup** | `pnpm install && cargo xtask build` for first-time setup |

The tasks use `$ZED_WORKTREE_ROOT` for `RUNTIMED_WORKSPACE_PATH`, giving each Zed worktree its own isolated daemon — no conflicts when working across branches.

**For agents in Zed:** The Zed task env vars aren't available in agent terminal sessions. Set them explicitly:

```bash
export RUNTIMED_DEV=1
export RUNTIMED_WORKSPACE_PATH="/path/to/your/worktree"
```
