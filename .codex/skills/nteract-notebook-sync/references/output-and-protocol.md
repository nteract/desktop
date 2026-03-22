# Output and Protocol

## Frame and protocol changes

When changing frame handling, keep the following aligned:

- `crates/notebook-doc/src/frame_types.rs`
- `apps/notebook/src/lib/frame-types.ts`
- Any relay or sync code that assumes a specific frame layout

When changing the wire handshake or typed frame semantics, also inspect:

- `crates/notebook-protocol/src/connection.rs`
- `crates/notebook-protocol/src/protocol.rs`
- `contributing/protocol.md`

## MIME classification contract

Keep these implementations in sync when changing text-vs-binary rules:

- `crates/runtimed/src/output_store.rs`
- `crates/runtimed-py/src/output_resolver.rs`
- `apps/notebook/src/lib/manifest-resolution.ts`

Important rule: `image/svg+xml` is text, not binary.

## Output pipeline

The daemon stores manifests and blobs; the CRDT stores only manifest hashes. Frontend and Python consumers resolve through the manifest layer rather than treating output payloads as inline notebook state.

## Common pitfalls

- Storing base64 text for binary blobs instead of raw bytes
- Reading binary blobs as UTF-8 strings
- Forgetting to update both frontend and Python MIME classification paths
- Changing sync reply or retry behavior without testing failed-delivery cases
