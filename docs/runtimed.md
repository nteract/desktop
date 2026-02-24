# runtimed Architecture

## Vision

runtimed is a long-lived daemon that owns the heavy, stateful parts of the notebook experience — environment pools, kernel processes, output storage, and document sync. Notebook windows become thin views: they subscribe to a CRDT document, render output from a blob store, and send execution requests. When the last window closes, the daemon keeps kernels alive and outputs safe. When a new window opens, it catches up instantly.

The architecture has two core ideas:

1. **Outputs live outside the CRDT.** Kernel outputs (images, HTML, logs) are write-once blobs from a single actor. Storing them in an Automerge document wastes CRDT history tracking on data that will never be concurrently edited. Instead, outputs go into a content-addressed blob store. The CRDT stores lightweight hash references.

2. **Two levels of output abstraction.** An "output" (the Jupyter-level concept — a display_data, stream, error, etc.) is described by a manifest that references raw content blobs. Small data is inlined in the manifest; large data points to the blob store. `GET /output/{id}` returns the manifest. `GET /blob/{hash}` returns raw bytes. Most renders need only one request.

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
│  │ Kernel manager (future)                 │    │
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

Only one daemon per user. A file lock (`~/.cache/runt/daemon.lock`) provides mutual exclusion. A sidecar JSON file (`~/.cache/runt/daemon.json`) advertises the running daemon's state:

```rust
pub struct DaemonInfo {
    pub endpoint: String,
    pub pid: u32,
    pub version: String,
    pub started_at: DateTime<Utc>,
    pub blob_port: Option<u16>,
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

### Settings.json file watcher

The daemon watches `~/.config/runt-notebook/settings.json` for external edits. Changes are debounced (500ms), applied to the Automerge settings doc, persisted as Automerge binary (not back to JSON, to avoid formatting churn), and broadcast to all connected sync clients.

### Service management

| Platform | Mechanism |
|----------|-----------|
| macOS | launchd user agent (`~/Library/LaunchAgents/io.runtimed.plist`) |
| Linux | systemd user service (`~/.config/systemd/user/runtimed.service`) |
| Windows | VBS script in Startup folder |

Auto-upgrade: the client detects version mismatches and replaces the binary.

### Key files

| File | Role |
|------|------|
| `daemon.rs` | Daemon state, pool management, warming loops, connection routing |
| `protocol.rs` | Request/Response enums, BlobRequest/BlobResponse |
| `connection.rs` | Unified framing, handshake enum, send/recv helpers |
| `client.rs` | Client library (PoolClient) for notebook apps |
| `singleton.rs` | File locking, DaemonInfo discovery |
| `service.rs` | Platform-specific install/start/stop |
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

The daemon holds the canonical document, persisted to `~/.cache/runt/settings.automerge` with a JSON mirror at `~/.config/runt-notebook/settings.json`. Backward-compatible migration from flat keys (`default_uv_packages: "numpy, pandas"`) to nested structures.

**Wire protocol**: Length-prefixed binary frames (4-byte BE length + Automerge sync message). Bidirectional, long-lived connections. Broadcast channel notifies all peers when any peer changes a setting.

### Notebook document sync

Each open notebook gets a "room" in the daemon. Multiple windows editing the same notebook sync through the room's canonical document.

**Document schema** (Automerge CRDT):

```
ROOT/
  notebook_id: Str
  cells/                        <- List of Map
    [i]/
      id: Str                   <- cell UUID
      cell_type: Str            <- "code" | "markdown" | "raw"
      source: Text              <- Automerge Text CRDT (character-level merging)
      execution_count: Str      <- JSON-encoded i32 or "null"
      outputs/                  <- List of Str (Phase 6 changes these to manifest hashes)
        [j]: Str                <- JSON-encoded Jupyter output
  metadata/
    runtime: Str
