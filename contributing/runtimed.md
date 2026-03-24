# Runtime Daemon (runtimed)

The runtime daemon manages prewarmed Python environments, notebook document sync, kernel execution, autosave, and widget state across notebook windows.

## Quick Reference

| Task | Command |
|------|---------|
| Install daemon from source | `cargo xtask install-daemon` |
| Run daemon | `cargo run -p runtimed` |
| Run with debug logs | `RUST_LOG=debug cargo run -p runtimed` |
| Check status | `cargo run -p runt-cli -- daemon status` |
| Ping daemon | `cargo run -p runt-cli -- daemon ping` |
| View logs | `cargo run -p runt-cli -- daemon logs -f` |
| Run tests | `cargo test -p runtimed` |

## Why It Exists

Each notebook window is a separate OS process (Tauri spawns via `spawn_new_notebook()` in `crates/notebook/src/lib.rs`). Without coordination:

1. **Race conditions**: Multiple windows try to claim the same prewarmed environment
2. **Wasted resources**: Each window creates its own pool of environments
3. **Slow cold starts**: First notebook waits for environment creation

The daemon provides a single coordinating entity that prewarms environments in the background and hands them out to windows on request.

## Architecture

```
┌──────────────────┐   ┌──────────────────┐   ┌──────────────────┐
│  Notebook Win 1  │   │  Notebook Win 2  │   │  Notebook Win N  │
│  (Tauri process) │   │  (Tauri process) │   │  (Tauri process) │
└────────┬─────────┘   └────────┬─────────┘   └────────┬─────────┘
         │                      │                      │
         │     Unix Socket      │     Unix Socket      │
         └──────────┬───────────┴───────────┬──────────┘
                    │                       │
                    ▼                       ▼
              ┌─────────────────────────────────┐
              │            runtimed             │
              │      (singleton daemon)         │
              │                                 │
              │  ┌──────────┐  ┌──────────────┐ │
              │  │ UV Pool  │  │  Conda Pool  │ │
              │  │ (3 envs) │  │   (3 envs)   │ │
              │  └──────────┘  └──────────────┘ │
              └─────────────────────────────────┘
```

**Key components:**

| Component | Purpose | Location |
|-----------|---------|----------|
| Unix socket | IPC endpoint | `~/Library/Caches/<cache_namespace>/runtimed.sock` (macOS) / `~/.cache/<cache_namespace>/runtimed.sock` (Linux) |
| Lock file | Singleton guarantee | `~/Library/Caches/<cache_namespace>/daemon.lock` (macOS) / `~/.cache/<cache_namespace>/daemon.lock` (Linux) |
| Info file | Discovery (PID, endpoint) | `~/Library/Caches/<cache_namespace>/daemon.json` (macOS) / `~/.cache/<cache_namespace>/daemon.json` (Linux) |
| Environments | Prewarmed venvs | `~/Library/Caches/<cache_namespace>/envs/` (macOS) / `~/.cache/<cache_namespace>/envs/` (Linux) |
| Blob store | Content-addressed outputs | `~/Library/Caches/<cache_namespace>/blobs/` (macOS) / `~/.cache/<cache_namespace>/blobs/` (Linux) |
| Notebook docs | Persisted Automerge docs | `~/Library/Caches/<cache_namespace>/notebook-docs/` (macOS) / `~/.cache/<cache_namespace>/notebook-docs/` (Linux) |
| Snapshots | Pre-delete safety copies | `~/Library/Caches/<cache_namespace>/notebook-docs/snapshots/` (macOS) / `~/.cache/<cache_namespace>/notebook-docs/snapshots/` (Linux) |

`<cache_namespace>` is `runt` for stable builds and `runt-nightly` for nightly builds. Source builds default to nightly unless `RUNT_BUILD_CHANNEL=stable`.

## Development Workflow

### Default: Let the notebook start it

The notebook app automatically tries to connect to or start the daemon on launch. If it's not running, the app falls back to in-process prewarming. You don't need to do anything special.

The notebook app calls `ensure_daemon_via_sidecar()` (a private function in `crates/notebook/src/lib.rs`) which takes a `tauri::AppHandle` and a progress callback to start and connect to the daemon.

### Install daemon from source

When you change daemon code and want the installed service to pick it up:

```bash
cargo xtask install-daemon
```

This builds runtimed in release mode, stops the running service, replaces the binary, and restarts it. You can verify the version with:

