---
name: nteract-automerge-protocol
description: Use when designing, reviewing, or changing nteract's Automerge document model, sync state, typed frame protocol, storage boundaries, or future subduction/samod-inspired protocol work. This skill distills patterns from local checkouts of automerge, automerge-repo, and samod's subduction branch for applying Automerge safely in nteract.
---

# nteract Automerge Protocol

Use this with `nteract-notebook-sync` when a change touches Automerge semantics, protocol framing, sync topology, or CRDT/storage ownership. Treat this as a design guardrail before editing; then read the narrower nteract rules for the exact paths you touch.

## Source Map

Upstream references. These are starting points, not the complete corpus. If a task needs more prior art, look for or clone related `automerge/*` and `inkandswitch/*` repositories in the user's normal source checkout area and cite the exact repo/branch inspected in your notes.

- `github.com/automerge/automerge` on `main`
  - Rust core and sync protocol: `rust/automerge/src/sync/*`, `rust/automerge/src/automerge.rs`, `rust/automerge/src/autocommit.rs`
  - JS wrapper behavior and patch semantics: `javascript/src/implementation.ts`, `javascript/src/apply_patches.ts`, `javascript/test/sync_test.ts`
- `github.com/automerge/automerge-repo` on `main`
  - Repo, DocHandle, storage/network boundaries: `packages/automerge-repo/src/Repo.ts`, `DocHandle.ts`, `network/*`, `storage/*`
  - React projection examples: `packages/automerge-repo-react-hooks/src/*`
- `github.com/alexjg/samod` on `subduction`
  - Current samod architecture: `samod-core/src/actors/**`, especially `document/*`, `hub/*`, `network/wire_protocol.rs`
  - Subduction branch design: `samod-core/docs/subduction-design.md`, `subduction-sans-io/src/{engine.rs,messages.rs,batch_sync.rs,storage_coord.rs}`
- nteract local rules:
  - `.claude/rules/crdt-mutations.md`
  - `.claude/rules/protocol.md`
  - `contributing/protocol.md`
  - `contributing/crdt-mutation-guide.md`

## First Questions

Before changing code, answer these in notes or in your head:

1. Which peer owns the state being changed: frontend user action, daemon projection, runtime-state document, pool document, or protocol metadata?
2. Is this persistent notebook content, ephemeral UI/session state, or transport bookkeeping?
3. Does the change require loading the Automerge document, or can it operate on heads, changes, storage metadata, or framed bytes?
4. Which state is per-peer and resettable, and which state is document data that must converge?
5. Will the change still work when sync frames, broadcasts, requests, and runtime-state frames interleave?

## Core Rules

- The Automerge document is the source of truth for notebook content. React stores, hook state, and materialized cell lists are projections.
- Keep exactly one author for each persistent field. Frontend authors user edits and structure changes; daemon authors outputs, execution counts, and runtime-derived projections.
- Do not write to the CRDT in response to a daemon broadcast that mirrors a daemon-authored CRDT change.
- Keep sync state per remote peer. Automerge `sync::State` tracks `shared_heads`, `last_sent_heads`, `their_heads`, `their_need`, `their_have`, `sent_hashes`, `in_flight`, and capabilities; sharing it across peers causes duplicate, missing, or suppressed sync messages.
- Respect Automerge's in-flight behavior. `generate_sync_message` may return `None` while an earlier message is unacknowledged; flushing code must not assume every local mutation immediately yields bytes.
- Prefer narrow transactions and explicit mutation APIs. For async work, fork before the await and merge after; for synchronous blocks use the local fork-and-merge helpers if available.
- Never independently `put_object` into shared structural keys from multiple actors. Concurrent object creation at the same key creates conflicts and can hide child data.
- Use Automerge heads/change hashes for convergence checks, not JSON equality of materialized projections.

## nteract Protocol Shape

nteract's notebook socket is not Automerge Repo's CBOR protocol. It is a runtimed protocol with a magic/version preamble, JSON handshake, and length-prefixed typed frames.

- `AutomergeSync` frame `0x00` carries raw Automerge sync bytes for the notebook document.
- `RuntimeStateSync` frame `0x05` and `PoolStateSync` frame `0x06` carry separate Automerge sync streams.
- Requests, responses, broadcasts, presence, and session-control frames can interleave with sync frames.
- The Tauri relay must stay a byte pipe. It should not maintain a second Automerge replica or generate sync messages, because that creates a dual-sync peer on one daemon connection.
- Protocol changes require Rust and TypeScript contract updates together. Check `crates/notebook-wire`, `crates/notebook-protocol`, and `packages/runtimed`.
- Frame readers must be cancel-safe and keep draining under pressure. Avoid command paths that block on a private receive loop while the main frame loop starves.

