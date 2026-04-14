"""BlobRef dataclass and ref-MIME bundle construction."""

from __future__ import annotations

from dataclasses import dataclass

BLOB_REF_MIME = "application/vnd.nteract.blob-ref+json"


@dataclass(frozen=True)
class BlobRef:
    """A content-addressed reference to a blob in the daemon's blob store.

    ``hash`` is the persistent identity (e.g. ``"sha256:abc123..."``).
    ``size`` is the blob's byte length as reported by the runtime agent.

    No URL is stored. The frontend derives the blob-server URL from the hash
    at render time (``ContentRef`` resolution in WASM), which keeps outputs
    durable across blob-server port changes. Kernel-side code that wants a
    URL for external tooling should call into the daemon explicitly (e.g.
    ``runt daemon status --json``); URLs are not part of dx's persistent
    protocol.
    """

    hash: str
    size: int


def build_ref_bundle(
    ref: BlobRef,
    *,
    content_type: str,
    summary: dict | None = None,
    query: dict | None = None,
) -> dict:
    """Build the JSON body for ``application/vnd.nteract.blob-ref+json``.

    The URL is intentionally omitted from the bundle — the frontend derives
    the current blob-server URL from the hash at render time (``ContentRef``
    resolution in WASM), which keeps outputs durable across blob-server
    port changes.

    ``summary`` is optional and advisory. ``query`` is reserved for the
    future interactive query backend and is always ``None`` in v1.
    """
    bundle: dict = {
        "hash": ref.hash,
        "content_type": content_type,
        "size": ref.size,
        "query": query,
    }
    if summary is not None:
        bundle["summary"] = summary
    return bundle
