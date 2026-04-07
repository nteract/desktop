# Agent Instructions

<!-- This file is canonical. CLAUDE.md is a symlink to AGENTS.md. -->

This document provides guidance for AI agents working in this repository. Claude agents also receive contextual rules (`.claude/rules/`) and skills (`.claude/skills/`) auto-loaded when relevant. All agents should run `cargo xtask help` to discover build commands.

Codex-specific repo skills live in `.codex/skills/`. Prefer them when the task matches:
- `nteract-daemon-dev` for per-worktree daemon lifecycle, socket setup, and daemon-backed verification
- `nteract-python-bindings` for `maturin develop`, venv selection, and MCP server work
- `nteract-notebook-sync` for Automerge ownership, output manifests, and sync-path changes
- `nteract-testing` for choosing and running the right verification path

## Quick Recipes (Common Dev Tasks)

### If you have `supervisor_*` tools — use them

If your MCP client provides `supervisor_status`, `supervisor_restart`, `supervisor_rebuild`, etc., **prefer those over manual terminal commands**. The supervisor manages the dev daemon lifecycle for you — no env vars, no extra terminals.

**Claude Code has nteract-dev locally** — the local dev environment connects Claude Code to the repo-local `nteract-dev` MCP entry via `cargo xtask run-mcp`. Codex app/CLI can use the same server when this repo's project-scoped `.codex/config.toml` is enabled in a trusted workspace. If your current environment does not expose the supervisor tools, use the manual `cargo xtask` commands below.

| Instead of… | Use… |
|-------------|------|
| `cargo xtask dev-daemon` (in a terminal) | `supervisor_restart(target="daemon")` |
| `maturin develop` (rebuild bindings) | `supervisor_rebuild` |
| `runt daemon status` (with env vars) | `supervisor_status` |
| `runt daemon logs` | `supervisor_logs` |
| `cargo xtask vite` | `supervisor_start_vite` |

The supervisor automatically handles per-worktree isolation, env var plumbing, and daemon restarts. You only need the manual commands below when the supervisor isn't available (e.g. cloud sessions, CI).

### Manual commands (when supervisor is not available)

For raw terminal commands, opt into dev mode explicitly. `RUNTIMED_DEV=1` is what enables per-worktree daemon isolation. `RUNTIMED_WORKSPACE_PATH` is the safest way to pin the current worktree, though binaries launched from the repo root can also discover the worktree via git.