```

**Design decisions**:
- Cell `source` uses `ObjType::Text` for proper concurrent edit merging. `update_source()` uses Automerge's `update_text()` (Myers diff internally) for efficient character-level patches.
- `outputs` are write-once from a single actor (the kernel), so they don't need CRDT text semantics. Stored as JSON strings now. Phase 6 changes these to output manifest hashes.
- `execution_count` is a string for JSON serialization consistency.

### Room architecture

```rust
pub struct NotebookRoom {
    pub doc: Arc<RwLock<NotebookDoc>>,
    pub changed_tx: broadcast::Sender<()>,
    pub persist_path: PathBuf,
    pub active_peers: AtomicUsize,
}
```

**Room lifecycle**:
1. First window opens notebook -> daemon acquires room via `get_or_create_room()`, loading persisted doc from disk (or creating fresh)
2. Client sends `Handshake::NotebookSync { notebook_id }`, then exchanges Automerge sync messages
3. Additional windows join the same room, incrementing `active_peers`
4. Changes from any peer -> applied under write lock -> persisted to disk (outside lock) -> broadcast to all other peers
5. Last peer disconnects -> `active_peers` hits 0 -> room evicted from map (doc already on disk)

**Persistence**: Documents saved to `~/.cache/runt/notebook-docs/{sha256(notebook_id)}.automerge`. SHA-256 hashing sanitizes notebook IDs (which may be file paths with special characters) into safe filenames. Persistence runs after every sync message, with serialization inside the write lock and disk I/O outside it.

**Corrupt document recovery**: If a persisted `.automerge` file can't be loaded, it's renamed to `.automerge.corrupt` and a fresh document is created. This preserves the corrupt data for debugging without blocking the user.

### Sync protocol

1. **Initial sync**: Server sends first. Both sides exchange Automerge sync messages with 100ms timeout until convergence.
2. **Watch loop**: `tokio::select!` on two channels — incoming frames from this client, and broadcast notifications from other peers. When either fires, generate and send sync messages.
3. **Persistence**: After applying each peer message, `doc.save()` runs inside the write lock (serialization), then `persist_notebook_bytes()` writes to disk outside the lock (I/O doesn't block other peers).

### Key files

| File | Role |
|------|------|
| `settings_doc.rs` | Settings Automerge document, schema, migration |
| `sync_server.rs` | Settings sync handler |
| `sync_client.rs` | Settings sync client library |
| `notebook_doc.rs` | Notebook Automerge document, cell CRUD, text editing, persistence |
| `notebook_sync_server.rs` | Room-based notebook sync, peer management, eviction |
| `notebook_sync_client.rs` | Notebook sync client library |

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

### API (async)

```rust
pub struct BlobStore { root: PathBuf }

impl BlobStore {
    pub async fn put(&self, data: &[u8], media_type: &str) -> io::Result<String>;
    pub async fn get(&self, hash: &str) -> io::Result<Option<Vec<u8>>>;
    pub async fn get_meta(&self, hash: &str) -> io::Result<Option<BlobMeta>>;
    pub async fn exists(&self, hash: &str) -> bool;
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

Port advertised in `daemon.json` via `DaemonInfo.blob_port`.

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
    NotebookSync { notebook_id: String },
    Blob,
}
```

The daemon's `route_connection()` reads the handshake via `recv_control_frame()` and dispatches:

| Channel | After handshake | Lifetime |
|---------|----------------|----------|
| `Pool` | Length-framed JSON request/response | Short-lived |
| `SettingsSync` | Automerge sync messages | Long-lived, bidirectional |
| `NotebookSync` | Automerge sync messages, room-routed by `notebook_id` | Long-lived, bidirectional |
| `Blob` | Binary blob writes | Short-lived |

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
| `connection.rs` | Unified framing, handshake enum, send/recv helpers |
| `daemon.rs` | Single accept loop, `route_connection()` dispatcher |
| `client.rs` | Uses `Handshake::Pool` |
| `sync_client.rs` | Uses `Handshake::SettingsSync` |
| `sync_server.rs` | Handler function (no longer owns accept loop) |
| `notebook_sync_client.rs` | Uses `Handshake::NotebookSync` |
| `notebook_sync_server.rs` | Handler function, room lookup |
| `protocol.rs` | `BlobRequest`/`BlobResponse` enums |

---

## Phase 5: Tauri <-> daemon notebook sync

> **Implemented** (PR #233, #238, #241)

Wire the Tauri app and React frontend to use the daemon's automerge doc as the source of truth for notebook state. This gives us multi-window sync immediately. Outputs still flow as inline JSON strings through the CRDT for now — Phase 6 makes them efficient.

### Current state (what changes)

Today the Tauri process holds notebook state in-memory:

```rust
pub struct NotebookState {
    pub notebook: Notebook,      // nbformat v4 — cells, outputs, metadata
    pub path: Option<PathBuf>,
    pub dirty: bool,
}
```

Cell mutations (`update_cell_source`, `add_cell`, `delete_cell`) modify this struct directly. Outputs flow as events from the kernel iopub listener through Tauri events to React state — they never touch `NotebookState` during execution. On save, the frontend's current state is serialized to `.ipynb`.

```
kernel -> iopub -> Tauri event "kernel:iopub" -> React state -> render
                                                     | (on save)
                                                 .ipynb file
