---
name: daemon-dev
description: Develop, debug, and manage the runtimed daemon. Use when working on daemon code, debugging kernel issues, or managing daemon lifecycle.
---

# Daemon Development (runtimed)

## Quick Reference

| Task | Command |
|------|---------|
| Start dev daemon | `cargo xtask dev-daemon` |
| Install nightly (Linux/headless only) | `cargo xtask install-nightly` |
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

### Install from source (Linux / headless)

When you change daemon code and want the system service to pick it up on a cloud box or headless Linux machine:

```bash
cargo xtask install-nightly
```

Builds runtimed + runt + runt-proxy (release), installs them to `~/.local/share/runt-nightly/bin/` with channel-suffixed names, writes + starts the systemd user unit on first install, upgrades in place on subsequent runs. On macOS it refuses by default — use the nteract Nightly app (it auto-updates). Pass `--on-macos` to override, `--replace-installed-app` if an app bundle is already present.

Verify: `runt-nightly daemon status` or `cat ~/.cache/runt-nightly/daemon.json`.

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

Each open notebook has a **room** (`NotebookRoom` in `notebook_sync_server.rs`), always keyed by UUID. A secondary `path_index: HashMap<PathBuf, Uuid>` maps file paths to room UUIDs.

### Autosave

Debounced: 2s quiet period, 10s max interval via `spawn_autosave_debouncer`. `NotebookAutosaved` broadcast clears frontend dirty flag. Explicit Cmd+S also runs cell formatting (ruff/deno fmt). Skips untitled notebooks and notebooks mid-load.

### Saving an untitled notebook

Room keys are always UUIDs (never change). When an untitled notebook is first saved:
1. Canonicalizes save path
2. Checks `path_index` for conflicts (`PathAlreadyOpen` error if collision)
3. Inserts into `path_index: HashMap<PathBuf, Uuid>`
4. Updates room's `path: RwLock<Option<PathBuf>>`
5. Spawns file watcher for new path
6. Broadcasts `PathChanged { path }` so peers update local path tracking

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

## Fork+Merge for Async CRDT Mutations

**Critical invariant:** Any daemon code that reads doc state, does async work (subprocess, I/O, network), then writes back MUST use `fork()` + `merge()`. Direct mutation after an async gap overwrites concurrent edits.

```rust
// Fork BEFORE async work — captures the baseline
let fork = {
    let mut doc = room.doc.write().await;
    doc.fork()
};

// Async work happens here (ruff, network, etc.)
let result = do_async_work().await;

// Apply on fork, then merge back
let mut fork = fork;
fork.update_source(&cell_id, &result).ok();
let mut doc = room.doc.write().await;
doc.merge(&mut fork).ok();
```

For synchronous mutation blocks (no `.await` between fork and merge), prefer the helpers:
```rust
doc.fork_and_merge(|fork| {
    fork.update_source(&cell_id, &new_source);
});
```

Do not use `fork_at(...)` in current daemon code paths. The file watcher now compares against `last_save_sources` and uses `fork()` at current heads because `fork_at(...)` can trigger automerge/automerge#1327 on complex text histories.

Key methods on `NotebookDoc`: `fork()`, `get_heads()`, `merge()`, `fork_and_merge(f)`.

All async CRDT mutation paths in the daemon are now protected — see #1216.

## Code Structure

