---
paths:
  - crates/notebook-protocol/**
  - crates/notebook-sync/**
  - apps/notebook/src/lib/frame-types*
  - apps/notebook/src/lib/notebook-frame-bus*
---

# Wire Protocol

## Compatibility

Two independent version numbers, separate from the artifact version:

- **Protocol version** (`PROTOCOL_VERSION` in `connection.rs`, currently `2`) -- governs wire compatibility. Validated by the 5-byte magic preamble at connection start. Bump when framing, handshake shape, or serialization format changes.
- **Schema version** (`SCHEMA_VERSION` in `notebook-doc/src/lib.rs`, currently `2`) -- governs Automerge document compatibility. Bump when document structure changes.

These are just incrementing integers that evolve independently from each other and from the artifact version.

## Connection Preamble

Every connection starts with 5 bytes before the JSON handshake:

| Bytes | Content |
|-------|---------|
| 0-3 | Magic: `0xC0 0xDE 0x01 0xAC` |
| 4 | Protocol version (currently `2`) |

The daemon validates both before reading the handshake. Non-runtimed connections get "invalid magic bytes". Protocol mismatches are rejected before JSON parsing.

## Connection Lifecycle

1. **Opening** -- Frontend invokes Tauri command; relay connects to daemon Unix socket and sends handshake.
2. **Handshake** -- JSON with `channel`, `notebook_id`, `protocol`. Daemon responds with `NotebookConnectionInfo` (protocol, notebook_id, cell_count, needs_trust_approval). The `Handshake` enum uses `#[serde(tag = "channel")]` -- flat wire format with `"channel"` discriminator.
3. **Initial sync** -- Both sides exchange Automerge sync messages until convergence. Frontend starts empty; all state comes from daemon. 2s connection timeout, 100ms per-frame timeout.
4. **Steady state** -- Concurrent Automerge sync, request/response, and broadcasts.
5. **Disconnection** -- Relay emits `daemon:disconnected`. Generation counter prevents stale callbacks.

## Wire Format

Length-prefixed frames: 4-byte big-endian u32 length + N bytes payload. Max: 100 MiB data frames, 64 KiB control/handshake.

### Typed Frames

After handshake, frames are typed by first byte:

| Type byte | Name | Payload format |
|-----------|------|----------------|
| `0x00` | AutomergeSync | Binary (raw Automerge sync message) |
| `0x01` | NotebookRequest | JSON |
| `0x02` | NotebookResponse | JSON |
| `0x03` | NotebookBroadcast | JSON |
| `0x04` | Presence | Binary (CBOR) |
| `0x05` | RuntimeStateSync | Binary (raw Automerge sync for RuntimeStateDoc) |
| `0x06` | PoolStateSync | Binary (raw Automerge sync for PoolDoc — global daemon pool state) |

Only `0x00` (AutomergeSync), `0x04` (Presence), and `0x06` (PoolStateSync) are valid outgoing types from the frontend.

## Key Request Types

| Request | Purpose |
|---------|---------|
| `LaunchKernel` | Start a kernel with environment config |
| `ExecuteCell { cell_id }` | Queue cell for execution (daemon reads source from synced doc) |
| `ClearOutputs { cell_id }` | Clear a cell's outputs |
| `InterruptExecution` | Send SIGINT to running kernel |
| `ShutdownKernel` | Stop the kernel process |
| `RunAllCells` | Execute all code cells in order |
| `SaveNotebook` | Persist Automerge doc to `.ipynb` |
| `SyncEnvironment` | Hot-install packages into running kernel |
| `SendComm { message }` | Send comm message to kernel (widget interactions) |
| `Complete { code, cursor_pos }` | Code completions from kernel |
| `GetHistory { pattern, n, unique }` | Search kernel input history |

## Key Response Types

| Response | Meaning |
|----------|---------|
| `KernelLaunched { env_source }` | Kernel started, includes environment origin label |
| `CellQueued` | Cell added to execution queue |
| `NotebookSaved` | File written to disk |
| `CompletionResult { items, cursor_start, cursor_end }` | Code completion results |
| `Error { error }` | Something went wrong |

## Key Broadcast Types

| Broadcast | Purpose |
|-----------|---------|
| `KernelStatus { status, cell_id }` | Kernel state: starting, idle, busy, error, shutdown |
| `ExecutionStarted { cell_id, execution_count }` | Cell began executing |
| `Output { cell_id, output_type, output_json, output_index }` | Cell produced output |
| `ExecutionDone { cell_id }` | Cell execution completed |
| `QueueChanged { executing, queued }` | Execution queue state changed (legacy; RuntimeStateDoc is authoritative) |
| `Comm { msg_type, content, buffers }` | Jupyter comm message (widget) |
| ~~`CommSync`~~ | Removed — widget state syncs via RuntimeStateDoc CRDT |
| `EnvProgress { env_type, phase }` | Environment setup progress |
| `EnvSyncState { in_sync, diff }` | Deps drifted from launched kernel |
| `RoomRenamed { new_notebook_id }` | Untitled notebook saved, room re-keyed |
| `NotebookAutosaved { path }` | Autosave completed, frontend clears dirty flag |

## Tauri Event Bridge

| Event | Direction | Payload | Purpose |
|-------|-----------|---------|---------|
| `notebook:frame` | Relay -> Frontend | `number[]` (typed frame bytes) | All daemon frames via unified pipe |
| `daemon:ready` | Relay -> Frontend | `DaemonReadyPayload` | Connection established |
| `daemon:disconnected` | Relay -> Frontend | -- | Connection lost |

Outgoing: `sendFrame(frameType, payload)` where `payload` is `Uint8Array` passed as raw binary via `tauri::ipc::Request`.

## In-Memory Frame Bus

After WASM `receive_frame()` demuxes frames, broadcasts and presence are dispatched via `notebook-frame-bus.ts` (synchronous, in-process, no serialization):

| Function | Purpose |
|----------|---------|
| `emitBroadcast(payload)` | Called after WASM demux for type `0x03` frames |
| `subscribeBroadcast(cb)` | Used by `useDaemonKernel`, `useEnvProgress` |
| `emitPresence(payload)` | Called after WASM CBOR decode for type `0x04` frames |
| `subscribePresence(cb)` | Used by `usePresence`, `cursor-registry` |

## Output Storage

Inline manifest system with blob offload for large payloads. When daemon receives kernel output:
1. Convert to nbformat JSON, create output manifest as an **inline Automerge Map** in RuntimeStateDoc (`output_store.rs`)
2. Each MIME type → `ContentRef`: `Inline` for ≤ 1KB, `Blob { hash, size }` for > 1KB
3. Large content stored in blob store (SHA-256 hashes, `~/.cache/runt/blobs/`)
4. MIME types and small payloads are readable directly from the CRDT without any blob fetch
5. Clients resolve large blobs from daemon's HTTP blob server (`GET /blob/{hash}`)

## RuntimeStateDoc (Shipped)

State-carrying broadcasts (kernel status, queue, env sync, trust) have been replaced by a **daemon-authoritative, per-notebook Automerge document** synced via frame type `0x05`. Clients read via `useRuntimeState()`.

Schema (in `crates/notebook-doc/src/runtime_state.rs`):

| Path | Type | Description |
|------|------|-------------|
| `kernel.status` | Str | `"idle"`, `"busy"`, `"starting"`, `"error"`, `"shutdown"`, `"not_started"` |
| `kernel.starting_phase` | Str | `""`, `"resolving"`, `"preparing_env"`, `"launching"`, `"connecting"` |
| `kernel.name`, `kernel.language`, `kernel.env_source` | Str | Kernel metadata |
| `queue.executing` | Str\|null | Cell ID currently executing |
| `queue.executing_execution_id` | Str\|null | Execution ID for the executing cell |
| `queue.queued` | List[Str] | Queued cell IDs |
| `queue.queued_execution_ids` | List[Str] | Parallel execution IDs for queued entries |
| `executions.{execution_id}` | Map | `{ cell_id, status, execution_count, success }` |
| `env.in_sync`, `env.added`, `env.removed` | bool/List | Environment drift state |
| `trust.status`, `trust.needs_approval` | Str/bool | Trust state |
| `last_saved` | Str\|null | ISO timestamp of last save |

The daemon is the sole writer. Frontends and Python clients read-only via Automerge sync.

### Execution ID Tracking

Each cell execution is assigned a unique `execution_id` (UUID). The `QueueEntry` struct pairs `cell_id` with `execution_id`, enabling:
- Multiple executions of the same cell to be tracked independently
- Python `Execution` handle to poll for lifecycle state
- Frontend to display per-execution progress

## Architectural Direction

**Comms in doc (#761)** -- Done. Widget state lives in `doc.comms/` in RuntimeStateDoc. `CommSync` broadcast has been removed. New clients receive widget state via normal CRDT sync.

## Key Source Files

| File | Role |
|------|------|
| `crates/notebook-protocol/src/connection.rs` | Frame protocol, handshake, preamble |
| `crates/notebook-protocol/src/protocol.rs` | Wire types: requests, responses, broadcasts |
| `crates/notebook-sync/src/relay.rs` | Relay handle for sync connections |
| `crates/notebook-sync/src/connect.rs` | Connection setup |
| `crates/notebook-sync/src/handle.rs` | `DocHandle` -- sync, per-cell accessors |
| `crates/notebook-doc/src/frame_types.rs` | Shared frame type constants (0x00-0x06) |
| `crates/notebook-doc/src/runtime_state.rs` | `RuntimeStateDoc` schema |
| `apps/notebook/src/lib/frame-types.ts` | Frame type constants + `sendFrame()` |
| `apps/notebook/src/lib/notebook-frame-bus.ts` | In-memory pub/sub |
| `apps/notebook/src/lib/runtime-state.ts` | Frontend runtime state store + hook |