```

### Target state

Replace `NotebookState` with a `NotebookSyncClient` connected to the daemon. The automerge doc becomes the single source of truth.

```
kernel -> iopub -> Tauri writes to automerge doc -> daemon syncs to all peers
                                                        |
frontend <- Tauri event "notebook:updated" <- Tauri recv_changes()
    |
render
```

### Tauri backend changes

**On notebook open** (`load_notebook`):
1. Determine notebook_id (derive from file path, or from notebook metadata)
2. Connect `NotebookSyncClient` to daemon with that notebook_id
3. If opening an .ipynb from disk: populate the automerge doc from the file contents (cells, source, metadata, outputs as JSON strings)
4. If reconnecting to existing room (another window already has it open): sync from the daemon's canonical doc
5. Spawn background task: `sync_client.recv_changes()` loop -> emit Tauri event `notebook:updated` with cell snapshots

**Cell mutations** — existing Tauri commands change to write through automerge:
- `update_cell_source(cell_id, source)` -> `sync_client.update_source(cell_id, source)` (character-level patch via `update_text`)
- `add_cell(cell_type, after_cell_id)` -> `sync_client.add_cell(index, cell_id, cell_type)`
- `delete_cell(cell_id)` -> `sync_client.delete_cell(cell_id)`

**Kernel outputs** — the **frontend** drives output sync, not the Tauri iopub listener. `App.tsx`'s `handleOutput` calls `invoke("sync_append_output")` after appending to React state. This design avoids syncing widget-captured outputs to the CRDT:
- `stream` / `display_data` / `execute_result` / `error` -> `handleOutput` calls `sync_append_output(cell_id, json_output_string)`
- `execute_cell` clears outputs first: `sync_clear_outputs(cell_id)`
- `execute_input` syncs execution count: `sync_execution_count(cell_id, count)`

The Tauri event `kernel:iopub` still fires for low-latency display (the frontend renders immediately from the event, then reconciles when the automerge update arrives). This is the same data, two delivery paths — event for speed, automerge for durability and sync. The sync calls are fire-and-forget (errors logged, not propagated to the UI).

**On save** (`save_notebook`):
1. Read cell state from the sync client's local replica
2. Serialize to nbformat `.ipynb` (same as today, but source of truth is automerge doc, not `NotebookState`)

### Frontend changes

**`useNotebook.ts`** — the hook's public API stays the same. Internally:
- `cells` state populated from `notebook:updated` Tauri events (instead of only `load_notebook`)
- `appendOutput` / `clearCellOutputs` / `setExecutionCount` still update local React state for immediate rendering
- When `notebook:updated` arrives from Tauri (triggered by automerge sync), reconcile local state

The frontend doesn't know about automerge. It still calls Tauri commands and receives Tauri events. The sync layer is invisible.

**`OutputArea.tsx`** — no changes in this phase. Outputs are still `JupyterOutput[]` objects parsed from JSON strings. Phase 6 changes this.

### What multi-window sync gives us

- Two windows open the same notebook -> both see the same cells, source, and outputs
- Edit source in window A -> window B sees the change (character-level merge via Automerge Text CRDT)
- Execute cell in window A -> outputs appear in window B (JSON strings synced through automerge)
- Both windows save -> same `.ipynb` content (both read from the same automerge doc)

### Known limitations

- **Output flicker in executing window** (#242): Outputs arrive via iopub (~10ms), the frontend renders them, then the automerge round-trip arrives (~100ms) and reconciliation re-renders. This can cause a brief visual flicker. Resolved by Phase 8 (daemon-managed kernels eliminate the dual delivery path).
- **Stream output fragmentation in receiving windows** (#243): Consecutive stdout prints (e.g., a `for` loop with `print()`) merge into a single stream block in the executing window, but each `sync_append_output` call creates a separate CRDT list entry. Receiving windows see fragmented output with extra spacing. Options: receive-side merging (practical short-term), daemon-layer merging (cleanest, ties into Phase 8), or debounced batching.
- **Stale execution_count on re-execution** (#244): `sync_clear_outputs` fires but `execution_count` remains stale until the kernel sends `execute_input`. Other windows briefly show the old count. Fix: clear execution_count alongside outputs atomically.

### Key files

| File | Role |
|------|------|
| `crates/notebook/src/lib.rs` | Tauri commands: `sync_append_output`, `sync_execution_count`, `sync_clear_outputs`, cell CRUD via sync client |
| `crates/notebook/src/notebook_state.rs` | Holds `NotebookSyncHandle` (send half of the sync client) |
| `crates/runtimed/src/notebook_sync_client.rs` | Handle/receiver split pattern for Tauri state sharing |
| `apps/notebook/src/App.tsx` | `handleOutput` routes output sync — frontend drives sync to avoid widget double-sync |
| `apps/notebook/src/hooks/useNotebook.ts` | Reconciliation logic, `notebook:updated` event handling |
| `apps/notebook/src/hooks/useKernel.ts` | Syncs execution counts via `sync_execution_count` |

---

## Phase 6: Output store

> **Foundation implemented** (PR #237 — `output_store.rs` with `ContentRef`, manifests, inlining threshold). Integration into the live pipeline is next.

Move outputs from inline JSON in the CRDT to the blob store. This solves the CRDT bloat problem from Phase 5 and introduces two-level serving.

**Note:** Stream output fragmentation (#243) is orthogonal to this phase — manifests don't fix the CRDT-level issue of per-`print()` list entries. That's addressed at the sync layer (see Phase 5 known limitations).

### The two levels

**Level 1 — Blob store** (`GET /blob/{hash}`): Pure content-addressed bytes. Returns raw PNG, text, JSON — whatever was stored. Used for `<img src>`, direct rendering, large data.

**Level 2 — Output store** (`GET /output/{id}`): Jupyter-aware. Returns structured information about an output — what type it is, what representations are available, and the data itself (inlined for small content, blob-referenced for large content). Used by the frontend to understand what to render.

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

**Default: 8 KB.** Below -> inline in manifest. Above -> blob store.

- Most `text/plain`: inline (one request)
- Most images: blob (two requests)
- Small stdout: inline
- Training loop logs: blob
- Error tracebacks: usually inline (1-5 KB)

Daemon-side decision at write time. The frontend just checks `inline` vs `blob`.

### Manifest storage

Manifests are themselves blobs (media type `application/x-jupyter-output+json`), content-addressed. `GET /output/{id}` is a thin view over `GET /blob/{hash}` that validates the media type.

### Automerge doc integration

Outputs change from JSON strings to manifest hashes:

```
cell/
  outputs/           <- List of Str
    [0]: Str         <- output manifest hash (e.g. "a1b2c3d4...")
