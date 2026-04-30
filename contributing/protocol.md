# Runtime Protocol

This document describes the wire protocol between notebook clients (frontend WASM + Tauri relay) and the runtimed daemon.

## Compatibility

Two independent version numbers handle compatibility, separate from the artifact version:

- **Protocol version** (`PROTOCOL_VERSION` defined in `connection/handshake.rs` and re-exported from `connection.rs`, currently `4`) — governs wire compatibility. Every connection sends the 5-byte magic preamble (`0xC0DE01AC` + version byte) at the start of the stream. Bump when the framing, handshake shape, or message serialization format changes. Protocol v4 removes legacy environment-sync request/response variants. The Pool channel remains version-tolerant for daemon upgrade probes; all notebook/runtime channels require current clients.
- **Schema version** (`SCHEMA_VERSION` in `notebook-doc/src/lib.rs`, currently `4`) — governs Automerge document compatibility. Stored in the doc root as `schema_version`. Bump when the document structure changes. The current schema stores cells as a fractional-indexed `Map` and keeps outputs in `RuntimeStateDoc` keyed by `execution_id`, with per-output `output_id` UUIDs on manifests. Future bumps MUST ship a `migrate_vN_to_v(N+1)` function that preserves user data — v1–v3 were pre-release and the v4 load path discards older docs on load, which is only safe because no real user data lives at those versions.

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
| 4 | Protocol version (currently `4`) |

There is no no-preamble fallback. The daemon validates magic bytes before
reading the handshake, so non-runtimed connections get a clear "invalid magic
bytes" error. It checks the protocol version after parsing the handshake
channel:

- `Pool` accepts any preamble version so older stable apps can ping the daemon
  during upgrade and read `protocol_version` / `daemon_version` metadata from
  the `Pong` response.
- All other channels reject versions outside
  `MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION`.

After the preamble, the notebook sync path also returns `protocol_version` and `daemon_version` in its `ProtocolCapabilities` / `NotebookConnectionInfo` responses for informational purposes.

### Desktop app compatibility

The desktop app bundles its own daemon binary. Version-mismatch detection between the app and its bundled daemon compares git commit hashes (appended as `+{sha}` at build time), not semver. This is because both are always built from the same commit in CI.

## Overview

The notebook app communicates with runtimed over a Unix socket (named pipe on Windows) using length-prefixed, typed frames. The protocol carries several classes of traffic:

1. **Automerge sync** — binary CRDT sync messages that keep the notebook document consistent between the frontend WASM peer and the daemon peer
2. **Request/response** — JSON messages where a client asks the daemon to do something (execute a cell, launch a kernel) and gets a reply
3. **Runtime and pool state sync** — binary Automerge sync for daemon-authored state documents
4. **Broadcasts** — JSON messages the daemon pushes to all connected clients for ephemeral comm messages and environment progress
5. **Presence and session control** — binary presence updates plus daemon-originated readiness/status frames

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

The **Tauri relay** is a transparent byte pipe for Automerge sync, request, runtime-state, pool-state, and presence frames — it does not maintain its own document replica. It forwards raw typed frames between the WASM/TypeScript peer and the daemon peer. Broadcast, response, presence, and session-control frames arrive through the unified `notebook:frame` event.

## Connection Lifecycle

### 1. Opening a notebook

The frontend invokes a Tauri command (`open_notebook_in_new_window`), which causes the relay to connect to the daemon's Unix socket and send a handshake frame. New notebook creation goes through Rust menu events (`spawn_new_notebook()` → `create_notebook_window_for_daemon()`), not a frontend `invoke()`.

### 2. Handshake

The first frame is a JSON `Handshake` message:

```json
{
  "channel": "notebook_sync",
  "notebook_id": "/path/to/notebook.ipynb",
  "protocol": "v4"
}
```

