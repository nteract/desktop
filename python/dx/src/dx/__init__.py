"""nteract/dx — data-experience library for Python kernels running under nteract.

Public API:

- :func:`install` — register IPython formatters and flip third-party
  visualization libraries to their nteract-aware renderers.
- :func:`display` — IPython's ``display()``, forwarded.
- :class:`BlobRef` — content-addressed blob reference (``hash``, ``size``).
- :class:`DxError` — base exception.

Design: ``docs/superpowers/specs/2026-04-13-nteract-dx-design.md``.
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
    """Wire up the nteract data-experience integration in the current kernel.

    Changes kernel-wide display behavior. Call from trusted environments only
    (the nteract kernel bootstrap will call this automatically once dx ships
    on PyPI). Idempotent.

    What it does:

    1. Registers pandas / polars DataFrame formatters. Bare ``df`` on the
       last cell line publishes a ``display_data`` whose bundle carries
       ``application/vnd.nteract.blob-ref+json`` + ``text/llm+plain``.
       Parquet bytes are attached to the Jupyter messaging envelope's
       trailing ZMQ ``buffers`` field, not base64'd inside the JSON.
       IPython's default chain fills in ``text/html`` / ``text/plain`` as
       a fallback for hosts that don't understand the ref MIME.
    2. Registers a hook on ``ZMQDisplayPublisher`` so ``h.update(df2)``
       on a ``DisplayHandle`` also carries buffers and preserves
       ``display_id`` on the wire.
    3. Flips visualization libraries that ship an ``"nteract"`` renderer:

       - ``altair``: ``alt.renderers.enable("nteract")``
       - ``plotly``: ``plotly.io.renderers.default = "nteract"``

       Each is a no-op if the library isn't present. Plotly's nteract
       renderer emits only ``application/vnd.plotly.v1+json`` — figures
       won't render in a plain-IPython terminal after install.
    """
    from dx._format_install import install_formatters

    install_formatters()


def display(obj: Any) -> None:
    """Forward to :func:`IPython.display.display`."""
    from dx._format_install import dx_display

    dx_display(obj)