```

The CRDT stores only hashes (~64 bytes each). All output structure and content lives in the blob store:
- No CRDT bloat from images or large text
- Clearing outputs removes hashes (no tombstone inflation from large data)
- Output history doesn't accumulate in the Automerge change log

### Tauri backend changes

The iopub listener (from Phase 5) changes what it writes to automerge:

**Before** (Phase 5): `sync_client.append_output(cell_id, json_string)` — full JSON output
**After** (Phase 6):
1. For each MIME type / stream text / traceback: size < 8KB -> inline, >= 8KB -> blob store via daemon
2. Construct output manifest JSON
3. Store manifest in blob store -> get manifest hash
4. `sync_client.append_output(cell_id, manifest_hash)` — just the hash

### Frontend changes

**`OutputArea.tsx`** — the big change. Currently receives `JupyterOutput[]` (parsed JSON). Now receives `string[]` (manifest hashes).

New rendering flow:
1. Cell outputs = `["hash1", "hash2", ...]`
2. For each hash, fetch `GET /output/{hash}` -> manifest JSON
3. Parse manifest, select MIME type by priority
4. For `ContentRef::Inline` — use data directly
5. For `ContentRef::Blob` — `<img src="http://localhost:{port}/blob/{blobHash}">` for images, `fetch()` for HTML/text

This needs a loading state per output (while manifest is being fetched) and caching (manifests are immutable, cache aggressively).

**Stream output handling during execution**: The iopub listener still emits `kernel:iopub` events for live display. The frontend renders stream text incrementally from events. When execution finishes, the finalized manifest hash appears in the automerge doc. The frontend transitions from live event-driven display to blob-backed display.

### Key files

| File | Role |
|------|------|
| `crates/runtimed/src/output_store.rs` | Manifest construction, ContentRef, inlining threshold |
| `crates/runtimed/src/blob_server.rs` | Add `GET /output/{id}` endpoint |
| `crates/notebook/src/kernel.rs` | iopub listener constructs manifests and stores blobs |
| `apps/notebook/src/components/cell/OutputArea.tsx` | Fetch manifests, resolve blob URLs |
| `apps/notebook/src/hooks/useOutputManifest.ts` | Hook for fetching/caching output manifests |

---

## Phase 7: ipynb round-tripping

The `.ipynb` file on disk is always a valid Jupyter notebook with fully inline outputs. The blob store is acceleration, not a dependency.

### Load (.ipynb -> automerge + blobs)

For each output in the notebook file:

1. **display_data / execute_result**: For each MIME entry — decode base64 for binary types, apply inlining threshold, build manifest
2. **stream**: Inline or blob based on size
3. **error**: Inline traceback (usually small)
4. Store manifest in blob store -> append manifest hash to automerge doc

Content addressing makes this idempotent.

### Save (automerge + blobs -> .ipynb)

For each manifest hash: fetch manifest, resolve ContentRefs (inline or blob), reconstruct standard Jupyter output dict (base64-encode binary), write valid nbformat JSON.

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

### Key files

| File | Role |
|------|------|
| `crates/runtimed/src/ipynb.rs` | Load/save, base64, metadata hints |
| `crates/runtimed/src/output_store.rs` | Manifest construction during load |
| `crates/notebook/src/lib.rs` | Tauri save/load commands use blob-aware round-tripping |

---

## Phase 8: Daemon-owned kernels

> See #242 for the motivating timing race between iopub and Automerge delivery paths.

The endgame. The daemon takes ownership of kernel processes and the full output pipeline. Notebook windows become pure views. This also resolves the Phase 5 known limitations: output flicker (#242), stream fragmentation (#243), and stale execution counts (#244) all go away when the daemon is the single actor writing outputs to the CRDT.

### Current model (Tauri-managed, through Phase 6)

```
Notebook window (Tauri)
  +-- spawns kernel process
  +-- subscribes to ZMQ iopub
  +-- writes outputs to blob store + automerge
  +-- renders from blob HTTP URLs
