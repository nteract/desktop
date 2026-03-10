# Runtime Protocol

This document describes the wire protocol between notebook clients (frontend WASM + Tauri relay) and the runtimed daemon.

## Versioning Contract

The protocol has a numeric version (`PROTOCOL_VERSION` in `connection.rs`) that governs compatibility between clients and the daemon. All published artifacts tie their major version to this protocol version:

| Artifact | Version scheme | Example |
|----------|---------------|---------|
| `runtimed` (PyPI) | `{PROTOCOL_VERSION}.{minor}.{patch}` | `2.0.0`, `2.1.0` |
| `runtimed` (Rust daemon) | `{PROTOCOL_VERSION}.{minor}.{patch}` | `2.0.0` |
| `runt-cli` | `{PROTOCOL_VERSION}.{minor}.{patch}` | `2.0.0` |
| nteract desktop app | `{PROTOCOL_VERSION}.{minor}.{patch}` | `2.0.0` |

**Rules:**

- **Major version = protocol version.** A `PROTOCOL_VERSION` bump (breaking wire change) forces a new major version across all artifacts. Any `runtimed 2.x.y` client can talk to any `2.x.y` daemon.
- **Minor version = new features.** Additive changes (new request/response/broadcast variants with serde defaults) bump the minor version. Old clients ignore unknown variants; new clients degrade gracefully against old daemons.
- **Patch version = bug fixes.** No protocol changes.

### Nightly pre-releases

Nightly builds publish Python wheels to PyPI as PEP 440 alpha pre-releases:

```
2.0.1a202507150900   (nightly from 2025-07-15)
```

These sort after the current stable (`2.0.0`) but before the next stable (`2.0.1`). Install with `pip install runtimed --pre`. Stable releases are published only via `python-v*` tags.

### Version negotiation

- **Notebook sync** (`NotebookSync`, `OpenNotebook`, `CreateNotebook` handshakes): The daemon returns `protocol_version` and `daemon_version` in its capabilities response. The client hard-fails on mismatch.
- **Pool IPC** (`Pool` handshake): The client sends `protocol_version` in the handshake. Old clients omit it (`None`); the daemon accepts them for backward compatibility. Future versions may reject mismatched clients with a clear error.

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

The frontend invokes a Tauri command (`open_notebook_in_new_window` or `create_notebook`), which causes the relay to connect to the daemon's Unix socket and send a handshake frame.

### 2. Handshake

The first frame is a JSON `Handshake` message:

```json
{
  "NotebookSync": {
    "notebook_id": "/path/to/notebook.ipynb",
    "protocol": "v2",
    "working_dir": null,
    "initial_metadata": "..."
  }
}
```

The daemon responds with a `NotebookConnectionInfo`:

```json
{
  "protocol": "v2",
  "protocol_version": 2,
  "daemon_version": "2.0.0+abc123",
  "notebook_id": "derived-id",
  "cell_count": 5,
  "needs_trust_approval": false,
  "error": null
}
```

### 3. Initial Automerge sync

After the handshake, both sides exchange Automerge sync messages until their documents converge. The frontend starts with an empty document — all notebook state comes from the daemon during this sync phase. A 2-second timeout guards against stalls.

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
  → Tauri invoke("send_automerge_sync", bytes)
  → Relay pipes bytes to daemon socket (frame type 0x00)
  → Daemon applies sync, updates canonical doc
  → Daemon generates response sync message
  → Relay receives bytes, emits "automerge:from-daemon" event
  → WASM handle.receive_sync_message(bytes)
  → materializeCells() updates React state if doc changed
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
| `Complete { code, cursor_pos }` | Get code completions from the kernel |
| `GetHistory { pattern, n }` | Search kernel input history |
| `GetKernelInfo` | Query current kernel status |
| `GetQueueState` | Query the execution queue |

### Key response types

| Response | Meaning |
|----------|---------|
| `KernelLaunched { env_source, ... }` | Kernel started, includes environment origin label |
| `CellQueued` | Cell added to execution queue |
| `ExecutionDone` | Cell finished executing |
| `NotebookSaved` | File written to disk |
| `CompletionResult { matches, ... }` | Code completion results |
| `Error { message }` | Something went wrong |

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
| `KernelStatus { status }` | Kernel state changed: `"starting"`, `"idle"`, `"busy"`, `"error"`, `"shutdown"` |
| `ExecutionStarted { cell_id }` | A cell began executing |
| `Output { cell_id, output }` | Cell produced output (stdout, display data, error) |
| `DisplayUpdate { display_id, output }` | Update an existing output by display ID |
| `ExecutionDone { cell_id, ... }` | Cell execution completed with timing and execution count |
| `QueueChanged { queue }` | Execution queue state changed |
| `KernelError { message }` | Kernel crashed or failed to launch |
| `Comm { msg_type, ... }` | Jupyter comm message (widget open/msg/close) |
| `FileChanged` | External file change merged into the doc |
| `EnvProgress { stage, message }` | Environment setup progress |
| `EnvSyncState { diff }` | Notebook dependencies drifted from launched kernel config |

### Broadcast flow

```
Kernel produces output
  → Daemon intercepts Jupyter IOPub message
  → Daemon writes output to Automerge doc (as blob manifest)
  → Daemon sends NotebookBroadcast::Output on broadcast channel
  → Frame type 0x03 sent to all connected clients
  → Relay receives, emits "daemon:broadcast" Tauri event
  → Frontend useDaemonKernel hook processes the broadcast
  → UI updates
```

## Tauri Event Bridge

The relay emits these Tauri events to the frontend:

| Event | Payload | Purpose |
|-------|---------|---------|
| `automerge:from-daemon` | `Vec<u8>` | Raw Automerge sync bytes from daemon |
| `daemon:broadcast` | JSON | Serialized `NotebookBroadcast` |
| `daemon:ready` | — | Connection established, ready to bootstrap |
| `daemon:disconnected` | — | Connection to daemon lost |

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
| `crates/runtimed/src/protocol.rs` | Message type definitions (Request, Response, Broadcast enums) |
| `crates/runtimed/src/notebook_sync_client.rs` | Client-side connection, channels, sync handle |
| `crates/runtimed/src/notebook_sync_server.rs` | Daemon-side room management, kernel dispatch, sync loop |
| `crates/runtimed/src/kernel_manager.rs` | Kernel process lifecycle, execution queue, output interception |
| `crates/notebook/src/lib.rs` | Tauri commands and relay tasks (pipes sync bytes, emits events) |
| `crates/runtimed-wasm/src/lib.rs` | WASM bindings for local-first cell mutations |
| `crates/notebook-doc/src/lib.rs` | Shared Automerge document schema and operations |
| `apps/notebook/src/hooks/useAutomergeNotebook.ts` | Frontend sync integration (WASM handle, sync loop, cell materialization) |
| `apps/notebook/src/hooks/useDaemonKernel.ts` | Frontend broadcast handling (kernel status, outputs, environment) |
