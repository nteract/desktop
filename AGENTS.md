# Agent Instructions

<!-- This file is canonical. CLAUDE.md is a symlink to AGENTS.md. -->

This document provides guidance for AI agents working in this repository. Claude agents also receive contextual rules (`.claude/rules/`) and skills (`.claude/skills/`) auto-loaded when relevant. All agents should run `cargo xtask help` to discover build commands.

## Quick Recipes (Common Dev Tasks)

These are copy-paste-ready commands. **All commands that interact with the dev daemon require two env vars.** Without them you'll hit the system daemon and cause problems.

```bash
# ── Dev daemon env vars (required for ALL dev commands) ────────────
export RUNTIMED_DEV=1
export RUNTIMED_WORKSPACE_PATH="$(pwd)"
```

### Interacting with the dev daemon

```bash
# Check status (MUST use env vars or you'll see the system daemon)
RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH=$(pwd) ./target/debug/runt daemon status

# Tail logs
RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH=$(pwd) ./target/debug/runt daemon logs -f

# List running notebooks
RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH=$(pwd) ./target/debug/runt ps
```

### Rebuilding Python bindings (runtimed-py)

There are **two venvs** that matter:

| Venv | Purpose | Used by |
|------|---------|---------|
| `.venv` (repo root) | Workspace venv — has `nteract`, `runtimed`, and `gremlin` as editable installs | MCP server (`uv run nteract`), gremlin agent |
| `python/runtimed/.venv` | Test-only venv — has `runtimed` + `maturin` + test deps | `pytest` integration tests |

```bash
# For the MCP server (most common — this is what supervisor_rebuild does):
cd crates/runtimed-py && VIRTUAL_ENV=../../.venv uv run --directory ../../python/runtimed maturin develop

# For integration tests only:
cd crates/runtimed-py && VIRTUAL_ENV=../../python/runtimed/.venv uv run --directory ../../python/runtimed maturin develop
```

**Common mistake:** Running `maturin develop` without `VIRTUAL_ENV` installs the `.so` into whichever venv `uv run` resolves, which is `python/runtimed/.venv`. The MCP server runs from `.venv` (repo root) and will never see it. Always set `VIRTUAL_ENV` explicitly.

### Running Python integration tests

```bash
# Run against the dev daemon (must be running)
RUNTIMED_SOCKET_PATH="$(RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH=$(pwd) ./target/debug/runt daemon status --json | python3 -c 'import sys,json; print(json.load(sys.stdin)["socket_path"])')" \
  python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_daemon_integration.py -v

# Unit tests only (no daemon needed)
python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_session_unit.py -v
```

### Running the notebook app (dev mode)

```bash
# Terminal 1: Start dev daemon
cargo xtask dev-daemon

# Terminal 2: Start the app (MUST have env vars to avoid clobbering system daemon)
RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH=$(pwd) cargo xtask notebook
```

### WASM rebuild (after changing notebook-doc or runtimed-wasm)

```bash
wasm-pack build crates/runtimed-wasm --target web --out-dir ../../apps/notebook/src/wasm/runtimed-wasm
# Commit the output — WASM artifacts are checked into the repo
```

### Subsystem guides

Before diving into a subsystem, read the relevant guide:

| Task | Guide |
|------|-------|
| Python bindings / MCP | `contributing/runtimed.md` § Python Bindings |
| Running tests | `contributing/testing.md` |
| Frontend architecture | `contributing/frontend-architecture.md` |
| Wire protocol / sync | `contributing/protocol.md` |
| Widget system | `contributing/widget-development.md` |
| Daemon development | `contributing/runtimed.md` |
| Environment management | `contributing/environments.md` |
| Output iframe sandbox | `contributing/iframe-isolation.md` |
| CRDT mutation rules | `contributing/crdt-mutation-guide.md` |
| Build dependencies | `contributing/build-dependencies.md` |
| Releasing | `contributing/releasing.md` |

## Code Formatting (Required Before Committing)

Run this command before every commit. CI will reject PRs that fail formatting checks.

```bash
cargo xtask lint --fix
```

This formats Rust, lints/formats TypeScript/JavaScript with Biome, and lints/formats Python with ruff.

For CI-style check-only mode: `cargo xtask lint`

Do not skip this. There are no pre-commit hooks — you must run it manually.

## Commit and PR Title Standard (Required)

Use the Conventional Commits format for **both**:
- Every git commit message
- Every pull request title

Required format:
```text
<type>(<optional-scope>)!: <short imperative summary>
```

Types: `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `ci`, `build`, `perf`, `revert`

Examples:
- `feat(kernel): add environment source labels`
- `fix(runtimed): handle missing daemon socket`

## Workspace Description

When working in a worktree, set a human-readable description:

```bash
mkdir -p .context
echo "Your description here" > .context/workspace-description
```

The `.context/` directory is gitignored.

## Python Workspace

The UV workspace root is the **repository root** — `pyproject.toml` and `.venv` live at the top level (not under `python/`). Three packages are workspace members:

| Package | Path | Purpose |
|---------|------|---------|
| `runtimed` | `python/runtimed` | Python bindings for the Rust daemon (PyO3/maturin) |
| `nteract` | `python/nteract` | MCP server for programmatic notebook interaction |
| `gremlin` | `python/gremlin` | Autonomous notebook agent for stress testing |

```bash
uv run nteract  # Run MCP server from repo root
```

### Python API Notes

- **`Output.data` is typed by MIME kind**: `str` for text MIME types, `bytes` for binary (raw bytes, no base64), `dict` for JSON MIME types. Image outputs include a synthesized `text/llm+plain` key with blob URLs.

## MCP Server (Local Development)

### Inkwell — MCP Supervisor

```bash
# Build and run the supervisor (starts daemon if needed)
cargo xtask run-mcp

