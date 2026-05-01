---
name: execution-pipeline
description: >
  The end-to-end cell execution pipeline from MCP tool call through
  daemon to kernel and back. Use when debugging execution failures,
  understanding output timing, investigating why outputs are missing
  or stale, or modifying the execute/run-all flow. Covers
  required_heads, CellQueued, RuntimeStateDoc polling, output-sync
  grace, and output resolution.
---

# Cell Execution Pipeline

Use this skill when debugging execution-related issues: cells that
don't execute, outputs that don't appear, execution that times out,
or MCP tools that return empty results. This traces the full path
from tool invocation to resolved outputs.

## The Five Stages

### Stage 1: Causal Precondition (required_heads)

Before sending an execute request, the client captures the current
Automerge heads of the notebook document:

```rust
let required_heads = handle.current_heads_hex()?;
```

These heads are attached to the request envelope. The daemon's
`wait_for_required_heads()` defers processing until all listed change
hashes exist in its copy of the notebook document (checked via
`get_change_by_hash` — a ChangeGraph containment check).

**Why this matters:** Without `required_heads`, the daemon might
execute against stale cell source — the client wrote "x = 1" but the
daemon hasn't received that sync frame yet, so it executes the old
"x = 0".

**Timeout:** 10 seconds. If heads don't arrive, the daemon logs a
warning and processes anyway (graceful degradation).

**Frontend optimization:** Before capturing heads, the frontend calls
`flushSync()` to push any pending source edits into the sync stream,
minimizing the daemon-side wait.

### Stage 2: Request Submission

The client sends an `ExecuteCell` or batch `RunAllCells` request:

```rust
let request = NotebookRequest::ExecuteCell {
    cell_id: cell_id.to_string(),
};
let response = handle.send_request_after_heads(request, required_heads).await;
```

`send_request_after_heads` wraps the request in a
`NotebookRequestEnvelope` with the captured heads and sends it through
the sync task.

### Stage 3: Daemon Queuing

The daemon receives the request, waits for required heads (Stage 1),
reads the cell source from its synced notebook document, and queues
execution with the kernel:

1. Daemon reads source from its `NotebookDoc` (the CRDT, not the
   `.ipynb` file)
2. Creates a unique `execution_id`
3. Writes to `RuntimeStateDoc`: execution entry with status `"queued"`,
   then `"running"` when the kernel starts
4. Responds with `CellQueued { execution_id, position }` immediately
5. Forwards code to the kernel via ZMQ `execute_request`

**Key invariant:** The daemon writes `set_execution_done(eid, success)`
to RuntimeStateDoc ONLY AFTER all output manifests for that execution
are committed. This ordering guarantee is what makes RuntimeStateDoc
polling reliable.

### Stage 4: Terminal Wait (RuntimeStateDoc Polling)

The client polls RuntimeStateDoc for execution completion:

```rust
await_execution_terminal(handle, &execution_id, timeout, None).await
```

**Phase 1 — Terminal status poll:**
- Polls every 50ms
- Checks `executions[eid].status` for `"done"` or `"error"`
- Also watches for kernel-level failure (`kernel.lifecycle == Error|Shutdown`)
- Returns `KernelFailed` if the kernel dies while execution is pending

**Phase 2 — Output-sync grace:**
- After terminal status is reached, the output list might still be
  empty on the client's replica (sync lag)
- Polls every 10ms for up to 500ms (the grace period)
- Exits as soon as outputs appear

**Why RuntimeStateDoc, not broadcasts:** The `ExecutionDone` broadcast
arrives over a separate channel and the client's Automerge replica may
not have caught up on the final stream writes. The RuntimeStateDoc is
authoritative — once status is `"done"`, outputs are guaranteed to be
in the same document.

### Stage 5: Output Resolution

Outputs are inline manifest Maps in RuntimeStateDoc, containing
`ContentRef` entries per MIME type:

```rust
let outputs = output_resolver::resolve_cell_outputs_for_llm(&output_manifests, ctx).await;
```

Resolution depends on MIME type:
- **Text MIME** (`text/*`, `application/json`, `image/svg+xml`):
  Inline string if ≤1KB, or fetch from blob store as UTF-8
- **Binary MIME** (`image/png`, `audio/*`, etc.):
  Always blob store. Frontend gets `http://` URL. Python gets raw bytes.
- **Widget output** (`application/vnd.jupyter.widget-view+json`):
  References comm state in RuntimeStateDoc

MCP execution paths use **preview mode** — output is truncated for
LLM consumption. Agents that need full output call
`get_cell(full_output=true)` separately.

