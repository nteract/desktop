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

Single canonical Rust implementation in `crates/notebook-doc/src/mime.rs` (`is_binary_mime()`, `mime_kind()`, `MimeKind` enum). All Rust crates use this module. The old per-crate copies have been deleted. WASM owns MIME classification end-to-end. A `looksLikeBinaryMime()` safety net remains in `manifest-resolution.ts` for blob refs that WASM couldn't resolve — it is not an authoritative copy.

Important rule: `image/svg+xml` is text, not binary.

## Output pipeline

The daemon stores content in the blob store; output manifests are inline Automerge Maps in RuntimeStateDoc. MIME types and sizes are readable directly from the CRDT without any blob fetch. Frontend and Python consumers resolve ContentRefs through the manifest layer — Inline for ≤1KB, Blob for >1KB.

## Common pitfalls

- Storing base64 text for binary blobs instead of raw bytes
- Reading binary blobs as UTF-8 strings
- Adding MIME classification outside `crates/notebook-doc/src/mime.rs` — there is one canonical implementation
- Changing sync reply or retry behavior without testing failed-delivery cases