```bash
# ── Recommended env vars for raw dev-daemon commands ───────────────
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

**Do not launch the notebook app from an agent terminal.** The app is a GUI process that blocks until the user quits it (⌘Q), and the agent will misinterpret the exit. Let the human launch it from their own terminal or Zed task.

With supervisor tools, the daemon and vite are already managed — the human just runs:
```bash
cargo xtask notebook
```

Without supervisor (human runs both):
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
| High-level architecture | `contributing/architecture.md` |
| Development setup | `contributing/development.md` |
| Python bindings / MCP | `contributing/runtimed.md` § Python Bindings |
| Running tests | `contributing/testing.md` |
| E2E tests (WebdriverIO) | `contributing/e2e.md` |
| Frontend architecture | `contributing/frontend-architecture.md` |
| UI components (Shadcn) | `contributing/ui.md` |
| nteract Elements library | `contributing/nteract-elements.md` |
| Wire protocol / sync | `contributing/protocol.md` |
| Widget system | `contributing/widget-development.md` |
| Daemon development | `contributing/runtimed.md` |
| Environment management | `contributing/environments.md` |
| Output iframe sandbox | `contributing/iframe-isolation.md` |
| Renderer plugins (markdown, plotly, vega, leaflet) | `contributing/iframe-isolation.md` § Renderer Plugins |
| CRDT mutation rules | `contributing/crdt-mutation-guide.md` |
| TypeScript bindings (ts-rs) | `contributing/typescript-bindings.md` |
| Logging guidelines | `contributing/logging.md` |
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
| `nteract` | `python/nteract` | MCP server convenience wrapper (finds and launches `runt mcp`) |
| `gremlin` | `python/gremlin` | Autonomous notebook agent for stress testing |

```bash
runt mcp         # Run MCP server (shipped with the desktop app)
uv run nteract   # Alternative: finds and launches runt mcp
```

### Stable vs Nightly

- Source builds default to the `nightly` channel. Only `RUNT_BUILD_CHANNEL=stable` opts a source-built `cargo xtask` or `cargo` flow into stable names, app launch behavior, and cache/socket namespaces.
- Use the default nightly flow for normal repo development. Opt into stable only when you are specifically validating stable branding, stable socket/cache paths, or stable app-launch behavior.
- `cargo xtask dev-daemon`, `cargo xtask notebook`, `cargo xtask run`, `cargo xtask run-mcp`, and `cargo xtask dev-mcp` all follow `RUNT_BUILD_CHANNEL`.

### Python API Notes

- **`Output.data` is typed by MIME kind**: `str` for text MIME types, `bytes` for binary (raw bytes, no base64), `dict` for JSON MIME types. Image outputs include a synthesized `text/llm+plain` key with blob URLs.
- **Execution API**: `cell.run()` is sugar for `(await cell.execute()).result()`. For granular control use `Execution` handle: `execution = await cell.execute()` → `execution.status`, `execution.execution_id`, `await execution.result()`, `execution.cancel()`. Or `await cell.queue()` to enqueue without waiting.
- **RuntimeState**: `notebook.runtime` provides sync reads of kernel status, queue, executions, env sync, and trust from the RuntimeStateDoc.
- Use `default_socket_path()` for the current process or test harness because it respects `RUNTIMED_SOCKET_PATH`.
- Use `socket_path_for_channel("stable"|"nightly")` only when you must target a specific channel explicitly or discover the other channel; it intentionally ignores `RUNTIMED_SOCKET_PATH`.

## Creating Notebooks with Dependencies

**Always use MCP tools** (`create_notebook`, `add_dependency`) to create notebooks and manage dependencies. Do not write `.ipynb` files by hand with dependency metadata — the metadata schema is internal and agents should not need to know it.

If you must write a `.ipynb` file directly (e.g., test fixtures), dependencies go at `metadata.runt.uv.dependencies`:

```json
{
  "metadata": {
    "runt": {
      "uv": {
        "dependencies": ["pandas>=2.0", "numpy"]
      }
    }
  }
}
```

## MCP Server (Local Development)

### nteract-dev — MCP Supervisor

```bash
# Build and run the supervisor (starts daemon if needed)
cargo xtask run-mcp

