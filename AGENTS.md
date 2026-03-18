# Agent Instructions

<!-- This file is canonical. CLAUDE.md is a symlink to AGENTS.md. -->

This document provides guidance for AI agents working in this repository.

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

The Python package `runtimed` wraps the Rust `runtimed-py` crate via PyO3. After changing Rust code in `crates/runtimed-py/`, rebuild into the correct venv.

There are **two venvs** that matter:

| Venv | Purpose | Used by |
|------|---------|---------|
| `python/.venv` | Workspace venv — has both `nteract` and `runtimed` as editable installs | MCP server (`uv run --directory python nteract`) |
| `python/runtimed/.venv` | Test-only venv — has `runtimed` + `maturin` + test deps | `pytest` integration tests |

```bash
# For the MCP server (most common — this is what supervisor_rebuild does):
cd crates/runtimed-py && VIRTUAL_ENV=../../python/.venv uv run --directory ../../python/runtimed maturin develop

# For integration tests only:
cd crates/runtimed-py && VIRTUAL_ENV=../../python/runtimed/.venv uv run --directory ../../python/runtimed maturin develop
```

**Common mistake:** Running `maturin develop` without `VIRTUAL_ENV` installs the `.so` into whichever venv `uv run` resolves, which is `python/runtimed/.venv`. The MCP server runs from `python/.venv` and will never see it. Always set `VIRTUAL_ENV` explicitly.

If using the MCP supervisor, `supervisor_rebuild` handles this automatically — it builds into `python/.venv` and restarts the MCP server.

### Running Python integration tests

```bash
# Run against the dev daemon (must be running)
RUNTIMED_SOCKET_PATH="$(RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH=$(pwd) ./target/debug/runt daemon status --json | python3 -c 'import sys,json; print(json.load(sys.stdin)["socket_path"])')" \
  python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_daemon_integration.py -v

# Run a specific test class
# ... add -k "TestClassName" to the above

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

### Key contributing docs

Before diving into a subsystem, read the relevant guide:

| Task | Read first |
|------|-----------|
| Python bindings / MCP | `contributing/runtimed.md` § Python Bindings |
| Running tests | `contributing/testing.md` |
| Frontend architecture | `contributing/frontend-architecture.md` |
| Wire protocol / sync | `contributing/protocol.md` |
| Widget system | `contributing/widget-development.md` |
| Daemon development | `contributing/runtimed.md` |
| Environment management | `contributing/environments.md` |
| Output iframe sandbox | `contributing/iframe-isolation.md` |

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

Rules:
- Use lowercase `type` and summary text
- Keep summaries concise and do not end with a period
- Use `!` only for breaking changes (and explain details in the commit body or PR description)
- For PR titles, choose the primary change type represented by the PR

Common types:
- `feat`, `fix`, `docs`, `chore`, `refactor`, `test`, `ci`, `build`, `perf`, `revert`

Examples:
- `feat(kernel): add environment source labels`
- `fix(runtimed): handle missing daemon socket`
- `docs(agents): enforce conventional commit and PR title format`

## Workspace Description

When working in a worktree, set a human-readable description of what you're working on by writing to `.context/workspace-description`:

```bash
mkdir -p .context
echo "Your description here" > .context/workspace-description
```

This description appears in the notebook app's debug banner (visible in debug builds only), helping identify what each worktree is testing when multiple are running in parallel.

Keep descriptions short and descriptive, e.g.:
- "Testing conda environment creation"
- "Fixing kernel interrupt handling"
- "Adding ipywidgets support"

The `.context/` directory is gitignored and used for per-worktree state that shouldn't be committed.

## MCP Server (Local Development)

For programmatic notebook interaction, use the [nteract MCP server](https://github.com/nteract/nteract) (`nteract` on PyPI).

### Inkwell — MCP Supervisor

When developing locally, the **Inkwell** MCP supervisor (`crates/mcp-supervisor/`)
provides a stable MCP proxy between your editor and the nteract Python MCP server.
It manages the dev daemon lifecycle, auto-restarts the Python server on crash, and
hot-reloads when source files change.

```bash
# Build and run the supervisor (starts daemon if needed)
cargo xtask mcp

