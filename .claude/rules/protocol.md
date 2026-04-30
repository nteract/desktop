---
paths:
  - crates/notebook-wire/**
  - crates/notebook-doc/**
  - crates/notebook-protocol/**
  - crates/notebook-sync/**
  - crates/runtime-doc/**
  - crates/runtimed/src/notebook_sync_server/**
  - crates/runtimed/src/requests/**
  - packages/runtimed/src/transport.ts
  - packages/runtimed/src/protocol-contract.ts
  - packages/runtimed/src/request-types.ts
  - apps/notebook/src/lib/frame-pipeline.ts
  - apps/notebook/src/lib/notebook-frame-bus.ts
---

# Wire Protocol

Canonical protocol documentation lives in `contributing/protocol.md`. Keep this
rule file short: it is loaded often, and the contributing guide is the source of
truth for frame bytes, handshakes, request/response shapes, RuntimeStateDoc, and
frontend transport flow.

## Invariants

- `notebook-wire` owns frame bytes, preamble constants, frame caps, typed-frame
  enum values, and session-control status shapes.
- `notebook-protocol` owns handshakes and JSON wire types:
  `NotebookRequest`, `NotebookResponse`, `NotebookBroadcast`, and the
  runtime-agent request/response envelopes.
- `notebook-doc` owns the NotebookDoc Automerge schema. Bump
  `SCHEMA_VERSION` only with a migration plan that preserves real user data.
- `runtime-doc` owns runtime-state schema. Kernel lifecycle, queue, outputs,
  env, trust, project, path, and save state are daemon/runtime-agent authored;
  widget comm state under `comms/` has an intentional frontend write path via
  the approved comm CRDT writer.
- Protocol version and NotebookDoc schema version are independent integers.
  Bump `PROTOCOL_VERSION` for breaking framing, handshake, or serialization
  changes; bump `SCHEMA_VERSION` for document schema changes.
- Every connection starts with the 5-byte magic/version preamble. `Pool` remains
  version-tolerant for daemon upgrade probes; notebook/runtime channels require
  `MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION`.
- Request/response frames use `NotebookRequestEnvelope` and
  `NotebookResponseEnvelope`; overlapping requests must route responses by id.
- Runtime state, outputs, queue, kernel lifecycle, trust, env drift, env
  progress snapshots, path, save state, and widget state belong in
  `RuntimeStateDoc`, not room-wide state broadcasts. Broadcasts are for
  ephemeral comm messages and high-frequency env progress events.
- Steady-state frame readers must keep draining. Register waiters/pending
  requests instead of blocking inside command paths.
- The runtimed socket is same-UID trusted, not app-private. New handshake
  variants must account for any same-user process holding the socket path.

## When Editing

- Update Rust protocol types and generated `packages/runtimed` TypeScript
  surfaces in the same patch.
- Run `cargo test -p notebook-protocol` for protocol-surface changes. Add the
  focused package tests when TypeScript transport or generated contracts move.
- Read `contributing/protocol.md` before making changes that affect sync,
  RuntimeStateDoc, frame handling, request routing, or socket authority.
