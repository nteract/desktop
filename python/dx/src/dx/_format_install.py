"""Install-time wiring: IPython formatters + fire-and-forget display_data+buffers.

v1 upload path uses the Jupyter messaging envelope's ``buffers`` field directly
on an ``IOPub`` ``display_data`` message (the same mechanism ipywidgets uses for
binary state). Python hashes the bytes locally and emits one message carrying
the ref-MIME entry plus the raw bytes as a trailing ZMQ frame. The runtime
agent, reading IOPub sequentially, writes the buffer to the blob store and
composes a ``ContentRef`` under the target content_type.

No comm, no ack, no round-trip — the hash is content-addressed and derivable
on both sides. The ``nteract.dx.*`` comm namespace stays reserved for future
bidirectional uses (push-down predicates, streaming Arrow, ``dx.attach``).
"""

from __future__ import annotations

import hashlib
import logging
import os
from typing import Any

from dx._env import Environment, detect_environment
from dx._format import serialize_dataframe
from dx._refs import BLOB_REF_MIME, BlobRef, build_ref_bundle
from dx._summary import summarize_dataframe

log = logging.getLogger("dx")

_INSTALLED = False

# Payload ceiling enforced on the kernel side. Server-side MAX_BLOB_SIZE is
# 100 MiB; leave ~10 MiB for overhead and safety.
_MAX_PAYLOAD_BYTES = int(os.environ.get("DX_MAX_PAYLOAD_BYTES", str(90 * 1024 * 1024)))


def _get_ipython_for_format() -> Any | None:
    """Extracted for test monkeypatching.

    Return type is ``Any`` because IPython's ``InteractiveShell`` has a
    dynamic ``display_formatter`` attribute we poke directly; a strictly
    typed return would need an IPython type stub we don't depend on.
    """
    try:
        from IPython import get_ipython as _gi
    except ImportError:
        return None
    return _gi()


def _kernel_session_and_socket() -> tuple[Any, Any] | None:
    """Return ``(session, iopub_socket)`` if we're under ipykernel.

    Returns ``None`` in plain IPython or plain python — caller falls back to
    emitting raw bytes in the mimebundle.
    """
    ip = _get_ipython_for_format()
    if ip is None:
        return None
    kernel = getattr(ip, "kernel", None)
    if kernel is None:
        return None
    session = getattr(kernel, "session", None)
    iopub_socket = getattr(kernel, "iopub_socket", None)
    if session is None or iopub_socket is None:
        return None
    return session, iopub_socket


def install_formatters() -> None:
    global _INSTALLED
    if _INSTALLED:
        return

    if detect_environment() != Environment.IPYKERNEL:
        log.debug("dx: not running under ipykernel — formatters will fall back to raw bytes.")

    ip = _get_ipython_for_format()
    if ip is None:
        _INSTALLED = True
        return

    # IPython's InteractiveShell exposes DisplayFormatter as an attribute,
    # not a method — do not call it.
    #
    # We register on `ipython_display_formatter` rather than the bundle
    # formatter because (a) we publish the display via `session.send`
    # ourselves (to carry buffers on the envelope) and (b) returning True
    # from an ipython_display formatter tells IPython's display chain to
    # skip every other formatter for this object. That prevents the default
    # pandas HTML/plain repr from being published alongside our own output
    # for a bare `df` on the last cell line.
    formatter = ip.display_formatter.ipython_display_formatter

    try:
        import pandas as pd

        formatter.for_type(pd.DataFrame, _pandas_ipython_display)
    except ImportError:
        pass

    try:
        import polars as pl

        formatter.for_type(pl.DataFrame, _polars_ipython_display)
    except ImportError:
        pass

    _enable_third_party_nteract_renderers()

    _INSTALLED = True


def _enable_third_party_nteract_renderers() -> None:
    """Flip visualization libraries that ship an 'nteract' renderer to it.

    Each library is guarded by ImportError so install stays a no-op when
    the library isn't present. Logs (debug) which switches fired so a
    curious user can see what dx changed.
    """
    # altair: alt.renderers is a RendererRegistry; `enable("nteract")` makes
    # Chart display produce an nteract-aware mime bundle.
    try:
        import altair as alt  # ty: ignore[unresolved-import]

        alt.renderers.enable("nteract")
        log.debug("dx: enabled altair 'nteract' renderer")
    except ImportError:
        pass
    except Exception as exc:  # pragma: no cover — defensive
        log.debug("dx: failed to enable altair 'nteract' renderer: %s", exc)

    # plotly: `plotly.io.renderers.default` is a simple string assignment;
    # "nteract" is a registered option that emits the plotly mime bundle
    # the isolated parent iframe knows how to render.
    try:
        import plotly.io as pio

        pio.renderers.default = "nteract"
        log.debug("dx: set plotly default renderer to 'nteract'")
    except ImportError:
        pass
    except Exception as exc:  # pragma: no cover — defensive
        log.debug("dx: failed to set plotly 'nteract' renderer: %s", exc)