The `Handshake` enum uses `#[serde(tag = "channel", rename_all = "snake_case")]`, so the wire format is flat with a `"channel"` discriminator field — not nested. Optional fields like `working_dir` and `initial_metadata` are omitted when `None` (via `skip_serializing_if`).

Other handshake variants include `Pool`, `SettingsSync`, `Blob`, `OpenNotebook { path }`, `CreateNotebook { runtime, ... }`, and `RuntimeAgent { ... }`. The `OpenNotebook` and `CreateNotebook` variants are the primary paths for opening/creating notebooks from the desktop app, while `NotebookSync` is used by programmatic clients (e.g., Python bindings).

The runtimed socket is not an app-only IPC surface. It is a same-UID trusted API:
the Unix socket permissions prevent cross-user access, but any process running as
the same OS user and holding the socket path can intentionally use the daemon.
`RUNTIMED_SOCKET_PATH` is therefore a capability-bearing pointer.

| Handshake | Authority exposed |
|-----------|-------------------|
| `Pool` | Pool status, environment claims/returns, daemon status/admin requests including shutdown |
| `SettingsSync` | Read/write access to the user's synced settings document |
| `NotebookSync` | Peer access to a notebook room and its runtime-state sync |
| `OpenNotebook` | Load or create a file-backed notebook from a path |
| `CreateNotebook` | Create an untitled or ephemeral notebook room |
| `Blob` | Store blobs and query the localhost blob HTTP port |
| `RuntimeAgent` | Attach a runtime-agent peer to a notebook room |

Do not add a new handshake variant under the assumption that only the desktop app
can reach it. If a channel needs tighter authority than same-UID access, design
an explicit capability or guard for that channel.

The daemon responds with a `NotebookConnectionInfo`:

```json
{
  "protocol": "v4",
  "notebook_id": "derived-id",
  "cell_count": 5,
  "needs_trust_approval": false
}
```

The `protocol_version`, `daemon_version`, and `error` fields are `Option` types with `skip_serializing_if = "Option::is_none"`, so they are only present when the daemon populates them. `protocol_version` and `daemon_version` appear for version negotiation; `error` appears only in failure cases. A minimal successful response omits all three.

### 3. Initial Automerge sync

After the handshake, both sides exchange Automerge sync messages until their documents converge. The frontend starts with an empty document — all notebook state comes from the daemon during this sync phase. Protocol v4 clients receive `SessionControl::SyncStatus` frames as the daemon advances notebook-doc, runtime-state, and initial-load readiness.

### 4. Steady state

Once synced, the connection carries all frame classes concurrently: ongoing Automerge sync for cell edits, request/response for explicit actions, RuntimeStateDoc sync, PoolDoc sync, presence, broadcasts, and session-control frames.

Steady-state frame fairness is a correctness requirement. A dedicated framed
reader prevents cancel-unsafe partial reads, but the consumer must also keep
draining its bounded queue. Confirmation waits should register target-head
waiters, request waits should register id-keyed pending entries, and command
handlers should return to the main frame loop after writing any immediate
outbound frame. Blocking `recv()` loops inside a command path can backpressure
the socket and make the daemon drop the peer.

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
| `0x05`    | RuntimeStateSync   | Binary (raw Automerge sync for per-notebook `RuntimeStateDoc`) |
| `0x06`    | PoolStateSync      | Binary (raw Automerge sync for the per-daemon `PoolDoc`) |
| `0x07`    | SessionControl     | JSON (`SessionControlMessage`, daemon-originated readiness/status) |

Frame direction is peer-specific:

| Sender | Valid frame types |
|--------|-------------------|
| Frontend / Tauri relay | `0x00` AutomergeSync, `0x01` NotebookRequest, `0x04` Presence, `0x05` RuntimeStateSync, `0x06` PoolStateSync |
| Daemon notebook peer | `0x00` AutomergeSync, `0x02` NotebookResponse, `0x03` NotebookBroadcast, `0x04` Presence, `0x05` RuntimeStateSync, `0x06` PoolStateSync, `0x07` SessionControl |
| Runtime agent peer | `0x00` AutomergeSync, `0x01` RuntimeAgentRequest/Envelope, `0x02` RuntimeAgentResponse/Envelope, `0x05` RuntimeStateSync |

