# Execution Log Design

## Motivation

PR #1052 introduced execution IDs as a spike. The core idea — giving every
cell execution a UUID and threading it through the stack — is sound. But the
spike stores queue state as parallel lists in the RuntimeStateDoc, which is
fragile. This document proposes a cleaner architecture that emerged from
reviewing the spike.

The key insight: **separate intent from acknowledgment**. Clients declare
what they want to run. The daemon declares what happened.

---

## Document Ownership

Today there are two Automerge documents:

| Document | Writers | Readers |
|---|---|---|
| **Notebook doc** | Frontend, Python client, daemon (outputs) | Everyone |
| **Runtime state doc** | Daemon only | Everyone |

The problem with execution state in the spike is that it lives entirely in
the runtime state doc, but clients need to *initiate* executions. Today that
happens through WebSocket request/response — the client sends `ExecuteCell`,
the daemon responds with `CellQueued`. The CRDT only reflects state after
the fact.

### Proposed split

| Concern | Document | Writer(s) |
|---|---|---|
| Cell source, metadata | Notebook doc | Any client |
| **Execution intents** | Notebook doc | Any client |
| Kernel status | Runtime state doc | Daemon |
| **Execution lifecycle** | Runtime state doc | Daemon |
| **Execution outputs** | Runtime state doc | Daemon |
| Queue state (derived) | Runtime state doc | Daemon |

Outputs move to the runtime state doc. At save time, both documents are
snapshotted and the save logic merges outputs back into the ipynb cell
structure. This is fast — just two snapshots, no merge required.

---

## Schema: Notebook Doc (cell state)

Each cell in the notebook doc gains an `execution_id` field:

```text
cells/
  {cell_id}/
    source: Str              (existing)
    cell_type: Str           (existing)
    metadata: Map            (existing)
    execution_id: Str|null   NEW — points to the active execution for this cell
```

When the daemon **acknowledges** an execution intent, it immediately writes
the `execution_id` onto the cell in the notebook doc — before the kernel
starts running. This is the **pointer** that connects a cell to its current
outputs in the runtime state doc. Because it's set early, the frontend can
start reading streamed outputs from `outputs/{execution_id}/` in the
runtime state doc as soon as the first IOPub message arrives. The cell
doesn't have to wait for completion to show results.

Clearing outputs is just setting this field to `null`.

---

## Schema: Notebook Doc (execution intents)

```text
execution_intents/          Map
  {execution_id}/           Map
    cell_id: Str            which cell to execute
    requested_by: Str       user/client identity (presence ID)
    requested_at: Str       ISO 8601 timestamp
    cancelled: Bool         client can set true to request cancellation
```

Each intent is a map keyed by execution ID (a UUID generated client-side).
Any connected client can write a new intent. The daemon watches for new
entries.

### Why a map of maps, not a list

Lists in Automerge use fractional indices and make reordering awkward.
A map keyed by execution ID means:

- No ordering conflicts (UUIDs are unique)
- Idempotent writes (reinserting the same key is a no-op)
- Easy lookup by execution ID
- The daemon determines execution order, not the CRDT structure

The daemon maintains its own internal queue ordering. The CRDT just holds
the set of intents.

### Cancellation

A client sets `cancelled: true` on an existing intent. The daemon checks
this flag before starting execution and during execution (to trigger
interrupt). This is cleaner than a separate `CancelExecution` protocol
message — it's just a CRDT write.

---

## Schema: Runtime State Doc (execution lifecycle)

```text
executions/                 Map
  {execution_id}/           Map
    cell_id: Str            echoed from intent
    status: Str             "queued" | "running" | "done" | "error"
    execution_count: Int    Jupyter execution count (set on start)
    started_at: Str|null    ISO 8601
    finished_at: Str|null   ISO 8601
    success: Bool|null      set on completion
    error_name: Str|null    set if errored
    error_value: Str|null   set if errored

queue/                      Map
  executing: Str|null       execution_id currently running
  order: List[Str]          execution_ids in queue order (daemon-managed)

outputs/                    Map
  {execution_id}/           List[Map]
    each entry:
      output_type: Str      "stream" | "display_data" | "execute_result" | "error"
      output_json: Str      full Jupyter output as JSON
```

