---
name: architecture-review
description: >
  Clean-room architecture review of the nteract desktop codebase. Use when you
  want a first-principles analysis of how the system *should* be structured,
  compared against how it *is* structured, to surface simplification and
  improvement opportunities.
---

# Clean-Room Architecture Review

You are a senior systems architect performing a clean-room architecture review of
a Jupyter-compatible notebook system. Your job is NOT to rubber-stamp what exists.
Your job is to design from first principles, then compare against reality, and
surface the gaps.

## The Product

A desktop notebook application (think Jupyter, but native) with:
- Real-time multi-window editing of the same notebook
- Instant kernel startup via prewarmed environment pools
- Secure execution of untrusted notebook outputs
- Python and Deno kernel support
- Agent/MCP integration for programmatic notebook interaction
- Offline-first editing with eventual consistency

## Hard Constraints (Do Not Redesign These)

These are settled decisions. Treat them as given:

1. **Automerge CRDT** for document sync. Not negotiable — the local-first,
   multi-peer sync model is core to the product.
2. **Tauri** as the desktop shell (Rust backend + webview frontend).
3. **Jupyter wire protocol** compatibility — kernels speak ZMQ, outputs follow
   nbformat MIME bundle conventions.
4. **ipywidgets / anywidget** support via Jupyter comm protocol.
5. **Iframe sandbox isolation** for untrusted output rendering (no
   `allow-same-origin`, blob URL origin isolation).
6. **Content-addressed blob store** for outputs (binary data stays out of the CRDT).
7. **Rust daemon** as the stateful backend (not an in-process library).
8. **Python bindings (PyO3/maturin)** for the SDK and convenience wrappers around the runtime tooling.

## What Currently Exists

The system is split into 16 Rust crates, a React/TypeScript frontend, and 3
Python packages. The major architectural seams are:

### Daemon ↔ Client Protocol
- Unix socket, length-prefixed typed frames (7 frame types)
- Preamble (magic + version) → JSON handshake → Automerge initial sync → steady state
- Frame types: AutomergeSync (0x00), Request (0x01), Response (0x02),
  Broadcast (0x03), Presence (0x04), RuntimeStateSync (0x05), PoolStateSync (0x06)
- Requests are fire-and-forget style (ExecuteCell, LaunchKernel, etc.)
- Broadcasts push state changes to all connected clients

### Document Model
- Automerge doc is the live notebook state; `.ipynb` on disk is a checkpoint
- Two peers: frontend WASM peer (local-first edits) and daemon peer (execution, outputs)
- Cell outputs stored as manifest hashes (content refs → blob store)
- Separate RuntimeStateDoc (frame 0x05) for kernel status, queue, env sync, trust
- Per-cell O(1) accessors in WASM, Rust, and Python (three implementations)
- CellChangeset for incremental field-level diffs (source, outputs, metadata, position...)

### CRDT Ownership Rules
- Frontend WASM writes: cell source, position, type, metadata, notebook metadata
- Daemon writes: cell outputs (manifest hashes), execution count, RuntimeStateDoc
- Rule: never write to CRDT in response to a daemon broadcast (daemon already wrote)

### Crate Boundaries
- `notebook-doc`: Automerge schema, cell CRUD, output writes, per-cell accessors, diffing
- `notebook-protocol`: Wire types (Request, Response, Broadcast)
- `notebook-sync`: DocHandle, sync infrastructure, Python-side per-cell accessors
- `runtimed`: Central daemon (kernel lifecycle, pools, notebook rooms, blob store, autosave)
- `runtimed-wasm`: WASM bindings for frontend (cell mutations, sync, accessors)
- `runtimed-py`: Python bindings (PyO3) for SDK/MCP
- `kernel-launch`: Tool bootstrapping (deno, uv, ruff via rattler)
- `kernel-env`: UV/Conda venv creation with progress reporting
- `runt-trust`: HMAC-SHA256 notebook trust
- `runt-workspace`: Per-worktree daemon socket isolation
- `runt`: CLI binary (daemon management, kernel control, notebook launching, MCP server)
- `runtimed-client`: Shared client library (output resolution, daemon paths, pool client)
- `runt-mcp`: Rust-native MCP server (26 tools for notebook interaction)
- `mcp-supervisor`: `nteract-dev` MCP server (proxies `runt mcp`, manages daemon/vite lifecycle)
- `repr-llm`: LLM-friendly text summaries of visualization specs
- `notebook`: Tauri desktop app (main GUI, bundles daemon+CLI as sidecars)

### Frontend
- React + CodeMirror, split cell store (per-cell subscriptions)
- Materialization pipeline: WASM CellChangeset → coalesce (32ms) → per-cell or structural update
- Manifest resolution: blob hashes → HTTP fetch from daemon blob server
- Widget rendering in isolated iframes via CommBridgeManager + postMessage

### Environment Management
- Detection chain: inline deps → project file walk-up → prewarmed pool
- Content-addressed env caching (SHA-256 of deps)
- Prewarmed pool: 3 UV + 3 Conda envs, warmed every 30s, max 2-day age

