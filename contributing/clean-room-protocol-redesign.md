# Clean-Room Protocol Redesign Brief

You are a systems architect designing the wire protocol for a notebook application from first principles. You have access to this codebase and should explore it thoroughly. But your job is to think about what the protocol *should* be, not to describe what it currently is.

## The Question

If you were to start over and write this protocol again from scratch, what would you do? Design the wire protocol, document schema, and state management architecture for a notebook application.

## Hard Constraints (Non-Negotiable)

These are architectural facts you cannot change:

1. **Tauri desktop app** — The frontend is a web view inside a Tauri (Rust) shell. The Tauri process acts as a relay between the web view and the daemon. Communication between webview and Tauri is via `invoke()` (request/response) and Tauri events (push).

2. **Separate daemon process (`runtimed`)** — A long-running background daemon manages kernels, environments, and notebook state. It runs as a system service. Multiple notebook windows connect to the same daemon. The daemon survives window closes and app restarts.

3. **Jupyter kernel protocol under the hood** — The daemon talks to kernels using the Jupyter wire protocol (ZeroMQ). Kernels are unmodified IPython/Deno kernels. The daemon intercepts IOPub, shell, stdin, and control channels. You must support the full Jupyter comm protocol for widgets (comm_open, comm_msg, comm_close).

4. **Automerge CRDT for the notebook document** — The notebook is an Automerge document. The frontend has a local WASM peer (compiled from the same Rust crate as the daemon). Cell editing is local-first with no round-trip. The daemon holds the canonical copy. You cannot switch away from Automerge.

5. **Unix socket transport** — Daemon and clients communicate over a Unix socket (named pipe on Windows) with length-prefixed frames.

6. **Content-addressed blob store** — Large binary outputs (images, plots, HTML) are stored in a content-addressed blob store on disk, served over HTTP. The sync protocol carries references (hashes), not inline data.

7. **Python bindings (`runtimed-py`)** — External Python clients (like the MCP server) also connect to the daemon. They use the same socket protocol. Some may maintain their own Automerge doc replicas (full peer mode) rather than being a transparent pipe.

8. **Security-isolated iframe** — Widget and HTML outputs render in a sandboxed iframe with an opaque origin (blob URL). No Tauri API access. Communication between parent and iframe is via `postMessage`.

9. **Multiple runtimes** — The system supports Python (via UV or Conda environments) and Deno kernels. Environment resolution is the daemon's responsibility.

## Key Capabilities to Support

The protocol must support all of these. Think about which should be document state vs. messages vs. something else:

### Notebook Editing
- Create, delete, move, and edit cells (code, markdown, raw)
- Real-time collaborative editing (multiple windows on the same notebook)
- Cursor/selection presence for remote peers
- Cell metadata (collapse state, tags, source_hidden)

### Execution
- Execute single cells, run all cells, interrupt, restart kernel
- Execution queue (ordered, daemon-managed)
- Execution count tracking
- Cell outputs: streams (stdout/stderr), display_data, execute_result, errors
- Output accumulation (a cell can produce many outputs over time)
- `clear_output(wait=True)` semantics (deferred clear until next output)
- `update_display_data` (update an existing output by display_id in any cell)
- Kernel status (idle, busy, starting, error, shutdown)

### Widgets (Jupyter Comm Protocol)
- **54 built-in ipywidgets**: sliders, buttons, dropdowns, text inputs, progress bars, etc.
- **Output widget**: A widget that captures cell outputs into itself (via `msg_id` matching). Supports nesting (`with out1: with out2:`).
- **anywidget/AFM**: Third-party widgets via ESM modules loaded at runtime. State via `model.get()`/`model.set()`, binary via `buffers`, custom messages via `model.send()`.
- **Widget state sync for new clients**: When a second window opens on the same notebook, it must see all existing widgets with their current state.
- **Binary buffers**: Some widget state includes ArrayBuffers (numpy arrays for plotly, image data). These can be large.
- **Container widgets**: Widgets reference other widgets via `IPY_MODEL_<comm_id>` strings. Layout widgets are separate models.
- **Comm lifecycle**: comm_open (create), comm_msg with method "update" (state delta), comm_msg with method "custom" (opaque messages), comm_close (destroy).

### Environment Management
- Kernel launch with environment source labels (e.g., "uv:inline", "conda:prewarmed")
- Environment progress reporting during kernel launch (repodata, solve, download, link phases)
- Hot-install packages into running kernel (sync environment)
- Detect environment drift (notebook metadata changed vs. running kernel)