### Lifecycle state machine

```text
intent written (notebook doc)
       │
       ▼
   "queued" ──────────────────► "error" (if cancelled before start)
       │
       ▼
   "running" ─────► "error" (kernel died, exception, cancelled)
       │
       ▼
    "done"
```

The daemon is the only writer. When it acknowledges an intent, it
simultaneously:

1. Creates the execution entry in the runtime state doc (status `"queued"`).
2. Writes the `execution_id` onto the cell in the notebook doc.

This ensures the cell pointer and the execution state are available to all
clients before the kernel even starts. As the kernel progresses, the daemon
updates status and streams outputs into `outputs/{execution_id}/`.

### Why outputs live here now

1. **Clear ownership**: Only the daemon writes outputs. No conflict with
   client edits to cells.
2. **Execution-scoped**: Outputs are keyed by execution ID, not cell ID.
   Re-executing a cell produces a new entry. History is preserved.
3. **Save is a snapshot**: At save time, read both docs. For each cell,
   find its most recent execution in the outputs map, serialize to ipynb
   output format. Fast, deterministic, no merge.

### Trimming old executions

The daemon trims execution entries and their outputs when the map exceeds
a threshold (e.g. 64 entries). It keeps the most recent execution per cell
plus the last N total. This bounds memory without losing useful state.

---

## Daemon Behavior

### Watching for intents

The daemon subscribes to changes on the notebook doc. When a new key
appears under `execution_intents/`, it:

1. Validates the intent (cell exists, has source code).
2. Creates a corresponding entry in `executions/` with status `"queued"`.
3. Adds the execution ID to `queue/order`.
4. Proceeds with normal queue processing.

### Processing the queue

Same as today — pop from the front of `queue/order`, send `execute_request`
to the Jupyter kernel, route IOPub messages to `outputs/{execution_id}/`.

### Handling cancellation

On each queue tick and during IOPub processing, the daemon checks
`execution_intents/{id}/cancelled`. If true:

- **Queued**: Remove from `queue/order`, set status to `"error"`, skip.
- **Running**: Send kernel interrupt, set status to `"error"` on completion.

### Kernel death

Same as today — set all in-flight executions to `"error"` with
`error_name: "KernelDied"`.

---

## Save Semantics

Saving a notebook snapshots both documents and merges them into ipynb:

1. Snapshot the notebook doc (cells, metadata, execution intents).
2. Snapshot the runtime state doc (executions, outputs).
3. For each cell:
   - Read `cell.execution_id`. If `null`, the cell has no outputs — write
     an empty `outputs` array in the ipynb.
   - If set, look up `outputs/{execution_id}/` in the runtime state doc
     snapshot. Deserialize each entry and write them as the cell's ipynb
     outputs.
   - Read `executions/{execution_id}/execution_count` for the cell's
     `execution_count` field in the ipynb.
4. Serialize to ipynb as usual.

This is fast — two snapshots (no merge), then a straightforward join on
execution ID. No live CRDT access needed during the write.

---

## Clear Outputs

Clear outputs is a **pure client operation** — no daemon round trip needed.

### Clear outputs for one cell

Set `cells/{cell_id}/execution_id` to `null` in the notebook doc. Done.
The outputs still exist in the runtime state doc but nothing references
them. They'll be cleaned up on the next trim cycle.

### Clear all outputs (e.g. "Restart kernel & clear all outputs")

Iterate every cell, set `execution_id` to `null`. One CRDT write per cell,
all local, instant. The daemon doesn't even need to know it happened.

### Re-execution

