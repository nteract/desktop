# PutBlob typed frame

**Status:** Draft
**Date:** 2026-04-30
**Related:** #1814 (PutBlob frame), #1334 (SSH remote runtimes), #1817 (`dx.attach(path)`), #2284 (URI scheme / daemons as peers), [Securing Notebooks](https://www.nteract.io/blog/security)

## Motivation

nteract has two local daemon surfaces by design:

- **Control:** owner-only Unix socket / Windows named pipe.
- **Data reads:** GET-only localhost HTTP blob server.

That split is part of the public security story. The socket is the authenticated
control plane; the HTTP blob server exists so browser and opaque-origin iframe
renderers can fetch content-addressed bytes. It must stay read-only.

Promoting notebook attachments into the document schema exposed the missing
write-side primitive. Frontend drag/drop, `dx.attach(path)`, runtime-agent blob
uploads, and remote daemon peers all need a way to put bytes into the daemon
blob store without:

- inventing feature-specific upload APIs,
- putting binary bytes into Automerge changes,
- base64-expanding binary data through JSON request frames,
- or opening an HTTP write surface.

`PutBlob` is the socket-native upload primitive. It adds one typed binary frame
to the existing ordered notebook wire and keeps Automerge payloads opaque.

## Goals

1. Add a general, authenticated blob-write primitive over the existing socket.
2. Keep the localhost HTTP blob server GET-only.
3. Preserve the CRDT invariant: Automerge documents store intent, metadata, and
   blob refs; blob bytes live outside Automerge.
4. Align with Automerge and samod-style transport boundaries: one ordered byte
   stream, explicit control/data envelopes, opaque Automerge payloads.
5. Support small one-shot uploads for image attachments without blocking larger
   protocol work.
6. Define multipart-style semantics now so large files do not force a later
   incompatible redesign.

## Non-goals

- Replacing Automerge sync with automerge-repo or samod.
- Adding HTTP POST/PUT to the blob server.
- Solving cloud/browser auth. Future HTTPS transports need their own bearer
  token / CORS model.
- Making the blob store mutable. Blobs remain content-addressed and immutable.
- Implementing resumable cross-process uploads in the first patch.
- Moving notebook attachment authoring to the daemon. The frontend still owns
  local editing mutations through WASM; `PutBlob` only stores bytes.

## Current state

The typed notebook frame byte space on `origin/main` is:

| Byte | Frame | Payload |
|------|-------|---------|
| `0x00` | AutomergeSync | opaque NotebookDoc Automerge bytes |
| `0x01` | NotebookRequest | JSON envelope |
| `0x02` | NotebookResponse | JSON envelope |
| `0x03` | NotebookBroadcast | JSON |
| `0x04` | Presence | CBOR |
| `0x05` | RuntimeStateSync | opaque RuntimeStateDoc Automerge bytes |
| `0x06` | PoolStateSync | opaque PoolDoc Automerge bytes |
| `0x07` | SessionControl | JSON |

#1814 originally suggested `0x07` for PutBlob. That value is now taken by
SessionControl, so PutBlob should be `0x08`.

There is also a separate `Handshake::Blob` socket channel that accepts
`BlobRequest::Store { media_type }` followed by a raw binary frame. Treat that
as a compatibility bridge, not the long-term shape. The destination is one
authenticated notebook/runtime peer stream with typed frames, not one side
channel per capability.

## Design summary

Add frame type:

| Byte | Frame | Payload |
|------|-------|---------|
| `0x08` | PutBlob | binary upload envelope |

`PutBlob` payloads use a short self-describing header followed by raw bytes:

```text
u32 header_len_be
header bytes: UTF-8 JSON
body bytes: raw blob or part bytes
```

The header carries an `id` so acknowledgements route through the existing
request/response correlation machinery without embedding binary data in JSON.
Responses are `NotebookResponse` (`0x02`) frames using new blob-upload response
variants in `NotebookResponseEnvelope`; `PutBlob` (`0x08`) is data-bearing only.
Do not add a second response path on `0x08`.

Automerge sync frames remain opaque. Receivers only decode Automerge at the
document actor boundary. `PutBlob` is a sibling data-plane frame, not an
Automerge extension.