```bash
cargo run -p runt-cli -- daemon status --json | jq -r '.daemon_info.version'
```

### Fast iteration: Daemon + bundled notebook

When iterating on daemon code, you often want to test changes in the notebook app without rebuilding the frontend.

**With Inkwell supervisor** (if you have `supervisor_*` MCP tools — e.g. in Zed):

The supervisor manages the dev daemon for you. No env vars or extra terminals needed.

- `supervisor_restart(target="daemon")` — start or restart the dev daemon after code changes
- `supervisor_rebuild` — rebuild Python bindings (`maturin develop`) + restart
- `supervisor_status` — check daemon status (`daemon_managed: true` confirms it's running)
- `supervisor_logs` — tail daemon logs
- `supervisor_start_vite` — start the Vite dev server for hot-reload

Then build and run the app normally:
```bash
cargo xtask build                 # Full build (includes frontend)
cargo xtask build --rust-only     # Fast rebuild (reuses frontend assets)
cargo xtask run                   # Run the bundled binary
```

**Without supervisor** (manual two-terminal workflow):

```bash
# Terminal 1: Run dev daemon (restart when you change daemon code)
cargo xtask dev-daemon

# Terminal 2: Build once, then iterate
cargo xtask build                 # Full build (includes frontend)
cargo xtask build --rust-only     # Fast rebuild (reuses frontend assets)
cargo xtask run                   # Run the bundled binary
```

The `--rust-only` flag skips `pnpm build`, reusing the existing frontend assets in `apps/notebook/dist/`. This is much faster when you're only changing Rust code.

### Stable vs nightly from source

Source-built binaries default to the nightly channel. That affects daemon cache/socket namespaces, CLI/app naming, and default app launch behavior. Only set `RUNT_BUILD_CHANNEL=stable` when you are intentionally validating the stable flow:

```bash
RUNT_BUILD_CHANNEL=stable cargo xtask dev-daemon
RUNT_BUILD_CHANNEL=stable cargo xtask build --rust-only
RUNT_BUILD_CHANNEL=stable cargo xtask run
RUNT_BUILD_CHANNEL=stable cargo xtask run-mcp
```

### Testing

```bash
# All tests (unit + integration)
cargo test -p runtimed

# Just integration tests
cargo test -p runtimed --test integration

# Specific test
cargo test -p runtimed test_daemon_ping_pong
```

Integration tests use temp directories for socket and lock files to avoid conflicts with a running daemon.

## Notebook Room Lifecycle

Each open notebook has a **room** (`NotebookRoom` in `notebook_sync_server.rs`), keyed by notebook ID (canonical file path or UUID for untitled notebooks).

### Autosave

The daemon autosaves `.ipynb` on a debounce (2s quiet period, 10s max interval) via `spawn_autosave_debouncer`. No user action required. `NotebookAutosaved` broadcast clears the frontend dirty flag. Explicit Cmd+S additionally runs cell formatting (ruff/deno fmt).

Autosave skips untitled notebooks (no file path) and notebooks mid-load (`is_loading` flag). After saving, the debouncer drains the change channel to detect mutations during the async write — the `NotebookAutosaved` broadcast only fires when the file is truly caught up.

### Room re-keying

When an untitled notebook (UUID room) is first saved, `rekey_ephemeral_room()`:
1. Canonicalizes the save path
2. Guards against overwriting an existing room
3. Re-keys the `NotebookRooms` HashMap (remove UUID, insert path)
4. Updates the room's `notebook_path` (`RwLock<PathBuf>`)
5. Deletes the old UUID-based persist file
6. Spawns a file watcher for the new path
7. Broadcasts `RoomRenamed { new_notebook_id }` so all peers update their local ID

The `NotebookSaved` response includes `new_notebook_id: Option<String>` for the re-key case.

### Crash recovery

Untitled notebooks persist their Automerge doc to `notebook-docs/{hash}.automerge`. Before deleting a persisted doc on reopen (saved notebooks reload from `.ipynb`), the daemon snapshots it to `notebook-docs/snapshots/` (max 5 per notebook hash).

`runt recover --list` scans all cache namespaces (stable, nightly, per-worktree). `runt recover <path>` finds the live doc or most recent snapshot and exports to `.ipynb`.

### Multi-window

Multiple windows join the same room as separate Automerge peers. The first window gets a deterministic label (for geometry persistence); additional windows get a UUID suffix. All peers receive sync frames and broadcasts independently.

### Eviction

When all peers disconnect, a delayed eviction task runs (configurable via `keep_alive_secs` setting, default 30s). If no peers reconnect, the kernel shuts down, the file watcher stops, and the room is removed. If peers reconnect during the window, eviction is cancelled.

## Per-Cell Accessors

`NotebookDoc` and `DocHandle` expose O(1) cell reads that avoid full-document materialization:

| Method | Returns | Used by |
|--------|---------|---------|
| `get_cell_source(id)` | `Option<String>` | Daemon (execution), Python SDK, WASM |
| `get_cell_type(id)` | `Option<String>` | MCP tools, WASM |
| `get_cell_outputs(id)` | `Option<Vec<String>>` | Python SDK output collection |
| `get_cell_execution_count(id)` | `Option<String>` | WASM materialization |
| `get_cell_metadata(id)` | `Option<Value>` | Python SDK, WASM |
| `get_cell_position(id)` | `Option<String>` | WASM, fractional index operations |
| `get_cell_ids()` | `Vec<String>` (position-sorted) | Daemon, Python SDK, WASM |

These are critical for performance — `get_cells()` materializes every cell's source, outputs, and metadata. Use per-cell accessors when you only need one cell or one field.

## Code Structure

```
crates/runtimed/
├── src/
│   ├── lib.rs                   # Public types, path helpers (default_socket_path, etc.)
│   ├── main.rs                  # CLI entry point (run, install, status, etc.)
│   ├── daemon.rs                # Daemon state, pool management, connection routing
│   ├── protocol.rs              # BlobRequest/BlobResponse + re-exports from notebook-protocol
│   ├── client.rs                # PoolClient for pool operations
│   ├── singleton.rs             # File-based locking for single instance
│   ├── service.rs               # Cross-platform service installation (launchd/systemd)
│   ├── settings_doc.rs          # Settings Automerge document, schema, migration
│   ├── sync_server.rs           # Settings sync handler
│   ├── sync_client.rs           # Settings sync client library
│   ├── notebook_sync_server.rs  # NotebookRoom, room lifecycle, autosave, re-keying, sync loop
│   ├── kernel_manager.rs        # RoomKernel: kernel lifecycle, execution queue, IOPub output routing
│   ├── kernel_pids.rs           # Kernel PID tracking and orphan reaping
│   ├── comm_state.rs            # Widget comm state + Output widget capture routing
│   ├── output_store.rs          # Output manifest creation, blob inlining threshold
│   ├── blob_store.rs            # Content-addressed blob store with metadata sidecars
│   ├── blob_server.rs           # HTTP read server for blobs (hyper 1.x)
│   ├── inline_env.rs            # Inline dependency environment caching (UV/Conda)
│   ├── project_file.rs          # Project file detection (pyproject.toml, pixi.toml, etc.)
│   ├── markdown_assets.rs       # Markdown image/asset resolution and rewriting
│   ├── stream_terminal.rs       # Stream terminal output handling (carriage return, ANSI)
│   ├── runtime.rs               # Runtime enum definition (Python/Deno/Other)
│   └── terminal_size.rs         # Terminal size tracking
└── tests/
    └── integration.rs           # Integration tests (daemon, pool, settings sync, notebook sync)
```

**Related crates** (shared across daemon, WASM, Python):

| Crate | What it owns |
|-------|-------------|
| `notebook-doc` | `NotebookDoc`: Automerge schema, cell CRUD, per-cell accessors, `CellChangeset` diffing |
| `notebook-protocol` | Wire types: `NotebookRequest`, `NotebookResponse`, `NotebookBroadcast`, `CommSnapshot` |
| `notebook-sync` | `DocHandle`: sync infrastructure, snapshot watch channel, per-cell accessors for Python |

For the full architecture (all phases, schemas, and design decisions), see [docs/runtimed.md](../docs/runtimed.md).

## Protocol

See [protocol.md](./protocol.md) for the full wire protocol specification covering:

- Connection handshake and lifecycle
- Frame format (length-prefixed, typed frames)
- Automerge sync messages
- Request/response protocol
- Broadcast messages

## CLI Commands (for testing)

The `runt` CLI has daemon subcommands for testing and service management:

```bash
# Service management
cargo run -p runt-cli -- daemon status        # Show service + pool statistics
cargo run -p runt-cli -- daemon status --json # JSON output
cargo run -p runt-cli -- daemon start         # Start the daemon service
cargo run -p runt-cli -- daemon stop          # Stop the daemon service
cargo run -p runt-cli -- daemon restart       # Restart the daemon service
cargo run -p runt-cli -- daemon logs -f       # Tail daemon logs
cargo run -p runt-cli -- daemon flush         # Flush pool and rebuild environments

# Debug/health checks
cargo run -p runt-cli -- daemon ping          # Check daemon is responding
cargo run -p runt-cli -- daemon shutdown      # Shutdown daemon via IPC
```

**Note:** In Conductor workspaces, use `./target/debug/runt` instead of `cargo run -p runt-cli --` for faster iteration. The debug binary connects to the worktree daemon automatically.

```bash
# Kernel and notebook inspection
cargo run -p runt-cli -- ps                   # List all kernels (connection-file + daemon)
cargo run -p runt-cli -- notebooks            # List open notebooks with kernel info
```

## Python Bindings (runtimed-py)

The `runtimed-py` crate provides Python bindings for interacting with the daemon programmatically. This is used by the [nteract MCP server](https://github.com/nteract/nteract) and can be used for testing.

### Installation

There are **two Python virtual environments** in the repo:

| Venv | Path (from repo root) | Purpose |
|------|-----------------------|---------|
| Workspace venv | `.venv` | Used by the MCP server and day-to-day development |
| Test venv | `python/runtimed/.venv` | Isolated env for `pytest` runs |

Install into the **workspace venv** (MCP server, general use):

```bash
cd crates/runtimed-py
VIRTUAL_ENV=../../.venv maturin develop
```

Install into the **test venv** (pytest):

```bash
cd crates/runtimed-py
VIRTUAL_ENV=../../python/runtimed/.venv maturin develop
```

### Basic Usage

```python
import asyncio
import runtimed

async def main():
    client = runtimed.Client()
    async with await client.create_notebook() as notebook:
        # Work with cells
        cell = await notebook.cells.create("print('hello')")
        result = await cell.run()
        print(result.stdout)  # "hello\n"

        cell = await notebook.cells.create("x = 42")
        await cell.run()

        # Sync reads from local CRDT
        print(cell.source)      # "x = 42"
        print(cell.cell_type)   # "code"
        print(cell.outputs)     # resolved outputs

asyncio.run(main())
```

See [docs/python-bindings.md](../docs/python-bindings.md) for the full API reference.

### Socket helper choice

Use `default_socket_path()` when you want the current process to honor `RUNTIMED_SOCKET_PATH` and otherwise follow its build channel. Use `socket_path_for_channel("stable"|"nightly")` only for explicit channel targeting or cross-channel discovery; it intentionally ignores `RUNTIMED_SOCKET_PATH`.

### Output.data Typing

`Output.data` is a `dict[str, str | bytes | dict]`. The value type depends on the MIME type:

| MIME category | Example | Python type | Notes |
|---------------|---------|-------------|-------|
| Binary image | `image/png`, `image/jpeg` | `bytes` | Raw binary data (not base64-encoded) |
| JSON | `application/json` | `dict` | Parsed JSON object |
| Text | `text/plain`, `text/html` | `str` | UTF-8 string |
| LLM hint | `text/llm+plain` | `str` | Synthesized blob URL (see below) |

### `text/llm+plain` Synthesis

When an output contains a binary image MIME (e.g. `image/png`), the daemon automatically synthesizes a `text/llm+plain` entry in `Output.data`. Its value is a multi-line description that combines any existing `text/plain`, image metadata (MIME type and size), and the blob URL. This lets LLM-based consumers reference the image without decoding binary data:

```python
result = session.run("display(Image(filename='chart.png'))")
output = result.outputs[0]

output.data["image/png"]        # b'\x89PNG\r\n...'  (raw bytes)
output.data["text/llm+plain"]   # '<IPython.core.display.Image object>\n📊 Image output (image/png, 42 KB)\nhttp://localhost:<port>/blob/<hash>'
output.data["text/plain"]       # '<IPython.core.display.Image object>'
```

### Socket Path Configuration

The Python bindings respect the `RUNTIMED_SOCKET_PATH` environment variable. This is important when testing with worktree daemons in Conductor workspaces.

**System daemon (default):**
```python
# Connects using default_socket_path(), which follows the current build
# channel unless RUNTIMED_SOCKET_PATH is already set.
client = runtimed.Client()
```

**Worktree daemon (for development):**
```bash
# Find and export your current worktree daemon socket
export RUNTIMED_SOCKET_PATH="$(
  RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH="$(pwd)" \
  ./target/debug/runt daemon status --json \
  | jq -r '.socket_path'
)"
python your_script.py
```

**In Conductor workspaces**, the daemon socket path varies by worktree. To test against a specific worktree daemon:

```bash
# Start the dev daemon (Terminal 1)
cargo xtask dev-daemon

# Find and export the socket path (Terminal 2)
export RUNTIMED_SOCKET_PATH="$(
  RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH="$(pwd)" \
  ./target/debug/runt daemon status --json \
  | jq -r '.socket_path'
)"

# Now Python bindings will use the worktree daemon
python -c "import asyncio, runtimed; asyncio.run(runtimed.Client().ping())"
```

## Troubleshooting

### Daemon won't start (lock held)

```bash
# Check what's holding the lock
cat ~/.cache/<cache_namespace>/daemon.json
lsof ~/.cache/<cache_namespace>/daemon.lock

# If stale (crashed daemon), remove manually
rm ~/.cache/<cache_namespace>/daemon.lock ~/.cache/<cache_namespace>/daemon.json
```

### Pool not replenishing

Check that uv/conda are installed and working:

```bash
uv --version
ls -la ~/.cache/<cache_namespace>/envs/
```

### Python bindings: "Failed to parse output" errors

If `session.run()` returns outputs like `Output(stream, stderr: "Failed to parse output: <hash>")`, the bindings are connecting to the wrong daemon (one without access to the blob store).

**Cause:** The blob store is per-daemon. When running from a Conductor workspace, you might be connecting to the system daemon while the blobs are stored in a worktree daemon's directory.

**Fix:** Set `RUNTIMED_SOCKET_PATH` to the correct daemon socket:

```bash
# Find your worktree daemon
./target/debug/runt dev worktrees

# Export the matching socket path
export RUNTIMED_SOCKET_PATH="$(
  RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH="$(pwd)" \
  ./target/debug/runt daemon status --json \
  | jq -r '.socket_path'
)"
```

### Python bindings: get_cell() returns empty outputs

If `session.run()` shows outputs but `session.get_cell()` returns `outputs=[]`:

1. **Check socket path** (see above) — the daemon needs access to the blob store
2. **Timing issue** — outputs may not be written to Automerge yet. Try a small delay or re-fetch.

## Shipped App Behavior

When shipped as a release build, the daemon installs as a system service that starts at login. This is handled by `crates/runtimed/src/service.rs`:

- **macOS**: launchd plist in `~/Library/LaunchAgents/`
- **Linux**: systemd user service in `~/.config/systemd/user/`
- **Windows**: Startup folder script

### Managing the System Daemon

These commands manage the **system daemon** (production). For development, use `cargo xtask dev-daemon` instead — it provides per-worktree isolation and doesn't interfere with the system daemon.

Examples below use the stable channel names. Nightly builds use the `-nightly` variants such as `runt-nightly`, `runtimed-nightly`, and `io.nteract.runtimed.nightly`.

**Cross-platform:**
```bash
# Check status
runt daemon status

# Stop/start the system daemon
runt daemon stop
runt daemon start

# View logs
runt daemon logs -f

# Full uninstall (removes binary and service config)
runt daemon uninstall
```

**Platform-specific (if runt isn't available):**

macOS:
```bash
launchctl bootout gui/$(id -u)/io.nteract.runtimed
launchctl list | grep io.nteract.runtimed
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.nteract.runtimed.plist
```

Linux:
```bash
systemctl --user stop runtimed.service
systemctl --user status runtimed.service
systemctl --user start runtimed.service
```

**Key paths (macOS):**
| File | Path |
|------|------|
| Installed binary | `~/Library/Application Support/<cache_namespace>/bin/<daemon_binary_basename>` |
| Service config | `~/Library/LaunchAgents/<daemon_launchd_label>.plist` |
| Socket | `~/Library/Caches/<cache_namespace>/runtimed.sock` |
| Daemon info | `~/Library/Caches/<cache_namespace>/daemon.json` |
| Logs | `~/Library/Caches/<cache_namespace>/runtimed.log` |

For stable, these expand to `runt`, `runtimed`, and `io.nteract.runtimed`. For nightly, they expand to `runt-nightly`, `runtimed-nightly`, and `io.nteract.runtimed.nightly`.
