# Runtime Protocol

This document describes the wire protocol between notebook clients (frontend WASM + Tauri relay) and the runtimed daemon.

## Compatibility

Two independent version numbers handle compatibility, separate from the artifact version:

- **Protocol version** (`PROTOCOL_VERSION` in `connection.rs`, currently `2`) — governs wire compatibility. Validated by the 5-byte magic preamble (`0xC0DE01AC` + version byte) at the start of every connection. Bump when the framing, handshake shape, or message serialization format changes.
- **Schema version** (`SCHEMA_VERSION` in `notebook-doc/src/lib.rs`, currently `2`) — governs Automerge document compatibility. Stored in the doc root as `schema_version`. Bump when the document structure changes (v2 switched cells from an ordered list to a fractional-indexed map).

These are just incrementing integers. They evolve independently from each other and from the artifact version. A protocol or schema bump doesn't automatically force a major version bump — that depends on whether the change is user-facing.

Artifact versions follow standard semver based on what users see.

### Release channels

**Stable:** Pushing a `v*` tag publishes Python wheels to PyPI at the version in `pyproject.toml`. No separate `python-v*` tag needed — the desktop release ships the Python package too.

**Nightly:** Daily builds publish PEP 440 alpha pre-releases (e.g., `2.0.1a202603100900`). Install with `pip install runtimed --pre`.

**Python-only:** The `python-v*` tag path (`python-package.yml`) exists for Python-specific patches that don't need a full desktop release.

See `contributing/releasing.md` for the full release procedures.

### Connection preamble

Every connection starts with a 5-byte preamble before the JSON handshake frame:

| Bytes | Content |
|-------|---------|
| 0–3 | Magic: `0xC0 0xDE 0x01 0xAC` |
| 4 | Protocol version (currently `2`) |

The daemon validates both before reading the handshake. Non-runtimed connections get a clear "invalid magic bytes" error. Protocol mismatches are rejected before any JSON parsing.

After the preamble, the notebook sync path also returns `protocol_version` and `daemon_version` in its `ProtocolCapabilities` / `NotebookConnectionInfo` responses for informational purposes.

### Desktop app compatibility

The desktop app bundles its own daemon binary. Version-mismatch detection between the app and its bundled daemon compares git commit hashes (appended as `+{sha}` at build time), not semver. This is because both are always built from the same commit in CI.

## Overview

The notebook app communicates with runtimed over a Unix socket (named pipe on Windows) using length-prefixed, typed frames. The protocol carries three kinds of traffic:

1. **Automerge sync** — binary CRDT sync messages that keep the notebook document consistent between the frontend WASM peer and the daemon peer
2. **Request/response** — JSON messages where a client asks the daemon to do something (execute a cell, launch a kernel) and gets a reply
3. **Broadcasts** — JSON messages the daemon pushes to all connected clients (kernel output, status changes, environment progress)

## Connection Topology

```
┌─────────────────────────────────────────────────────────┐
│  Notebook Window (Tauri webview)                        │
│                                                         │
│  ┌──────────┐   Tauri invoke()   ┌──────────────────┐  │
│  │ Frontend  │ ←───────────────→ │   Tauri Relay     │  │
│  │ (WASM +   │   Tauri events    │ (NotebookSync-    │  │
│  │  React)   │ ←──────────────── │  Client)          │  │
│  └──────────┘                    └────────┬─────────┘  │
│                                           │             │
└───────────────────────────────────────────│─────────────┘
                                            │ Unix socket
                                            ▼
                                   ┌─────────────────┐
                                   │    runtimed      │
                                   │  (daemon)        │
                                   └─────────────────┘
```

The **Tauri relay** is a transparent byte pipe for Automerge sync frames — it does not maintain its own document replica. It forwards raw bytes between the WASM peer and the daemon peer. For requests and broadcasts, it bridges Tauri IPC commands to the daemon's socket protocol.

## Connection Lifecycle

### 1. Opening a notebook