## Upload modes

Two modes share the same frame type.

### One-shot upload

One frame uploads a complete blob:

```json
{
  "op": "put",
  "id": "request-uuid",
  "media_type": "image/png",
  "size": 48291,
  "sha256": "expected_hex",
  "purpose": "notebook_attachment"
}
```

The frame body is the full blob. The daemon:

1. verifies `body.len() == size`,
2. validates `sha256`,
3. stores bytes with `BlobStore::put(body, media_type)`,
4. responds with:

```json
{
  "id": "request-uuid",
  "result": "blob_stored",
  "hash": "computed_sha256_hex",
  "size": 48291,
  "media_type": "image/png"
}
```

`sha256` is required for one-shot uploads. The sender already has the full byte
buffer in memory, and requiring the expected hash makes validation, retries, and
dedupe semantics explicit. If a future sender truly cannot compute the hash
before sending, that should be a separate protocol variant rather than an
optional field whose absence weakens validation.

This is enough for drag/drop images and other small attachment writes.

Phase 1 one-shot uploads are capped by the `PutBlob` frame cap. With the
starting caps below, files larger than roughly 32 MiB require Phase 3 multipart
support even though today's `BlobStore::put` limit is 100 MiB. That tradeoff is
intentional: image attachment upload should not force the first implementation
to accept 100 MiB frames in the frame pump.

### Multipart upload

Large uploads use explicit multipart semantics. This is intentionally close to
S3-style multipart behavior, adapted to a single ordered socket:

1. **Create.** Reserve an upload session.
2. **Upload parts.** Send independently addressable, idempotent parts.
3. **Complete.** Atomically assemble and publish the final content-addressed
   blob after all parts validate.
4. **Abort.** Delete temporary parts and release the session.

The daemon must never expose a partially uploaded blob by final hash before
`complete` succeeds.

#### Create

Create is JSON control carried as a normal `NotebookRequest` so it reuses the
existing request/response routing:

```json
{
  "type": "create_blob_upload",
  "media_type": "application/vnd.apache.parquet",
  "size": 67108864,
  "sha256": "optional_expected_final_hex",
  "part_size": 8388608,
  "purpose": "dx_attach"
}
```

Response:

```json
{
  "upload_id": "upload-uuid",
  "part_size": 8388608,
  "expires_at": "2026-04-30T20:00:00Z"
}
```

#### Upload part

Each part is one `PutBlob` frame:

```json
{
  "op": "part",
  "id": "request-uuid",
  "upload_id": "upload-uuid",
  "part_number": 1,
  "size": 8388608,
  "sha256": "part_sha256_hex"
}
```

The body is exactly the part bytes.

Part semantics:

- `part_number` starts at 1 and is stable.
- Re-sending the same `part_number` with the same part hash is idempotent.
- Re-sending the same `part_number` with different bytes is an error.
- Parts may arrive in order in v1, but the protocol should not require that
  forever. The `(part_number, size, sha256)` tuple is the durable ordering
  contract.
- The receiver acknowledges each part after it is durably staged.

Do not include an `offset` field in v1. Multipart protocols such as S3 assemble
parts in part-number order; duplicating that with an offset introduces a second
source of truth and unnecessary validation edge cases.

#### Complete

Complete is JSON control:

```json
{
  "type": "complete_blob_upload",
  "upload_id": "upload-uuid",
  "parts": [
    {"part_number": 1, "sha256": "part_sha256_hex", "size": 8388608},
    {"part_number": 2, "sha256": "part_sha256_hex", "size": 12345}
  ]
}
```

The daemon validates:

- no missing parts,
- no duplicate part numbers,
- staged part hash and size match the manifest,
- concatenated size matches the declared total size,
- final SHA-256 matches the declared final hash when provided.

Only then does it write or link the final content-addressed blob and respond:

```json
{
  "result": "blob_stored",
  "hash": "final_sha256_hex",
  "size": 67108864,
  "media_type": "application/vnd.apache.parquet"
}
```