When a cell is re-executed, the daemon writes the new `execution_id` onto
the cell at acknowledgment time — immediately, before the kernel starts.
The old execution's outputs remain in the runtime state doc (reachable by
ID for history/undo) but the cell now points to the new one. The frontend
sees the pointer update, clears the displayed outputs, and starts showing
new outputs as they stream in under the new execution ID. Next save picks
up the new outputs automatically.

---

## Client Behavior

### Submitting an execution (Python)

```text
execution_id = uuid4()
# Write intent to notebook doc
session.notebook_doc.put("execution_intents", execution_id, {
    "cell_id": cell_id,
    "requested_by": session.presence_id,
    "requested_at": now_iso8601(),
    "cancelled": False,
})
# Return handle that watches runtime state doc
return Execution(execution_id, session)
```

The `Execution` handle reads from the runtime state doc:

- `status` → reads `executions/{id}/status`
- `result()` → polls until status is `"done"` or `"error"`, reads outputs
- `stream()` → watches for changes to `outputs/{id}/` and `executions/{id}/status`
- `cancel()` → sets `execution_intents/{id}/cancelled = true` in notebook doc

### Submitting an execution (Frontend)

Same pattern. The React hook writes an intent to the notebook doc CRDT.
The UI reads execution status and outputs from the runtime state doc CRDT.
No WebSocket request/response needed for the execution flow itself.

### Late consumers

No special handling needed. The execution entry and outputs persist in the
runtime state doc until trimmed. Any client can read them at any time.

---

## Migration from the spike

### What stays

- Execution IDs as UUIDs — same concept, same generation.
- `Execution` handle class — same API surface.
- `QueueState` in Python with execution IDs — same shape.
- Frontend reading queue state from CRDT — same pattern.

### What changes

- Parallel lists → map of maps in the CRDT.
- Queue state derived from `executions/` map, not stored as parallel lists.
- Intents written to notebook doc, not sent as WebSocket requests.
- Outputs keyed by execution ID in runtime state doc.
- Completion log replaced by `executions/` map with status field.
- Cancel is a CRDT write, not a protocol message (future).

### Migration path

1. New PR implements the schema above.
2. `ExecuteCell` WebSocket request still works as a fallback — daemon
   treats it as an intent written on behalf of the client.
3. Clients migrate to CRDT-based intent writing over time.
4. Old protocol path removed once all clients are updated.

---

## Open Questions

1. **Ordering of intents**: If two clients write intents simultaneously,
   who goes first? Proposal: daemon uses `requested_at` timestamp as a
   hint, breaks ties by execution ID lexicographic order. Not perfect,
   but deterministic.

2. **Intent cleanup**: When do old intents get removed from the notebook
   doc? Proposal: daemon removes them after the execution reaches a
   terminal state (done or error) and a grace period (e.g. 30 seconds).
   Or: never remove, let them serve as an audit log, trim only on save.

3. **Output size**: Large outputs (images, dataframes) in the runtime
   state doc could be heavy. Consider blob storage with references in
   the CRDT, similar to existing blob store path support.

4. **Multi-kernel**: If a notebook ever supports multiple kernels, the
   intent needs a `kernel_id` field. Not needed now, but the schema
   accommodates it easily (just add a field to the intent map).

5. **Who writes execution_id onto the cell?** The daemon is the natural
   choice — it owns the execution lifecycle. But this means the daemon
   writes to the notebook doc (which is multi-writer). That's already
   true today for outputs. The key constraint: the daemon only writes
   `execution_id` on cells, never `source` or `metadata`. Clients only
   write `execution_id: null` (to clear) and execution intents.

6. **Undo semantics**: If a user clears outputs (sets `execution_id` to
   `null`) and then undoes, should the old execution_id be restored?
   Automerge supports undo natively via history, so this could work for
   free. The outputs would still exist in the runtime state doc. But
   the trim cycle could have removed them — need a grace period or
   reference counting.