The frontend invokes a Tauri command (`open_notebook_in_new_window`), which causes the relay to connect to the daemon's Unix socket and send a handshake frame. New notebook creation goes through Rust menu events (`spawn_new_notebook()` → `create_notebook_window_for_daemon()`), not a frontend `invoke()`.

### 2. Handshake

The first frame is a JSON `Handshake` message:

```json
{
  "channel": "notebook_sync",
  "notebook_id": "/path/to/notebook.ipynb",
  "protocol": "v2"
}
```

The `Handshake` enum uses `#[serde(tag = "channel", rename_all = "snake_case")]`, so the wire format is flat with a `"channel"` discriminator field — not nested. Optional fields like `working_dir` and `initial_metadata` are omitted when `None` (via `skip_serializing_if`).

Other handshake variants include `Pool`, `SettingsSync`, `Blob`, `PoolStateSubscribe`, `OpenNotebook { path }`, and `CreateNotebook { runtime, ... }`. The `OpenNotebook` and `CreateNotebook` variants are the primary paths for opening/creating notebooks from the desktop app, while `NotebookSync` is used by programmatic clients (e.g., Python bindings).

The daemon responds with a `NotebookConnectionInfo`:

```json
{
  "protocol": "v2",
  "notebook_id": "derived-id",
  "cell_count": 5,
  "needs_trust_approval": false
}
```

The `protocol_version`, `daemon_version`, and `error` fields are `Option` types with `skip_serializing_if = "Option::is_none"`, so they are only present when the daemon populates them. `protocol_version` and `daemon_version` appear for version negotiation; `error` appears only in failure cases. A minimal successful response omits all three.

### 3. Initial Automerge sync

After the handshake, both sides exchange Automerge sync messages until their documents converge. The frontend starts with an empty document — all notebook state comes from the daemon during this sync phase. A 2-second timeout guards against the initial socket connection; the sync loop itself uses a 100ms per-frame timeout to drain incoming frames.

### 4. Steady state

Once synced, the connection carries all three frame types concurrently: ongoing Automerge sync for cell edits, request/response for explicit actions, and broadcasts for kernel activity.

### 5. Disconnection

When the broadcast stream ends, the relay emits a `daemon:disconnected` event to the frontend. A generation counter prevents stale callbacks from earlier connections from processing events after reconnection.

## Wire Format

### Frame structure

Every message on the socket is length-prefixed:

```
┌──────────────┬──────────────────────┐
│ 4 bytes      │ N bytes              │
│ (big-endian  │ (payload)            │
│  u32 length) │                      │
└──────────────┴──────────────────────┘
```

Maximum frame sizes: 100 MiB for data frames, 64 KiB for control/handshake frames.

### Typed frames

After the handshake, frames are typed by their first byte:

| Type byte | Name               | Payload format |
|-----------|--------------------|----------------|
| `0x00`    | AutomergeSync      | Binary (raw Automerge sync message) |
| `0x01`    | NotebookRequest    | JSON |
| `0x02`    | NotebookResponse   | JSON |
| `0x03`    | NotebookBroadcast  | JSON |
| `0x04`    | Presence           | Binary (CBOR, see `notebook_doc::presence`) |

## Automerge Sync

The notebook document is a CRDT shared between two peers:

- **Frontend (WASM)** — `NotebookHandle` from `crates/runtimed-wasm`, compiled to WASM and loaded in the webview. Cell mutations (add, delete, edit source) happen instantly in the local WASM document.
- **Daemon** — `NotebookDoc` from `crates/notebook-doc`. The canonical document used for kernel execution, output writing, and persistence.

Both sides use the same Rust `automerge = "0.7"` crate, which guarantees schema compatibility (the JS `@automerge/automerge` package uses different CRDT types for string fields).

### Sync flow