### Invariants That Bite
- `is_binary_mime()` has one canonical Rust implementation in `notebook-doc::mime` — the single source of truth. All Rust crates (`runtimed`, `runtimed-client`, `runtimed-wasm`) use this module. On the TypeScript side, `looksLikeBinaryMime()` in `manifest-resolution.ts` exists only as a safety net for blob refs that WASM couldn't resolve — it is not an authoritative copy.
- Iframe must never get `allow-same-origin`
- Per-cell accessors must stay in sync across WASM, Rust, and Python

## Your Review Process

### Phase 1: First-Principles Design (Think Before Looking)

Given the hard constraints and product requirements above, sketch how YOU would
architect this system. Focus on:

1. **State ownership**: Who owns what state? How does it flow?
2. **Crate/module boundaries**: Where would you draw the lines? What's the
   dependency graph?
3. **Protocol design**: How would clients talk to the daemon? What abstractions?
4. **Output pipeline**: How would kernel outputs flow from ZMQ → storage → display?
5. **Environment lifecycle**: How would you manage kernel environments?
6. **Frontend data flow**: How would CRDT state become React state?
7. **Security boundaries**: How would you isolate untrusted content?

Write this design BEFORE deeply reading the implementation. Use the summary above
as your starting point, but think independently about what the ideal structure
looks like.

### Phase 2: Gap Analysis

Now read the actual implementation. For each subsystem, compare your
first-principles design against reality. Look for:

**Unnecessary complexity:**
- Are there abstractions that don't earn their keep?
- Are there indirections that could be collapsed?
- Are there crate boundaries that create friction without providing value?

**Missing abstractions:**
- Are there repeated patterns that should be unified?
- Are there concepts that are implicit but should be explicit?

**Ownership confusion:**
- Is state ownership clear at every level?
- Are there places where the same concept is owned/defined in multiple places
  unnecessarily? (The `is_binary_mime` three-way sync is a known example — are
  there others?)

**Protocol awkwardness:**
- Does the frame protocol have the right set of frame types?
- Are request/response/broadcast the right categories?
- Is the handshake sequence pulling its weight?

**Dependency graph health:**
- Do crates depend on the right things?
- Are there circular-ish dependencies or unnecessary coupling?
- Could anything be collapsed or split to improve clarity?

**Frontend architecture:**
- Is the materialization pipeline (WASM → coalesce → React) the simplest path?
- Is the split between shared (`src/`) and app-specific (`apps/notebook/src/`)
  well-drawn?
- Are hooks doing the right amount of work?

### Phase 3: Recommendations

For each finding, provide:
1. **What you'd change** — concrete, specific
2. **Why** — what problem it solves or what it simplifies
3. **Risk/effort** — is this a quick win or a deep refactor?
4. **What you'd leave alone** — explicitly call out things that ARE well-designed

Organize recommendations into:
- **Quick wins**: Low-risk simplifications, API cleanups, naming improvements
- **Medium refactors**: Crate boundary adjustments, protocol changes, abstraction
  improvements
- **Deep redesigns**: Fundamental structural changes (only if strongly justified)

## Key Files to Read

Start with these to understand the actual implementation:

**Architecture docs:**
- `contributing/architecture.md`
- `contributing/frontend-architecture.md`
- `contributing/protocol.md`
- `contributing/runtimed.md`

**Core daemon:**
- `crates/runtimed/src/daemon.rs`
- `crates/runtimed/src/notebook_sync_server.rs`
- `crates/runtimed/src/kernel_manager.rs`
- `crates/runtimed/src/output_store.rs`
- `crates/runtimed/src/blob_store.rs`

**Document model:**
- `crates/notebook-doc/src/lib.rs`
- `crates/notebook-doc/src/diff.rs`
- `crates/notebook-protocol/src/protocol.rs`
- `crates/notebook-protocol/src/connection.rs`

**WASM bindings:**
- `crates/runtimed-wasm/src/lib.rs`

**Frontend core:**
- `apps/notebook/src/hooks/useAutomergeNotebook.ts`
- `apps/notebook/src/hooks/useDaemonKernel.ts`
- `apps/notebook/src/lib/materialize-cells.ts`
- `apps/notebook/src/lib/notebook-cells.ts`
- `apps/notebook/src/lib/manifest-resolution.ts`

**Isolation & widgets:**
- `src/components/isolated/isolated-frame.tsx`
- `src/components/isolated/comm-bridge-manager.ts`
- `src/components/widgets/`

**Python bindings:**
- `crates/runtimed-py/src/lib.rs`
- `crates/notebook-sync/src/handle.rs`

**Environment management:**
- `crates/kernel-env/src/`
- `crates/kernel-launch/src/`

## What Makes a Good Review

- **Be opinionated.** "It's fine" is not useful. If it's fine, say WHY it's the
  right design and what alternatives you considered.
- **Be concrete.** "The crate boundaries could be better" is useless. Say which
  crate should absorb which, and why.
- **Respect the constraints.** Don't suggest switching from Automerge to
  something else, or from Tauri to Electron. Work within the givens.
- **Think about the humans.** A crate boundary that makes the dependency graph
  pretty but confuses every new contributor is a bad boundary.
- **Consider the 80/20.** Some architectural debt is cheap to carry. Focus on
  the things that actually slow people down or cause bugs.