```
crates/runtimed/src/
  lib.rs                   — Public types, path helpers
  main.rs                  — CLI entry point
  daemon.rs                — Daemon state, pool management, connection routing
  notebook_sync_server.rs  — NotebookRoom, room lifecycle, autosave, path_index
  jupyter_kernel.rs        — JupyterKernel: process spawn, ZMQ socket wiring, IOPub output routing
  kernel_manager.rs        — Shared kernel plumbing: QueueCommand, KernelStatus, QueuedCell, output conversion, widget buffers
  runtime_agent.rs         — Process-isolated runtime agent: kernel lifecycle, IOPub, RuntimeStateDoc writes
  runtime_agent_handle.rs  — Coordinator-side runtime agent process management
  output_store.rs          — Output manifest creation, blob inlining threshold
  blob_store.rs            — Content-addressed blob store with metadata sidecars
  blob_server.rs           — HTTP read server for blobs
  inline_env.rs            — Inline dependency environment caching
  stream_terminal.rs       — Stream terminal output handling
  singleton.rs             — Daemon singleton management (lock file, PID tracking)
  kernel_pids.rs           — Kernel process ID tracking and cleanup
  markdown_assets.rs       — Markdown output asset rendering and resolution
  terminal_size.rs         — Terminal size detection for kernel PTY
  project_file.rs          — Unified project file discovery (pyproject, pixi, env.yml)
  sync_server.rs           — Settings Automerge sync handler
crates/runtimed-client/src/
  lib.rs                   — Crate root
  client.rs                — Client APIs used by Python bindings and MCP
  daemon_paths.rs          — Shared socket/blob path resolution
  output_resolver.rs       — Shared Rust manifest resolution
  resolved_output.rs       — Output resolution types
  singleton.rs             — File-based daemon discovery/locking helpers
  protocol.rs              — Client-side protocol helpers and typed request wrappers
  settings_doc.rs          — Settings Automerge document, schema, migration
  sync_client.rs           — Settings sync client wrapper
  service.rs               — System service install/uninstall helpers
  runtime.rs               — Runtime enum (Python, Deno) and detection
```

## Related Crates

| Crate | What it owns |
|-------|-------------|
| `notebook-doc` | `NotebookDoc`: Automerge schema, cell CRUD, per-cell accessors, `CellChangeset` |
| `notebook-protocol` | Wire types: `NotebookRequest`, `NotebookResponse`, `NotebookBroadcast` |
| `notebook-sync` | `DocHandle`: sync infrastructure, snapshot watch, per-cell accessors for Python |

## RuntimeStateDoc

Each notebook room has a **RuntimeStateDoc** — a daemon-authoritative Automerge document synced via frame type `0x05`. It replaces state-carrying broadcasts for kernel status, queue, env sync, and trust.

### Schema

```
ROOT/
  kernel/
    status: "idle" | "busy" | "starting" | "error" | "shutdown" | "not_started"
    starting_phase: "" | "resolving" | "preparing_env" | "launching" | "connecting"
    name, language, env_source: Str
  queue/
    executing: Str|null (cell_id)
    executing_execution_id: Str|null
    queued: List[Str] (cell_ids)
    queued_execution_ids: List[Str]
  executions/ Map (keyed by execution_id)
    {id}/ { cell_id, status, execution_count, success }
  env/ { in_sync, added, removed, channels_changed, deno_changed }
  trust/ { status, needs_approval }
  last_saved: Str|null (ISO timestamp)
```

### Who writes what

- **Daemon only** writes to RuntimeStateDoc (kernel status, queue state, execution lifecycle, env sync, trust)
- **Frontend reads only** via `useRuntimeState()` hook in `apps/notebook/src/lib/runtime-state.ts`
- **Python reads** via `notebook.runtime` property (`RuntimeState` class)

Key files: `crates/notebook-doc/src/runtime_state.rs` (schema), `apps/notebook/src/lib/runtime-state.ts` (frontend).

## Execution Lifecycle

Each cell execution is tracked by a unique `execution_id` (UUID):

1. Client sends `ExecuteCell { cell_id }` → daemon generates `execution_id`
2. Daemon writes `QueueEntry { cell_id, execution_id }` to RuntimeStateDoc queue
3. When execution starts: status → `"running"`, execution_count assigned
4. When done: status → `"done"` or `"error"`, success flag set
5. Python `Execution` handle polls RuntimeStateDoc for lifecycle updates

## Settings Sync

Settings are synced via a **separate Automerge document** (not the notebook doc). The daemon holds the canonical copy and persists to disk. Any window can write; all others receive changes via sync.

Key files: `crates/runtimed-client/src/settings_doc.rs` (schema), `src/hooks/useSyncedSettings.ts` (frontend).

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
- `cargo xtask install-nightly` — gracefully installs/reinstalls the full nightly stack (Linux/headless only; refuses on macOS by default)

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