```
User types in cell
  → React calls WASM handle.update_source(cell_id, text)
  → WASM applies mutation locally (instant)
  → handle.generate_sync_message() → sync bytes
  → Frontend prepends 0x00 type byte → invoke("send_frame", { frameData })
  → Tauri send_frame dispatches by type → relay pipes to daemon socket
  → Daemon applies sync, updates canonical doc
  → Daemon generates response sync message → frame type 0x00
  → Relay receives, emits "notebook:frame" Tauri event (raw typed bytes)
  → Frontend useAutomergeNotebook listener → WASM handle.receive_frame(bytes)
  → WASM demuxes by first byte, applies sync, returns FrameEvent[]
  → sync_applied event → materializeCells() updates React state if doc changed
  → sync_reply event → prepend 0x00, invoke("send_frame", { frameData }) back to daemon
```

## Request / Response

Requests are one-shot JSON messages sent from the client to the daemon. Each request gets exactly one response.

### Key request types

| Request | Purpose |
|---------|---------|
| `LaunchKernel` | Start a kernel with environment config |
| `ExecuteCell { cell_id }` | Queue a cell for execution (daemon reads source from synced doc) |
| `ClearOutputs { cell_id }` | Clear a cell's outputs |
| `InterruptExecution` | Send SIGINT to the running kernel |
| `ShutdownKernel` | Stop the kernel process |
| `RunAllCells` | Execute all code cells in order |
| `SaveNotebook` | Persist the Automerge doc to `.ipynb` on disk |
| `SyncEnvironment` | Hot-install packages into the running kernel's environment |
| `SendComm { message }` | Send a comm message to the kernel (widget interactions) |
| `Complete { code, cursor_pos }` | Get code completions from the kernel |
| `GetHistory { pattern, n, unique }` | Search kernel input history |
| `GetKernelInfo` | Query current kernel status |
| `GetQueueState` | Query the execution queue |

### Key response types

| Response | Meaning |
|----------|---------|
| `KernelLaunched { env_source, ... }` | Kernel started, includes environment origin label |
| `CellQueued` | Cell added to execution queue |
| `NotebookSaved` | File written to disk |
| `CompletionResult { items, cursor_start, cursor_end }` | Code completion results (`items: Vec<CompletionItem>`) |
| `Error { error }` | Something went wrong |

### Request flow through the stack

```
Frontend: invoke("execute_cell_via_daemon", { cellId })
  → Tauri command handler
  → Relay: handle.send_request(NotebookRequest::ExecuteCell { cell_id })
  → Frame type 0x01 sent on socket
  → Daemon processes request
  → Frame type 0x02 returned
  → Relay receives response via oneshot channel
  → Returns to frontend
```

## Broadcasts

Broadcasts are daemon-initiated messages pushed to all connected clients for a notebook. They are not replies to any specific request.

### Key broadcast types

| Broadcast | Purpose |
|-----------|---------|
| `KernelStatus { status, cell_id }` | Kernel state changed: `"starting"`, `"idle"`, `"busy"`, `"error"`, `"shutdown"` |
| `ExecutionStarted { cell_id, execution_count }` | A cell began executing |
| `Output { cell_id, output_type, output_json, output_index }` | Cell produced output (stdout, display data, error) |
| `DisplayUpdate { display_id, data, metadata }` | Update an existing output by display ID |
| `ExecutionDone { cell_id }` | Cell execution completed |
| `QueueChanged { executing, queued }` | Execution queue state changed |
| `KernelError { error }` | Kernel crashed or failed to launch |
| `Comm { msg_type, ... }` | Jupyter comm message (widget open/msg/close) |
| `FileChanged` | External file change merged into the doc |
| `EnvProgress { env_type, phase }` | Environment setup progress (`phase` is a flattened `EnvProgressPhase`) |
| `EnvSyncState { in_sync, diff }` | Notebook dependencies drifted from launched kernel config |

### Broadcast flow

