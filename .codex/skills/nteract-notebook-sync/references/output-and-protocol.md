# Output and Protocol

## Frame and protocol changes

When changing frame handling, keep the following aligned:

- `crates/notebook-doc/src/frame_types.rs`
- `packages/runtimed/src/transport.ts`
- Any relay or sync code that assumes a specific frame layout

When changing the wire handshake or typed frame semantics, also inspect:

- `crates/notebook-protocol/src/connection.rs` (public re-export facade)
- `crates/notebook-protocol/src/connection/framing.rs` (preamble, typed frames, frame caps)
- `crates/notebook-protocol/src/connection/handshake.rs` (handshake, capabilities, connection info)
- `crates/notebook-protocol/src/connection/env.rs` (launch spec and env metadata types)
- `crates/notebook-protocol/src/protocol.rs`
- `contributing/protocol.md`

## Connection compatibility invariants

- Every daemon connection sends the 5-byte magic preamble before the JSON
  handshake. There is no no-preamble fallback.
- The Pool channel is the only version-tolerant channel. It accepts older
  preamble versions so stable apps from previous releases can ping the daemon
  during upgrade and read `protocol_version` / `daemon_version` from `Pong`.
- Notebook, runtime, settings, blob, and open/create channels reject versions
  outside `MIN_PROTOCOL_VERSION..=PROTOCOL_VERSION`.
- When bumping `PROTOCOL_VERSION`, keep the old-stable Pool ping test aligned
  with the launcher semantics instead of downloading an old daemon binary.

## Frame pump invariants

Frame readers are only half the fix. A dedicated reader prevents cancel-unsafe
partial reads, but the consumer must also stay hot enough to drain the bounded
frame queue.

- Confirmation waits must be waiter-based: register the target heads, send any
  immediate sync frame, and let normal inbound `AutomergeSync` handling resolve
  the waiter.
- Request waits must be pending-map based: every overlapping request needs a
  correlation id, and responses must route by id instead of by "next response
  wins".
- Do not put `recv()` loops inside command handlers. Long waits in command code
  starve broadcasts, state sync, session-control frames, and sync replies.
- Bounded frame queues still backpressure the socket when the consumer is
  blocked. Treat any per-command sleep, timeout loop, or "drain a few frames"
  helper in the sync task as a potential session-drop bug.
- Broadcasts and RuntimeStateSync frames can arrive while requests and confirms
  are pending. Route them through the main frame handler instead of dropping or
  deferring them.

## MIME classification contract

Single canonical Rust implementation in `crates/notebook-doc/src/mime.rs` (`is_binary_mime()`, `mime_kind()`, `MimeKind` enum). All Rust crates use this module. The old per-crate copies have been deleted. WASM owns MIME classification end-to-end. A `looksLikeBinaryMime()` safety net remains in `manifest-resolution.ts` for blob refs that WASM couldn't resolve — it is not an authoritative copy.

Important rule: `image/svg+xml` is text, not binary.

## Output pipeline

The daemon stores content in the blob store; output manifests are inline Automerge Maps in RuntimeStateDoc. MIME types and sizes are readable directly from the CRDT without any blob fetch. Frontend and Python consumers resolve ContentRefs through the manifest layer — Inline for ≤1KB, Blob for >1KB.

## Common pitfalls

- Storing base64 text for binary blobs instead of raw bytes
- Reading binary blobs as UTF-8 strings
- Adding MIME classification outside `crates/notebook-doc/src/mime.rs` — there is one canonical implementation
- Changing sync reply or retry behavior without testing failed-delivery cases