## Execution ID Lifecycle

```
Client sends ExecuteCell
  → Daemon returns CellQueued { execution_id: "exec-abc" }
  → RuntimeStateDoc: executions["exec-abc"] = { status: "queued" }
  → Kernel starts: executions["exec-abc"].status = "running"
  → Outputs arrive: executions["exec-abc"].outputs = [manifest1, ...]
  → Kernel done: executions["exec-abc"] = { status: "done", success: true }
  → Client reads outputs from executions["exec-abc"].outputs
```

The `execution_id` is the stable reference for one execution attempt.
If the same cell is executed twice, each gets a different
`execution_id`. Agents can pass `execution_id` to `get_cell()` to
read outputs for a specific execution rather than the cell's current
outputs.

## Run-All Flow

`run_all_cells` follows the same pipeline but batched:

1. Captures `required_heads` once
2. Sends `RunAllCells` request with all cell IDs
3. Daemon returns `AllCellsQueued { cell_execution_ids }` — a map of
   `cell_id → execution_id` for every queued cell
4. Client polls each `execution_id` in parallel with a shared deadline
5. Returns per-cell results

**Timeout:** The shared deadline applies to the entire run, not per-cell.
If one cell takes 90% of the budget, remaining cells get less time.

## Common Failure Modes

### "Outputs are empty"

1. **Output-sync grace too short:** The execution finished but outputs
   haven't synced yet. The 500ms grace usually suffices, but very large
   outputs (big DataFrames, many plots) may need more time.
2. **execution_id mismatch:** Reading outputs with the wrong
   `execution_id` or reading the cell's "current" outputs after
   re-execution replaced them.
3. **Blob store unreachable:** Binary outputs reference blobs. If the
   blob HTTP server is down, resolution fails silently.

### "Cell didn't execute"

1. **required_heads timeout:** Daemon waited 10s for heads that never
   arrived. Check if the sync stream is healthy.
2. **Kernel not ready:** The kernel isn't started or is in error state.
   The daemon returns an error response, not `CellQueued`.
3. **Trust gate:** Untrusted notebooks may block execution pending
   approval.

### "Execution timed out"

1. **Long-running cell:** The cell genuinely takes longer than the
   timeout (default varies by caller — MCP uses 120s).
2. **Kernel hung:** The kernel process is alive but not responding.
   Check `kernel.lifecycle` in RuntimeStateDoc.
3. **Sync stall:** Terminal status was written by the daemon but the
   client's sync stream stopped delivering frames. Check daemon logs
   for sync errors.

### "Stale outputs from previous execution"

The execution completed but the outputs visible belong to an earlier
run. This happens when:
1. Reading cell outputs without using the `execution_id` — the cell's
   "current" pointer may not have been updated yet
2. The RuntimeStateDoc sync hasn't delivered the latest writes

Fix: Always use `execution_id` from the `CellQueued` response to
read outputs for a specific execution.

## Two Document Architecture

Execution spans both Automerge documents in a notebook room:

| Document | What it holds for execution |
|----------|---------------------------|
| **NotebookDoc** | Cell source code (what to execute) |
| **RuntimeStateDoc** | Execution lifecycle, outputs, kernel status |

The `required_heads` gate ensures NotebookDoc is synced before
execution starts. RuntimeStateDoc polling ensures outputs are
available before the client reads them. Both sync streams run
concurrently on the same socket connection.

## Key Source Files

| File | Role |
|------|------|
| `crates/runt-mcp/src/execution.rs` | `execute_and_wait`, `run_all_and_wait` — MCP entry points |
| `crates/notebook-sync/src/execution_wait.rs` | `await_execution_terminal` — shared two-phase polling |
| `crates/notebook-sync/src/handle.rs` | `send_request_after_heads`, `current_heads_hex` |
| `crates/runtimed/src/notebook_sync_server/peer_writer.rs` | `wait_for_required_heads` — daemon-side causal gate |
| `crates/runtimed/src/runtime_agent/` | Kernel management, output routing, `set_execution_done` |
| `crates/runtime-doc/src/doc.rs` | RuntimeStateDoc schema — executions map, kernel status |
| `crates/runtimed-client/src/output_resolver.rs` | `resolve_cell_outputs_for_llm` — manifest → Output |
| `crates/notebook-doc/src/mime.rs` | MIME classification (text vs binary) |
| `packages/runtimed/src/notebook-client.ts` | Frontend `executeCell` with `getRequiredHeads` callback |