`NotebookRequest` and `NotebookResponse` payloads are carried in flattened
`NotebookRequestEnvelope` and `NotebookResponseEnvelope` values. Concurrent
requests must include an `id`, and clients must route responses by id because
broadcasts, state sync, and out-of-order responses may interleave freely.

The TypeScript protocol surface in `packages/runtimed` is checked against
these Rust wire discriminants:

- `packages/runtimed/src/transport.ts` owns the exported `FrameType` constants.
- `packages/runtimed/src/request-types.ts` owns the frontend-visible
  `NotebookRequest` and `NotebookResponse` unions.
- `packages/runtimed/src/protocol-contract.ts` exports the request, response,
  and session-control discriminant lists with TypeScript exhaustiveness checks.
- `crates/notebook-protocol/src/protocol.rs` includes a contract test that
  compares those TypeScript lists and frame bytes to the Rust protocol.

When adding or renaming a request, response, frame type, or session-control
phase, update the Rust protocol and `packages/runtimed` contract in the same
patch, then run `cargo test -p notebook-protocol` and the focused
`packages/runtimed` tests.

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
  → engine.scheduleFlush() (20ms debounce) → flush_local_changes() → sync bytes
  → sendFrame(frame_types.AUTOMERGE_SYNC, msg) → raw binary via tauri::ipc::Request
  → Tauri send_frame dispatches by type → relay pipes to daemon socket
  → Daemon applies sync, updates canonical doc
  → Daemon generates response sync message → frame type 0x00
  → Relay receives, emits "notebook:frame" Tauri event (raw typed bytes)
  → Frontend useAutomergeNotebook listener → WASM handle.receive_frame(bytes)
  → WASM demuxes by first byte, applies sync, returns FrameEvent[]
  → FrameEvent::SyncApplied includes a CellChangeset (field-level diff)
  → scheduleMaterialize coalesces within 32ms, then dispatches:
      - structural change (cells added/removed/reordered) → full materializeCells()
      - output changes → per-cell cache-aware resolution (cache hits use materializeCellFromWasm(), cache misses resolve just that cell async)
      - source/metadata/exec_count only → per-cell materializeCellFromWasm() via O(1) accessors
  → React state updated via split cell store (only affected cells re-render)
  → scheduleSyncReply → 50ms debounce → handle.generate_sync_reply() → sendFrame() (one reply per window)