# Or print config JSON for your MCP client
cargo xtask run-mcp --print-config
```

Use `nteract-dev` as the MCP server name for this source tree. Keep `nteract` for the global/system-installed MCP server. In clients that namespace tools by server name, that keeps repo-local tools distinct from the global install.

For Codex app/CLI, this repository also includes a project-scoped MCP config in `.codex/config.toml` that points at the same `mcp-supervisor` server using the `nteract-dev` entry name.

### MCP Server

The supervisor always uses the Rust-native `runt mcp` server (direct Automerge access, no Python overhead). It auto-builds `runt-cli` on startup and watches `crates/runt-mcp/src/` for hot reload.

`runt mcp` can also be run standalone (no supervisor): `./target/debug/runt mcp`. It reads `RUNTIMED_SOCKET_PATH` for the daemon connection.

For the installed app, `runt mcp` ships as a sidecar binary alongside `runtimed`, so MCP clients can use it directly without Python or uv.

### Supervisor Tools (from nteract-dev / `mcp-supervisor`)

| Tool | Purpose |
|------|---------|
| `supervisor_status` | Check child process, daemon, build mode, restart count, last error |
| `supervisor_restart` | Restart child (`target="child"`) or daemon (`target="daemon"`) |
| `supervisor_rebuild` | Rebuild the daemon binary and Rust Python bindings, restart the daemon, then restart the MCP child |
| `supervisor_logs` | Tail the daemon log file |
| `supervisor_vite_logs` | Tail the Vite dev server log file |
| `supervisor_start_vite` | Start the Vite dev server for hot-reload frontend development |
| `supervisor_stop` | Stop a managed process by name (e.g. `"vite"`) |
| `supervisor_set_mode` | Switch the managed daemon between `debug` and `release` builds and restart it |

### nteract MCP Tools (27 tools for notebook interaction)

When nteract-dev is active, agents also get the full nteract tool suite. **Use these to audit your own work** — open a notebook, execute cells, and inspect outputs to verify changes actually work before committing.

| Category | Tools |
|----------|-------|
| Session | `list_active_notebooks`, `show_notebook`, `join_notebook`, `open_notebook`, `create_notebook`, `save_notebook` |
| Kernel | `interrupt_kernel`, `restart_kernel` |
| Dependencies | `add_dependency`, `remove_dependency`, `get_dependencies`, `sync_environment` |
| Cell CRUD | `create_cell`, `get_cell`, `get_all_cells`, `set_cell`, `delete_cell`, `move_cell` |
| Cell metadata | `set_cells_source_hidden`, `set_cells_outputs_hidden`, `add_cell_tags`, `remove_cell_tags` |
| Find/Replace | `replace_match`, `replace_regex` |
| Execution | `execute_cell`, `run_all_cells`, `clear_outputs` |

**Audit workflow example:** After modifying daemon or kernel code, use `open_notebook` on a test fixture, `execute_cell` to run it, then `get_cell` to inspect outputs — confirming the change works end-to-end without leaving the agent session.

### Hot reload

The supervisor watches source directories and auto-restarts the child on changes:
- **`crates/runt-mcp/src/`** → `cargo build -p runt-cli` + restart (Rust MCP mode)
- **`crates/runtimed-client/src/`** → `cargo build -p runt-cli` + `maturin develop` + restart (shared code)
- **`crates/runtimed-py/src/`, `crates/runtimed/src/`** → `maturin develop` + `cargo build` + restart
- **`python/nteract/src/`, `python/runtimed/src/`** → child restart (Python mode) or background `maturin develop` (Rust mode)

### Tool availability

- **Local Claude Code / Zed / Codex app/CLI with MCP configured** → Configure the repo-local MCP entry as `nteract-dev`. nteract-dev exposes all `supervisor_*` tools plus the proxied nteract notebook tools. **Prefer supervisor tools for daemon lifecycle** — they handle env vars and isolation automatically.
- **Environments without supervisor tools** → use `cargo xtask` commands directly for build, daemon, and testing.
- **nteract MCP only** → The global/system `nteract` server exposes notebook tools only, with no `supervisor_*`. Use manual terminal commands for daemon management.
- **No MCP server** → use `cargo xtask run-mcp` to set one up
- **Dev daemon not running** → nteract-dev starts it automatically via `supervisor_restart(target="daemon")`

## Workspace Crates (17)

| Crate | Purpose |
|-------|---------|
| `runtimed` | Central daemon — env pools, notebook sync, runtime agent subprocess coordination |
| `runtimed-client` | Shared client library — output resolution, daemon paths, pool client |
| `runtimed-py` | Python bindings for daemon (PyO3/maturin) |
| `runtimed-wasm` | WASM bindings for notebook doc (Automerge, used by frontend) |
| `notebook` | Tauri desktop app — main GUI, bundles daemon+CLI as sidecars |
| `notebook-doc` | Shared Automerge schema — cells, outputs, RuntimeStateDoc, PEP 723, MIME classification |
| `notebook-protocol` | Wire types — requests, responses, broadcasts |
| `notebook-sync` | Automerge sync client — `DocHandle`, per-cell Python accessors |
| `runt` | CLI — daemon management, kernel control, notebook launching, MCP server |
| `runt-mcp` | Rust-native MCP server — 27 tools for notebook interaction via `runt mcp` |
| `runt-trust` | Notebook trust (HMAC-SHA256 over dependency metadata) |
| `runt-workspace` | Per-worktree daemon isolation, socket path management |
| `kernel-launch` | Kernel launching, tool bootstrapping (deno, uv, ruff via rattler) |
| `kernel-env` | Python environment management (UV + Conda) with progress reporting |
| `repr-llm` | LLM-friendly text summaries of visualization specs incl. GeoJSON (`text/llm+plain` synthesis) |
| `mcp-supervisor` | nteract-dev — MCP supervisor proxy, daemon/vite lifecycle management |
| `xtask` | Build system orchestration |

## Build System (`cargo xtask`)

All build, lint, and dev commands go through `cargo xtask`. **Run `cargo xtask help` at the start of each session** — it's the source of truth.

### Quick Reference

| Category | Command | Description |
|----------|---------|-------------|
| Dev | `cargo xtask dev` | Full setup: deps + build + daemon + app |
| | `cargo xtask dev --skip-build` | Reuse existing build artifacts before launch |
| | `cargo xtask dev --skip-install` | Reuse existing pnpm install before launch |
| | `cargo xtask notebook` | Hot-reload dev server (Vite on port 5174) |
| | `cargo xtask notebook --attach` | Attach Tauri to existing Vite server |
| | `cargo xtask vite` | Start Vite standalone |
| | `cargo xtask build` | Full debug build (frontend + Rust) |
| | `cargo xtask build --rust-only` | Rebuild Rust only, reuse frontend |
| | `cargo xtask run` | Run bundled debug binary |
| Release | `cargo xtask build-app` | Build the desktop app bundle with icons |
| | `cargo xtask build-dmg` | Build a DMG bundle (CI/release packaging) |
| Daemon | `cargo xtask dev-daemon` | Per-worktree dev daemon |
| | `cargo xtask dev-daemon --release` | Run the per-worktree daemon in release mode |
| | `cargo xtask install-daemon` | Install runtimed as system daemon |
| MCP | `cargo xtask run-mcp` | nteract-dev supervisor (daemon + MCP + auto-restart) |
| | `cargo xtask run-mcp --print-config` | Print MCP client config JSON |
| | `cargo xtask dev-mcp` | Direct nteract MCP (no supervisor) |
| | `cargo xtask dev-mcp --print-config` | Print direct MCP client config JSON |
| | `cargo xtask mcp-inspector` | Launch MCP Inspector UI for testing runt mcp |
| Lint | `cargo xtask lint` | Check formatting (Rust, JS/TS, Python) |
| | `cargo xtask lint --fix` | Auto-fix formatting |
| Test | `cargo xtask integration [filter]` | Python integration tests with isolated daemon |
| | `cargo xtask e2e [build|test|test-fixture|test-all]` | E2E testing (WebdriverIO) |
| Other | `cargo xtask wasm` | Rebuild runtimed-wasm |
| | `cargo xtask icons [source.png]` | Generate icon variants |
| | `cargo xtask mcpb` | Package nteract as a Claude Desktop extension (`.mcpb`) |

## Runtime Daemon (`runtimed`)

The daemon is a separate process from the notebook app. When you change code in `crates/runtimed/`, the running daemon still uses the old binary until you reinstall it.

### Do NOT Use pkill or killall

**Never** use `pkill runtimed`, `killall runtimed`, or similar commands. These kill **all** runtimed processes system-wide, disrupting other agents and worktrees.

Use instead:
- `./target/debug/runt daemon stop` — stops only your worktree's daemon
- `cargo xtask install-daemon` — gracefully reinstalls the system daemon

### Per-Worktree Daemon Isolation

Each git worktree runs its own isolated daemon in dev mode. If you have supervisor tools, the daemon is managed for you — use `supervisor_restart(target="daemon")` to start or restart it, and `supervisor_status` to check it.

Without supervisor (manual two-terminal workflow):

```bash
# Terminal 1: Start dev daemon
cargo xtask dev-daemon