## Lessons From Automerge

- Separate document changes from sync negotiation. Automerge stores document history in changes; sync messages are a peer-to-peer negotiation over which changes are missing.
- Persist document bytes/changes separately from sync state. Persisting encoded sync state may be useful only when the same peer identity reconnects; it is not document truth.
- Empty messages can be meaningful. A peer may need to advertise heads or capabilities even when it has no changes.
- Read-only sync is a protocol mode, not a local permission check. Switching modes may require resetting sync state so ignored changes are resent.
- Patches are projections from head ranges. They are useful for UI updates, but persistent correctness comes from applying changes and comparing heads.

## Lessons From Automerge Repo

- `Repo` owns discovery, storage, network adapters, and share policy; `DocHandle` is the mutation and event surface for one document.
- Network adapters are pluggable message transports. Do not let transport code learn document internals beyond document IDs, peer IDs, and encoded payloads.
- Storage adapters persist document data independently of network reachability. Local load, remote find, and unavailable states are distinct states.
- `DocHandle.change` synchronously captures a mutation, updates heads, and emits document/patch events. nteract's WASM handle and materializers should preserve the same mental model: mutate first, project second, sync as a side effect.
- Default share-all behavior in Automerge Repo is convenient but dangerous for nteract authority boundaries. Runtimed socket channels expose same-UID authority; add explicit capability checks when a new channel has stronger powers.

## Lessons From samod and subduction

- samod's strongest pattern is sans-IO state machines: pure protocol engines accept input events and return IO/signing/storage actions for the runtime to execute.
- Keep protocol choice per connection. The `subduction` branch runs Automerge sync and Subduction as separate connection protocols, with no multiplexing on one wire stream.
- Put metadata-only sync in the hub, not per-document actors, when it does not require loading the Automerge document. This avoids expensive document loads on the hot connection path.
- Use a thin adapter between app-specific IO types and protocol-engine types. `samod-core/src/actors/hub/subduction_sync.rs` is a useful model: convert IDs, schedule storage/signing work, and feed completions back to the engine.
- Storage coordination should track issued operation IDs and correlate completions. Avoid hidden async tasks that mutate protocol state behind the engine's back.
- Subduction's useful idea for nteract: sync can be split into metadata/fingerprint exchange plus blob transfer, so future large-notebook sync should not assume the full Automerge doc is always loaded.
- Subduction's current limitation matters: it does not replace Automerge Repo's JS interop or transitive request forwarding. Keep fallback or compatibility paths explicit.

## Design Patterns For nteract

- For a local user edit: mutate the WASM/NotebookDoc CRDT, materialize for immediate UI, schedule or flush sync, then let daemon confirmation converge normally.
- For daemon output/runtime updates: daemon writes its authoritative doc/state, frontend receives sync, then materializes. Broadcasts may update ephemeral UI but must not re-author persistent fields.
- For new protocol metadata: keep it outside the notebook document unless users need it persisted as notebook content.
- For large binary or output payloads: store blobs/manifests out-of-line and sync stable identifiers through Automerge; validate MIME classification on both Rust and TS sides.
- For a new sync stream: allocate a distinct frame type, per-peer sync state, readiness/status handling, and focused protocol contract tests.
- For reconnect: reset transport state, preserve document truth, and be deliberate about whether per-peer sync state should survive.

## Validation Checklist

Run the smallest tests that exercise the touched layer:

- CRDT schema or notebook-doc mutation: focused `notebook-doc`, `runtimed-wasm`, or materialization tests.
- Frame bytes, handshakes, or request/response variants: `cargo test -p notebook-protocol` plus relevant `packages/runtimed` tests.
- Frontend sync/materialization: targeted Vitest tests under `apps/notebook` or `packages/runtimed`.
- Daemon sync or relay behavior: use `nteract-daemon-dev` and `nteract-testing`; prefer the per-worktree dev daemon.
- Mutex/async Rust paths: ensure no `tokio::sync` guard crosses `.await`; run the focused lint if runtimed code changed.

When in doubt, add a convergence test that creates two peers, applies concurrent changes, exchanges sync frames until quiescent, and asserts equal heads plus expected materialized state.