The current `BlobStore::put` rejects blobs above 100 MiB. Multipart does not
automatically change that storage invariant: either complete rejects final blobs
above the configured blob size limit, or the multipart implementation lands
with a deliberate blob-store limit increase and streaming assembly path. Do not
silently imply that multipart can publish larger-than-supported blobs.

#### Abort and cleanup

Abort is JSON control:

```json
{"type": "abort_blob_upload", "upload_id": "upload-uuid"}
```

The daemon also garbage-collects expired upload sessions. Temporary upload parts
must live outside the content-addressed blob namespace until completion.

### Upload session ownership

Multipart session state is shared across JSON control requests and `PutBlob`
part frames. Store it in a peer-scoped upload registry owned by the connection
or peer session, not in global daemon state. The registry tracks:

- `upload_id`,
- media type and expected final size/hash,
- negotiated part size and expiry,
- staged part hashes/sizes,
- pending byte count.

Peer-scoped state prevents one client from completing or aborting another
client's upload by guessing an `upload_id`, and gives the daemon a clear place
to enforce per-peer byte budgets.

## Flow control and fairness

Large uploads must not starve sync, request, presence, or session-control
traffic.

Rules:

- Keep part frames bounded. Start with an 8 MiB part cap and a 32 MiB absolute
  `PutBlob` frame cap.
- Handle `PutBlob` in the main frame loop by enqueueing blob writes to a bounded
  worker, then return to reading frames.
- Limit in-flight upload bytes per peer. A good v1 default is one in-flight part
  per upload and one active upload per peer.
- Limit staged-but-uncompleted bytes per peer. Expired or aborted sessions must
  release that budget and delete temporary parts.
- A peer that exceeds caps gets a structured error response and the frame is
  discarded after read.
- Multipart upload is opt-in. Small uploads should not pay the session cost.

This keeps the first implementation small while leaving room for higher
throughput later.

Implementation requirement: do not let `FramedReader` queue many large
`PutBlob` frames in memory. Either lower the reader queue capacity for
blob-enabled connections or make the peer loop apply backpressure before another
large frame can be enqueued. The one-active-part rule must be enforced by the
daemon, not trusted to clients.

## Security model

`PutBlob` is allowed only on authenticated socket/pipe connections. Same-machine
auth remains the OS owner boundary:

- Unix socket: filesystem permissions (`0600`) and parent directory ownership.
- Windows named pipe: user-scoped pipe ACLs.
- SSH transport: SSH is the auth boundary.
- Future HTTPS transport: out of scope until bearer-token/CORS policy exists.

Do not add blob writes to the localhost HTTP server. The HTTP server remains:

- `GET /blob/{sha256}`
- `GET /plugins/{name}`
- health-style read endpoints only

This matters because output iframes are intentionally opaque-origin and the blob
server currently uses localhost HTTP for renderer fetches. A write-capable HTTP
blob server would create a new browser-origin attack surface.

## CRDT integration

`PutBlob` only stores bytes. It does not mutate NotebookDoc or RuntimeStateDoc by
itself.

Examples:

- Drag/drop image into a markdown cell:
  1. frontend uploads bytes with one-shot `PutBlob`,
  2. daemon returns `{hash, media_type, size}`,
  3. frontend performs a normal WASM-authored NotebookDoc mutation that inserts
     or updates the nbformat attachment ref and markdown source.
- Runtime agent output:
  1. agent uploads or writes bytes,
  2. agent writes a RuntimeStateDoc output manifest with `ContentRef::Blob`.
- Remote daemon peer:
  1. peer sends needed blobs over `PutBlob`,
  2. Automerge sync carries refs and document state separately.

This separation avoids treating the blob store as CRDT state while still making
the CRDT the source of truth for references.

## Capability negotiation

Adding a frame type is not useful unless clients can know it is safe to send.

Add a capability such as:

```json
{
  "put_blob": {
    "version": 1,
    "single_frame_max": 33554432,
    "default_part_size": 8388608,
    "multipart": true
  }
}
```

Expose this as an optional field on both handshake response shapes clients
already read:

- `ProtocolCapabilities` for direct `NotebookSync` handshakes,
- `NotebookConnectionInfo` for open/create notebook handshakes.

