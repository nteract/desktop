# runtimed Architecture

## Vision

runtimed is a long-lived daemon that owns the heavy, stateful parts of the notebook experience — environment pools, kernel processes, output storage, and document sync. Notebook windows become thin views: they subscribe to a CRDT document, render output from a blob store, and send execution requests. When the last window closes, the daemon keeps kernels alive and outputs safe. When a new window opens, it catches up instantly.

The architecture has two core ideas:

1. **Live outputs live outside NotebookDoc.** Kernel outputs (images, HTML, logs) are write-once data from a single actor. NotebookDoc stores source and structure; RuntimeStateDoc stores live execution/output state. Large text and binary payloads spill to the content-addressed blob store.

2. **Two levels of output abstraction.** An "output" (the Jupyter-level concept — a display_data, stream, error, etc.) is described by a RuntimeStateDoc manifest that references raw content blobs. Small text is inlined in the manifest; large text and binary data point to the blob store. `GET /blob/{hash}` returns raw bytes.

---

## Architecture layers

```
┌─────────────────────────────────────────────────┐
│  Notebook window (thin view)                    │
│  - Subscribes to automerge doc                  │
│  - Fetches outputs via HTTP                     │
│  - Sends execution requests                     │
└──────────────┬──────────────────────────────────┘
               │ single unix socket (multiplexed)
┌──────────────▼──────────────────────────────────┐
│  runtimed (daemon)                              │
│                                                 │
│  ┌─────────────┐  ┌──────────────────────────┐  │
│  │ Pool        │  │ CRDT sync layer          │  │
│  │ (UV, Conda) │  │ - Settings doc           │  │
│  └─────────────┘  │ - Notebook docs (rooms)  │  │
│                   └──────────────────────────┘  │
│  ┌─────────────────────────────────────────┐    │
│  │ Output store                            │    │
│  │ - Output manifests (Jupyter semantics)  │    │
│  │ - ContentRef (inline / blob)            │    │
│  │ - Inlining threshold                    │    │
│  └──────────────┬──────────────────────────┘    │
│  ┌──────────────▼──────────────────────────┐    │
│  │ Blob store (content-addressed)          │    │
│  │ - On-disk CAS with metadata sidecars    │    │
│  │ - HTTP read server on localhost         │    │
│  └─────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────┐    │
│  │ Kernel manager                          │    │
│  │ - Owns kernel processes                 │    │
│  │ - Subscribes to iopub                   │    │
│  │ - Writes outputs to store               │    │
│  └─────────────────────────────────────────┘    │
└─────────────────────────────────────────────────┘
```

---

## Platform paths

This document uses `~/.cache/runt/` as shorthand for the platform-appropriate cache directory:

