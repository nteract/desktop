# Protocol v3: Clean-Room Wire Protocol Design

> A first-principles redesign of the runtimed wire protocol, document schema,
> and state management architecture.

## Table of Contents

1. [Design Principles](#design-principles)
2. [Document Schema](#1-document-schema)
3. [Wire Format and Frame Types](#2-wire-format-and-frame-types)
4. [Message Types](#3-message-types)
5. [State Ownership](#4-state-ownership)
6. [Sync Flows](#5-sync-flows)
7. [Binary Data Strategy](#6-binary-data-strategy)
8. [Migration Path](#7-migration-path)

---

## Design Principles

These principles guide every decision in the protocol:

1. **If it's convergent state, put it in the CRDT.** The Automerge document is
   the shared truth. Anything that should survive reconnection, be visible to
   late joiners, or be replicated across peers belongs in the document. Broadcasts
   are for events that *happen*, not for state that *exists*.

2. **One data path per concept.** The current system has outputs written to the
   CRDT *and* broadcast as events. Widget state lives outside the CRDT with its
   own sync mechanism. Every dual path is a source of inconsistency bugs. Each
   piece of state should have exactly one canonical home.

3. **The relay is a dumb pipe.** The Tauri relay forwards bytes. It does not
   parse, merge, buffer, or transform. The web view cannot talk directly to the
   Unix socket (browser sandbox), so the relay exists only to bridge that gap.

4. **Binary data uses the blob store.** All large binary content — cell outputs,
   widget buffers, image data — flows through the content-addressed blob store.
   The CRDT carries hashes, never inline binary.

5. **Presence is ephemeral, everything else is durable.** Cursor positions and
   selection highlights are fire-and-forget. Kernel status, execution state,
   widget state, and outputs are durable document state.

---

## 1. Document Schema

### 1.1 Root Structure

```
ROOT (Automerge Map)
├── schema_version: u64 = 3
├── notebook_id: Str
├── cells: Map<cell_id → Cell>
├── metadata: Map<key → Str>         # notebook-level metadata (JSON strings)
├── comms: Map<comm_id → CommModel>   # NEW: widget state in the doc
├── execution: Map                    # NEW: execution state in the doc
│   ├── queue: List<Str>              # ordered cell_ids pending execution
│   ├── current: Str | null           # cell_id currently executing
│   └── kernel_status: Str            # "not_started"|"starting"|"idle"|"busy"|"error"|"shutdown"
└── kernel: Map                       # NEW: kernel metadata in the doc
    ├── kernel_type: Str | null       # "python"|"deno"|null
    ├── env_source: Str | null        # "uv:inline"|"conda:prewarmed"|etc.|null
    └── launched_config: Str | null   # JSON of LaunchedEnvConfig for drift detection
```

### 1.2 Cell Structure

```
cells/<cell_id> (Automerge Map)
├── id: Str                           # cell UUID (redundant but fast lookup)
├── cell_type: Str                    # "code"|"markdown"|"raw"
├── position: Str                     # fractional index hex (unchanged from v2)
├── source: Text                      # Automerge Text CRDT — character-level merging
├── execution_count: Str              # "null" or "5" (JSON string, last-write-wins)
├── outputs: List<Str>                # JSON strings or blob manifest hashes
├── metadata: Str                     # JSON string (cell-level metadata, last-write-wins)
├── resolved_assets: Map<Str → Str>   # markdown ref → blob hash
└── execution_state: Str              # NEW: "idle"|"queued"|"running"|"done"|"error"
```

**What changed from v2:**

- `execution_state` on each cell. The daemon writes this; the frontend reads it.
  This replaces the `ExecutionStarted`, `ExecutionDone`, and `QueueChanged`
  broadcasts. Late joiners see execution state without needing a `GetQueueState`
  request.

### 1.3 Comm/Widget Structure (NEW)

```
comms/<comm_id> (Automerge Map)
├── comm_id: Str
├── target_name: Str                  # "jupyter.widget", "jupyter.widget.version"
├── model_module: Str | null          # "@jupyter-widgets/controls", "anywidget"
├── model_name: Str | null            # "IntSliderModel", "OutputModel"
├── state: Str                        # JSON string of full widget state
├── buffer_refs: List<Str>            # blob store hashes for binary buffers
└── closed: bool                      # false = active, true = closed (tombstone)
```

**Why put widgets in the CRDT:**

- **Late joiner sync is free.** No separate `CommSync` broadcast. A second window
  opening on the same notebook sees all widgets immediately through normal
  Automerge sync.
- **Persistence.** Widget state survives daemon restarts. Reopening a notebook
  could restore widget state (useful for interactive dashboards).
- **One sync mechanism.** Eliminates the parallel `CommState`/`CommSync` system.

**Why `state` is a JSON string (not a nested Automerge Map):**

Widget state changes at the granularity of `model.set(key, value)` +
`model.save_changes()`, which sends a delta object. Jupyter's wire protocol
sends partial state dicts, and the daemon merges them. Using a JSON string
with last-write-wins at the whole-state level matches this semantics. Making
each widget property a separate Automerge key would create schema explosion
(hundreds of keys across 54+ widget types) and conflict resolution behavior
that doesn't match the Jupyter comm protocol's intent.

**Performance concern — slider drag:**

A slider drag generates rapid state updates (`value: 42`, `value: 43`, ...).
These are small JSON merges. The daemon throttles state writes to the CRDT:

- Widget state updates from the kernel are merged into the in-memory
  `CommState` immediately (for low-latency forwarding to frontends).
- The CRDT is updated at most once per 100ms per comm, batching intermediate
  values. This keeps the Automerge change history small.
- The broadcast `Comm` event still fires for every update so frontends see
  real-time slider movement. The CRDT catches up asynchronously.

This hybrid approach gives us: real-time UI responsiveness via broadcasts,
durable state via CRDT, and manageable CRDT history size.

### 1.4 Output Widget Simplification

With widget state in the CRDT, the Output widget's capture protocol simplifies:

**Current approach:** The Output widget uses custom comm messages (`method:
"output"`, `method: "clear_output"`) to route kernel outputs to itself.

**New approach:** The daemon handles Output widget capture entirely on the
server side:

1. When an Output widget's `state.msg_id` is set, the daemon records the
   capture mapping (unchanged from current `CommState` tracking).
2. When the daemon receives IOPub output matching a captured `msg_id`, it
   writes the output to `comms/<widget_comm_id>/state` (updating the
   `outputs` array in the widget's state JSON) instead of to
   `cells/<cell_id>/outputs`.
3. The frontend renders Output widget outputs by reading from the widget's
   state, exactly like any other widget property.

This eliminates the custom `"output"`/`"clear_output"` messages entirely. The
Output widget becomes just another widget whose state happens to include an
`outputs` array.

### 1.5 Metadata Structure

Notebook-level metadata remains as JSON strings in a map. This is the right
choice because:

- Notebook metadata has unknown keys that must be preserved through round-trips.
- Different tools write different metadata (JupyterLab, VS Code, Runt).
- Last-write-wins at the key level is the correct conflict resolution.

```
metadata/
├── notebook_metadata: Str   # JSON: {kernelspec, language_info, runt: {uv, conda, ...}}
├── runtime: Str             # "python"|"deno"
```

### 1.6 Schema Versioning

The document root's `schema_version` field bumps from `2` to `3`. Migration
from v2 to v3:

1. Add `comms` map (empty — widgets are transient, no historical migration
   needed).
2. Add `execution` map with defaults (`queue: []`, `current: null`,
   `kernel_status: "not_started"`).
3. Add `kernel` map with defaults (all null).
4. Add `execution_state: "idle"` to each existing cell.

This migration is additive — v2 documents gain new fields without losing any
existing data. The daemon performs migration on load, same as v1→v2.

---

## 2. Wire Format and Frame Types

### 2.1 Connection Preamble (unchanged)

```
┌────────────────────┬─────────────┐
│ 4 bytes: magic     │ 1 byte:     │
│ 0xC0DE01AC         │ version (3) │
└────────────────────┴─────────────┘
```

Protocol version bumps from `2` to `3`. The daemon can support both v2 and v3
clients during transition (see Migration Path).

### 2.2 Frame Structure (unchanged)

```
┌──────────────────┬───────────────────────┐
│ 4 bytes          │ N bytes               │
│ (big-endian u32  │ (payload)             │
│  payload length) │                       │
└──────────────────┴───────────────────────┘
```

### 2.3 Typed Frames

After the handshake, each frame's first payload byte indicates its type:

| Byte | Name             | Payload Format | Direction       |
|------|------------------|----------------|-----------------|
| 0x00 | AutomergeSync    | Binary         | Bidirectional   |
| 0x01 | Request          | JSON           | Client → Daemon |
| 0x02 | Response         | JSON           | Daemon → Client |
| 0x03 | Event            | JSON           | Daemon → Client |
| 0x04 | Presence         | CBOR           | Bidirectional   |

**What changed:**

- `NotebookBroadcast` (0x03) is renamed to `Event`. The semantics change
  from "state that happens to be pushed" to "something that happened."
  This naming reinforces the principle: if it's state, it goes in the
  CRDT. Events are for things that *occur* — transient notifications that
  don't need to be replayed.

### 2.4 Handshake (refined)

The first frame is JSON declaring the channel:

```json
{
  "channel": "notebook_sync",
  "notebook_id": "/path/to/notebook.ipynb",
  "protocol": "v3",
  "working_dir": null,
  "initial_metadata": "...",
  "peer_id": "window-abc123",
  "peer_label": "human"
}
```

**New fields:**

- `peer_id`: Every connecting client declares its peer identity upfront. This
  replaces the daemon having to assign or track peer IDs separately. For the
  Tauri relay, this is a random UUID generated per window. For `runtimed-py`
  clients, it's user-chosen.
- `peer_label`: Human-readable label ("human", "agent", "mcp-server").

The daemon responds with `NotebookConnectionInfo` (unchanged except
`protocol_version: 3`).

---

## 3. Message Types

### 3.1 Requests (Client → Daemon)

These are the actions a client can ask the daemon to perform. Each gets
exactly one response.

```rust
enum Request {
    // ── Kernel lifecycle ──
    LaunchKernel {
        kernel_type: String,         // "python" | "deno"
        env_source: String,          // "uv:inline" | "conda:prewarmed" | etc.
        notebook_path: Option<String>,
    },
    ShutdownKernel {},
    InterruptExecution {},
    RestartKernel {                   // NEW: restart without re-specifying config
        run_all_after: bool,          // if true, re-execute all cells after restart
    },

    // ── Execution ──
    ExecuteCell { cell_id: String },
    RunAllCells {},
    ClearOutputs { cell_id: String },

    // ── Comm (widget) ──
    SendComm {
        message: Value,               // full Jupyter message envelope
    },

    // ── Persistence ──
    SaveNotebook {
        format_cells: bool,
        path: Option<String>,
    },
    CloneNotebook { path: String },

    // ── Environment ──
    SyncEnvironment {},

    // ── Completions and history ──
    Complete { code: String, cursor_pos: usize },
    GetHistory { pattern: Option<String>, n: i32, unique: bool },

    // ── Document ──
    GetDocBytes {},                    // bootstrap: get full CRDT state
    GetRawMetadata { key: String },
    SetRawMetadata { key: String, value: String },
}
```

**What was removed compared to v2:**

- `GetKernelInfo` — kernel type, env_source, and status are now in the
  document (`kernel` and `execution` maps). No request needed; read the CRDT.
- `GetQueueState` — queue state is in the document (`execution.queue`,
  `execution.current`). No request needed; read the CRDT.

**What was added:**

- `RestartKernel` — convenience for the common restart-and-run-all pattern.

### 3.2 Responses (Daemon → Client)

```rust
enum Response {
    KernelLaunched { kernel_type: String, env_source: String, launched_config: Value },
    KernelAlreadyRunning { kernel_type: String, env_source: String, launched_config: Value },
    KernelShuttingDown {},
    InterruptSent {},
    NoKernel {},

    CellQueued { cell_id: String },
    AllCellsQueued { count: usize },
    OutputsCleared { cell_id: String },

    NotebookSaved { path: String },
    NotebookCloned { path: String },

    CompletionResult { items: Vec<CompletionItem>, cursor_start: usize, cursor_end: usize },
    HistoryResult { entries: Vec<HistoryEntry> },

    SyncEnvironmentStarted { packages: Vec<String> },
    SyncEnvironmentComplete { synced_packages: Vec<String> },
    SyncEnvironmentFailed { error: String, needs_restart: bool },

    DocBytes { bytes: Vec<u8> },
    RawMetadata { value: Option<String> },
    MetadataSet {},

    Ok {},
    Error { error: String },
}
```

Responses are essentially unchanged. They are ACKs or results for
request/response RPC calls.

### 3.3 Events (Daemon → All Clients)

This is the big reduction. Events (formerly "broadcasts") are strictly for
things that *happen* — transient occurrences that don't need replay.

```rust
enum Event {
    // ── Comm (widget) messages ──
    // These are the irreducible stream: real-time widget interactions
    // that must be forwarded with minimal latency.
    Comm {
        msg_type: String,            // "comm_open" | "comm_msg" | "comm_close"
        content: Value,
        buffers: Vec<Vec<u8>>,       // base64 in JSON, binary in msgpack
    },

    // ── Environment progress ──
    // Transient progress reporting during kernel launch. Not worth persisting.
    EnvProgress {
        env_type: String,
        phase: EnvProgressPhase,     // repodata, solve, download, link, etc.
    },

    // ── File system ──
    FileChanged {},                  // external .ipynb edit was merged into the doc

    // ── Errors ──
    KernelError { error: String },   // kernel process crashed / failed to launch

    // ── Environment drift ──
    EnvSyncState {
        in_sync: bool,
        diff: Option<EnvSyncDiff>,
    },
}
```

**What was removed (moved to CRDT):**

| Former Broadcast | New Home |
|------------------|----------|
| `KernelStatus` | `doc.execution.kernel_status` (daemon writes, syncs automatically) |
| `ExecutionStarted` | `doc.cells[id].execution_state = "running"` + `doc.execution.current = id` |
| `ExecutionDone` | `doc.cells[id].execution_state = "done"` + `doc.execution.current = null` |
| `QueueChanged` | `doc.execution.queue` (list of cell_ids, daemon writes) |
| `Output` | Daemon writes to `doc.cells[id].outputs` (already does this; broadcast was redundant) |
| `DisplayUpdate` | Daemon updates output in `doc.cells[id].outputs` by display_id (already does this) |
| `OutputsCleared` | Daemon clears `doc.cells[id].outputs` (already does this) |
| `CommSync` | `doc.comms` map syncs via Automerge (late joiners get all widget state automatically) |

**What remained as events and why:**

- **`Comm`**: Widget interactions (`comm_msg` with `method: "update"` for
  real-time slider values, `method: "custom"` for opaque widget messages)
  need sub-frame-rate latency. The CRDT catches up asynchronously, but the
  event stream delivers the value *now*. This is the irreducible stream —
  `model.send()` in the anywidget API has no CRDT equivalent.

- **`EnvProgress`**: Environment setup phases (repodata download, dependency
  solving) are transient progress indicators. They're not useful after the
  kernel launches. Persisting them in the CRDT would add clutter.

- **`FileChanged`**: A notification that the daemon merged an external file
  change. The actual data flows through the CRDT; the event is a UI hint
  to show a toast notification.

- **`KernelError`**: Kernel crash notifications. These could arguably be CRDT
  state, but they're typically one-shot notifications that the UI shows as
  error banners. The CRDT's `execution.kernel_status = "error"` captures
  the persistent state; the event carries the human-readable error message.

- **`EnvSyncState`**: Environment drift detection. This is a computed diff
  that changes when notebook metadata changes relative to the launched
  kernel config. It's derived state, not source-of-truth state.

**Net result:** 13 broadcast variants → 5 event variants. The CRDT absorbs
8 former broadcasts, eliminating their dual-path consistency bugs.

### 3.4 Presence (unchanged in format, refined in semantics)

Presence remains a separate CBOR-encoded frame type (0x04). This is correct:

- Presence is high-frequency (cursor moves on every keystroke).
- Presence is ephemeral (no persistence, no replay).
- Presence has its own TTL/heartbeat lifecycle.
- CBOR is more compact than JSON for small binary-ish payloads.

Channels:

| Channel | Data | Owner |
|---------|------|-------|
| `cursor` | `{cell_id, line, column}` | Each frontend peer |
| `selection` | `{cell_id, anchor_line, anchor_col, head_line, head_col}` | Each frontend peer |
| `custom` | `Vec<u8>` | Any peer (extensibility) |

**What changed:**

- `kernel_state` channel is **removed** from presence. Kernel status is now
  durable document state (`doc.execution.kernel_status`), not ephemeral
  presence. This was always an awkward fit — kernel status isn't "presence"
  in any meaningful sense.

---

## 4. State Ownership

### 4.1 Ownership Table

| State | Owner (writes) | Location | Sync mechanism |
|-------|----------------|----------|----------------|
| Cell source text | Frontend WASM | CRDT `cells[id].source` | Automerge sync |
| Cell ordering | Frontend WASM | CRDT `cells[id].position` | Automerge sync |
| Cell add/delete/move | Frontend WASM | CRDT `cells` map | Automerge sync |
| Cell metadata | Frontend WASM | CRDT `cells[id].metadata` | Automerge sync |
| Cell outputs | Daemon | CRDT `cells[id].outputs` | Automerge sync |
| Execution count | Daemon | CRDT `cells[id].execution_count` | Automerge sync |
| Execution state per cell | Daemon | CRDT `cells[id].execution_state` | Automerge sync |
| Execution queue | Daemon | CRDT `execution.queue` | Automerge sync |
| Kernel status | Daemon | CRDT `execution.kernel_status` | Automerge sync |
| Kernel type/env | Daemon | CRDT `kernel.*` | Automerge sync |
| Widget state | Daemon | CRDT `comms[id].state` | Automerge sync |
| Widget buffers | Daemon | Blob store (refs in CRDT) | Automerge sync + HTTP |
| Notebook metadata | Frontend WASM (deps) / Daemon (language_info) | CRDT `metadata` | Automerge sync |
| Cursor position | Frontend | Wire (presence frame) | CBOR broadcast |
| Selection range | Frontend | Wire (presence frame) | CBOR broadcast |
| Env progress | Daemon | Wire (event frame) | JSON event |
| Real-time widget updates | Kernel (via daemon) | Wire (event frame) | JSON event |

### 4.2 The Two-Speed Widget Pattern

Widget state has two speeds:

1. **Real-time (events):** Every `comm_msg` with `method: "update"` is forwarded
   to all clients as a `Comm` event immediately. Frontends apply these to
   their local `WidgetStore` for instant rendering (slider tracks the thumb
   in real time).

2. **Durable (CRDT):** The daemon batches widget state updates and writes
   the merged state to `doc.comms[id].state` at most once per 100ms. This
   ensures late joiners and reconnecting clients converge to the correct
   state via normal Automerge sync.

The frontend reconciles these two sources:

- On receiving a `Comm` event: apply immediately to `WidgetStore` (fast path).
- On receiving an Automerge sync with `comms` changes: merge into
  `WidgetStore` only if the CRDT state is *newer* than what's already in
  the store (slow path, catches up after reconnection).

### 4.3 The Frontend's Three Layers

```
┌────────────────────────────────────────────────┐
│  React State (cells, UI)                       │
│  Derived from WASM handle via get_cells_json() │
│  + WidgetStore for real-time widget rendering  │
├────────────────────────────────────────────────┤
│  WASM NotebookHandle (Automerge peer)          │
│  Owns local CRDT replica                       │
│  Instant mutations for cell editing            │
│  Receives sync from daemon                     │
├────────────────────────────────────────────────┤
│  Tauri Relay (transparent pipe)                │
│  Forwards typed frames between WASM and daemon │
│  No parsing, no merging, no state              │
└────────────────────────────────────────────────┘
```

### 4.4 The Iframe's Isolation

```
┌─────────────────────────────────────────┐
│  Parent Window                          │
│  ┌─────────────────────────────────┐    │
│  │  WidgetStore (all widget state) │    │
│  └──────────┬──────────────────────┘    │
│             │ postMessage                │
│  ┌──────────▼──────────────────────┐    │
│  │  Sandboxed Iframe               │    │
│  │  - Local WidgetStore replica    │    │
│  │  - Renders widget HTML/JS       │    │
│  │  - Cannot access Tauri APIs     │    │
│  │  - Cannot access parent DOM     │    │
│  └─────────────────────────────────┘    │
└─────────────────────────────────────────┘
```

The iframe receives `comm_open`, `comm_msg`, `comm_close` via `postMessage`
from the parent. It sends state updates and custom messages back via
`postMessage`. This boundary is unchanged.

---

## 5. Sync Flows

### 5.1 Cell Editing (Local-First)

```
User types character
  → React calls WASM handle.update_source(cell_id, text)
  → WASM applies mutation to local Automerge doc (instant)
  → Synchronous rematerialization → React state updates
  → handle.generate_sync_message() → sync bytes
  → Prepend 0x00 type byte → invoke("send_frame")
  → Relay pipes to daemon socket
  → Daemon applies sync, updates canonical doc
  → Daemon generates reply sync → 0x00 frame
  → Relay emits "notebook:frame" event
  → WASM handle.receive_frame() → applies sync
  → If doc changed, rematerialize cells
```

Unchanged from v2. This is the core strength of the architecture.

### 5.2 Cell Execution

```
User clicks "Run"
  → Frontend sends Request::ExecuteCell { cell_id }
  → Daemon receives request
  → Daemon writes to CRDT:
      cells[cell_id].execution_state = "queued"
      execution.queue.push(cell_id)
  → Daemon responds: Response::CellQueued { cell_id }
  → Automerge sync delivers queue state to all frontends
  → (React sees execution_state change, shows spinner)

  → When cell reaches front of queue:
  → Daemon writes to CRDT:
      cells[cell_id].execution_state = "running"
      cells[cell_id].outputs = []   (clear)
      execution.current = cell_id
      execution.kernel_status = "busy"
  → Daemon sends code to kernel via Jupyter wire protocol

  → Kernel produces output (IOPub):
  → Daemon writes output to CRDT:
      cells[cell_id].outputs.push(output_json_or_manifest)
  → Automerge sync delivers to all frontends
  → (React rematerializes, shows output)

  → Kernel finishes (execute_reply):
  → Daemon writes to CRDT:
      cells[cell_id].execution_state = "done"
      cells[cell_id].execution_count = "5"
      execution.current = null
      execution.kernel_status = "idle"
      execution.queue.remove(cell_id)
  → Automerge sync delivers to all frontends
```

**Key change from v2:** No broadcast events for execution lifecycle. The CRDT
*is* the delivery mechanism. All frontends see state changes through normal
Automerge sync. This eliminates the race condition where a broadcast arrives
before or after the CRDT sync that wrote the output data.

### 5.3 Widget Interaction (Slider Drag)

```
User drags slider in iframe
  → Iframe postMessage: { type: "comm_msg", comm_id, state: {value: 42} }
  → Parent WidgetStore applies locally (instant visual update)
  → Parent sends Request::SendComm { message: jupyter_envelope }
  → Daemon receives, forwards to kernel via Jupyter shell channel
  → Kernel processes, may produce comm_msg back

  → Kernel sends comm_msg (IOPub): { method: "update", state: {value: 42, description: "..."} }
  → Daemon receives:
      1. Merges into in-memory CommState (immediate)
      2. Emits Event::Comm to all clients (immediate)
      3. Schedules CRDT write: comms[comm_id].state = merged_state (throttled, ≤100ms)
  → All frontends receive Event::Comm → update WidgetStore → iframe re-renders
  → CRDT sync delivers durable state (may lag behind events by up to 100ms)
```

### 5.4 New Client Joining (Second Window)

```
Second window opens, connects to daemon
  → Handshake: { channel: "notebook_sync", notebook_id: "...", peer_id: "window-2" }
  → Daemon responds with NotebookConnectionInfo
  → WASM creates empty doc via create_empty()
  → Automerge sync begins:
      → Daemon sends all document state (cells, outputs, metadata, comms, execution)
      → WASM applies, React materializes cells and outputs
  → Widget state:
      → CRDT sync delivers doc.comms (all active widgets with their state)
      → Frontend reconstructs WidgetStore from doc.comms on initial sync
      → No separate CommSync broadcast needed
  → Presence:
      → Daemon sends Presence::Snapshot to new peer
      → New peer sees all existing cursors/selections
  → Execution state:
      → CRDT sync delivers execution.queue, execution.current, kernel_status
      → Each cell's execution_state shows whether it's running/queued/idle
      → No separate GetQueueState request needed
```

**Key improvement:** The second window converges to full state through
*one mechanism* (Automerge sync) instead of three (sync + CommSync +
GetQueueState).

### 5.5 Daemon Restart Recovery

```
Daemon crashes
  → All socket connections break
  → Frontends receive "daemon:disconnected" event
  → Frontends show "reconnecting" UI

  → Daemon restarts, loads persisted Automerge docs from disk
  → Frontends reconnect (retry with backoff)
  → Handshake + Automerge sync:
      → Daemon's doc has all state up to crash point
      → Frontend's WASM doc may have mutations made while disconnected
      → Automerge sync merges both directions automatically
  → Kernel state:
      → Kernel processes likely died with the daemon
      → Daemon writes execution.kernel_status = "shutdown"
      → Clears execution.queue and execution.current
      → Resets all cells' execution_state to "idle"
  → Widget state:
      → If widgets were persisted in CRDT: restored from doc
      → Kernel-side comm channels are gone (kernel died)
      → Daemon marks all comms as closed: comms[*].closed = true
      → Frontend shows "widgets stale, restart kernel" indicator
```

### 5.6 clear_output(wait=True) Semantics

```
Kernel sends clear_output with wait=True
  → Daemon records pending clear for this cell (in-memory flag)
  → Does NOT clear outputs in CRDT yet

  → Next output arrives for this cell
  → Daemon clears outputs in CRDT first: cells[cell_id].outputs = []
  → Then writes new output: cells[cell_id].outputs.push(new_output)
  → Both mutations in same Automerge transaction = atomic from sync perspective
```

This is already how the daemon handles it. No change needed.

### 5.7 update_display_data

```
Kernel sends update_display_data with display_id
  → Daemon scans cells[*].outputs for matching display_id
  → Updates the output in place in the CRDT
  → Automerge sync delivers the update to all frontends
  → Frontend rematerializes affected cells
```

No separate `DisplayUpdate` broadcast needed. The CRDT mutation *is* the
notification.

---

## 6. Binary Data Strategy

### 6.1 Unified Blob Store

All large binary content flows through one system:

```
┌─────────────────────────────┐
│  Content-Addressed Blob Store │
│  Key: Blake3 hash            │
│  Storage: ~/.cache/runt/blobs/ │
│  Access: HTTP server on localhost │
└─────────────────────────────┘
```

| Data Type | Current Approach | v3 Approach |
|-----------|------------------|-------------|
| Cell outputs (images, plots) | Blob store (manifest hash in CRDT) | Unchanged |
| Widget binary buffers | Inline base64 in JSON events | **Blob store** (hash refs in CRDT + events) |
| Large HTML outputs | Blob store | Unchanged |
| Notebook attachments | Blob store (resolved_assets) | Unchanged |

### 6.2 Widget Buffer Flow

```
Kernel sends comm_msg with buffers (e.g., numpy array for plotly)
  → Daemon receives message with binary buffers
  → Daemon stores each buffer in blob store → gets hash per buffer
  → Daemon writes to CRDT: comms[comm_id].buffer_refs = [hash1, hash2, ...]
  → Daemon sends Event::Comm with buffer_refs instead of inline buffers:
      { msg_type: "comm_msg", content: {...}, buffer_refs: ["hash1", "hash2"] }
  → Frontend resolves buffers from blob HTTP server when needed
  → Iframe receives buffer_refs, fetches from blob server on demand
```

**Why this is better:**

- No base64 inflation (33% overhead) in JSON events.
- Large buffers (plotly numpy arrays can be megabytes) don't bloat the
  event stream.
- Same caching infrastructure as cell output blobs.
- Content-addressed deduplication (same numpy array used by multiple
  widgets only stored once).

**The Comm event still carries small state updates inline** (they're JSON
key-value pairs, typically under 1KB). Only `buffers` move to the blob store.

### 6.3 Blob Resolution Protocol

Unchanged from v2:

1. Frontend gets blob port from daemon (via `get_blob_port()` Tauri command).
2. Resolves `http://localhost:{port}/blob/{hash}` for each hash.
3. Manifests are JSON files that reference other blob hashes for multi-part
   outputs.

---

## 7. Migration Path

### 7.1 Phased Rollout

The migration from v2 to v3 can happen incrementally:

**Phase 1: Schema v3 with backward-compatible events**

- Add `comms`, `execution`, `kernel` maps to the document schema.
- Daemon writes to both CRDT (new) and broadcasts (old) during transition.
- Frontends can opt into reading from CRDT or broadcasts.
- Protocol version stays at 2 (wire format unchanged).

**Phase 2: Widget state in CRDT**

- Daemon writes widget state to `doc.comms` (throttled).
- New frontends reconstruct `WidgetStore` from CRDT on connect.
- `CommSync` broadcast still sent for v2 clients.
- Widget binary buffers move to blob store.

**Phase 3: Execution state in CRDT**

- Daemon writes execution lifecycle to CRDT.
- Remove `GetQueueState` and `GetKernelInfo` requests.
- Remove `ExecutionStarted`, `ExecutionDone`, `QueueChanged`, `Output`,
  `OutputsCleared`, `DisplayUpdate` broadcasts.
- `KernelStatus` broadcast replaced by `doc.execution.kernel_status`.

**Phase 4: Protocol version bump**

- Bump preamble version to 3.
- Remove backward-compatible broadcast paths.
- Remove `CommSync`, `KernelStatus`, and other eliminated broadcasts.
- Remove `kernel_state` from presence channels.
- `runtimed-py` clients updated to use new protocol.

### 7.2 WASM Bindings Changes

The `runtimed-wasm` crate needs new methods:

```rust
impl NotebookHandle {
    // Read execution state
    fn get_execution_state(&self) -> ExecutionState;
    fn get_cell_execution_state(&self, cell_id: &str) -> String;

    // Read widget state
    fn get_comms_json(&self) -> String;
    fn get_comm_state(&self, comm_id: &str) -> Option<String>;

    // Read kernel info
    fn get_kernel_info(&self) -> KernelInfo;
}
```

These are read-only. The daemon owns writes to these fields.

### 7.3 Frontend Hook Changes

- `useAutomergeNotebook`: Gains `executionState`, `kernelStatus`, `comms`
  derived from the WASM doc. Replaces broadcast-driven state tracking.
- `useDaemonKernel`: Simplified — no longer tracks kernel status or
  execution lifecycle from broadcasts. Only handles `Comm` events (for
  real-time widget updates) and `EnvProgress`/`EnvSyncState` events.
- `WidgetStore`: Initialized from `doc.comms` on load, updated by `Comm`
  events in real time.

### 7.4 What Stays the Same

- Cell editing (local-first WASM mutations + Automerge sync).
- Blob store and manifest system for outputs.
- Presence protocol (CBOR frames, heartbeat/TTL).
- Tauri relay as transparent pipe.
- Request/response for imperative actions (execute, save, complete).
- Iframe isolation and postMessage bridge.
- Trust system (HMAC-SHA256 signing of dependency metadata).
- Unix socket transport with length-prefixed framing.

---

## Appendix A: Current vs. v3 Comparison

### Broadcast Variants: 13 → 5

| v2 Broadcast | v3 Fate |
|--------------|---------|
| `KernelStatus` | CRDT: `execution.kernel_status` |
| `ExecutionStarted` | CRDT: `cells[id].execution_state` |
| `ExecutionDone` | CRDT: `cells[id].execution_state` |
| `QueueChanged` | CRDT: `execution.queue` |
| `Output` | CRDT: `cells[id].outputs` (already written, broadcast was redundant) |
| `DisplayUpdate` | CRDT: `cells[id].outputs` (update in place) |
| `OutputsCleared` | CRDT: `cells[id].outputs` (clear) |
| `CommSync` | CRDT: `doc.comms` (normal Automerge sync) |
| `Comm` | **Kept** as Event (real-time widget interactions) |
| `FileChanged` | **Kept** as Event (UI notification hint) |
| `EnvProgress` | **Kept** as Event (transient progress) |
| `KernelError` | **Kept** as Event (error message delivery) |
| `EnvSyncState` | **Kept** as Event (computed drift notification) |

### Request Variants: 17 → 15

| v2 Request | v3 Fate |
|------------|---------|
| `GetKernelInfo` | Removed — read from CRDT |
| `GetQueueState` | Removed — read from CRDT |
| `RestartKernel` | **Added** (new convenience request) |
| All others | Unchanged |

### Sync Mechanisms: 3 → 1 (+1 for real-time)

| v2 | v3 |
|----|-----|
| Automerge sync for cells/outputs | Automerge sync for cells, outputs, widgets, execution state, kernel info |
| CommSync broadcast for widget state | Absorbed into Automerge sync |
| Broadcasts for execution lifecycle | Absorbed into Automerge sync |
| Comm events for real-time widgets | Kept (irreducible stream) |

## Appendix B: Why Not Put Everything in the CRDT

Two categories of data intentionally stay *outside* the CRDT:

### Real-time widget interactions (Comm events)

A slider drag at 60fps generates 60 state updates per second. Writing all of
these to Automerge would create a massive change history with no value (nobody
needs to replay every intermediate slider position). The CRDT gets a throttled
snapshot (10/sec); the event stream carries the real-time values.

The anywidget `model.send(content, callbacks, buffers)` API sends arbitrary
opaque messages between frontend and kernel. These are not state — they're
commands ("draw this line", "fetch this data"). They have no CRDT representation.

### Presence (cursor/selection)

Cursors move on every keystroke. Writing cursor positions to the CRDT would
generate more Automerge operations than all cell edits combined, and the data
is worthless after the peer disconnects. The separate CBOR presence channel
with TTL-based cleanup is the right design.

## Appendix C: The `runtimed-py` Full Peer Mode

Python clients (MCP server) that maintain their own Automerge doc replica
benefit directly from this design:

- They see widget state through normal Automerge sync (no need to handle
  `CommSync` broadcasts).
- They see execution state through the CRDT (no need to poll `GetQueueState`).
- They can read kernel status from the document (no need for `GetKernelInfo`).
- They still receive `Comm` events if they need real-time widget updates
  (most MCP use cases don't).

## Appendix D: Schema Version 3 Automerge Operations

### Initial document creation

```rust
fn create_v3_doc(notebook_id: &str) -> NotebookDoc {
    let mut doc = AutoCommit::new();
    doc.put(ROOT, "schema_version", 3u64);
    doc.put(ROOT, "notebook_id", notebook_id);

    let cells = doc.put_object(ROOT, "cells", ObjType::Map);
    let metadata = doc.put_object(ROOT, "metadata", ObjType::Map);
    let comms = doc.put_object(ROOT, "comms", ObjType::Map);

    let execution = doc.put_object(ROOT, "execution", ObjType::Map);
    doc.put(&execution, "kernel_status", "not_started");
    doc.put(&execution, "current", ()); // null
    let queue = doc.put_object(&execution, "queue", ObjType::List);

    let kernel = doc.put_object(ROOT, "kernel", ObjType::Map);
    doc.put(&kernel, "kernel_type", ()); // null
    doc.put(&kernel, "env_source", ()); // null
    doc.put(&kernel, "launched_config", ()); // null

    doc
}
```

### Migration from v2

```rust
fn migrate_v2_to_v3(doc: &mut AutoCommit) {
    // Add comms map
    doc.put_object(ROOT, "comms", ObjType::Map);

    // Add execution map
    let execution = doc.put_object(ROOT, "execution", ObjType::Map);
    doc.put(&execution, "kernel_status", "not_started");
    doc.put(&execution, "current", ());
    doc.put_object(&execution, "queue", ObjType::List);

    // Add kernel map
    let kernel = doc.put_object(ROOT, "kernel", ObjType::Map);
    doc.put(&kernel, "kernel_type", ());
    doc.put(&kernel, "env_source", ());
    doc.put(&kernel, "launched_config", ());

    // Add execution_state to each cell
    let cells_id = doc.get(ROOT, "cells").unwrap().1;
    for key in doc.keys(&cells_id).collect::<Vec<_>>() {
        let cell_id = doc.get(&cells_id, &key).unwrap().1;
        doc.put(&cell_id, "execution_state", "idle");
    }

    // Bump version
    doc.put(ROOT, "schema_version", 3u64);
}
```
