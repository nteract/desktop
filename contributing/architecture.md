# Runtime Architecture Principles

This document defines the core architectural principles for the runtimed daemon and notebook system. These principles guide design decisions and help maintain consistency as the codebase evolves.

## Principles

### 1. Daemon as Source of Truth

The runtimed daemon owns all runtime state. Clients (UI, agents, CLI) are views into daemon state, not independent state holders.

**Implications:**
- Clients subscribe to daemon state, they don't maintain parallel copies
- State changes flow through the daemon, not peer-to-peer between clients
- If the daemon restarts, clients reconnect and resync

### 2. Automerge Document as Canonical Notebook State

The automerge document is the source of truth for notebook content: cells, their sources, metadata, and structure. All clients sync to this shared document.

**Implications:**
- Cell source code lives in the automerge doc
- To execute a cell: write it to the doc first, then request execution by cell_id
- Multiple clients editing the same notebook see each other's changes in real-time
- The daemon reads from the doc when executing, never from ad-hoc request parameters

### 3. On-Disk Notebook as Checkpoint

The `.ipynb` file on disk is a checkpoint/snapshot that the daemon periodically saves. It is not the live state.

**Implications:**
- Daemon reads `.ipynb` on first open, loads into automerge doc
- Daemon writes `.ipynb` on explicit save or auto-save intervals
- Unknown metadata keys in `.ipynb` are preserved through round-trips
- Crash recovery: last checkpoint + automerge doc replay

### 4. Local-First Editing, Synced Execution

Editing is local-first for responsiveness. Execution is always against synced state.

**Implications:**
- Type freely in cells; automerge handles sync and conflict resolution
- When you run a cell, you execute what's in the synced document
- No executing code that differs from the document state
- If a cell is mid-sync, wait for sync before allowing execution

### 5. Binary Separation via Manifests

Cell outputs are stored as content-addressed blobs with manifest references. This keeps large binary data (images, plots) out of the sync protocol.

**Implications:**
- Output broadcasts contain blob hashes, not inline data
- Clients resolve blobs from the blob store (disk or HTTP)
- Manifest format allows lazy loading and deduplication
- Large outputs don't block document sync

### 6. Daemon Manages Runtime Resources

The daemon owns kernel lifecycle, environment pools, and tooling (ruff, deno, etc.).

**Implications:**
- Clients request kernel launch; they don't spawn kernels directly
- Environment selection is the daemon's decision based on notebook metadata
- Tool availability is the daemon's responsibility (bootstrap via rattler if needed)
- Clients are stateless with respect to runtime resources

## Anti-Pattern: Bypassing the Document

The principle of "automerge as canonical state" is violated when execution requests include code directly instead of reading from the document.

**Correct flow:**
```
Client                              Daemon
  |                                   |
  |-- [sync: update cell source] ---->|
  |<-- [sync: ack] -------------------|
  |                                   |
  |-- ExecuteCell { cell_id } ------->|  // No code parameter
  |<-- CellQueued --------------------|
  |                                   |
  |<-- ExecutionStarted --------------|
  |<-- Output -------------------------|
  |<-- ExecutionDone -----------------|
```

**Incorrect flow (anti-pattern):**
```
Client                              Daemon
  |                                   |
  |-- QueueCell { cell_id, code } --->|  // Code passed directly!
  |<-- CellQueued --------------------|
  |                                   |
  // Other clients don't see the code
  // Document and execution are out of sync
```

## Testing Philosophy

- **E2E tests** (Playwright): Slow but comprehensive, test full user journeys
- **Integration tests** (Python bindings): Fast daemon interaction tests via `runtimed-py`
- **Unit tests**: Pure logic, no I/O, fast feedback

Preference: Fast integration tests over slow E2E where possible. Use E2E for critical user journeys, integration tests for daemon behavior, unit tests for algorithms.

## Conformance Status

We are working toward full conformance with these principles.

| Principle | Status |
|-----------|--------|
| Daemon as source of truth | Conformant |
| Automerge as canonical state | Partial |
| On-disk as checkpoint | Conformant |
| Local-first editing, synced execution | Partial |
| Binary separation | Conformant |
| Daemon manages resources | Conformant |

**In progress:** Adding `ExecuteCell` request that reads from the document instead of accepting code as a parameter. This will bring us to full conformance with principles 2 and 4.

## References

- `crates/runtimed/src/protocol.rs` - Request/response types
- `crates/runtimed/src/notebook_doc.rs` - Automerge document operations
- `crates/runtimed/src/notebook_sync_server.rs` - Sync protocol handling
- `crates/runtimed/src/kernel_manager.rs` - Kernel lifecycle