```

### CellChangeset

The WASM module computes a structural diff after each sync by walking `doc.diff(before, after)` patches (in `notebook-doc/src/diff.rs`). This produces a `CellChangeset`:

- **`changed`**: cells that existed before and after, with per-field flags (`source`, `outputs`, `execution_count`, `metadata`, `position`, `cell_type`, `resolved_assets`)
- **`added`**: new cell IDs
- **`removed`**: deleted cell IDs
- **`order_changed`**: whether any position was modified or cells were added/removed

Cost is O(delta), not O(doc). Multiple changesets within a throttle window are merged via `mergeChangesets()` (union on field flags, dedup on added/removed).

This is the key primitive that makes the sync pipeline incremental — the frontend knows exactly which cells changed and which fields, avoiding full-notebook materialization on every frame.

## Request / Response

Requests are one-shot JSON messages sent from the client to the daemon. Each request gets exactly one response.

### Key request types

| Request | Purpose |
|---------|---------|
| `LaunchKernel` | Start a kernel with environment config |
| `ExecuteCell { cell_id }` | Queue a cell for execution (daemon reads source from synced doc) |
| `ExecuteCellGuarded { cell_id, observed_heads }` | Queue a cell only if the approved notebook heads still match |
| `ClearOutputs { cell_ids }` | Clear visible outputs for one or more cells by removing their execution pointers |
| `InterruptExecution` | Send SIGINT to the running kernel |
| `ShutdownKernel` | Stop the kernel process |
| `RunAllCells` | Execute all code cells in order |
| `RunAllCellsGuarded { observed_heads }` | Run all code cells only if the approved notebook heads still match |
| `SaveNotebook { format_cells, path? }` | Persist the Automerge doc to `.ipynb` on disk, optionally save-as to `path` |
| `SyncEnvironment` | Hot-install packages into the running kernel's environment |
| `ApproveTrust` | Sign current dependency metadata after user approval |
| `ApproveProjectEnvironment` | Record local approval for a project-file environment |
| `CloneAsEphemeral { source_notebook_id }` | Fork an existing loaded notebook into a new in-memory room |
| `SendComm { message }` | Send a comm message to the kernel (widget interactions) |
| `Complete { code, cursor_pos }` | Get code completions from the kernel |
| `GetHistory { pattern, n, unique }` | Search kernel input history |
| `GetDocBytes` | Fetch canonical Automerge bytes to bootstrap a WASM peer |

`LaunchKernel.env_source` is a request-time `LaunchSpec` string on the wire:
`"auto"`, `"auto:uv"`, `"auto:conda"`, `"auto:pixi"`, or a concrete
environment source such as `"uv:inline"`. The daemon resolves that spec before
launch and downstream protocol responses carry a concrete `EnvSource`.

### Key response types

| Response | Meaning |
|----------|---------|
| `KernelLaunched { env_source, ... }` | Kernel started, includes resolved concrete environment origin label |
| `KernelAlreadyRunning { env_source, ... }` | Existing kernel reused |
| `CellQueued` | Cell added to execution queue |
| `AllCellsQueued { queued }` | All runnable code cells queued with execution IDs |
| `NotebookSaved { path }` | File written to disk |
| `SaveError { error }` | Save failed with structured error details |
| `GuardRejected { reason }` | Guarded action rejected because observed notebook state changed |
| `NotebookCloned { notebook_id, working_dir }` | Ephemeral fork created |
| `SyncEnvironmentComplete` / `SyncEnvironmentFailed` | Hot-sync result |
| `DocBytes { bytes }` | Canonical Automerge doc bytes |
| `CompletionResult { items, cursor_start, cursor_end }` | Code completion results (`items: Vec<CompletionItem>`) |
| `HistoryResult { entries }` | Kernel input history search results |
| `Error { error }` | Something went wrong |

### Request flow through the stack

```
Frontend: host.transport.sendRequest({ type: "execute_cell", cell_id })
  → TauriTransport encodes NotebookRequestEnvelope with a correlation id
  → send_frame(0x01 + JSON envelope)
  → Relay: handle.forward_frame(NotebookRequest)
  → Frame type 0x01 sent on socket
  → Daemon processes request
  → Frame type 0x02 returned with matching id
  → Relay emits "notebook:frame"
  → TauriTransport response tap resolves the pending request by id
```

## Broadcasts

Broadcasts are daemon-initiated messages pushed to all connected clients for a notebook. They are not replies to any specific request.

### Key broadcast types

| Broadcast | Purpose |
|-----------|---------|
| `Comm { msg_type, content, buffers }` | Jupyter comm message (widget open/msg/close). Custom one-shot events; widget state syncs via RuntimeStateDoc. |
| `EnvProgress { env_type, phase }` | Environment setup progress (`phase` is a flattened `EnvProgressPhase`). RuntimeStateDoc remains authoritative for durable env state. |
| ~~`CommSync`~~ | Removed — widget state syncs via RuntimeStateDoc CRDT |

The old state-carrying broadcast variants were removed after RuntimeStateDoc became authoritative: kernel state, execution lifecycle, queue, outputs/display updates, path/autosave, and env sync state now flow through CRDT sync. The `Comm` variant is limited to custom messages (`method != "update"`) — state updates flow through RuntimeStateDoc instead.

### Output sync flow

```
Kernel produces output
  → Daemon intercepts Jupyter IOPub message
  → Daemon writes output manifest to RuntimeStateDoc
  → RuntimeStateDoc sync produces a frame type 0x05 message
  → Relay receives, emits "notebook:frame" Tauri event
  → WASM handle.receive_frame() demuxes → RuntimeStateDoc merge
  → frame-pipeline.ts plans output materialization
  → UI updates
