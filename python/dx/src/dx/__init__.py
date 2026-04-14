"""nteract/dx — efficient Python → blob store display.

Public API:

- :func:`install` — register IPython formatters and open the runtime-agent comm channel.
- :func:`display` — upgraded display that routes DataFrames through the blob store.
- :func:`put` — low-level upload primitive. Returns a :class:`BlobRef`.
- :func:`display_blob_ref` — emit a display_data bundle referencing an existing blob.
- :class:`BlobRef` — dataclass returned by :func:`put`.
- Exceptions: :class:`DxError`, :class:`DxNoAgentError`, :class:`DxTimeoutError`,
  :class:`DxPayloadTooLargeError`.

See ``docs/superpowers/specs/2026-04-13-nteract-dx-design.md`` for the protocol.
"""

from __future__ import annotations

from typing import Any

from dx._refs import BLOB_REF_MIME, BlobRef

__all__ = [
    "BLOB_REF_MIME",
    "BlobRef",
    "DxError",
    "DxNoAgentError",
    "DxTimeoutError",
    "DxPayloadTooLargeError",
    "display",
    "display_blob_ref",
    "install",
    "put",
]

__version__ = "0.1.0"


class DxError(Exception):
    """Base class for dx exceptions."""


class DxNoAgentError(DxError):
    """Raised when a blob-store operation is requested but no runtime agent is reachable."""


class DxTimeoutError(DxError):
    """Raised when a runtime agent comm request does not ack within the configured timeout."""


class DxPayloadTooLargeError(DxError):
    """Raised when an upload exceeds the runtime agent's ``MAX_BLOB_SIZE``."""


def install() -> None:
    """Install IPython formatters and open the ``nteract.dx.blob`` comm.

    Idempotent. Safe to call in vanilla Jupyter or plain Python — when no
    runtime agent is reachable it installs a no-op fallback and returns silently.
    """
    from dx._format_install import install_formatters

    install_formatters()


def display(obj: Any) -> None:
    """Display ``obj`` using dx's upgraded path when available.

    Falls back to :func:`IPython.display.display` for objects dx does not handle.
    """
    from dx._format_install import dx_display

    dx_display(obj)


def put(data: bytes, content_type: str) -> BlobRef:
    """Upload ``data`` to the blob store and return a :class:`BlobRef`."""
    from dx._comm import put_blob

    return put_blob(data, content_type)


def display_blob_ref(
    ref: BlobRef,
    *,
    content_type: str,
    summary: dict | None = None,
) -> None:
    """Emit a display_data bundle referencing an existing blob."""
    from dx._format_install import publish_ref_with_type

    publish_ref_with_type(ref, content_type=content_type, summary=summary)