```

Closing the window still kills the kernel.

### Target model

```
Notebook window (thin view)
  +-- sends "execute cell" request to daemon
  +-- subscribes to automerge doc for output hashes
  +-- fetches output manifests + blobs via HTTP
  +-- renders

runtimed (daemon)
  +-- owns kernel process
  +-- subscribes to ZMQ iopub
  +-- writes blobs to store
  +-- constructs output manifests
  +-- updates automerge doc with manifest hashes
```

### Output pipeline

```
kernel process
    | ZMQ iopub
    v
daemon iopub listener
    | parse Jupyter message
    v
output processor
    | for each MIME type / stream text / traceback:
    |   size < 8KB? -> inline
    |   size >= 8KB? -> blob store -> hash
    v
manifest builder
    | construct output manifest JSON
    | store manifest -> blob store -> manifest hash
    v
automerge doc
    | append manifest hash to cell outputs
    v
sync broadcast
    | all subscribed windows receive the new hash
    v
frontend (per window)
    | fetch GET /output/{hash} -> manifest
    | fetch GET /blob/{hash} for large data
    v
render
```

### Benefits

- **Kernel survives window close**: Close the laptop, reopen — kernel still running, outputs preserved
- **Multi-window**: Both windows see live output in real-time
- **Clean separation**: Frontend is a pure rendering layer
- **Output dedup**: Same image from two cells stored once

### Execution protocol

New channel on the unified socket:

```json
{"channel": "kernel", "notebook_id": "abc123"}
```

```json
// Client -> Daemon
{"action": "execute", "cell_id": "cell-uuid", "code": "print('hello')"}
{"action": "interrupt"}
{"action": "restart"}
{"action": "shutdown"}