# Or print config JSON for your MCP client
cargo xtask mcp --print-config
```

For `.zed/settings.json` (gitignored, per-developer):
```json
{
  "context_servers": {
    "nteract": {
      "command": "./target/debug/mcp-supervisor",
      "args": [],
      "env": { "RUNTIMED_DEV": "1" }
    }
  }
}
```

### Available MCP tools

If the Inkwell supervisor is active in your session, you have access to these
tools in addition to the standard nteract notebook tools:

| Tool | Purpose |
|------|---------|
| `supervisor_status` | Check child process, daemon, restart count, last error |
| `supervisor_restart` | Restart child (`target="child"`) or daemon (`target="daemon"`) |
| `supervisor_rebuild` | Run `maturin develop` to rebuild Rust Python bindings, then restart |
| `supervisor_logs` | Tail the daemon log file |
| `supervisor_start_vite` | Start the Vite dev server for hot-reload frontend development. Returns the port number. If already running, returns the existing port. |
| `supervisor_stop` | Stop a managed process by name (e.g. `"vite"`). |

The nteract tools (`list_active_notebooks`, `create_notebook`, `execute_cell`, etc.)
are proxied through the supervisor. If tools start failing, call
`supervisor_status` to diagnose, then `supervisor_restart` or
`supervisor_rebuild` to recover.

### Hot reload

The supervisor watches `python/nteract/src/`, `python/runtimed/src/`,
`crates/runtimed-py/src/`, and `crates/runtimed/src/` for changes:

- **Python changes** → child process restarts automatically
- **Rust changes** → `maturin develop` runs first, then child restarts
- **Behavior changes** take effect immediately on the next tool call
- **New/removed tools** may take a moment for the client to discover

### Tool availability

MCP tools may or may not be available depending on your session:

- **Inkwell active** → all supervisor + nteract tools available
- **nteract MCP only** (no supervisor) → nteract tools only, no `supervisor_*`
- **No MCP server** → use `cargo xtask dev-mcp` or `cargo xtask mcp` to set one up
- **Dev daemon not running** → Inkwell starts it automatically; for manual control use `cargo xtask dev-daemon`

See `contributing/development.md` for the full MCP development workflow.

## Contributing Guidelines

See the `contributing/` directory for detailed guides:
- `contributing/architecture.md` - Runtime architecture principles (daemon, state, sync)
- `contributing/build-dependencies.md` - Build dependency graph
- `contributing/development.md` - Development workflow and build commands
- `contributing/e2e.md` - End-to-end testing guide
- `contributing/environments.md` - Environment management architecture
- `contributing/frontend-architecture.md` - Frontend code organization (src/ vs apps/)
- `contributing/iframe-isolation.md` - Security architecture for output isolation
- `contributing/logging.md` - Logging conventions
- `contributing/nteract-elements.md` - Working with nteract/elements registry
- `contributing/protocol.md` - Wire protocol between clients and daemon
- `contributing/releasing.md` - Versioning scheme, release procedures, tag conventions
- `contributing/runtimed.md` - Daemon development guide
- `contributing/testing.md` - Testing guide (Vitest, Rust, Hone, Python, E2E)
- `contributing/typescript-bindings.md` - ts-rs type generation from Rust
- `contributing/ui.md` - UI components and shadcn setup
- `contributing/widget-development.md` - Widget system internals

## Runtime Daemon (`runtimed`)

The notebook app connects to a background daemon (`runtimed`) that manages prewarmed environments, settings sync, and notebook document sync. The daemon runs as a system service (`io.nteract.runtimed` on macOS).

**Important:** The daemon is a separate process from the notebook app. When you change code in `crates/runtimed/`, the running daemon still uses the old binary until you reinstall it. This is a common source of "it works in tests but not in the app" confusion.

### Per-Worktree Daemon Isolation (Development)

Each git worktree can run its own isolated daemon during development, preventing daemon restarts in one worktree from affecting others.

**Important:** In dev mode, the Tauri app does NOT auto-install the system daemon. You must start the dev daemon yourself first.

**Conductor Users (automatic):** If you're using Conductor, dev mode is enabled automatically. Each workspace gets its own daemon:

```bash
# Terminal 1: Start dev daemon (keeps running)
cargo xtask dev-daemon

