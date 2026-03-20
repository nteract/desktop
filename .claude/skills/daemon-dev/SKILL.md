---
name: daemon-dev
description: Develop, debug, and manage the runtimed daemon. Use when working on daemon code, debugging kernel issues, or managing daemon lifecycle.
---

# Daemon Development (runtimed)

## Quick Reference

| Task | Command |
|------|---------|
| Start dev daemon | `cargo xtask dev-daemon` |
| Install system daemon | `cargo xtask install-daemon` |
| Check status | `./target/debug/runt daemon status` |
| Check status (JSON) | `./target/debug/runt daemon status --json` |
| Tail logs | `./target/debug/runt daemon logs -f` |
| List kernels | `./target/debug/runt ps` |
| List notebooks | `./target/debug/runt notebooks` |
| Flush pool | `./target/debug/runt daemon flush` |
| Stop daemon | `./target/debug/runt daemon stop` |
| Run tests | `cargo test -p runtimed` |

## Why the Daemon Exists

Each notebook window is a separate Tauri process. Without coordination: race conditions on prewarmed environments, wasted resources from duplicate pools, and slow cold starts. The daemon is a singleton that prewarms environments and hands them out.

## Architecture

The daemon (`runtimed`) is a singleton process that communicates with notebook windows over a Unix socket. Key components:

- **Unix socket** — IPC endpoint for all notebook windows
- **Lock file** — Singleton guarantee (only one daemon runs)
- **Info file** (`daemon.json`) — Discovery: PID, endpoint, version
- **UV Pool + Conda Pool** — Prewarmed Python environments (configurable pool size)
- **Blob store** — Content-addressed output storage (`blobs/`)
- **Notebook docs** — Persisted Automerge documents (`notebook-docs/`)

## Development Workflow

### Let the notebook start it (default)

The notebook app auto-connects to or starts the daemon. If unavailable, falls back to in-process prewarming.

### Install from source

When you change daemon code and want the system service to pick it up:

```bash
cargo xtask install-daemon
```

Builds release, stops old service, replaces binary, restarts. Verify: `cat ~/.cache/runt/daemon.json`.

### Fast iteration

```bash
# Terminal 1: Run dev daemon
cargo xtask dev-daemon

# Terminal 2: Build once, iterate on Rust
cargo xtask build                 # Full build (includes frontend)
cargo xtask build --rust-only     # Fast rebuild (reuses frontend assets)
cargo xtask run                   # Run the bundled binary
```

### Testing

```bash
cargo test -p runtimed                          # All tests
cargo test -p runtimed --test integration       # Integration tests only
cargo test -p runtimed test_daemon_ping_pong    # Specific test
```

Integration tests use temp directories for socket/lock files to avoid conflicts.

## Notebook Room Lifecycle

Each open notebook has a **room** (`NotebookRoom` in `notebook_sync_server.rs`), keyed by notebook ID (file path or UUID for untitled).

### Autosave

Debounced: 2s quiet period, 10s max interval via `spawn_autosave_debouncer`. `NotebookAutosaved` broadcast clears frontend dirty flag. Explicit Cmd+S also runs cell formatting (ruff/deno fmt). Skips untitled notebooks and notebooks mid-load.

### Room Re-keying

When an untitled notebook is first saved, `rekey_ephemeral_room()`:
1. Canonicalizes save path
2. Guards against overwriting existing room
3. Re-keys `NotebookRooms` HashMap (remove UUID, insert path)
4. Updates room's `notebook_path`
5. Deletes old UUID-based persist file
6. Spawns file watcher for new path
7. Broadcasts `RoomRenamed` so all peers update

### Crash Recovery

Untitled notebooks persist to `notebook-docs/{hash}.automerge`. Before deletion on reopen, snapshots go to `notebook-docs/snapshots/` (max 5 per hash). `runt recover --list` scans all cache namespaces. `runt recover <path>` exports to `.ipynb`.

### Multi-Window

Multiple windows join the same room as separate Automerge peers. First window gets deterministic label; additional get UUID suffix.

### Eviction

When all peers disconnect, delayed eviction runs (default 30s via `keep_alive_secs` setting). If no reconnection: kernel shuts down, file watcher stops, room removed.

## Per-Cell Accessors

O(1) cell reads that avoid full-document materialization:

| Method | Returns |
|--------|---------|
| `get_cell_source(id)` | `Option<String>` |
| `get_cell_type(id)` | `Option<String>` |
| `get_cell_outputs(id)` | `Option<Vec<String>>` |
| `get_cell_execution_count(id)` | `Option<String>` |
| `get_cell_metadata(id)` | `Option<Value>` |
| `get_cell_position(id)` | `Option<String>` |
| `get_cell_ids()` | `Vec<String>` (position-sorted) |

