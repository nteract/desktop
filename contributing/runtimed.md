# Runtime Daemon (runtimed)

The runtime daemon manages prewarmed Python environments, notebook document sync, and kernel execution across notebook windows.

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
| Unix socket | IPC endpoint | `~/.cache/runt/runtimed.sock` |
| Lock file | Singleton guarantee | `~/.cache/runt/daemon.lock` |
| Info file | Discovery (PID, endpoint) | `~/.cache/runt/daemon.json` |
| Environments | Prewarmed venvs | `~/.cache/runt/envs/` |

## Development Workflow

### Default: Let the notebook start it

The notebook app automatically tries to connect to or start the daemon on launch. If it's not running, the app falls back to in-process prewarming. You don't need to do anything special.

```rust
// crates/notebook/src/lib.rs:2408
runtimed::client::ensure_daemon_running(None).await
```

### Install daemon from source

When you change daemon code and want the installed service to pick it up:

```bash
cargo xtask install-daemon
```

This builds runtimed in release mode, stops the running service, replaces the binary, and restarts it. You can verify the version with:

```bash
cat ~/.cache/runt/daemon.json   # check "version" field
```

### Fast iteration: Daemon + bundled notebook

When iterating on daemon code, you often want to test changes in the notebook app without rebuilding the frontend:

```bash
# Terminal 1: Run dev daemon (restart when you change daemon code)
cargo xtask dev-daemon

# Terminal 2: Build once, then iterate
cargo xtask build                 # Full build (includes frontend)
cargo xtask build --rust-only     # Fast rebuild (reuses frontend assets)
cargo xtask run                   # Run the bundled binary
```

The `--rust-only` flag skips `pnpm build`, reusing the existing frontend assets in `apps/notebook/dist/`. This is much faster when you're only changing Rust code.

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

## Code Structure

```
crates/runtimed/
├── src/
│   ├── lib.rs                   # Public types, path helpers (default_socket_path, etc.)
│   ├── main.rs                  # CLI entry point (run, install, status, etc.)
│   ├── daemon.rs                # Daemon state, pool management, connection routing
│   ├── connection.rs            # Unified framing, Handshake enum, send/recv helpers
│   ├── protocol.rs              # Request/Response enums, BlobRequest/BlobResponse
│   ├── client.rs                # PoolClient for pool operations
│   ├── singleton.rs             # File-based locking for single instance
│   ├── service.rs               # Cross-platform service installation
│   ├── settings_doc.rs          # Settings Automerge document, schema, migration
│   ├── sync_server.rs           # Settings sync handler
│   ├── sync_client.rs           # Settings sync client library
│   ├── (uses notebook_doc crate) # Shared `NotebookDoc` from crates/notebook-doc/src/lib.rs
│   ├── notebook_sync_server.rs  # Room-based notebook sync, peer management, eviction
│   ├── notebook_sync_client.rs  # Notebook sync client library
│   ├── blob_store.rs            # Content-addressed blob store with metadata sidecars
│   ├── blob_server.rs           # HTTP read server for blobs (hyper 1.x)
│   ├── runtime.rs               # Runtime detection (Python/Deno)
│   ├── kernel_manager.rs        # Kernel lifecycle, ZMQ iopub watching, execution queue
│   ├── inline_env.rs            # Inline dependency environment caching (UV/Conda)
│   ├── project_file.rs          # Project file detection (pyproject.toml, pixi.toml, etc.)
│   ├── comm_state.rs            # Comm message state for ipywidgets
│   ├── output_store.rs          # Output persistence and retrieval
│   ├── (metadata via notebook_doc) # `notebook_doc::metadata` re-exported as `notebook_metadata`
│   ├── stream_terminal.rs       # Stream terminal output handling
│   └── terminal_size.rs         # Terminal size tracking
└── tests/
    └── integration.rs           # Integration tests (daemon, pool, settings sync, notebook sync)
```

For the full architecture (all phases, schemas, and design decisions), see [docs/runtimed.md](../docs/runtimed.md).

## Protocol

All daemon communication goes through a single Unix socket with channel-based routing. Connections start with a JSON handshake:

```rust
pub enum Handshake {
    Pool,
    SettingsSync,
    NotebookSync { notebook_id: String },
    Blob,
}
```

**Pool channel** uses length-framed JSON request/response (short-lived). Request types: `ping`, `status`, `take`, `return`, `shutdown`, `flush_pool`, `list_rooms`.

