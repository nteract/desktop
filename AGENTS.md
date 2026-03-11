# Agent Instructions

This document provides guidance for AI agents working in this repository.

## Code Formatting (Required Before Committing)

Run these commands before every commit. CI will reject PRs that fail formatting checks.

```bash
# Format Rust code
cargo fmt

# Format and lint TypeScript/JavaScript (auto-fixes issues)
npx @biomejs/biome check --fix apps/notebook/src/ e2e/
```

Do not skip these. There are no pre-commit hooks — you must run them manually.

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

## MCP Server

For programmatic notebook interaction, use the [nteract MCP server](https://github.com/nteract/nteract) (`nteract` on PyPI).

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
cargo xtask dev              # Notebook connects to workspace daemon
runt daemon status           # Shows workspace info
runt daemon list-worktrees   # See all workspace daemons
```

**Non-Conductor Users (manual opt-in):** Set `RUNTIMED_DEV=1` to enable per-worktree isolation:

```bash
# Terminal 1
RUNTIMED_DEV=1 cargo xtask dev-daemon

# Terminal 2
RUNTIMED_DEV=1 cargo xtask dev
RUNTIMED_DEV=1 runt daemon status
```

Per-worktree state is stored in `~/.cache/runt/worktrees/{hash}/`.

### Do NOT Use pkill or killall

**Never** use `pkill runtimed`, `killall runtimed`, or similar commands to stop the daemon. These commands kill **all** runtimed processes system-wide, disrupting other agents and worktrees.

Use the proper commands instead:
- `./target/debug/runt daemon stop` — stops only your worktree's daemon
- `cargo xtask install-daemon` — gracefully reinstalls the system daemon

### Agent Access to Dev Daemon (Conductor Workspaces)

When working in a Conductor workspace developing nteract/desktop, the xtask commands translate Conductor's environment variables to runtimed-specific ones:

| Conductor Variable | Translated To | Purpose |
|-------------------|---------------|---------|
| `CONDUCTOR_WORKSPACE_PATH` | `RUNTIMED_WORKSPACE_PATH` | Daemon state isolated to `~/.cache/runt/worktrees/{hash}/` |
| `CONDUCTOR_WORKSPACE_NAME` | `RUNTIMED_WORKSPACE_NAME` | Human-readable workspace name for display |
| `CONDUCTOR_PORT` | (used directly) | Vite dev server port (avoids conflicts between workspaces) |

**Important:** The translation only happens when running `cargo xtask dev` or `cargo xtask dev-daemon`. This allows using Conductor for unrelated projects without interfering with the system daemon.

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

**Why `./target/debug/runt`?** The debug binary is built with `RUNTIMED_WORKSPACE_PATH` in its environment (via xtask), so it connects to the worktree daemon. A system-installed `runt` connects to the system daemon instead.

**Where state lives in dev mode:**
```
~/.cache/runt/worktrees/{hash}/
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

`cargo xtask dev` and `cargo xtask build` do **not** reinstall the daemon. If you're changing daemon code (settings, sync, environments), you must run `cargo xtask install-daemon` separately to test your changes.

For faster iteration when only changing Rust code, use `cargo xtask build --rust-only` to skip frontend rebuild (requires an initial `cargo xtask build` first).

See `docs/runtimed.md` for service management and troubleshooting.

### Daemon Logs

The daemon logs to:
```
~/Library/Caches/runt/runtimed.log  (macOS)
~/.cache/runt/runtimed.log          (Linux)
```

In dev mode, logs are at `~/.cache/runt/worktrees/{hash}/runtimed.log`.

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

- **Frontend (WASM)** — `NotebookHandle` from `crates/runtimed-wasm`, loaded in the webview. Cell mutations (add, delete, move, edit source) execute locally in WASM. React state is derived from the WASM doc via `handle.get_cells_json()`. The WASM starts with an empty doc (`create_empty()`); the sync protocol delivers all state from the daemon.
- **Daemon** — `NotebookDoc` from `crates/notebook-doc/src/lib.rs` (re-exported by `crates/runtimed/src/lib.rs`). Canonical doc for kernel execution, output writing, and persistence.

The **Tauri relay** (`NotebookSyncClient` in `crates/runtimed/src/notebook_sync_client.rs`) is a transparent byte pipe — it forwards raw Automerge sync frames between the WASM and the daemon without merging or maintaining its own doc replica. The daemon's `peer_state` tracks the WASM peer directly through the pipe. A non-pipe "full peer" mode exists for `runtimed-py` (Python bindings), where the relay does maintain a local doc replica — but this is not the Tauri path.

Cells are stored in an Automerge Map keyed by cell ID, with a `position` field (fractional index hex string) for ordering. `move_cell` updates only the position field — no delete/re-insert. `get_cells()` returns cells sorted by position with cell ID as tiebreaker.

Mutation flow: React → WASM `handle.add_cell_after()` → `handle.generate_sync_message()` → `invoke("send_automerge_sync")` → relay pipe → daemon.

Incoming sync: daemon → relay pipe → `automerge:from-daemon` event → WASM `handle.receive_sync_message()` → `materializeCells()` → React state.

The `runtimed-wasm` crate compiles from the same `automerge = "0.7"` as the daemon. This is critical — the JS `@automerge/automerge` package creates `Object(Text)` CRDTs for all string fields, but Rust uses scalar `Str` for metadata fields (`id`, `cell_type`, `execution_count`). Using the same Rust code in WASM guarantees schema compatibility.

**Important:** Like the daemon binary, `runtimed-wasm` is a separate build artifact. Changes to `crates/runtimed-wasm/` require rebuilding with `wasm-pack build crates/runtimed-wasm --target web --out-dir ../../apps/notebook/src/wasm/runtimed-wasm` and committing the output. The WASM artifacts are committed to the repo so developers don't need wasm-pack installed for normal development.

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

The backend returns an `env_source` string with the `KernelLaunched` response (via `daemon:broadcast`) so the frontend can display the environment origin. Values:

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

Dependencies are signed with HMAC-SHA256 using a per-machine key at `~/.config/runt/trust-key`. The signature covers `metadata.uv` and `metadata.conda` only (not cell contents or outputs). Shared notebooks are always untrusted on a new machine because the key is machine-specific. If you change the dependency metadata structure, you must update `crates/notebook/src/trust.rs`.

### Key Files

| File | Role |
|------|------|
| `crates/kernel-launch/src/lib.rs` | Shared kernel launching API |
| `crates/kernel-launch/src/tools.rs` | Tool bootstrapping (deno, uv, ruff) via rattler |
| `crates/runtimed/src/notebook_sync_server.rs` | `auto_launch_kernel()` — runtime detection and environment resolution |
| `crates/runtimed/src/kernel_manager.rs` | `RoomKernel::launch()` — spawns Python or Deno kernel processes |
| `crates/runtimed/src/inline_env.rs` | Cached environment creation for inline deps (UV and Conda) |
| `crates/notebook/src/lib.rs` | Tauri commands (save, format, kernel, env), Automerge sync pipe (forwards raw frames between WASM and daemon). Cell mutation commands removed — mutations go through WASM. |
| `crates/notebook/src/project_file.rs` | Unified closest-wins project file detection |
| `crates/notebook/src/uv_env.rs` | UV environment creation and caching |
| `crates/notebook/src/conda_env.rs` | Conda environment creation via rattler |
| `crates/notebook/src/pyproject.rs` | pyproject.toml discovery and parsing |
| `crates/notebook/src/pixi.rs` | pixi.toml discovery and parsing |
| `crates/notebook/src/environment_yml.rs` | environment.yml discovery and parsing |
| `crates/notebook/src/deno_env.rs` | Deno config detection and version checking |
| `crates/notebook/src/trust.rs` | HMAC trust verification |
| `crates/runtimed-wasm/src/lib.rs` | WASM bindings for NotebookDoc — cell mutations, sync messages |
| `crates/notebook-doc/src/lib.rs` | Shared Automerge document operations (`NotebookDoc`) used by daemon and WASM bindings |
| `apps/notebook/src/hooks/useAutomergeNotebook.ts` | Local-first notebook hook — owns NotebookHandle WASM, drives React cell state, sync-only bootstrap (empty doc, no GetDocBytes) |
| `apps/notebook/src/hooks/useDaemonKernel.ts` | Daemon-owned kernel execution, status broadcasts, environment sync |
| `apps/notebook/src/hooks/useDependencies.ts` | Frontend UV dependency management |
| `apps/notebook/src/hooks/useCondaDependencies.ts` | Frontend conda dependency management |
| `apps/notebook/src/lib/materialize-cells.ts` | Converts CellSnapshot[] from WASM/sync to NotebookCell[] for React (resolves blob manifests) |