| Platform | Path |
|----------|------|
| Linux | `~/.cache/runt/` (or `$XDG_CACHE_HOME/runt/`) |
| macOS | `~/Library/Caches/runt/` |
| Windows | `{FOLDERID_LocalAppData}/runt/` (typically `%LOCALAPPDATA%\runt\`) |

Similarly, `~/.config/runt/` refers to the platform config directory (`~/Library/Application Support/runt/` on macOS, `{FOLDERID_RoamingAppData}\runt\` on Windows).

In code, use the `dirs` crate (`dirs::cache_dir()`, `dirs::config_dir()`) rather than hardcoding any of these paths.

---

## Phase 1: Daemon & environment pool

> **Implemented**

The foundation. A singleton daemon that prewarms Python environments so notebook startup is instant.

### Singleton management

Only one daemon per user. A file lock (`~/.cache/runt/daemon.lock`) provides mutual exclusion. Daemon discovery is socket-first: clients connect to the socket and ask the daemon for live info. The sidecar JSON file (`~/.cache/runt/daemon.json`) remains only as a fallback for older daemons and should not be used by new code.

```rust
pub struct DaemonInfo {
    pub endpoint: String,
    pub pid: u32,
    pub version: String,
    pub started_at: DateTime<Utc>,
    pub blob_port: Option<u16>,
    pub worktree_path: Option<String>,        // dev mode only
    pub workspace_description: Option<String>, // dev mode only
}
```

### Pool architecture

Two pools — UV and Conda — each with a configurable target size (default 3). Background warming loops replenish environments as they're consumed.

**UV environments**: `uv venv` + `uv pip install ipykernel ipywidgets` + default packages from settings. A warmup script triggers `.pyc` compilation.

**Conda environments**: Uses rattler (Rust-native conda) — repodata fetch, dependency solving, package installation. Same default packages.

Environments stored in `~/.cache/runt/envs/runtimed-{uv|conda}-{uuid}/`. Stale environments (>2 days) pruned on startup.

### IPC protocol

Length-prefixed binary framing over a single Unix socket (Unix) or named pipe (Windows). All connections start with a JSON handshake declaring their channel (see Phase 4).

| Request | Response | Purpose |
|---------|----------|---------|
| `Take { env_type }` | `Env { ... }` or `Empty` | Acquire a prewarmed env |
| `Return { env }` | `Returned` | Give an env back to the pool |
| `Status` | `Stats { ... }` | Pool metrics |
| `Ping` | `Pong` | Health check |
| `Shutdown` | `ShuttingDown` | Graceful stop |
| `FlushPool` | `Flushed` | Drain and rebuild all envs |
| `InspectNotebook { notebook_id }` | `NotebookState { ... }` | Debug notebook sync state |
| `ListRooms` | `RoomsList { rooms }` | List active notebook sync rooms |
| `ShutdownNotebook { notebook_id }` | `NotebookShutdown { found }` | Shutdown kernel and evict room |

### Settings.json file watcher

The daemon watches `~/.config/nteract/settings.json` for external edits. Changes are debounced (500ms), applied to the Automerge settings doc, persisted as Automerge binary (not back to JSON, to avoid formatting churn), and broadcast to all connected sync clients.

### Service management

| Platform | Mechanism |
|----------|-----------|
| macOS | launchd user agent (`~/Library/LaunchAgents/io.nteract.runtimed.plist`) |
| Linux | systemd user service (`~/.config/systemd/user/runtimed.service`) |
| Windows | VBS script in Startup folder |

**CLI commands** (cross-platform):
```bash
runt daemon status     # Check service and pool status
runt daemon start      # Start the daemon service
runt daemon stop       # Stop the daemon service
runt daemon restart    # Restart the daemon
runt daemon logs -f    # Tail daemon logs
runt daemon install    # Install as system service (system daemon only)
runt daemon uninstall  # Uninstall system service (system daemon only)
runt daemon doctor     # Diagnose installation issues (--fix to auto-repair)
runt daemon flush      # Flush and rebuild all pooled environments
runt daemon shutdown   # Request graceful daemon shutdown via IPC
runt daemon ping       # Health-check the running daemon
```

**Machine-readable output** (`--json`):

```bash
runt daemon status --json
```

Returns structured JSON with:
- `socket_path` — Unix socket or named pipe path
- `running` — boolean daemon status
- `daemon_info` — PID, version, blob_port, worktree_path (when running)
- `pool_stats` — environment pool counts including `uv_target`/`conda_target`
- `paths` — computed dev paths (base_dir, log_path, envs_dir, blobs_dir)
- `env` — environment variables (RUNTIMED_DEV, RUNTIMED_WORKSPACE_PATH, etc.)
- `blob_url` — HTTP blob server URL (when running)
- `worktree_hash` — 12-char hash for dev mode isolation

Useful for scripts that need to discover daemon configuration without parsing human-readable output.

Most commands work with both the system daemon and dev worktree daemons. The `install`/`uninstall` commands are system-only — don't run these in a worktree context.

Auto-upgrade: the client detects version mismatches and replaces the binary.

### Key files

| File | Role |
|------|------|
| `daemon.rs` | Daemon state, pool management, warming loops, connection routing |
| `crates/notebook-protocol/src/protocol.rs` | Notebook request/response/broadcast wire types |
| `crates/notebook-protocol/src/connection.rs` | Unified framing, handshake enum, send/recv helpers |
| `crates/runtimed-client/src/client.rs` | Client library (`PoolClient`, notebook clients) |
| `crates/runtimed-client/src/singleton.rs` | File locking, `DaemonInfo` discovery |
| `crates/runtimed-client/src/service.rs` | Platform-specific install/start/stop helpers |
| `main.rs` | CLI entry point |

---

## Phase 2: CRDT sync layer

> **Implemented** (settings sync in PR #220, notebook sync in PR #223)

Real-time state synchronization across notebook windows using Automerge.

### Settings sync

A single Automerge document shared by all windows, covering user preferences:

```
ROOT/
  theme: "system"
  default_runtime: "python"
  default_python_env: "uv"
  uv/
    default_packages: ["numpy", "pandas"]
  conda/
    default_packages: ["scipy"]
```

The daemon holds the canonical document, persisted to `~/.cache/runt/settings.automerge` with a JSON mirror at `~/.config/nteract/settings.json`. Backward-compatible migration from flat keys (`default_uv_packages: "numpy, pandas"`) to nested structures.

**Wire protocol**: Length-prefixed binary frames (4-byte BE length + Automerge sync message). Bidirectional, long-lived connections. Broadcast channel notifies all peers when any peer changes a setting.

### Notebook document sync

Each open notebook gets a "room" in the daemon. Multiple windows editing the same notebook sync through the room's canonical document.

**Document schema** (Automerge CRDT, currently v4):

```
ROOT/
  schema_version: u64           <- Document schema version (currently 4)
  notebook_id: Str
  cells/                        <- Map keyed by cell ID (O(1) lookup)
    {cell_id}/
      id: Str                   <- cell UUID (redundant but convenient)
      cell_type: Str            <- "code" | "markdown" | "raw"
      position: Str             <- Fractional index hex string for ordering
      source: Text              <- Automerge Text CRDT (character-level merging)
      execution_count: Str      <- JSON-encoded i32 or "null"
      metadata/                 <- Native Automerge map (legacy: JSON string fallback)
      resolved_assets/          <- Map of markdown asset ref -> blob hash
  metadata/                     <- Map
    runtime: Str
    kernelspec/                 <- Native Automerge map
    language_info/              <- Native Automerge map
    runt/                       <- Native Automerge map
    notebook_metadata: Str      <- Legacy JSON mirror (dual-written)
```

Outputs live in `RuntimeStateDoc` (separate Automerge doc, frame type `0x05`) keyed by `execution_id`, with each manifest carrying an `output_id` UUID for addressable references. They are not stored in the notebook doc. Execution counts are duplicated intentionally: `RuntimeStateDoc` is authoritative for live sessions, while `NotebookDoc.execution_count` is the persisted nbformat-history fallback used when runtime state is unavailable.

Cell ordering uses fractional indexing via the `position` field. Cells are sorted lexicographically by `position`, with `cell_id` as a tiebreaker for the (rare) case where two cells receive the same fractional index.

**Design decisions**:
- Cell `source` uses `ObjType::Text` for proper concurrent edit merging. `update_source()` uses Automerge's `update_text()` (Myers diff internally) for efficient character-level patches.
- Outputs are write-once from the runtime agent, keyed by `execution_id` in `RuntimeStateDoc` so they never touch the notebook doc's CRDT ops.
- `execution_count` is a string for JSON serialization consistency.

### Room architecture

`NotebookRoom` (defined in `crates/runtimed/src/notebook_sync_server/room.rs`) is keyed by UUID and groups the notebook doc, runtime state, persistence, and runtime-agent coordination. Key fields:

| Field | Type | Role |
|-------|------|------|
| `id` | `uuid::Uuid` | Stable room identity and `NotebookRooms` key |
| `doc` | `Arc<RwLock<NotebookDoc>>` | Canonical Automerge document |
| `broadcasts` | `RoomBroadcasts` | Fan-out channels for peer sync loops |
| `blob_store` | `Arc<BlobStore>` | Content-addressed output storage |
| `trust_state` | `Arc<RwLock<TrustState>>` | HMAC trust for auto-launch |
| `identity` | `RoomIdentity` | Persist path, `.ipynb` path, working directory, and ephemeral flag |
| `connections` | `RoomConnections` | Active-peer and had-peer accounting |
| `persistence` | `RoomPersistence` | Debounced persistence, autosave/file watcher shutdown, save baselines, attachments |
| `state` | `RuntimeStateHandle` | Per-notebook daemon-authoritative runtime state |
| `runtime_agent_handle` | `Arc<Mutex<Option<RuntimeAgentHandle>>>` | Runtime agent subprocess handle |
| `runtime_agent_request_tx` | `Arc<Mutex<Option<RuntimeAgentRequestSender>>>` | RPC channel to the connected runtime agent |

See source for the full definition and the split support structs (`RoomIdentity`, `RoomPersistence`, `RoomConnections`, `RoomBroadcasts`).

**Room lifecycle**:
1. First window opens notebook -> daemon acquires room via `get_or_create_room()`, loading persisted doc from disk (or creating fresh)
2. Client sends `Handshake::NotebookSync { notebook_id }`, then exchanges Automerge sync messages
3. Additional windows join the same room, incrementing `active_peers`
4. Changes from any peer -> applied under write lock -> persisted to disk (outside lock) -> broadcast to all other peers
5. Last peer disconnects -> `active_peers` hits 0 -> delayed eviction begins (`keep_alive_secs`, default 30s); if no peer reconnects, the kernel shuts down and the room is removed

**Persistence**: Documents saved to `~/.cache/runt/notebook-docs/{sha256(notebook_id)}.automerge`. SHA-256 hashing sanitizes notebook IDs (which may be file paths with special characters) into safe filenames. Persistence runs after every sync message, with serialization inside the write lock and disk I/O outside it.

**Corrupt document recovery**: If a persisted `.automerge` file can't be loaded, it's renamed to `.automerge.corrupt` and a fresh document is created. This preserves the corrupt data for debugging without blocking the user.

### Sync protocol

1. **Initial sync**: Server sends first. Both sides exchange Automerge sync messages with 100ms timeout until convergence.
2. **Watch loop**: `tokio::select!` on two channels — incoming frames from this client, and broadcast notifications from other peers. When either fires, generate and send sync messages.
3. **Persistence**: After applying each peer message, `doc.save()` runs inside the write lock (serialization), then `persist_notebook_bytes()` writes to disk outside the lock (I/O doesn't block other peers).

### Key files

| File | Role |
|------|------|
| `crates/runtimed-client/src/settings_doc.rs` | Settings Automerge document, schema, migration |
| `crates/runtimed/src/sync_server.rs` | Settings sync handler |
| `crates/runtimed-client/src/sync_client.rs` | Settings sync client library |
| `crates/notebook-doc/src/lib.rs` | Notebook Automerge document, cell CRUD, text editing, persistence |
| `crates/runtimed/src/notebook_sync_server/` | Room-based notebook sync, peer management, persistence, metadata/trust/project context |
| `crates/notebook-sync/src/relay.rs` | Relay handle for notebook sync connections |

---

## Phase 3: Blob store

> **Implemented** (PR #220)

Content-addressed storage for output data. The blob store knows nothing about Jupyter — it's a generic CAS that stores bytes with a media type.

### On-disk layout

```
~/.cache/runt/blobs/
  a1/
    b2c3d4e5f6...           # raw bytes
    b2c3d4e5f6....meta      # JSON metadata sidecar
```

Two-character prefix directories prevent filesystem bottlenecks.

**Metadata sidecar**:
```json
{
  "media_type": "image/png",
  "size": 45000,
  "created_at": "2026-02-23T12:00:00Z"
}
```

### API

```rust
pub struct BlobStore { root: PathBuf }

impl BlobStore {
    pub async fn put(&self, data: &[u8], media_type: &str) -> io::Result<String>;
    pub async fn get(&self, hash: &str) -> io::Result<Option<Vec<u8>>>;
    pub async fn get_meta(&self, hash: &str) -> io::Result<Option<BlobMeta>>;
    pub fn exists(&self, hash: &str) -> bool;
    pub async fn delete(&self, hash: &str) -> io::Result<bool>;
    pub async fn list(&self) -> io::Result<Vec<String>>;
}
```

**Hashing**: SHA-256 over raw bytes only (not media type), hex-encoded. Same bytes = same hash regardless of type label.

**Write semantics**: Write to temp file, then `rename()` into place. Atomic. On Windows, `rename` returning `AlreadyExists` is treated as success (concurrent writer race with identical content).

**Hash validation**: Methods validate hash strings contain only hex characters before constructing filesystem paths.

**Size limit**: 100 MB hard cap.

**GC strategy**: None for now. Users can clear `~/.cache/runt/blobs/` manually.

### HTTP read server

Minimal hyper 1.x server on `127.0.0.1:0` (random port).

**`GET /blob/{hash}`**
- Raw bytes with `Content-Type` from metadata sidecar (falls back to `application/octet-stream`)
- Blob data and metadata fetched concurrently via `tokio::join!`
- `Cache-Control: public, max-age=31536000, immutable`
- `Access-Control-Allow-Origin: *`

**`GET /health`** — 200 OK

Blob server details are reported by the live daemon info response. `daemon.json` may contain the same data only as a legacy discovery fallback.

### Security model

- **Writes**: Unix socket / named pipe only. Filesystem permissions on the socket ARE the auth.
- **Reads**: Unauthenticated HTTP GET on localhost. Safe: content-addressed (256-bit hash), non-secret data, read-only.

### Key files

| File | Role |
|------|------|
| `blob_store.rs` | On-disk CAS with metadata sidecars |
| `blob_server.rs` | hyper 1.x HTTP read server |

---

## Phase 4: Protocol consolidation

> **Implemented** (PR #220 for pool/settings/blob, PR #223 for notebook sync)

All daemon communication goes through a single multiplexed socket with channel-based routing.

### Unified framing (`connection.rs`)

One socket: `~/.cache/runt/runtimed.sock`

Every connection begins with a 5-byte preamble: 4-byte magic (`0xC0DE01AC`) + 1-byte protocol version. The daemon validates both before reading the handshake frame.

After the preamble, all frames use length-prefixed framing:

```
[4 bytes: payload length (big-endian u32)] [payload bytes]
```

Helpers: `send_frame()` / `recv_frame()` for raw binary, `send_json_frame()` / `recv_json_frame()` for JSON, `recv_control_frame()` with a **64 KB size limit** for handshakes.

### Connection handshake

```rust
#[serde(tag = "channel", rename_all = "snake_case")]
pub enum Handshake {
    Pool,
    SettingsSync,
    NotebookSync {
        notebook_id: String,
        protocol: Option<String>,        // version negotiation (v2 = typed frames)
        working_dir: Option<String>,      // for untitled notebook project detection
        initial_metadata: Option<String>, // kernelspec JSON for auto-launch
    },
    Blob,
    OpenNotebook { path: String },        // daemon loads from disk, returns NotebookConnectionInfo
    CreateNotebook {                      // daemon creates empty room
        runtime: String,                  // "python" or "deno"
        working_dir: Option<String>,
        notebook_id: Option<String>,      // restore hint for previous session
    },
}
```

The daemon's `route_connection()` validates the preamble first via `recv_preamble()`, then reads the handshake via `recv_control_frame()` and dispatches:

| Channel | After handshake | Lifetime |
|---------|----------------|----------|
| `Pool` | Length-framed JSON request/response | Short-lived |
| `SettingsSync` | Automerge sync messages | Long-lived, bidirectional |
| `NotebookSync` | Automerge sync messages, room-routed by `notebook_id` | Long-lived, bidirectional |
| `Blob` | Binary blob writes | Short-lived |
| `OpenNotebook` | Returns `NotebookConnectionInfo`, then notebook sync | Long-lived |
| `CreateNotebook` | Returns `NotebookConnectionInfo`, then notebook sync | Long-lived |

### Blob channel protocol

```
Client -> Server:
  Frame 1: Handshake       {"channel": "blob"}
  Frame 2: JSON request    {"Store": {"media_type": "image/png"}}
  Frame 3: Raw binary      <the actual blob bytes>

Server -> Client:
  Frame 1: JSON response   {"Stored": {"hash": "a1b2c3d4..."}}
```

```rust
pub enum BlobRequest {
    Store { media_type: String },
    GetPort,
}

pub enum BlobResponse {
    Stored { hash: String },
    Port { port: u16 },
    Error { error: String },
}
```

### Key files

| File | Role |
|------|------|
| `crates/notebook-protocol/src/connection.rs` | Unified framing, handshake enum, send/recv helpers |
| `daemon.rs` | Single accept loop, `route_connection()` dispatcher |
| `crates/runtimed-client/src/client.rs` | Uses `Handshake::Pool` |
| `crates/runtimed-client/src/sync_client.rs` | Uses `Handshake::SettingsSync` |
| `crates/runtimed/src/sync_server.rs` | Handler function (no longer owns accept loop) |
| `crates/notebook-sync/src/connect.rs` | Uses `Handshake::NotebookSync` for relay connections |
| `crates/runtimed/src/notebook_sync_server/peer_connection.rs` | Notebook sync connection handler |
| `crates/runtimed/src/notebook_sync_server/catalog.rs` | Room lookup and creation |
| `crates/runtimed-client/src/protocol.rs` | `BlobRequest`/`BlobResponse` enums |

---

## Phase 5: Local-first Automerge notebook sync

> **Implemented** — Frontend owns a local Automerge doc via WASM. Cell state syncs bidirectionally with the daemon over binary Automerge messages.

The frontend runs `runtimed-wasm` (compiled from `crates/runtimed-wasm/`) as a WASM module, giving it a local Automerge `NotebookHandle`. All cell mutations (source edits, add/delete, reorder) happen locally in WASM and propagate to the daemon via binary sync messages relayed through Tauri. The daemon's copy is authoritative for outputs and execution state.

### Architecture

```
┌─────────────────────────────────────────────────────────┐
│ Frontend (React)                                        │
│                                                         │
│  useAutomergeNotebook.ts                                │
│    ├── NotebookHandle (runtimed-wasm, local Automerge)  │
│    ├── materialize-cells.ts (doc → React cell state)    │
│    └── Tauri event listeners                            │
│                                                         │
│  Cell edits ──► WASM mutates local doc                  │
│                   │                                     │
│                   ▼                                     │
│              Binary sync message                        │
│                   │                                     │
│                   ▼                                     │
│  Tauri relay (send_frame / notebook:frame — unified pipe)  │
│                   │                                     │
│                   ▼                                     │
│  Daemon (NotebookRoom)                                  │
│    ├── Merges changes into canonical doc                │
│    ├── Writes kernel outputs to doc                     │
│    └── Syncs to all connected peers                     │
└─────────────────────────────────────────────────────────┘
```

### How cell mutations work

The frontend calls methods on the WASM `NotebookHandle` directly — no Tauri commands for cell source, add, or delete. The WASM module mutates its local Automerge doc and produces a binary sync message. Tauri relays that message to the daemon via the notebook sync connection.

Character-level source edits use Automerge's `update_text` for CRDT-friendly merging across windows.

### How outputs arrive

Outputs flow through RuntimeStateDoc sync, not Tauri events:

1. Kernel emits iopub message → daemon's `output_prep` receives it
2. Daemon creates an output manifest with `ContentRef` entries; small text is inlined, large text and binary data go to the blob store
3. Daemon writes the structured manifest to RuntimeStateDoc under the execution record
4. RuntimeStateDoc produces a sync message → Tauri relay forwards raw bytes to the frontend
5. Frontend receives a typed frame → WASM `receive_frame()` demuxes and merges runtime state
6. `materialize-cells.ts` and `notebook-outputs.ts` resolve manifests into React output state

Output broadcasts are not the rendering path. Outputs are rendered from RuntimeStateDoc manifests and resolved through WASM/frontend manifest-resolution helpers.

### Save and format-on-save

Save is delegated to the daemon via `NotebookRequest::SaveNotebook`. The daemon:
1. Reads the canonical Automerge doc
2. Runs format-on-save if enabled (ruff for Python, deno fmt for TypeScript)
3. Serializes to nbformat `.ipynb` and writes to disk
4. Syncs any formatter changes back to all peers

### What multi-window sync gives us

- Two windows open the same notebook → both have local Automerge docs synced through the daemon
- Edit source in window A → binary sync message → daemon → window B sees the change
- Execute cell in window A → daemon writes RuntimeStateDoc execution/output state → both windows materialize it
- Save from either window → daemon writes the same canonical `.ipynb`

### Key files

| File | Role |
|------|------|
| `crates/runtimed-wasm/` | WASM module exposing `NotebookHandle` to the frontend |
| `apps/notebook/src/hooks/useAutomergeNotebook.ts` | Frontend hook owning the local Automerge doc and sync lifecycle |
| `apps/notebook/src/lib/materialize-cells.ts` | Converts Automerge doc state into React cell arrays |
| `crates/runtimed/src/notebook_sync_server/` | Daemon-side notebook room management and sync |
| `crates/runtimed/src/output_prep.rs` | Daemon-side iopub → Automerge output conversion and blob-store offload |
| `crates/notebook/src/lib.rs` | Tauri commands and relay plumbing |

---

## Phase 6: Output store

> **Implemented**

Outputs are structured manifests in RuntimeStateDoc. Manifest fields use `ContentRef` values: small text is inlined directly in the CRDT, while larger text and all binary data are stored in the blob store. NotebookDoc does not store live outputs.

### The two levels

**Level 1 — Blob store** (`GET /blob/{hash}`): Pure content-addressed bytes. Returns raw PNG, text, JSON — whatever was stored. Used for `<img src>`, direct rendering, large data.

**Level 2 — Output manifests** (RuntimeStateDoc): Jupyter-aware structured objects describing output type, available representations, metadata, execution count, and `ContentRef` payloads. Used by frontend, Python, and MCP consumers to understand what to render or summarize.

### ContentRef

The fundamental type for "content that might be inlined or might be in the blob store":

```rust
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
pub enum ContentRef {
    Inline { inline: String },
    Blob { blob: String, size: u64 },
}
```

```json
{"inline": "hello world"}
{"blob": "a1b2c3d4...", "size": 45000}
```

### Output manifest

An output manifest describes a single Jupyter output. It mirrors the Jupyter message format but replaces inline data with `ContentRef`:

**display_data / execute_result**:

```json
{
  "output_type": "display_data",
  "data": {
    "text/plain": {"inline": "Red Pixel"},
    "image/png": {"blob": "a1b2c3d4...", "size": 45000}
  },
  "metadata": {
    "image/png": {"width": 640, "height": 480}
  }
}
```

**stream** — small logs inline, large logs blob:

```json
{
  "output_type": "stream",
  "name": "stdout",
  "text": {"inline": "training epoch 1/10\n"}
}
```

```json
{
  "output_type": "stream",
  "name": "stdout",
  "text": {"blob": "c3d4e5f6...", "size": 2097152}
}
```

Stream blobs stored with media type `text/plain`.

**error**:

```json
{
  "output_type": "error",
  "ename": "ValueError",
  "evalue": "invalid literal for int()",
  "traceback": {"inline": "[\"Traceback (most recent call last):\", ...]"}
}
```

Traceback is a ContentRef holding the JSON-serialized array of traceback lines. Blob media type `application/json` for the rare massive traceback case.

### Inlining threshold

**Default: 1 KB.** Text below the threshold is inlined in the manifest. Text at or above the threshold goes to the blob store. Binary content always goes to the blob store.

- Most `text/plain`: inline (one request)
- Most images: blob (two requests)
- Small stdout: inline
- Training loop logs: blob
- Error tracebacks: usually inline (1-5 KB)

Daemon-side decision at write time. The frontend just checks `inline` vs `blob`.

### RuntimeStateDoc integration

RuntimeStateDoc stores execution records keyed by `execution_id`. Each record points at output IDs, and each output is a structured manifest object with `ContentRef` fields. The cell's current execution pointer lives in RuntimeStateDoc as well, so live outputs and live execution counts disappear when runtime state is gone. NotebookDoc keeps only source, metadata, ordering, resolved assets, and nbformat/import-export fallback fields.

### Frontend changes

The frontend reads output manifests through WASM/runtime-state sync:

1. RuntimeStateDoc sync updates execution/output records.
2. `frame-pipeline.ts` plans which cells need output or chrome refreshes.
3. `materialize-cells.ts` and `notebook-outputs.ts` resolve manifests with cache-aware helpers.
4. `manifest-resolution.ts` fetches blobs through the daemon blob server when a `ContentRef::Blob` is encountered.

### Python bindings: MIME type contract

The Python bindings delegate output resolution to `crates/runtimed-client/src/output_resolver.rs`, which resolves manifests and ContentRefs into native Python values, typed by MIME category:

| MIME category | Python type | Examples |
|---------------|-------------|----------|
| Text | `str` | `text/plain`, `text/html`, `image/svg+xml`, `application/javascript` |
| Binary | `bytes` | `image/png`, `image/jpeg`, `audio/*`, `video/*` |
| JSON | `dict` / `list` | `application/json`, `*+json` |

Key differences from the frontend path:

- **Binary types return raw bytes, not base64.** Inline binary ContentRefs are base64-decoded before returning to Python; blob ContentRefs are read as raw bytes from disk or HTTP. Python callers receive `bytes` they can write directly to a file or pass to an image library.
- **JSON types return native dicts.** `application/json` and `*+json` ContentRefs are parsed into Python dicts/lists, not returned as JSON strings.
- **`text/llm+plain` synthesis.** When an output contains binary image data but no `text/llm+plain` entry, the output resolver synthesizes one. The synthesized text includes the image MIME type, size in KB, and — when available — the blob URL (`http://localhost:{port}/blob/{hash}`). This gives LLM-based agents a text representation of image outputs without requiring them to consume raw bytes.

The MIME classification logic has one Rust source of truth in `crates/notebook-doc/src/mime.rs`. Rust consumers import `mime_kind()`, `MimeKind`, and `is_binary_mime()` from there. The frontend receives already-resolved `ContentRef` variants from WASM; `looksLikeBinaryMime()` in `manifest-resolution.ts` is only a safety net for unresolved blob refs.

### Key files

| File | Role |
|------|------|
| `crates/runtimed/src/output_store.rs` | Manifest construction, ContentRef, inlining threshold |
| `crates/runtimed/src/blob_server.rs` | HTTP read server (`GET /blob/{hash}`, `GET /health`) |
| `crates/runtimed/src/output_prep.rs` | iopub listener constructs manifests and stores blobs |
| `crates/runtimed-client/src/output_resolver.rs` | Shared manifest resolution, MIME typing, `text/llm+plain` synthesis used by Python/MCP consumers |
| `apps/notebook/src/lib/manifest-resolution.ts` | Resolve `ContentRef` payloads and blob URLs |
| `apps/notebook/src/lib/notebook-outputs.ts` | Output store projected from RuntimeStateDoc |

---

## Phase 7: ipynb round-tripping

The `.ipynb` file on disk is always a valid Jupyter notebook with fully inline outputs. The blob store is acceleration, not a dependency.

### Load (.ipynb -> NotebookDoc + RuntimeStateDoc + blobs)

For each output in the notebook file:

1. **display_data / execute_result**: For each MIME entry — decode base64 for binary types, apply inlining threshold, build manifest
2. **stream**: Inline or blob based on size
3. **error**: Inline traceback (usually small)
4. Store output manifests in RuntimeStateDoc and large payloads in the blob store

Content addressing makes this idempotent.

### Save (NotebookDoc + RuntimeStateDoc + blobs -> .ipynb)

For each cell's current execution output manifests, resolve ContentRefs (inline or blob), reconstruct standard Jupyter output dict (base64-encode binary), and write valid nbformat JSON.

### Metadata hints for fast re-load

Embed blob hashes in ipynb output metadata:

```json
{
  "metadata": {
    "image/png": {
      "runt": {"blob_hash": "a1b2c3d4..."}
    }
  }
}
```

Advisory — if the blob is missing, re-import from inline data.

### Graceful degradation

The .ipynb is always the durable format. If blobs are missing (cache cleared, new machine), fall back to inline data from the file.

### Key files (planned)

| File | Role |
|------|------|
| `crates/runtimed/src/output_store.rs` | Manifest construction during load |
| `crates/notebook/src/lib.rs` | Tauri save/load commands use blob-aware round-tripping |

---

## Phase 8: Daemon-owned kernels

> **Implemented** (PRs #258, #259, #265, #267, #271)

The daemon owns kernel processes and the output pipeline. Notebook windows are views. This is now the default and only kernel execution path.

### Architecture (implemented)

```
Notebook window (thin view)
  +-- sends LaunchKernel/ExecuteCell/RunAllCells to daemon
  +-- receives RuntimeStateDoc sync and ephemeral broadcasts
  +-- syncs cell source via Automerge
  +-- renders outputs from RuntimeStateDoc manifests

runtimed (daemon)
  +-- owns kernel process per notebook room
  +-- subscribes to ZMQ iopub
  +-- writes execution, output, and comm state to RuntimeStateDoc
  +-- broadcasts only ephemeral events
  +-- auto-detects project files for environment selection
```

### Dual-channel design

| Channel | Purpose | Persisted? |
|---------|---------|------------|
| **NotebookDoc Sync** | Persisted notebook state: cells, source, metadata, resolved assets, nbformat fallback fields | Yes |
| **RuntimeStateDoc Sync** | Daemon-authored runtime state: kernel lifecycle, queue, outputs, comms, env progress, trust/project context | No |
| **Broadcasts** | Ephemeral events that are not durable state | No |

**Why all three?** NotebookDoc sync provides local-first editing and persistence. RuntimeStateDoc sync gives late joiners the current daemon-owned state without replaying historical broadcasts. Broadcasts remain only for event-like messages that should not become durable state.

Broadcast types (see `NotebookBroadcast` in `crates/notebook-protocol/src/protocol.rs`):
- `OutputsCleared { cell_id }` — outputs cleared for a cell
- `KernelError { error }` — launch failure or crash
- `Comm { msg_type, content, buffers }` — ephemeral custom comm messages; widget state updates flow through RuntimeStateDoc
- ~~`CommSync`~~ — removed; widget state syncs via RuntimeStateDoc CRDT

Several formerly state-carrying broadcasts are filtered at the relay stage and never reach clients: `KernelStatus`, `ExecutionStarted`, `ExecutionDone`, `QueueChanged`, and `EnvSyncState`. Outputs are rendered from RuntimeStateDoc manifests and resolved through the blob/content-ref layer.

### Project file auto-detection

When daemon receives `LaunchKernel { env_source: "auto" }`:

1. Check notebook metadata for inline deps (`uv.dependencies` / `conda.dependencies`)
2. Walk up from notebook directory looking for project files
3. First match wins (closest-wins semantics)

Detection priority:
| File | env_source |
|------|------------|
| `metadata.runt.uv.dependencies` | `uv:inline` |
| `metadata.runt.conda.dependencies` | `conda:inline` |
| `pyproject.toml` | `uv:pyproject` |
| `pixi.toml` | `pixi:toml` |
| `environment.yml` | `conda:env_yml` |
| No match | `uv:prewarmed` (or `conda:prewarmed` per user pref) |

Walk-up stops at `.git` boundary or home directory.

> **Note:** The daemon also checks the legacy paths `metadata.uv.dependencies` and `metadata.conda.dependencies` as fallbacks for notebooks that haven't been migrated to the `metadata.runt.*` namespace.

### Widget support

> **Implemented** — widget state syncs through RuntimeStateDoc so late-joining windows can reconstruct widget models.

Widgets require bidirectional comm message routing through the daemon:

```
Frontend ←──comm_msg──→ Daemon ←──ZMQ──→ Kernel
```

The implementation:
1. **Kernel → Daemon**: runtime agent records `comm_open`, `comm_msg(update)`, and `comm_close` state in RuntimeStateDoc.
2. **Daemon → Frontend**: clients receive widget state through RuntimeStateDoc sync; late joiners do not need historical `comm_open` broadcasts.
3. **Frontend → Kernel**: frontend-originated widget updates write to RuntimeStateDoc, and the runtime agent diffs comm state on each sync to forward deltas to the kernel.
4. **Ephemeral events**: custom comm messages that are not model state still travel as `NotebookBroadcast::Comm`.

### Benefits

- **Kernel survives window close**: Close notebook, reopen — kernel still running, outputs preserved
- **Multi-window sync**: Both windows see live outputs in real-time
- **Clean separation**: Frontend is a pure rendering layer
- **Project file detection**: Daemon auto-detects pyproject.toml, pixi.toml, environment.yml

### Key files

| File | Role |
|------|------|
| `crates/runtimed/src/output_prep.rs` | Output-prep helpers: iopub → nbformat conversion, widget buffer handling, blob-store offload |
| `crates/runtimed/src/notebook_sync_server/` | Room management, peer sync, persistence, metadata/trust/project context |
| `crates/runtimed/src/requests/` | Notebook request handling |
| `crates/runtimed/src/project_file.rs` | Project file detection for auto-env |
| `crates/notebook-doc/src/lib.rs` | Automerge doc operations, output persistence |
| `crates/notebook/src/lib.rs` | Tauri commands (`launch_kernel_via_daemon`, etc.) |
| `apps/notebook/src/hooks/useDaemonKernel.ts` | Frontend daemon kernel hook |

---

## Design decisions

Cross-cutting decisions that affect multiple phases. These are living answers — expect them to evolve as implementation reveals new constraints.

### Acceptance criteria per phase

**Phase 5**: Two windows open the same notebook, cell source edits propagate between them, and outputs from execution in window A appear in window B. Save from either window produces the same `.ipynb`. The daemon is required — all notebook operations go through the daemon connection.

**Phase 6**: Outputs render from manifests + blob store. Images no longer bloat the CRDT. Re-opening a notebook with existing outputs renders them correctly from blobs, and new execution outputs use the manifest path.

### Output format compatibility

NotebookDoc may still contain legacy nbformat output data from older documents or import/export fallback paths. Current live output rendering reads RuntimeStateDoc execution records and structured output manifests. Save/export resolves those manifests back to nbformat-compatible JSON when writing `.ipynb`.

### ipynb metadata hints are advisory only

Blob hash hints embedded in `.ipynb` output metadata (Phase 7) are a performance optimization, not a correctness requirement. If the blob is missing (cache cleared, new machine), silently re-import from inline data. The `.ipynb` file is always self-contained. Log missing blobs at debug level only.

### Kernel channel is control-plane only (Phase 8)

The kernel channel carries explicit commands (`execute`, `interrupt`, `restart`, `shutdown`) and lightweight events (`status`, `execute_input`). Output content never flows over this channel. It goes: kernel -> daemon iopub listener -> blob store -> automerge doc -> notebook sync -> frontend.

### Blob HTTP security: hash-only, no auth token

Localhost-only binding, content-addressed with 256-bit hashes (unguessable), non-secret data (notebook outputs), read-only. Token-gating would complicate `<img src=...>` URLs for no current threat model. Revisit only if the blob store ever serves content from other users or over a network.

### Multi-window sync latency targets

Source edits: sub-200ms perceived. The `sync_to_daemon` round-trip is ~1-5ms locally (Unix socket). The daemon broadcasts immediately. The bottleneck is React re-render, not sync.

Outputs during execution: the dual delivery path (iopub events for speed, automerge for durability) means the executing window sees outputs instantly. Other windows see them after the automerge round-trip (<50ms). Acceptable for outputs which are inherently asynchronous.

If latency becomes an issue during rapid output bursts (e.g., training loops), the first optimization is batching sync messages rather than syncing per-output.

### Schema versioning: lightweight, not a framework

The notebook doc root contains a `schema_version: u64` field. Current docs are v4 (cells as a `Map` with fractional indexing, outputs in `RuntimeStateDoc` keyed by `execution_id`, per-output `output_id` UUIDs on manifests). `load_or_create_inner` migrates v3 docs to v4 in-place (version bump only — `output_id` is minted at capture time). v1–v2 predate the nteract 2.0 pre-release series and use incompatible cell schemas; those are discarded on load. Any v5 schema MUST ship a `migrate_v4_to_v5` function that preserves user data. No formal migration framework — the schema is simple enough that version-checking `if` branches suffice, but the branch is only correct when the migration actually carries data forward.

For output manifests, the `output_type` field provides structural versioning. New fields can be added without breaking old readers.

---

## Known Limitations

### Output Flow

Output **rendering** is driven by RuntimeStateDoc sync: the daemon writes execution/output manifests into runtime state, produces a sync message, and the Tauri relay forwards raw bytes to the frontend WASM where `materialize-cells.ts` and `notebook-outputs.ts` render them.

Output latency is bounded by the RuntimeStateDoc sync round-trip rather than direct broadcast delivery. That is intentional: it gives every window the same daemon-authored state and avoids duplicate output paths.

### Multi-Window Widget Sync

Widget model state lives in RuntimeStateDoc. New windows receive the current comm map through normal CRDT sync, and the frontend synthesizes the widget model openings needed by the renderer. Custom comm messages remain ephemeral broadcasts because they represent events, not durable widget state.

---

## Summary

| Phase | What | Status |
|-------|------|--------|
| **1** | Daemon & environment pool | Implemented |
| **2** | CRDT sync (settings + notebooks) | Implemented (PR #220, #223) |
| **3** | Blob store (on-disk CAS + HTTP server) | Implemented (PR #220) |
| **4** | Protocol consolidation (single socket) | Implemented (PR #220, #223) |
| **5** | Local-first Automerge notebook sync | Implemented — frontend owns local Automerge doc via `runtimed-wasm` WASM, cell mutations happen in WASM, sync to daemon via binary messages |
| **6** | Output store (manifests, ContentRef, inlining) | Implemented (PR #237) |
| **7** | ipynb round-tripping | Future (outputs already persist in nbformat) |
| **8** | Daemon-owned kernels | Implemented (PRs #258, #259, #265, #267, #271) |