# Terminal 2: Run the notebook app
cargo xtask notebook         # Notebook connects to workspace daemon
# or
cargo xtask dev              # One-shot setup + daemon + app
runt daemon status           # Shows workspace info
runt daemon list-worktrees   # See all workspace daemons
```

**Non-Conductor Users (manual opt-in):** Set `RUNTIMED_DEV=1` to enable per-worktree isolation:

```bash
# Terminal 1
RUNTIMED_DEV=1 cargo xtask dev-daemon

# Terminal 2
RUNTIMED_DEV=1 cargo xtask notebook
RUNTIMED_DEV=1 runt daemon status
```

Per-worktree state is stored in:
- macOS: `~/Library/Caches/runt-nightly/worktrees/{hash}/`
- Linux: `~/.cache/runt-nightly/worktrees/{hash}/`

### Do NOT Use pkill or killall

**Never** use `pkill runtimed`, `killall runtimed`, or similar commands to stop the daemon. These commands kill **all** runtimed processes system-wide, disrupting other agents and worktrees.

Use the proper commands instead:
- `./target/debug/runt daemon stop` — stops only your worktree's daemon
- `cargo xtask install-daemon` — gracefully reinstalls the system daemon

### Agent Access to Dev Daemon (Conductor Workspaces)

When working in a Conductor workspace developing nteract/desktop, the xtask commands translate Conductor's environment variables to runtimed-specific ones:

| Conductor Variable | Translated To | Purpose |
|-------------------|---------------|---------|
| `CONDUCTOR_WORKSPACE_PATH` | `RUNTIMED_WORKSPACE_PATH` | Daemon state isolated to `<cache>/runt-nightly/worktrees/{hash}/` |
| `CONDUCTOR_PORT` | (used directly) | Vite dev server port (avoids conflicts between workspaces) |

**Important:** The translation only happens when running `cargo xtask dev`, `cargo xtask notebook`, or `cargo xtask dev-daemon`. This allows using Conductor for unrelated projects without interfering with the system daemon.

**Interacting with the daemon:**

Use `./target/debug/runt` to interact with the worktree daemon. When started via `cargo xtask dev-daemon`, the daemon receives `RUNTIMED_WORKSPACE_PATH` and uses per-worktree isolation.

```bash
# Check daemon status and pool info
./target/debug/runt daemon status

# Tail daemon logs (useful for debugging kernel issues)
./target/debug/runt daemon logs -f

# List all running kernels
./target/debug/runt ps

# List open notebooks with kernel and peer info
./target/debug/runt notebooks

# Flush and rebuild environment pools
./target/debug/runt daemon flush
```

### Machine-Readable Status (`--json`)

For scripts that need daemon configuration programmatically:

```bash
# Get socket path for RUNTIMED_SOCKET_PATH
./target/debug/runt daemon status --json | jq -r .socket_path

# Get blob server URL (when daemon is running)
./target/debug/runt daemon status --json | jq -r .blob_url

# Get all computed paths
./target/debug/runt daemon status --json | jq '.paths'

# Get environment variables
./target/debug/runt daemon status --json | jq '.env'

