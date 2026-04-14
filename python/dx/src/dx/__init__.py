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

    This is an **opt-in** call that mutates kernel-wide display behavior.
    Two extension points are wired up:

    1. **IPython ``mimebundle_formatter`` for pandas / polars DataFrames.**
       The formatter serializes to parquet, hashes locally, stashes the
       bytes in a thread-local buffer map keyed by hash, and returns a
       bundle with ``application/vnd.nteract.blob-ref+json`` +
       ``text/llm+plain``. IPython merges this with the default pandas
       HTML / plain formatters, so vanilla hosts still have a fallback
       to render.

    2. **ipykernel ``display_pub.register_hook`` hook** on the kernel's
       ``ZMQDisplayPublisher``. Fires for every ``display_data`` and
       ``update_display_data`` message. When the hook sees our ref MIME,
       it pops the stashed parquet bytes by hash and calls
       ``session.send(..., buffers=[parquet])`` directly — the parquet
       rides the Jupyter messaging envelope's ``buffers`` field (same
       mechanism ipywidgets uses), not a base64 string inside the JSON
       content. ``h.update(df)`` on a ``DisplayHandle`` works natively
       because ``update_display_data`` goes through the hook too, with
       ``transient.display_id`` already populated.

    Also flips third-party visualization libraries that ship an
    ``"nteract"`` renderer:

    - ``altair``: ``alt.renderers.enable("nteract")``
    - ``plotly``: ``plotly.io.renderers.default = "nteract"``

    Each is guarded by ``ImportError``. Plotly's nteract renderer emits
    only ``application/vnd.plotly.v1+json``, dropping the terminal /
    browser fallback — plotly figures stop rendering in plain-IPython
    sessions after install.

    Idempotent. Safe to call without ipykernel (formatters register but
    the display publisher hook is skipped; the DataFrame formatter
    still returns a bundle that renders via the default chain).

    See ``docs/superpowers/specs/2026-04-13-nteract-dx-design.md``.
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