### Persistence
- Save to .ipynb (checkpoint from Automerge doc)
- External file change detection (someone edited the .ipynb outside the app)
- Preserve unknown metadata keys through round-trips
- Inline dependency trust (HMAC-SHA256 per-machine signing)

### Settings
- Settings are synced across all windows and the daemon via a **separate Automerge document** (not the notebook doc). The daemon holds the canonical copy and persists to disk.
- Settings include: theme, default_runtime, default_python_env, default UV/Conda packages, keep_alive_secs, onboarding state.
- Any window can write a setting; all other windows receive the change via Automerge sync.
- The settings sync uses the same Unix socket transport as notebook sync, differentiated by the handshake (`"SettingsSync"` vs `"NotebookSync"`).
- Frontend falls back to a local `settings.json` if the daemon is unavailable.

### Other
- Code completions from the kernel
- Kernel input history search
- Notebook metadata read/write

## Current Architecture Summary (for context)

The current system uses three parallel channels over one socket connection:

1. **Automerge sync frames** (binary, type byte `0x00`) — bidirectional CRDT sync for cells, outputs, and metadata
2. **Request/response** (JSON, types `0x01`/`0x02`) — client asks daemon to do things (execute, launch kernel, save)
3. **Broadcasts** (JSON, type `0x03`) — daemon pushes events to all clients (kernel output, status, comm messages, env progress)

The document schema stores cells in a map keyed by cell ID with fractional-index positions. Outputs are stored as JSON strings in an Automerge list (with blob hashes for large content). Notebook metadata is stored as JSON strings in a metadata map.

Widget state is tracked **outside** the Automerge doc, in an in-memory `CommState` on the daemon and a `WidgetStore` on the frontend. New clients receive a `CommSync` broadcast (snapshot of all active widgets) when they connect. Widget messages flow as broadcasts, not document mutations.

Presence (cursors/selections) uses a separate binary frame type (`0x04`, CBOR-encoded).

## What to Think About

Consider these tensions and trade-offs:

1. **Document state vs. message streams** — What belongs in the CRDT (convergent, persistent, multiplayer-native) vs. what belongs in ephemeral message streams? The current system puts cells and outputs in the doc but widgets outside it. Is that the right boundary?

2. **Widget state in the CRDT** — Widget state (`value`, `description`, `disabled`, `outputs` for Output widgets) is currently managed in a separate `WidgetStore`/`CommState` with its own sync mechanism (`CommSync`). Could this live in the Automerge doc? What are the implications for performance (slider drag = many rapid state updates), binary data, and the anywidget model interface?

3. **Output widget specifically** — The Output widget's entire protocol (`method: "output"`, `method: "clear_output"` custom messages) exists because Jupyter didn't have a shared document. If outputs are already in the CRDT, does the Output widget need custom messages at all, or can the daemon just write to `doc.comms[widgetId].state.outputs` directly?

4. **Reducing broadcast surface** — The current broadcast enum has 13 variants. How many of these could become document state that syncs automatically? Which ones are genuinely ephemeral events that need a message stream?

5. **The Tauri relay's role** — Currently it's a transparent byte pipe. Should it do more? Less? Should the frontend talk directly to the daemon socket?

6. **Binary data strategy** — Currently: blob store for cell outputs, inline base64 for widget buffers. Is there a unified approach?

7. **Presence** — Currently a separate frame type with CBOR. Is there a better way?

8. **Schema evolution** — The current doc has `schema_version`. How should the protocol handle schema migrations when the doc structure changes?

9. **Error handling and reconnection** — What happens when the daemon dies mid-execution? When a sync stalls? When the socket drops?

10. **The anywidget AFM interface** — `model.get(key)`, `model.set(key, value)`, `model.save_changes()`, `model.on("change:key", cb)`, `model.send(content, callbacks, buffers)`. If widget state is in the CRDT, the first four map naturally. `model.send()` is the irreducible stream. How does this affect the architecture?

## Deliverable

Write a protocol specification that covers:

1. **Document schema** — What lives in the Automerge doc
2. **Frame types and wire format** — What goes over the socket
3. **Message types** — Requests, responses, and any remaining broadcasts/events
4. **State ownership** — Who owns what (daemon, frontend WASM, frontend React, iframe)
5. **Sync flow** — How state gets from A to B for each major operation (edit, execute, widget interaction, new client joining)
6. **Binary data** — Unified strategy for blobs, widget buffers, and large outputs
7. **Migration path** — How to get from the current architecture to the new one incrementally (if the new design differs)

Think from first principles. You have the full codebase available — explore it. But design what *should* be, not what is.