# Check if daemon is running
./target/debug/runt daemon status --json | jq -r .running
```

The JSON output includes computed paths, environment variables, pool targets, and blob server URL — everything needed to configure dev scripts without manual computation.

**Why `./target/debug/runt`?** The debug binary is built with `RUNTIMED_WORKSPACE_PATH` in its environment (via xtask), so it connects to the worktree daemon. A system-installed `runt` connects to the system daemon instead.

**Where state lives in dev mode** (macOS: `~/Library/Caches/`, Linux: `~/.cache/`):
```
<cache>/runt-nightly/worktrees/{hash}/
├── runtimed.sock      # Unix socket for IPC
├── runtimed.log       # Daemon logs
├── daemon.json        # PID, version, endpoint info
├── daemon.lock        # Singleton lock
├── envs/              # Prewarmed environments
├── blobs/             # Content-addressed blob store
└── notebook-docs/     # Automerge notebook docs
```

### System Service (Production)

For production use, install the daemon as a system service:

```bash
# Reinstall daemon with your changes (builds release, stops old, copies, restarts)
cargo xtask install-daemon
```

`cargo xtask dev`, `cargo xtask notebook`, and `cargo xtask build` do **not** reinstall the daemon. If you're changing daemon code (settings, sync, environments), you must run `cargo xtask install-daemon` separately to test your changes.

For faster iteration when only changing Rust code, use `cargo xtask build --rust-only` to skip frontend rebuild (requires an initial `cargo xtask build` first).

See `docs/runtimed.md` for service management and troubleshooting.

### Daemon Logs

The daemon logs to:
```
~/Library/Caches/runt/runtimed.log  (macOS)
~/.cache/runt/runtimed.log          (Linux)
```

In dev mode, logs are at `<cache>/runt-nightly/worktrees/{hash}/runtimed.log` (macOS: `~/Library/Caches/`, Linux: `~/.cache/`).

To check daemon logs:
```bash
runt daemon logs -n 100    # Last 100 lines
runt daemon logs -f        # Follow/tail logs
```

To check which daemon version is running:
```bash
runt daemon status
```

## Notebook Document Architecture (Local-First)

The notebook uses a local-first CRDT architecture. The frontend owns its own Automerge document via WASM, making cell mutations instant. Two Automerge peers participate:

- **Frontend (WASM)** — `NotebookHandle` from `crates/runtimed-wasm`, loaded in the webview. Cell mutations (add, delete, move, edit source) execute locally in WASM. The WASM starts with an empty doc (`create_empty()`); the sync protocol delivers all state from the daemon.
- **Daemon** — `NotebookDoc` from `crates/notebook-doc/src/lib.rs`. Canonical doc for kernel execution, output writing, and persistence.

The **Tauri relay** (`NotebookSyncClient` in `crates/runtimed/src/notebook_sync_client.rs`) is a transparent byte pipe — it forwards raw Automerge sync frames between the WASM and the daemon without merging or maintaining its own doc replica. A non-pipe "full peer" mode exists for `runtimed-py` (Python bindings), where the relay does maintain a local doc replica — but this is not the Tauri path.

Cells are stored in an Automerge Map keyed by cell ID, with a `position` field (fractional index hex string) for ordering. `move_cell` updates only the position field — no delete/re-insert. `get_cells()` returns cells sorted by position with cell ID as tiebreaker.

Mutation flow: React → WASM `handle.add_cell_after()` → `handle.generate_sync_message()` → `sendFrame(frame_types.AUTOMERGE_SYNC, msg)` (binary IPC via `frame-types.ts` helper, no `Array.from`) → relay pipe → daemon.

Incoming sync: daemon → relay pipe → `notebook:frame` event → WASM `handle.receive_frame()` → demux by type byte → returns `FrameEvent[]` with `CellChangeset` → incremental cell updates → React state. The frontend calls `handle.generate_sync_reply()` on a 50ms debounce timer (`scheduleSyncReply`) to batch multiple inbound frames into a single outbound reply. Broadcasts and presence are re-emitted via an in-memory frame bus (`notebook-frame-bus.ts`) for downstream hooks.

The `runtimed-wasm` crate compiles from the same `automerge = "0.7"` as the daemon. This is critical — the JS `@automerge/automerge` package creates `Object(Text)` CRDTs for all string fields, but Rust uses scalar `Str` for metadata fields (`id`, `cell_type`, `execution_count`). Using the same Rust code in WASM guarantees schema compatibility.

**Important:** Like the daemon binary, `runtimed-wasm` is a separate build artifact. Changes to `crates/runtimed-wasm/` require rebuilding with `wasm-pack build crates/runtimed-wasm --target web --out-dir ../../apps/notebook/src/wasm/runtimed-wasm` and committing the output. The WASM artifacts are committed to the repo so developers don't need wasm-pack installed for normal development.

### State Ownership

The frontend and daemon both write to the same CRDT. The convention of who writes what is a protocol rule:

| State | Writer | Notes |
|-------|--------|-------|
| Cell source (`Text` CRDT) | Frontend WASM | Local-first, character-level merge |
| Cell position, type, metadata | Frontend WASM | User-initiated via UI |
| Notebook metadata (deps, runtime) | Frontend WASM | User edits deps, runtime picker |
| Cell outputs (manifest hashes) | Daemon | Kernel IOPub → blob store → hash in doc |
| Execution count | Daemon | Set on `execute_input` from kernel |
| Widget state | Daemon (via `CommState`) | Kernel `comm_open`/`comm_msg` — see #761 for plans to move to `doc.comms` |

**Reads are free for both sides.** The daemon reads cell source from the doc for execution. The frontend reads outputs from the doc for rendering. Both use the same Automerge sync to stay current.

### Incremental Sync Pipeline

The sync pipeline avoids full-notebook re-reads on every change. Each layer is delta-aware:

1. **WASM `receive_frame()`** — applies sync message, computes a `CellChangeset` by walking `doc.diff(before, after)` patches. Returns field-level flags per changed cell (`source`, `outputs`, `execution_count`, `metadata`, `position`). Cost is O(delta), not O(doc).

2. **`scheduleMaterialize()`** — coalesces multiple sync frames within a 32ms window via `mergeChangesets()`. Dispatches:
   - Structural changes (cells added/removed/reordered) → full materialization
   - Output changes → per-cell cache-aware resolution: cache hits use `materializeCellFromWasm()` (fast sync path); cache misses resolve just that cell's outputs async via `resolveOutput()`, not the full document
   - Source/metadata/execution_count only → per-cell `materializeCellFromWasm()` via O(1) WASM accessors

3. **Split cell store** — `Map<id, NotebookCell>` with per-cell subscriptions. `useCell(id)` re-renders only when that specific cell changes. `useCellIds()` re-renders only on structural changes.

4. **Debounced outbound sync** — source edits batch keystrokes via `debouncedSyncToRelay` (20ms). `flushSync()` fires immediately before execute/save.

**Per-cell WASM accessors** (O(1) Automerge map lookups, no full doc serialization):
- `get_cell_source(id)`, `get_cell_type(id)`, `get_cell_outputs(id)`, `get_cell_execution_count(id)`, `get_cell_metadata(id)`, `get_cell_position(id)`
- `get_cell_ids()` — position-sorted IDs (O(n log n) sort, reads only position strings, skips source/outputs/metadata)

The `CellChangeset` and diff logic live in `crates/notebook-doc/src/diff.rs`. The TypeScript mirror types are defined inline in `apps/notebook/src/hooks/useAutomergeNotebook.ts` (module-private; tests duplicate them in `apps/notebook/src/lib/__tests__/cell-changeset.test.ts`).

Full materialization (structural changes, initial load) still uses `handle.get_cells_json()` for bulk serialization. The per-cell path is for incremental updates only.

### Crate Boundaries

Three crates have "notebook" in the name. They have distinct roles:

| Crate | What it owns | Used by |
|-------|-------------|---------|
| `notebook-doc` | Automerge document schema and operations (`NotebookDoc`), cell CRUD, output writes, fractional indexing, `CellChangeset` diffing, presence encoding, frame type constants | daemon, WASM, `notebook-protocol`, `notebook-sync`, Python bindings |
| `notebook-protocol` | Wire protocol types (`NotebookRequest`, `NotebookResponse`, `NotebookBroadcast`, `CommSnapshot`), connection handshake, frame parsing | daemon, `notebook-sync`, Python bindings |
| `notebook-sync` | Sync infrastructure (`DocHandle`), snapshot watch channel, per-cell accessors for Python clients, sync task management | Tauri app crate (`notebook`), Python bindings (`runtimed-py`) |

**Rule of thumb:** If you're changing the document schema or cell operations → `notebook-doc`. If you're adding a new request/response/broadcast type → `notebook-protocol`. If you're changing how Python clients sync → `notebook-sync`.

The Tauri app crate (`crates/notebook/`) is the glue — it wires Tauri commands to daemon requests and manages the socket relay. It does not own protocol types or document operations.

### Blob Store and Output Manifests

Large binary outputs (images, plots, HTML) are stored in a content-addressed blob store, not inline in the CRDT. The Automerge doc carries only 64-char SHA-256 hashes.

**Two-tier indirection:**
1. **Cell outputs list** → manifest hashes (64-char hex strings in `doc.cells[id].outputs`)
2. **Manifest** (JSON in blob store) → `ContentRef` per MIME type: `{"inline": "<data>"}` for ≤8KB, `{"blob": "<hash>", "size": N}` for >8KB

**Blob store location:** `~/.cache/runt/blobs/` (sharded by first 2 hex chars). Each blob has a `.meta` sidecar with `{media_type, size}`.

**Blob HTTP server:** `127.0.0.1:<dynamic-port>`, serves `GET /blob/{hash}`. The port is included in the connection capabilities and available via `runt daemon status --json | jq -r .blob_url`.

**Frontend resolution:** `materialize-cells.ts` resolves manifest hashes → fetches manifest JSON from HTTP → resolves `ContentRef::Blob` entries → renders. An output cache deduplicates fetches.

**Key files:** `crates/runtimed/src/output_store.rs` (manifest creation, inlining threshold), `crates/runtimed/src/blob_store.rs` (content-addressed storage), `crates/runtimed/src/blob_server.rs` (HTTP server).

### Notebook Room Lifecycle

Each open notebook is a **room** on the daemon (`NotebookRoom` in `notebook_sync_server.rs`). Rooms are keyed by notebook ID (file path or UUID for untitled notebooks).

**Autosave:** The daemon autosaves `.ipynb` on a debounce (2s quiet period, 10s max interval). No user action required. The `NotebookAutosaved` broadcast clears the frontend's dirty flag. Explicit Cmd+S still works and additionally runs cell formatting.

**Room re-keying:** When an untitled notebook (UUID room) is first saved to a file path, `rekey_ephemeral_room()` atomically re-keys the room in the HashMap, spawns a file watcher, cleans up the old persist file, and broadcasts `RoomRenamed` so all peers update their `notebook_id`.

**Crash recovery:** Untitled notebooks are persisted to `notebook-docs/{hash}.automerge` in the cache directory. On daemon restart, the room loads from this file. Saved notebooks reload from `.ipynb`. Before deleting a persisted doc (on reopen), the daemon snapshots it to `notebook-docs/snapshots/`. `runt recover` can export any snapshot to `.ipynb`.

**Multi-window:** Multiple windows can open the same notebook. Each connects as a separate Automerge peer to the same room. The first window gets a deterministic label (for geometry persistence); additional windows get a UUID suffix. All peers receive the same sync frames and broadcasts.

**Eviction:** When all peers disconnect, a delayed eviction task runs (configurable via `keep_alive_secs` setting, default 30s). If no peers reconnect within the window, the kernel is shut down and the room is removed.

### Widget State (Current Architecture)

Widget state currently lives **outside** the Automerge doc, in parallel in-memory stores:
- **Daemon:** `CommState` in `comm_state.rs` — tracks all active Jupyter comm channels (ipywidgets), maintains output capture routing for Output widgets
- **Frontend:** `WidgetStore` in `src/components/widgets/widget-store.ts` — per-model subscriptions, IPY_MODEL_ reference resolution, custom message buffering

New clients receive a `CommSync` broadcast (snapshot of all active widgets) when they connect. Widget messages flow as `NotebookBroadcast::Comm` events, not document mutations.

**Planned:** Move widget state into `doc.comms/` in the Automerge document (#761). This eliminates `CommSync`, simplifies Output widget routing, and means new clients get widget state via normal CRDT sync. See the phased plan in #808–#811.

See `contributing/widget-development.md` for the widget rendering architecture and `contributing/protocol.md` for the wire protocol details.

### Settings Sync

Settings (theme, default_runtime, default_python_env, keep_alive_secs, etc.) are synced via a **separate Automerge document** — not the notebook doc. The daemon holds the canonical copy and persists to disk.

Any window can write a setting; all other windows receive the change via Automerge sync. The frontend connects with a `SettingsSync` handshake on the same Unix socket. Frontend falls back to a local `settings.json` if the daemon is unavailable.

Settings types: `crates/runtimed/src/settings_doc.rs`. Frontend hook: `src/hooks/useSyncedSettings.ts`.

## Environment Management

Runt supports multiple environment backends (UV, Conda) and project file formats (pyproject.toml, environment.yml, pixi.toml). See `contributing/environments.md` for the full architecture and `docs/environments.md` for the user-facing guide.

### Detection Priority Chain

Kernel launching uses a two-stage detection:

**Stage 1: Runtime Detection** (Python vs Deno)

The daemon reads the notebook's kernelspec to determine runtime type:
1. **Notebook kernelspec** — `metadata.kernelspec.name == "deno"` → Deno kernel; contains "python" → Python kernel
2. **Fallback checks** — `kernelspec.language` or `language_info.name` == "typescript" → Deno
3. **User setting** — `default_runtime` preference for new/unknown notebooks

**Key invariant**: The notebook's encoded kernelspec takes priority over project files. A Deno notebook in a directory with `pyproject.toml` will launch a Deno kernel, allowing Python and Deno notebooks to coexist in the same project directory.

**Stage 2: Python Environment Resolution**

For Python notebooks, the daemon resolves the environment:
1. **Inline deps in notebook metadata** (uv or conda) — use those directly
2. **Closest project file** — single walk-up from the notebook directory, checking for `pyproject.toml`, `pixi.toml`, and `environment.yml` at each level. The first (closest) match wins. Same-directory tiebreaker: pyproject.toml > pixi.toml > environment.yml
3. **No project file** — use prewarmed env from pool (UV or Conda based on `default_python_env` setting)

The walk-up stops at `.git` boundaries and the home directory, preventing cross-repository project file pollution.

**Deno Kernel Launching**

Deno kernels don't use environment pools. The daemon:
1. Gets deno via `kernel_launch::tools::get_deno_path()` (PATH first, then bootstrap from conda-forge)
2. Launches: `deno jupyter --kernel --conn <connection_file>`

### Environment Source Labels

The backend returns an `env_source` string with the `KernelLaunched` response (via `notebook:broadcast`) so the frontend can display the environment origin.

- `"uv:inline"` / `"uv:pyproject"` / `"uv:prewarmed"`
- `"conda:inline"` / `"conda:env_yml"` / `"conda:pixi"` / `"conda:prewarmed"`

### Inline Dependency Environments

For notebooks with inline UV dependencies (`metadata.runt.uv.dependencies`), the daemon creates **cached environments** in `~/.cache/runt/inline-envs/`. Environments are keyed by a hash of the sorted dependencies, enabling fast reuse:

```
~/.cache/runt/inline-envs/
  inline-a1b2c3d4/    # Hash of ["requests"]
  inline-e5f6g7h8/    # Hash of ["pandas", "numpy"]