// Daemon -> Client
{"event": "status", "state": "busy"}
{"event": "status", "state": "idle"}
{"event": "execute_input", "cell_id": "cell-uuid", "execution_count": 5}
```

Output content doesn't flow over this channel. The daemon writes to the blob store and automerge doc. The frontend picks up changes via notebook sync.

### Key files

| File | Role |
|------|------|
| `crates/runtimed/src/kernel_manager.rs` | Kernel process lifecycle, ZMQ subscriptions |
| `crates/runtimed/src/output_processor.rs` | iopub -> blob store -> manifest -> automerge |
| `crates/runtimed/src/protocol.rs` | Kernel channel message types |
| `crates/notebook/src/lib.rs` | Tauri commands thin down to daemon RPC |

---

## Design decisions

Cross-cutting decisions that affect multiple phases. These are living answers — expect them to evolve as implementation reveals new constraints.

### Acceptance criteria per phase

**Phase 5** *(met)*: Two windows open the same notebook, cell source edits propagate between them, and outputs from execution in window A appear in window B. Save from either window produces the same `.ipynb`. The sync client connects gracefully — if the daemon isn't running, sync operations are no-ops (errors are logged but don't block the UI). Known limitations deferred to Phase 8: output flicker (#242), stream fragmentation (#243), stale execution_count (#244).

**Phase 6**: Outputs render from manifests + blob store. Images no longer bloat the CRDT. Re-opening a notebook with existing outputs renders them correctly from blobs, and new execution outputs use the manifest path.

### Output format backward compatibility (Phase 5 -> 6)

The outputs list is `List of Str`. A string that starts with `{` and parses as a Jupyter output object is Phase 5 inline JSON. A string that's 64 hex characters is a Phase 6 manifest hash. The reader can detect which format it's looking at trivially. Phase 6 rolls out incrementally — old outputs keep working, new outputs use manifests. No migration step needed. This is the key enabler for incremental Phase 6 work: the frontend can support both formats simultaneously.

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

Add a `schema_version: Str` field to the notebook doc root (`"1"` for Phase 5, `"2"` when Phase 6 changes outputs to manifest hashes). The reader checks this on load and handles both versions with simple branching. No formal migration framework — the schema is simple enough that version-checking `if` branches suffice. This mirrors how settings doc migration already works (flat keys -> nested structure).

For output manifests, the `output_type` field provides structural versioning. New fields can be added without breaking old readers.

---

## Summary

| Phase | What | Status |
|-------|------|--------|
| **1** | Daemon & environment pool | Implemented |
| **2** | CRDT sync (settings + notebooks) | Implemented (PR #220, #223) |
| **3** | Blob store (on-disk CAS + HTTP server) | Implemented (PR #220) |
| **4** | Protocol consolidation (single socket) | Implemented (PR #220, #223) |
| **5** | Tauri <-> daemon notebook sync (multi-window) | Implemented (PR #233, #238, #241) — known limitations: #242, #243, #244 |
| **6** | Output store (manifests, ContentRef, inlining) | Foundation landed (PR #237), pipeline integration next |
| **7** | ipynb round-tripping | After 6 |
| **8** | Daemon-owned kernels (#242) | After 7 — resolves Phase 5 limitations |