```

## Tauri Event Bridge & Frame Bus

The relay and frontend use these Tauri events for cross-process communication:

| Event | Direction | Payload | Purpose |
|-------|-----------|---------|---------|
| `notebook:frame` | Relay → Frontend | `number[]` (typed frame bytes) | All daemon frames (sync, broadcast, presence) via unified pipe |
| `daemon:ready` | Relay → Frontend | `DaemonReadyPayload` | Connection established, ready to bootstrap |
| `daemon:disconnected` | Relay → Frontend | — | Connection to daemon lost |

Outgoing frames from the frontend use `sendFrame(frameType, payload)` where `payload` is `Uint8Array` passed as raw binary via `tauri::ipc::Request`. The relay accepts frontend-originated `0x00` (AutomergeSync), `0x01` (NotebookRequest), `0x04` (Presence), `0x05` (RuntimeStateSync), and `0x06` (PoolStateSync). `0x02` responses, `0x03` broadcasts, and `0x07` session-control frames are daemon-originated.

### In-memory frame bus

After WASM `receive_frame()` demuxes typed frames, broadcast and presence payloads are dispatched via an in-memory pub/sub bus (`notebook-frame-bus.ts`) instead of Tauri webview events. This avoids an event loop round-trip:

| Function | Purpose |
|----------|---------|
| `emitBroadcast(payload)` | Called by `useAutomergeNotebook` after WASM demux for type `0x03` frames |
| `subscribeBroadcast(cb)` | Used by `useDaemonKernel` for ephemeral runtime events; persistent env state is RuntimeStateDoc-backed |
| `emitPresence(payload)` | Called by `useAutomergeNotebook` after WASM CBOR decode for type `0x04` frames |
| `subscribePresence(cb)` | Used by `usePresence`, `cursor-registry` to receive remote cursor updates |

All dispatch is synchronous and in-process — no serialization or Tauri event loop hop.

## Output Storage

Cell outputs use inline manifests with blob offload for large payloads. When the daemon receives output from a kernel:

1. The output is converted to nbformat JSON, then a **manifest** is created as an inline Automerge Map in RuntimeStateDoc (`output_store.rs`)
2. Each MIME type's content becomes a `ContentRef`: `Inline` for ≤ 1KB, `Blob { hash, size }` for > 1KB
3. Large binary content (images, plots) is stored in a content-addressed **blob store** (`blob_store.rs`, SHA-256 hashes, `~/.cache/runt/blobs/`)
4. MIME types and small payloads are readable directly from the CRDT without any blob fetch
5. Clients resolve large blobs from the daemon's HTTP blob server (`GET /blob/{hash}` on a dynamic port)

This keeps the CRDT efficient: manifests are structured Maps with compact ContentRef entries. MIME type metadata is always available without touching the blob store.

Stream outputs (stdout/stderr) are special: text is fed through a terminal emulator (`stream_terminals`) for carriage return and ANSI escape handling before manifest creation. `upsert_stream_output` updates in-place when consecutive stream outputs arrive.

## Notebook Lifecycle

**Autosave:** The daemon autosaves `.ipynb` on a debounce (2s quiet, 10s max). Explicit save (Cmd+S) additionally formats cells; frontend dirty state is derived from sync/save confirmations rather than a room broadcast.

**UUID-stable rooms:** Room keys are always UUIDs. Saving an untitled notebook updates a secondary `path_index` map and the daemon-authored room state so peers can update their local path tracking. The UUID never changes.

**Crash recovery:** Untitled notebooks persist their Automerge doc to `notebook-docs/{hash}.automerge`. Before overwriting on reopen, the daemon snapshots to `notebook-docs/snapshots/`. Outputs are ephemeral (RuntimeStateDoc, not persisted), so snapshots hold source and metadata only.

**Multi-window:** Multiple windows join the same room as separate Automerge peers. Each gets sync frames and broadcasts independently. The daemon tracks `active_peers` per room for eviction.

## Runtime State

### RuntimeStateDoc

State-carrying broadcasts (kernel status, env sync diff, queue) have been
replaced because they suffer from silent drops, no initial state for late
joiners, and ordering races between windows. `RuntimeStateDoc` is the
daemon-authoritative, per-notebook Automerge document synced via frame type
`0x05` on the existing notebook connection.

The daemon writes kernel status, execution queue, environment progress, project
context, trust state, path/save state, and outputs. Clients receive those fields
via normal Automerge sync, with unexpected client changes stripped. Widget comm
state is the intentional exception: frontend-originated widget state updates
write under `doc.comms/` through the approved comm CRDT writer, and the runtime
agent forwards those deltas to the kernel. The frontend reads runtime state via
`useRuntimeState()` and the project runtime stores.

**Key files:** `crates/runtime-doc/src/doc.rs` (schema + setters), `crates/runtime-doc/src/handle.rs` (handle), `apps/notebook/src/lib/runtime-state.ts` (frontend store + hook).

### Widget comm state

Widget state now lives in `doc.comms/` in RuntimeStateDoc. The daemon writes comm entries from kernel IOPub, and new clients receive widget state via normal CRDT sync. `CommSync` broadcast has been removed. The `Comm` broadcast variant is limited to custom messages (ephemeral events like button clicks). Frontend-originated widget state updates write to the CRDT, and the runtime agent diffs comm state on each sync to forward deltas to the kernel.

## Runtime Agent Subprotocol

Runtime agents are same-socket peers that connect with
`Handshake::RuntimeAgent`. They use the shared framing layer but carry a
different JSON subprotocol on request/response frame types:

| Frame | Runtime-agent payload |
|-------|-----------------------|
| `0x00` | NotebookDoc Automerge sync |
| `0x01` | `RuntimeAgentRequestEnvelope` |
| `0x02` | `RuntimeAgentResponseEnvelope` |
| `0x05` | RuntimeStateDoc Automerge sync |

Coordinator-to-agent RPC covers kernel lifecycle and query-style operations
such as launch, restart, shutdown, completion, history, comm sends, interrupts,
and hot environment sync. Cell execution itself is CRDT-driven: the coordinator
writes execution entries into RuntimeStateDoc, and the runtime agent discovers
and executes queued entries through RuntimeStateDoc sync.

Because the daemon socket is a same-UID trusted API, the runtime-agent handshake
must be treated as a powerful attach operation. Do not add runtime-agent
authority under the assumption that only the desktop app can reach the socket.

## Key Source Files

| File | Role |
|------|------|
| `crates/notebook-protocol/src/connection.rs` | Public connection API facade and compatibility re-exports |
| `crates/notebook-wire/src/lib.rs` | Low-level wire constants, preamble bytes, frame caps, typed-frame enum, session-control status shapes |
| `crates/notebook-protocol/src/connection/framing.rs` | Frame protocol I/O: preamble validation and length-prefixed typed-frame send/receive |
| `crates/notebook-protocol/src/connection/handshake.rs` | Protocol version, handshake, capabilities, connection info |
| `crates/notebook-protocol/src/connection/env.rs` | Launch spec, package manager, and environment source wire types |
| `crates/notebook-protocol/src/protocol.rs` | Canonical wire types: `NotebookRequest`, `NotebookResponse`, `NotebookBroadcast` |
| `packages/runtimed/src/request-types.ts` | Generated TypeScript request/response protocol unions consumed by JS clients |
| `packages/runtimed/src/protocol-contract.ts` | Generated TypeScript discriminant lists for frame/session/request/response drift tests |
| `crates/runtimed-client/src/protocol.rs` | Daemon-internal types (`Request`, `Response`, `BlobRequest`), re-exports from `notebook-protocol` |
| `crates/notebook-sync/src/relay.rs` | Relay handle for notebook sync connections |
| `crates/notebook-sync/src/connect.rs` | Connection setup (`connect_open_relay`, `connect_create_relay`) |
| `crates/notebook-sync/src/handle.rs` | `DocHandle` — sync infrastructure, per-cell accessors for Python clients |
| `crates/runtimed/src/notebook_sync_server/mod.rs` | Notebook sync server module facade |
| `crates/runtimed/src/notebook_sync_server/catalog.rs` | Room lookup and creation |
| `crates/runtimed/src/notebook_sync_server/room.rs` | `NotebookRoom` and room-owned state |
| `crates/runtimed/src/notebook_sync_server/peer_connection.rs` | Notebook sync connection handler |
| `crates/runtimed/src/notebook_sync_server/peer_loop.rs` | Main peer sync loop and frame dispatch |
| `crates/runtimed/src/notebook_sync_server/peer_session.rs` | Initial readiness/session-control state |
| `crates/runtimed/src/notebook_sync_server/peer_writer.rs` | Bounded peer writer |
| `crates/runtimed/src/notebook_sync_server/metadata.rs` | Metadata, trust, project-file, and environment detection helpers |
| `crates/runtimed/src/requests/` | Notebook request routing handlers |
| `crates/runtimed/src/output_prep.rs` | IOPub output-prep helpers: message-to-nbformat conversion, widget buffers, blob-store offload |
| `crates/runtimed/src/runtime_agent.rs` | Runtime-agent peer loop, kernel lifecycle, comm-state diff forwarding, RuntimeStateDoc writes |
| `crates/runtimed/src/output_store.rs` | Output manifest creation, blob inlining threshold |
| `crates/runtimed/src/blob_store.rs` | Content-addressed blob storage |
| `crates/notebook/src/lib.rs` | Tauri commands and relay tasks (transparent byte pipe) |
| `crates/runtimed-wasm/src/lib.rs` | WASM bindings: cell mutations, sync, per-cell accessors, `CellChangeset` |
| `crates/notebook-doc/src/lib.rs` | `NotebookDoc`: Automerge schema, cell CRUD, nbformat fallback fields, per-cell accessors |
| `crates/notebook-doc/src/diff.rs` | `CellChangeset`: structural diff from Automerge patches |
| `crates/runtime-doc/src/doc.rs` | `RuntimeStateDoc`: per-notebook daemon-authoritative state (kernel, queue, env sync) |
| `apps/notebook/src/lib/runtime-state.ts` | Frontend runtime state store + `useRuntimeState()` hook |
| `packages/runtimed/src/transport.ts` | TypeScript `FrameType` constants and transport boundary |
| `apps/notebook/src/lib/frame-pipeline.ts` | App-side frame event processing and materialization planning |
| `apps/notebook/src/hooks/useAutomergeNotebook.ts` | WASM handle owner, `scheduleMaterialize`, `CellChangeset` dispatch |
| `apps/notebook/src/hooks/useDaemonKernel.ts` | Kernel execution, widget comm routing, broadcast handling |
| `apps/notebook/src/lib/materialize-cells.ts` | `materializeCellFromWasm()` (per-cell) + `cellSnapshotsToNotebookCells()` (full) |
| `apps/notebook/src/lib/notebook-cells.ts` | Split cell store: `useCell(id)`, `useCellIds()`, per-cell subscriptions |
| `apps/notebook/src/lib/notebook-frame-bus.ts` | In-memory sync pub/sub for broadcasts and presence |