Prefer these over `get_cells()` which materializes everything.

## Code Structure

```
crates/runtimed/src/
  lib.rs                   — Public types, path helpers
  main.rs                  — CLI entry point
  daemon.rs                — Daemon state, pool management, connection routing
  protocol.rs              — BlobRequest/BlobResponse + re-exports
  notebook_sync_server.rs  — NotebookRoom, room lifecycle, autosave, re-keying
  kernel_manager.rs        — RoomKernel: lifecycle, execution queue, IOPub routing
  comm_state.rs            — Widget comm state + Output widget capture routing
  output_store.rs          — Output manifest creation, blob inlining threshold
  blob_store.rs            — Content-addressed blob store with metadata sidecars
  blob_server.rs           — HTTP read server for blobs
  inline_env.rs            — Inline dependency environment caching
  settings_doc.rs          — Settings Automerge document, schema, migration
  sync_server.rs           — Settings sync handler
  stream_terminal.rs       — Stream terminal output handling
```

## Related Crates

| Crate | What it owns |
|-------|-------------|
| `notebook-doc` | `NotebookDoc`: Automerge schema, cell CRUD, per-cell accessors, `CellChangeset` |
| `notebook-protocol` | Wire types: `NotebookRequest`, `NotebookResponse`, `NotebookBroadcast` |
| `notebook-sync` | `DocHandle`: sync infrastructure, snapshot watch, per-cell accessors for Python |

## Settings Sync

Settings are synced via a **separate Automerge document** (not the notebook doc). The daemon holds the canonical copy and persists to disk. Any window can write; all others receive changes via sync.

Key files: `crates/runtimed/src/settings_doc.rs` (schema), `src/hooks/useSyncedSettings.ts` (frontend).

## Troubleshooting

### Daemon won't start (lock held)

```bash
cat ~/.cache/runt/daemon.json
lsof ~/.cache/runt/daemon.lock

# If stale (crashed daemon), remove manually
rm ~/.cache/runt/daemon.lock ~/.cache/runt/daemon.json
```

### Pool not replenishing

```bash
uv --version
ls -la ~/.cache/runt/envs/
```

Check that uv/conda are installed and working.

## NEVER Use pkill or killall

**Never** use `pkill runtimed`, `killall runtimed`, or similar. These kill ALL runtimed processes system-wide, disrupting other agents and worktrees. Use:

- `./target/debug/runt daemon stop` — stops only your worktree's daemon
- `cargo xtask install-daemon` — gracefully reinstalls the system daemon

## Shipped App Behavior

Production daemon installs as a system service at login:
- **macOS**: launchd plist in `~/Library/LaunchAgents/`
- **Linux**: systemd user service in `~/.config/systemd/user/`

### Managing the System Daemon

```bash
runt daemon status
runt daemon stop
runt daemon start
runt daemon logs -f
runt daemon uninstall   # Full uninstall
```

**macOS (if runt unavailable):**
```bash
launchctl bootout gui/$(id -u)/io.nteract.runtimed
launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.nteract.runtimed.plist
```

**Key paths (macOS):**

| File | Path |
|------|------|
| Installed binary | `~/Library/Application Support/runt/bin/runtimed` |
| Service config | `~/Library/LaunchAgents/io.nteract.runtimed.plist` |
| Socket | `~/Library/Caches/runt/runtimed.sock` |
| Daemon info | `~/Library/Caches/runt/daemon.json` |
| Logs | `~/Library/Caches/runt/runtimed.log` |

## Dev Mode: Per-Worktree Isolation

Each git worktree can run its own isolated daemon.

**Conductor users:** Automatic. `cargo xtask dev-daemon` translates `CONDUCTOR_WORKSPACE_PATH` to `RUNTIMED_WORKSPACE_PATH`.

**Non-Conductor users:** Set `RUNTIMED_DEV=1`:

```bash
# Terminal 1
RUNTIMED_DEV=1 cargo xtask dev-daemon

# Terminal 2
RUNTIMED_DEV=1 cargo xtask notebook
```

**State location** (macOS: `~/Library/Caches/`, Linux: `~/.cache/`):

```
<cache>/runt-nightly/worktrees/{hash}/
  runtimed.sock, runtimed.log, daemon.json, daemon.lock
  envs/, blobs/, notebook-docs/
```

**Useful commands:**

```bash
./target/debug/runt daemon status           # Shows dev mode, worktree, version
./target/debug/runt dev worktrees           # List all dev daemons
./target/debug/runt daemon logs -f          # Tail logs
./target/debug/runt daemon status --json    # Machine-readable (socket path, blob URL, etc.)
```