```
Kernel produces output
  → Daemon intercepts Jupyter IOPub message
  → Daemon writes output to Automerge doc (as blob manifest)
  → Daemon sends NotebookBroadcast::Output on broadcast channel
  → Frame type 0x03 sent to all connected clients
  → Relay receives, emits "notebook:frame" Tauri event
  → WASM handle.receive_frame() demuxes → Broadcast event
  → useAutomergeNotebook dispatches via emitBroadcast() (in-memory frame bus)
  → useDaemonKernel subscribeBroadcast() callback processes the broadcast
  → UI updates
```

## Tauri Event Bridge & Frame Bus

The relay and frontend use these Tauri events for cross-process communication:

| Event | Direction | Payload | Purpose |
|-------|-----------|---------|---------|
| `notebook:frame` | Relay → Frontend | `number[]` (typed frame bytes) | All daemon frames (sync, broadcast, presence) via unified pipe |
| `daemon:ready` | Relay → Frontend | `DaemonReadyPayload` | Connection established, ready to bootstrap |
| `daemon:disconnected` | Relay → Frontend | — | Connection to daemon lost |

Outgoing frames from the frontend use `invoke("send_frame", { frameData })` where `frameData` is `number[]` with the first byte as the frame type. Only `0x00` (AutomergeSync) and `0x04` (Presence) are valid outgoing types.

### In-memory frame bus

After WASM `receive_frame()` demuxes typed frames, broadcast and presence payloads are dispatched via an in-memory pub/sub bus (`notebook-frame-bus.ts`) instead of Tauri webview events. This avoids an event loop round-trip:

| Function | Purpose |
|----------|---------|
| `emitBroadcast(payload)` | Called by `useAutomergeNotebook` after WASM demux for type `0x03` frames |
| `subscribeBroadcast(cb)` | Used by `useDaemonKernel`, `useEnvProgress` to receive kernel/env broadcasts |
| `emitPresence(payload)` | Called by `useAutomergeNotebook` after WASM CBOR decode for type `0x04` frames |
| `subscribePresence(cb)` | Used by `usePresence`, `cursor-registry` to receive remote cursor updates |

All dispatch is synchronous and in-process — no serialization or Tauri event loop hop.

## Output Storage

Cell outputs use a blob manifest system rather than inline data. When the daemon receives output from a kernel:

1. Binary content (images, plots) is stored in a content-addressed blob store
2. The Automerge doc stores a manifest referencing the blob by hash
3. Clients resolve blobs from the daemon's HTTP blob server (`get_blob_port()`)
4. This keeps large binary data out of the sync protocol

## Key Source Files

| File | Role |
|------|------|
| `crates/runtimed/src/connection.rs` | Frame protocol implementation (length-prefixed, typed frames) |
| `crates/notebook-protocol/src/protocol.rs` | Canonical message type definitions (Request, Response, Broadcast enums) — shared crate |
| `crates/runtimed/src/protocol.rs` | Daemon-internal protocol types, plus re-exports from `notebook-protocol` |
| `crates/runtimed/src/notebook_sync_client.rs` | Client-side connection, channels, sync handle |
| `crates/runtimed/src/notebook_sync_server.rs` | Daemon-side room management, kernel dispatch, sync loop |
| `crates/runtimed/src/kernel_manager.rs` | Kernel process lifecycle, execution queue, output interception |
| `crates/notebook/src/lib.rs` | Tauri commands and relay tasks (pipes sync bytes, emits events) |
| `crates/runtimed-wasm/src/lib.rs` | WASM bindings for local-first cell mutations |
| `crates/notebook-doc/src/lib.rs` | Shared Automerge document schema and operations |
| `crates/notebook-doc/src/frame_types.rs` | Shared frame type constants (0x00–0x04) |
| `apps/notebook/src/lib/frame-types.ts` | TypeScript mirror of frame type constants |
| `apps/notebook/src/hooks/useAutomergeNotebook.ts` | Frontend sync integration (WASM handle, sync loop, cell materialization) |
| `apps/notebook/src/hooks/useDaemonKernel.ts` | Frontend broadcast handling (kernel status, outputs, environment) |