# Or print config JSON for your MCP client
cargo xtask run-mcp --print-config
```

### Available MCP tools

| Tool | Purpose |
|------|---------|
| `supervisor_status` | Check child process, daemon, build mode, restart count, last error |
| `supervisor_restart` | Restart child (`target="child"`) or daemon (`target="daemon"`) |
| `supervisor_rebuild` | Run `maturin develop` to rebuild Rust Python bindings, then restart |
| `supervisor_logs` | Tail the daemon log file |
| `supervisor_start_vite` | Start the Vite dev server for hot-reload frontend development |
| `supervisor_stop` | Stop a managed process by name (e.g. `"vite"`) |

### Hot reload

The supervisor watches `python/nteract/src/`, `python/runtimed/src/`, `crates/runtimed-py/src/`, and `crates/runtimed/src/`:
- **Python changes** → child process restarts automatically
- **Rust changes** → `maturin develop` runs first, then child restarts

### Tool availability

- **Inkwell active** → all supervisor + nteract tools available
- **nteract MCP only** → nteract tools only, no `supervisor_*`
- **No MCP server** → use `cargo xtask run-mcp` to set one up
- **Dev daemon not running** → Inkwell starts it automatically

## Build System (`cargo xtask`)

All build, lint, and dev commands go through `cargo xtask`. **Run `cargo xtask help` at the start of each session** — it's the source of truth.

## Runtime Daemon (`runtimed`)

The daemon is a separate process from the notebook app. When you change code in `crates/runtimed/`, the running daemon still uses the old binary until you reinstall it.

### Do NOT Use pkill or killall

**Never** use `pkill runtimed`, `killall runtimed`, or similar commands. These kill **all** runtimed processes system-wide, disrupting other agents and worktrees.

Use instead:
- `./target/debug/runt daemon stop` — stops only your worktree's daemon
- `cargo xtask install-daemon` — gracefully reinstalls the system daemon

### Per-Worktree Daemon Isolation

Each git worktree runs its own isolated daemon in dev mode.

```bash
# Terminal 1: Start dev daemon
cargo xtask dev-daemon

# Terminal 2: Run the notebook app
cargo xtask notebook
```

Use `./target/debug/runt` to interact with the worktree daemon:

```bash
./target/debug/runt daemon status
./target/debug/runt daemon logs -f
./target/debug/runt ps
./target/debug/runt notebooks
./target/debug/runt daemon flush
./target/debug/runt daemon status --json | jq -r .socket_path
```

### Conductor Workspace Integration

| Conductor Variable | Translated To | Purpose |
|-------------------|---------------|---------|
| `CONDUCTOR_WORKSPACE_PATH` | `RUNTIMED_WORKSPACE_PATH` | Per-worktree daemon isolation |
| `CONDUCTOR_PORT` | (used directly) | Vite dev server port |

## High-Risk Architecture Invariants

These invariants prevent bad edits. Read before modifying the relevant subsystems.

### The `is_binary_mime` Contract

Three implementations **must stay in sync** — if you change MIME classification, update all three:

| Location | Language | Function |
|----------|----------|----------|
| `crates/runtimed/src/output_store.rs` | Rust | `is_binary_mime()` |
| `crates/runtimed-py/src/output_resolver.rs` | Rust | `is_binary_mime()` |
| `apps/notebook/src/lib/manifest-resolution.ts` | TypeScript | `isBinaryMime()` |

The rule: `image/*` → binary (EXCEPT `image/svg+xml` — that's text). `audio/*`, `video/*` → binary. `application/*` → binary by default (EXCEPT json, javascript, xml, and `+json`/`+xml` suffixes). `text/*` → always text.

### Crate Boundaries

| Crate | Owns | Modify when |
|-------|------|-------------|
| `notebook-doc` | Automerge schema, cell CRUD, output writes, `CellChangeset` | Changing document schema or cell operations |
| `notebook-protocol` | Wire types (`NotebookRequest`, `NotebookResponse`, `NotebookBroadcast`) | Adding request/response/broadcast types |
| `notebook-sync` | `DocHandle`, sync infrastructure, per-cell accessors for Python | Changing Python client sync behavior |

### CRDT State Ownership

| State | Writer | Notes |
|-------|--------|-------|
| Cell source | Frontend WASM | Local-first, character-level merge |
| Cell position, type, metadata | Frontend WASM | User-initiated via UI |
| Notebook metadata (deps, runtime) | Frontend WASM | User edits deps, runtime picker |
| Cell outputs (manifest hashes) | Daemon | Kernel IOPub → blob store → hash in doc |
| Execution count | Daemon | Set on `execute_input` from kernel |

**Never write to the CRDT in response to a daemon broadcast.** The daemon already wrote. Writing again creates redundant sync traffic and incorrectly marks the notebook as dirty.

### Iframe Security

**NEVER add `allow-same-origin` to the iframe sandbox.** This is the single most important security invariant — tested in CI. It would give untrusted notebook outputs full access to Tauri APIs.