def _pandas_ipython_display(df: Any) -> bool | None:
    """IPython display formatter for pandas DataFrames.

    Returns ``True`` when we've taken over display (and no other formatter
    should run), ``None`` otherwise (so IPython falls back to the default
    pandas HTML/plain formatters — e.g. when we're not under ipykernel).
    """
    return _emit_dataframe(df, total_rows=len(df))


def _polars_ipython_display(df: Any) -> bool | None:
    """IPython display formatter for polars DataFrames."""
    return _emit_dataframe(df, total_rows=df.height)


def _emit_dataframe(df: Any, *, total_rows: int) -> bool | None:
    """Serialize, publish a display_data with buffers, return True.

    Returns ``None`` when we fall back to the default repr — in that case
    IPython runs the rest of the formatter chain as if dx weren't there.
    """
    try:
        data, content_type = serialize_dataframe(df, max_bytes=_MAX_PAYLOAD_BYTES)
    except Exception as exc:
        log.debug("dx: serialize failed: %s — falling back to default repr", exc)
        return None

    # Detect whether the serializer downsampled by reading parquet metadata.
    sampled = False
    included = total_rows
    try:
        import io

        import pyarrow.parquet as pq

        meta = pq.read_metadata(io.BytesIO(data))
        if meta.num_rows != total_rows:
            sampled = True
            included = meta.num_rows
    except Exception:
        pass

    h = hashlib.sha256(data).hexdigest()
    ref = BlobRef(hash=h, size=len(data))
    summary_hints = {
        "total_rows": total_rows,
        "included_rows": included,
        "sampled": sampled,
        "sample_strategy": "head" if sampled else "none",
    }
    ref_bundle = build_ref_bundle(ref, content_type=content_type, summary=summary_hints)
    # Convention: exactly one BLOB_REF_MIME entry per display_data, referencing
    # buffers[0]. Future work may extend to multiple refs via a buffer_index
    # field.
    ref_bundle["buffer_index"] = 0

    llm = summarize_dataframe(df, total_rows=total_rows, included_rows=included, sampled=sampled)

    session_info = _kernel_session_and_socket()
    if session_info is None:
        # Plain IPython / plain Python: no buffer path available. Let the
        # default formatter chain run (HTML / plain / etc) by returning None.
        return None

    session, iopub_socket = session_info
    try:
        _send_display_data_with_buffers(
            session=session,
            iopub_socket=iopub_socket,
            data={BLOB_REF_MIME: ref_bundle, "text/llm+plain": llm},
            buffers=[data],
        )
    except Exception as exc:
        log.debug("dx: session.send failed: %s — falling back to default repr", exc)
        return None

    # We've already published the display_data ourselves; tell IPython to
    # skip every other formatter for this object (no duplicate HTML/plain).
    return True


def _send_display_data_with_buffers(
    *,
    session,
    iopub_socket,
    data: dict,
    buffers: list[bytes],
) -> None:
    """Publish a ``display_data`` message on IOPub with trailing binary buffers.

    Mirrors how ipykernel's own ``publish_display_data`` calls
    ``Session.send`` — ``ident`` is the IOPub topic (bytes, e.g.
    ``b"display_data"``), ``parent`` is the parent header dict. Extracted
    for test monkeypatching.
    """
    from IPython import get_ipython

    ip = get_ipython()
    kernel = ip.kernel
    parent = None
    try:
        parent = kernel.get_parent("shell")
    except Exception:
        try:
            parent = kernel._parent_header
        except Exception:
            parent = None
    try:
        ident = kernel.topic("display_data")
    except Exception:
        ident = None
    session.send(
        iopub_socket,
        "display_data",
        content={"data": data, "metadata": {}, "transient": {}},
        parent=parent,
        ident=ident,
        buffers=buffers,
    )


def dx_display(obj: Any) -> None:
    """Upgraded display; hands off to IPython for non-DataFrame types."""
    from IPython.display import display

    display(obj)