Clients MUST NOT send `0x08` unless the current connection advertised
`put_blob`. Older daemons may discard unknown frame types without response, so a
capability violation can hang. Treat missing capability as "feature unavailable"
and fall back to existing paths or disable upload features.

Whether this requires bumping `PROTOCOL_VERSION` depends on rollout:

- If `0x08` is optional and capability-gated, it can be additive.
- If any required client path depends on it, bump the protocol and update
  frontend/TS contract tests in the same PR.

## Rollout plan

This should not block unrelated notebook or runtime work. Break it into narrow,
reviewable slices:

### Phase 0: Spec and frame contract

- Land this design.
- Add `PUT_BLOB = 0x08` constants, TypeScript constants, frame caps, protocol
  contract tests, and docs.
- Add capability shape, but no production caller.
- Add blob-upload `NotebookResponseEnvelope` variants and document that
  `0x08` never carries responses.
- Update the Tauri relay and host transport allowlists so frontend-originated
  `0x08` frames can be forwarded once callers exist.

### Phase 1: One-shot socket upload

- Implement single-frame `op: "put"`.
- Keep `Handshake::Blob` channel intact.
- Add a Rust client helper that uploads bytes and returns `{hash, size,
  media_type}`.
- Cover with daemon protocol tests and cap/error tests.
- Enforce `sha256` as required and return structured errors for missing or
  mismatched hashes.

### Phase 2: Markdown attachment ingestion

- Add a high-level attachment API that takes attachment name, MIME type, and raw
  bytes.
- Use one-shot `PutBlob` for the bytes.
- Write the NotebookDoc mutation through the existing frontend/WASM local edit
  path.
- Ship drag/drop images on this path.

### Phase 3: Multipart sessions

- Add create/part/complete/abort.
- Stage temporary parts outside the blob namespace.
- Add timeout cleanup and idempotent retry tests.
- Enforce per-peer staged-byte budgets.
- Keep final object size within the configured blob-store limit unless that
  phase explicitly raises the limit and adds streaming assembly support.
- Start using this for `dx.attach(path)` and large local files.

### Phase 4: Remote peers and legacy cleanup

- Use multipart `PutBlob` for remote runtime / daemon peer blob movement.
- Deprecate `Handshake::Blob::Store`.
- Keep `Handshake::Blob::GetPort` or replace it with daemon-info capability
  depending on the state of the blob-port API at that point.

## Testing

- Protocol contract: Rust and TypeScript frame constants include `PUT_BLOB`.
- Frame caps: oversized `PutBlob` frames are rejected without corrupting the
  frame stream.
- One-shot success: upload bytes, assert blob exists, metadata matches, and the
  hash is content-addressed.
- Missing one-shot hash: request fails before publishing a blob.
- Hash mismatch: declared SHA-256 mismatch returns error and does not publish a
  blob under the declared hash.
- Multipart idempotency: repeated identical part succeeds; conflicting repeat
  fails.
- Multipart complete: missing, duplicate, wrong-size, wrong-hash, and wrong
  final-hash manifests fail.
- Abort/expiry: staged parts are removed and never become readable blobs.
- Budget: pending multipart bytes are capped per peer and released on abort,
  completion, expiry, and disconnect.
- Fairness: a peer uploading parts does not block Automerge sync or request
  responses behind long blob writes.

## Open questions

1. Should one-shot uploads be internally implemented as a single-part multipart
   session for code reuse, or should they take the direct `BlobStore::put` fast
   path?
2. What is the right default part size on Windows named pipes?
3. Do we need per-purpose policy caps, e.g. smaller for notebook attachments and
   larger for `dx.attach`?
4. When remote daemon peers need missing blobs referenced by Automerge state,
   should they push eagerly or request blobs lazily by hash?

## Review checklist

- Does the design keep HTTP blob reads separate from socket-authenticated blob
  writes?
- Are Automerge sync bytes treated as opaque payloads?
- Does multipart avoid exposing partial uploads?
- Does the rollout let image attachments ship without waiting for remote
  runtime blob replication?
- Are all frame constants, caps, TS contracts, and protocol docs listed in the
  implementation phase?
