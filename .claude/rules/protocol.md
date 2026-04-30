---
paths:
  - crates/notebook-protocol/**
  - crates/notebook-sync/**
  - packages/runtimed/src/transport.ts
  - packages/runtimed/src/protocol-contract.ts
  - packages/runtimed/src/request-types.ts
  - apps/notebook/src/lib/frame-pipeline.ts
  - apps/notebook/src/lib/notebook-frame-bus.ts
---

# Wire Protocol

## Compatibility

Two independent version numbers, separate from the artifact version:

- **Protocol version** (`PROTOCOL_VERSION` in `connection/handshake.rs`, re-exported by `connection.rs`, currently `4`) -- governs wire compatibility. Validated by the 5-byte magic preamble at connection start. Bump when framing, handshake shape, or serialization format changes. v4 removes legacy environment-sync request/response variants and requires current clients.
- **Schema version** (`SCHEMA_VERSION` in `notebook-doc/src/lib.rs`, currently `4`) -- governs Automerge document compatibility. Bump when document structure changes.

These are just incrementing integers that evolve independently from each other and from the artifact version.

## Connection Preamble

Every connection starts with 5 bytes before the JSON handshake:

| Bytes | Content |
|-------|---------|
| 0-3 | Magic: `0xC0 0xDE 0x01 0xAC` |
| 4 | Protocol version (currently `4`) |

There is no no-preamble fallback. The daemon validates magic bytes before reading the handshake. Protocol version is checked after parsing the handshake channel: the Pool channel accepts any version so older stable apps can ping during upgrade and read `protocol_version` / `daemon_version` from `Pong`; all other channels require `MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION`.

## Connection Lifecycle

1. **Opening** -- Frontend invokes Tauri command; relay connects to daemon Unix socket and sends handshake.
2. **Handshake** -- JSON with `channel`, `notebook_id`, `protocol`. Daemon responds with `NotebookConnectionInfo` (protocol, notebook_id, cell_count, needs_trust_approval). The `Handshake` enum uses `#[serde(tag = "channel")]` -- flat wire format with `"channel"` discriminator.
3. **Initial sync** -- Both sides exchange Automerge sync messages until convergence. Frontend starts empty; all state comes from daemon. v4 clients receive `SessionControl::SyncStatus` frames for notebook-doc, runtime-state, and initial-load readiness.
4. **Steady state** -- Concurrent Automerge sync, request/response, RuntimeStateDoc sync, PoolDoc sync, presence, broadcasts, and session-control frames. The frame consumer must stay hot: request and confirmation waits belong in pending maps/waiter lists, not blocking `recv()` loops.
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
| `0x07` | SessionControl | JSON (`SessionControlMessage`, daemon-originated readiness/status) |

Frontend/relay outbound frames are `0x00` (AutomergeSync), `0x01` (NotebookRequest), `0x04` (Presence), `0x05` (RuntimeStateSync), and `0x06` (PoolStateSync). `0x02` responses, `0x03` broadcasts, and `0x07` session-control frames are daemon-originated.

Notebook request and response frames use `NotebookRequestEnvelope` and `NotebookResponseEnvelope`. Every overlapping request must carry an `id`, and responses must route by id rather than by receive order.

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
| `ApproveTrust` / `ApproveProjectEnvironment` | Record user approval for dependency or project-file environments |
| `CloneAsEphemeral` | Fork the current notebook into a new in-memory room |
| `GetDocBytes` | Fetch canonical Automerge bytes to bootstrap a WASM peer |
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

After progressively migrating room state to `RuntimeStateDoc`, broadcasts are now narrow: kernel comm messages and env-install progress. Kernel state, execution lifecycle, queue, outputs, the notebook's `path`, and `last_saved` timestamp all live in `RuntimeStateDoc` (frame `0x05`).

| Broadcast | Purpose |
|-----------|---------|
| `Comm { msg_type, content, buffers }` | Jupyter comm message (widget). Custom one-shot events; widget *state* syncs via RuntimeStateDoc. |
| `EnvProgress { env_type, phase }` | Environment install/solve/download progress (high-frequency stream). |

## SessionControl

`SessionControl` frames (`0x07`) are connection-local daemon messages, not room
broadcasts. Today they carry `SessionControlMessage::SyncStatus`, a full
readiness snapshot with:

- `notebook_doc`: `pending | syncing | interactive`
- `runtime_state`: `pending | syncing | ready`
- `initial_load`: `not_needed | streaming | ready | failed`

The daemon emits the full current state on transitions.

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
| `subscribeBroadcast(cb)` | Used by `useDaemonKernel` for ephemeral runtime events; persistent env state is RuntimeStateDoc-backed |
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

Schema (in `crates/runtime-doc/src/doc.rs`):

| Path | Type | Description |
|------|------|-------------|
| `kernel.lifecycle` | Str | Typed lifecycle: `"NotStarted"`, `"AwaitingTrust"`, `"AwaitingEnvBuild"`, `"Resolving"`, `"PreparingEnv"`, `"Launching"`, `"Connecting"`, `"Running"`, `"Error"`, `"Shutdown"` |
| `kernel.activity` | Str | `""`, `"Unknown"`, `"Idle"`, `"Busy"`; meaningful when lifecycle is `Running` |
| `kernel.error_reason`, `kernel.error_details` | Str | Typed and free-form error context when lifecycle is `Error` |
| `kernel.runtime_agent_id` | Str | Runtime agent subprocess currently owning the kernel |
| `kernel.name`, `kernel.language`, `kernel.env_source` | Str | Kernel metadata |
| `queue.executing` | Str\|null | Cell ID currently executing |
| `queue.executing_execution_id` | Str\|null | Execution ID for the executing cell |
| `queue.queued` | List[Str] | Queued cell IDs |
| `queue.queued_execution_ids` | List[Str] | Parallel execution IDs for queued entries |
| `executions.{execution_id}` | Map | `{ cell_id, status, execution_count, success }` |
| `env.in_sync`, `env.added`, `env.removed`, `env.channels_changed`, `env.deno_changed` | bool/List | Environment drift state |
| `env.prewarmed_packages`, `env.progress` | List/Map | Current prewarmed package snapshot and latest env progress |
| `trust.status`, `trust.needs_approval` | Str/bool | Trust state |
| `project_context` | Map | Daemon-observed project-file detection and parsed dependency context |
| `display_index` | Map | `display_id` -> ordered `"execution_id\0output_id"` entries |
| `comms` | Map | Widget state keyed by `comm_id` |
| `last_saved` | Str\|null | ISO timestamp of last save |

`kernel.status` and `kernel.starting_phase` remain source-compatible projection fields on `KernelState`, but new code should read `kernel.lifecycle` and `kernel.activity`. The daemon is the sole writer. Frontends and Python clients read-only via Automerge sync.

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
| `crates/notebook-protocol/src/connection.rs` | Public connection API facade and compatibility re-exports |
| `crates/notebook-protocol/src/connection/framing.rs` | Frame protocol, preamble, typed frames, frame caps |
| `crates/notebook-protocol/src/connection/handshake.rs` | Protocol version, handshake, capabilities, connection info |
| `crates/notebook-protocol/src/protocol.rs` | Wire types: requests, responses, broadcasts |
| `crates/notebook-sync/src/relay.rs` | Relay handle for sync connections |
| `crates/notebook-sync/src/connect.rs` | Connection setup |
| `crates/notebook-sync/src/handle.rs` | `DocHandle` -- sync, per-cell accessors |
| `crates/notebook-doc/src/frame_types.rs` | Shared frame type constants (0x00-0x07) |
| `crates/runtime-doc/src/doc.rs` | `RuntimeStateDoc` schema |
| `packages/runtimed/src/transport.ts` | TypeScript `FrameType` constants and transport boundary |
| `apps/notebook/src/lib/notebook-frame-bus.ts` | In-memory pub/sub |
| `apps/notebook/src/lib/runtime-state.ts` | Frontend runtime state store + hook |