# Terminal 2: Run the notebook app
cargo xtask notebook
```

Use `./target/debug/runt` to interact with the worktree daemon (or `supervisor_status`/`supervisor_logs` if available):

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

### Fork+Merge for Async CRDT Mutations

**Any code path that reads from the CRDT doc, does async work, then writes back MUST use `fork()` + `merge()`.** Direct mutation after an async gap can silently overwrite concurrent edits from other peers, the frontend, or background tasks.

```rust
// 1. Fork BEFORE the async work (captures the doc baseline)
let fork = {
    let mut doc = room.doc.write().await;
    doc.fork()
};

// 2. Do async work (subprocess, network, I/O)
let result = do_async_work().await;

// 3. Apply result on the fork (diffs against the pre-async baseline)
let mut fork = fork;
fork.update_source(&cell_id, &result).ok();

// 4. Merge back — concurrent edits compose via Automerge's text CRDT
let mut doc = room.doc.write().await;
doc.merge(&mut fork).ok();
```

For synchronous mutation blocks (no `.await` between fork and merge), use the helper:

```rust
// Fork at current heads, apply mutations, merge back
doc.fork_and_merge(|fork| {
    fork.update_source("cell-1", "x = 1\n");
});
```

**Do NOT use `fork_at(historical_heads)`** — it triggers an automerge bug
(`MissingOps` panic in the change collector) on documents with interleaved
text splices and merges. See automerge/automerge#1327. Use `fork()` instead.

**Key methods on `NotebookDoc`:** `fork()`, `get_heads()`, `merge()`, `fork_and_merge(f)`.

### No Independent `put_object` on Shared Keys

**Never call `put_object()` on a key that another peer also creates.** Two independent `put_object(ROOT, "cells", Map)` calls from different actors create two distinct Automerge Map objects at the same key — a conflict. Automerge picks one winner; the loser's children become invisible.

This is why `NotebookDoc::bootstrap()` only writes `schema_version` (a scalar). It does **not** create `cells` or `metadata` maps — those are created once by the daemon in `new_inner()` and arrive at other peers via sync. If bootstrap also created them, the daemon's populated maps (with cells, deps, etc.) could be shadowed by the bootstrap's empty maps depending on conflict resolution order.

**Rule:** Document structure (Maps, Lists at well-known keys) must be created by exactly one peer — the daemon. All other peers receive it via Automerge sync. Scalars at the same key are safe (identical values converge).

### The `is_binary_mime` Contract

One canonical Rust implementation in `notebook-doc::mime` is the single source of truth for MIME classification. It exports `is_binary_mime()`, `mime_kind()`, and the `MimeKind` enum. All Rust crates (`runtimed`, `runtimed-client`, `runtimed-wasm`) use this module — the old per-crate copies have been deleted.

On the TypeScript side, `isBinaryMime()` has been deleted from `manifest-resolution.ts`. **WASM now owns MIME classification end-to-end** — it resolves `ContentRef`s to `Inline`/`Url`/`Blob` variants directly, so the frontend never needs to classify MIMEs itself.

| Location | Function |
|----------|----------|
| `crates/notebook-doc/src/mime.rs` | `is_binary_mime()`, `mime_kind()`, `MimeKind` |

The classification rules: `image/*` → binary (EXCEPT `image/svg+xml` — that's text). `audio/*`, `video/*` → binary. `application/*` → binary by default (EXCEPT json, javascript, xml, and `+json`/`+xml` suffixes). `text/*` → always text.

### Crate Boundaries

| Crate | Owns | Modify when |
|-------|------|-------------|
| `notebook-doc` | Automerge schema, cell CRUD, output writes, MIME classification, `CellChangeset` | Changing document schema or cell operations |
| `notebook-protocol` | Wire types (`NotebookRequest`, `NotebookResponse`, `NotebookBroadcast`) | Adding request/response/broadcast types |
| `notebook-sync` | `DocHandle`, sync infrastructure, per-cell accessors for Python | Changing Python client sync behavior |

### CRDT State Ownership

| State | Writer | Notes |
|-------|--------|-------|
| Cell source | Frontend WASM | Local-first, character-level merge |
| Cell position, type, metadata | Frontend WASM | User-initiated via UI |
| Notebook metadata (deps, runtime) | Frontend WASM | User edits deps, runtime picker |
| Cell outputs (inline manifests) | Runtime agent subprocess | Kernel IOPub → blob store → inline manifest Maps in RuntimeStateDoc |
| Execution count | Runtime agent subprocess | Set on `execute_input` from kernel |
| Execution queue (source, seq, status) | Coordinator writes `queued`, runtime agent transitions to `running`/`done` | CRDT-driven execution — no RPC for cell execution |
| RuntimeStateDoc (kernel, queue, executions, env, trust) | Runtime agent + Coordinator | Separate Automerge doc, frame type `0x05` |

**Never write to the CRDT in response to a daemon broadcast.** The daemon already wrote. Writing again creates redundant sync traffic and incorrectly marks the notebook as dirty.

### Iframe Security

**NEVER add `allow-same-origin` to the iframe sandbox.** This is the single most important security invariant — tested in CI. It would give untrusted notebook outputs full access to Tauri APIs.

### Renderer Plugins (Isolated Iframe)

Heavy output renderers (markdown, plotly, vega, leaflet) are loaded as **on-demand CJS plugins** — not bundled into the core IIFE. Plugins are identified by **MIME types directly** — MIME types flow from CRDT outputs to the loading boundary without translation. Each plugin has its own Vite virtual module (`virtual:renderer-plugin/{name}`) for code splitting. The iframe's CJS loader provides React via a custom `require` shim — no window globals. `text/latex` is rendered via KaTeX inside the markdown renderer plugin. See `contributing/iframe-isolation.md` § Renderer Plugins for the full architecture and step-by-step guide to adding new plugins.

**Key files:** `src/isolated-renderer/index.tsx` (registry + loader), `src/isolated-renderer/*-renderer.tsx` (plugins), `apps/notebook/vite-plugin-isolated-renderer.ts` (build), `src/components/isolated/iframe-libraries.ts` (single MIME→plugin mapping layer: `PLUGIN_MIME_TYPES`, `needsPlugin`, `loadPluginForMime`).

### Cell List Stable DOM Order (Iframe Reload Prevention)

**The cell list in `NotebookView.tsx` MUST render in a stable DOM order (sorted by cell ID) and use CSS `order` for visual positioning.** Do NOT iterate `cellIds` directly in the JSX — iterate `stableDomOrder` instead.

Moving an `<iframe>` element in the DOM causes the browser to destroy and reload it. React's keyed-list reconciliation uses `insertBefore` to reorder DOM nodes when children change position. This causes iframe reloads — visible as white flashes, lost widget state, and re-rendered outputs.

The fix: render cells in a deterministic DOM order (`[...cellIds].sort()`) so React never moves existing nodes. Visual ordering is achieved via CSS `order` on each cell's wrapper, with the parent using `display: flex; flex-direction: column`.

Key files:
- `apps/notebook/src/components/NotebookView.tsx` — `stableDomOrder`, `cellIdToIndex`, flex container
- `src/components/isolated/isolated-frame.tsx` — iframe reload detection (the fallback path if DOM does move)