**SettingsSync / NotebookSync** channels use Automerge sync messages (long-lived, bidirectional).

**Blob channel** uses binary framing for storing content-addressed blobs.

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

The `runtimed-py` crate provides Python bindings for interacting with the daemon programmatically. This is used by the nteract MCP server and can be used for testing.

### Installation

```bash
cd crates/runtimed-py
maturin develop
```

### Basic Usage

```python
import runtimed

session = runtimed.Session()
session.connect()
session.start_kernel()

result = session.run("print('hello')")
print(result.stdout)  # "hello\n"
print(result.outputs)  # [Output(stream, stdout: "hello\n")]

# Get cell with outputs (includes historical outputs from other clients)
cell = session.get_cell(result.cell_id)
print(cell.outputs)  # [Output(stream, stdout: "hello\n")]
```

### Socket Path Configuration

The Python bindings respect the `RUNTIMED_SOCKET_PATH` environment variable. This is important when testing with worktree daemons in Conductor workspaces.

**System daemon (default):**
```python
# Connects to system daemon at ~/Library/Caches/runt/runtimed.sock
session = runtimed.Session()
session.connect()
```

**Worktree daemon (for development):**
```bash
# Find your worktree daemon socket
cat ~/Library/Caches/runt/worktrees/*/daemon.json | grep -A1 worktree_path

# Set the socket path before running Python
export RUNTIMED_SOCKET_PATH="/Users/you/Library/Caches/runt/worktrees/{hash}/runtimed.sock"
python your_script.py
```

**In Conductor workspaces**, the daemon socket path varies by worktree. To test against a specific worktree daemon:

```bash
# Start the dev daemon (Terminal 1)
cargo xtask dev-daemon

# Find and export the socket path (Terminal 2)
export RUNTIMED_SOCKET_PATH=$(cat ~/Library/Caches/runt/worktrees/*/daemon.json | \
  jq -r 'select(.worktree_path == "'$(pwd)'") | .endpoint')

# Now Python bindings will use the worktree daemon
python -c "import runtimed; s = runtimed.Session(); s.connect(); print('Connected!')"
```

### Cross-Session Output Visibility

The `Cell.outputs` field is populated from the Automerge document, enabling agents to see outputs from cells executed by other clients:

```python
# Session 1 executes code
s1 = runtimed.Session(notebook_id="shared")
s1.connect()
s1.start_kernel()
s1.run("x = 42")

# Session 2 sees outputs without executing
s2 = runtimed.Session(notebook_id="shared")
s2.connect()
cells = s2.get_cells()
print(cells[0].outputs)  # Shows outputs from s1's execution
```

## Troubleshooting

### Daemon won't start (lock held)

```bash
# Check what's holding the lock
cat ~/.cache/runt/daemon.json
lsof ~/.cache/runt/daemon.lock

# If stale (crashed daemon), remove manually
rm ~/.cache/runt/daemon.lock ~/.cache/runt/daemon.json
```

### Pool not replenishing

Check that uv/conda are installed and working:

```bash
uv --version
ls -la ~/.cache/runt/envs/
```

### Python bindings: "Failed to parse output" errors

If `session.run()` returns outputs like `Output(stream, stderr: "Failed to parse output: <hash>")`, the bindings are connecting to the wrong daemon (one without access to the blob store).

**Cause:** The blob store is per-daemon. When running from a Conductor workspace, you might be connecting to the system daemon while the blobs are stored in a worktree daemon's directory.

**Fix:** Set `RUNTIMED_SOCKET_PATH` to the correct daemon socket:

```bash
# Find your worktree daemon
cat ~/Library/Caches/runt/worktrees/*/daemon.json | jq -r '.worktree_path + " -> " + .endpoint'

# Export the matching socket path
export RUNTIMED_SOCKET_PATH="/Users/you/Library/Caches/runt/worktrees/{hash}/runtimed.sock"
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
| Installed binary | `~/Library/Application Support/runt/bin/runtimed` |
| Service config | `~/Library/LaunchAgents/io.nteract.runtimed.plist` |
| Socket | `~/Library/Caches/runt/runtimed.sock` |
| Daemon info | `~/Library/Caches/runt/daemon.json` |
| Logs | `~/Library/Caches/runt/runtimed.log` |