```

**Flow:**
1. `notebook_sync_server.rs` detects `uv:inline` from trusted notebook metadata
2. Calls `inline_env::prepare_uv_inline_env(deps)` which returns cached env or creates new one
3. Kernel launches with the cached env's Python

**Cache hit = instant startup.** First launch with new deps takes time to `uv venv` + `uv pip install`.

### Adding a New Project File Format

Follow the pattern established by `environment_yml.rs` and `pixi.rs`:

1. Create `crates/notebook/src/{format}.rs` with `find_{format}()` (directory walk) and `parse_{format}()` functions
2. Add Tauri commands in `lib.rs`: `detect_{format}`, `get_{format}_dependencies`, `import_{format}_dependencies`
3. Wire detection into the daemon's `auto_launch_kernel()` in `notebook_sync_server.rs` at the correct priority position
4. Add frontend detection in `useDaemonKernel.ts` and `useCondaDependencies.ts` or `useDependencies.ts`
5. Add test fixture in `crates/notebook/fixtures/audit-test/`

### Trust System

Dependencies are signed with HMAC-SHA256 using a per-machine key at `~/.config/runt/trust-key`. The signature covers `metadata.uv` and `metadata.conda` only (not cell contents or outputs). Shared notebooks are always untrusted on a new machine because the key is machine-specific. If you change the dependency metadata structure, you must update the `crates/runt-trust/` crate (the `crates/notebook/src/trust.rs` file just re-exports from it).

### Key Files

| File | Role |
|------|------|
| `crates/kernel-launch/src/lib.rs` | Shared kernel launching API |
| `crates/kernel-launch/src/tools.rs` | Tool bootstrapping (deno, uv, ruff) via rattler |
| `crates/runtimed/src/notebook_sync_server.rs` | `NotebookRoom`, `auto_launch_kernel()`, room lifecycle, autosave, re-keying |
| `crates/runtimed/src/kernel_manager.rs` | `RoomKernel::launch()` — spawns kernel, IOPub handler, output routing |
| `crates/runtimed/src/comm_state.rs` | Widget comm state tracking + Output widget capture routing |
| `crates/runtimed/src/output_store.rs` | Output manifest creation, blob inlining threshold |
| `crates/runtimed/src/blob_store.rs` | Content-addressed blob storage |
| `crates/runtimed/src/inline_env.rs` | Cached environment creation for inline deps (UV and Conda) |
| `crates/runtimed/src/settings_doc.rs` | Settings Automerge doc schema and persistence |
| `crates/notebook/src/lib.rs` | Tauri commands, Automerge sync pipe (transparent byte relay between WASM and daemon) |
| `crates/notebook/src/project_file.rs` | Unified closest-wins project file detection |
| `crates/notebook/src/uv_env.rs` | UV environment creation and caching |
| `crates/notebook/src/conda_env.rs` | Conda environment creation via rattler |
| `crates/notebook/src/pyproject.rs` | pyproject.toml discovery and parsing |
| `crates/notebook/src/pixi.rs` | pixi.toml discovery and parsing |
| `crates/notebook/src/environment_yml.rs` | environment.yml discovery and parsing |
| `crates/notebook/src/deno_env.rs` | Deno config detection and version checking |
| `crates/notebook/src/trust.rs` | HMAC trust verification (re-exports from `runt-trust`) |
| `crates/notebook-doc/src/lib.rs` | `NotebookDoc` — Automerge schema, cell CRUD, output writes, per-cell accessors |
| `crates/notebook-doc/src/diff.rs` | `CellChangeset` — structural diff from Automerge patches |
| `crates/notebook-doc/src/presence.rs` | CBOR-encoded ephemeral presence (cursors, selections, kernel state) |
| `crates/notebook-protocol/src/protocol.rs` | Wire types: `NotebookRequest`, `NotebookResponse`, `NotebookBroadcast`, `CommSnapshot` |
| `crates/notebook-sync/src/handle.rs` | `DocHandle` — sync infrastructure + per-cell accessors for Python clients |
| `crates/runtimed-wasm/src/lib.rs` | WASM bindings — cell mutations, sync, per-cell accessors, `CellChangeset` |
| `apps/notebook/src/hooks/useAutomergeNotebook.ts` | WASM handle owner, `scheduleMaterialize`, debounced sync, `CellChangeset` dispatch |
| `apps/notebook/src/hooks/useDaemonKernel.ts` | Kernel execution, status broadcasts, widget comm routing |
| `apps/notebook/src/hooks/useDependencies.ts` | Frontend UV dependency management |
| `apps/notebook/src/hooks/useCondaDependencies.ts` | Frontend conda dependency management |
| `apps/notebook/src/lib/materialize-cells.ts` | `materializeCellFromWasm()` (per-cell) + `cellSnapshotsToNotebookCells()` (full) |
| `apps/notebook/src/lib/notebook-cells.ts` | Split cell store — `useCell(id)`, `useCellIds()`, per-cell subscriptions |
| `apps/notebook/src/lib/notebook-frame-bus.ts` | In-memory sync pub/sub for broadcasts and presence (no Tauri event hop) |
| `apps/notebook/src/lib/frame-types.ts` | Frame type constants + `sendFrame()` binary IPC helper |
| `src/components/widgets/widget-store.ts` | `WidgetStore` — per-model subscriptions, IPY_MODEL_ resolution |
