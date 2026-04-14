"""nteract/dx — efficient Python → blob store display.

v1 API:

- :func:`install` — register IPython formatters.
- :func:`display` — upgraded display that routes DataFrames through the blob
  store via a single ``display_data`` IOPub message with the raw bytes attached
  as a trailing ZMQ buffer frame.
- :class:`BlobRef` — content-addressed reference (``hash``, ``size``).
- :class:`DxError` — base exception.

The v1 upload path is fire-and-forget: Python hashes the bytes locally, fills
in the ref-MIME entry, and publishes one message whose trailing buffer the
runtime agent stores in the blob store. No comm round-trip, no ack, no
deadlock. See ``docs/superpowers/specs/2026-04-13-nteract-dx-design.md``.

The reserved ``nteract.dx.*`` comm namespace is kept intact for future
bidirectional features (push-down predicates, streaming Arrow, ``dx.attach``).
"""

from __future__ import annotations

from typing import Any

from dx._refs import BLOB_REF_MIME, BlobRef

__all__ = [
    "BLOB_REF_MIME",
    "BlobRef",
    "DxError",
    "display",
    "install",
]

__version__ = "2.0.0"


class DxError(Exception):
    """Base class for dx exceptions."""


def install() -> None:
    """Install the nteract data-experience integration.

    - Registers IPython formatters for ``pandas.DataFrame`` and
      ``polars.DataFrame`` (if installed). Bare ``df`` on the last cell
      line then rides the blob-store path via ``display_data`` + IOPub
      buffers instead of shipping base64'd bytes.
    - Flips third-party visualization libraries that ship an ``"nteract"``
      renderer to use it:
      - altair: ``alt.renderers.enable("nteract")``
      - plotly: ``plotly.io.renderers.default = "nteract"``
      Each is guarded by ``ImportError`` and is a no-op if the library
      isn't present.

    Idempotent. Safe to call in vanilla Jupyter or plain Python — when
    no ipykernel is reachable, DataFrame formatters return ``None`` so
    IPython's default display chain runs unchanged.
    """
    from dx._format_install import install_formatters

    install_formatters()


def display(obj: Any) -> None:
    """Display ``obj`` using dx's upgraded path when available.

    Falls back to :func:`IPython.display.display` for objects dx does not
    handle.
    """
    from dx._format_install import dx_display

    dx_display(obj)
