"""DataFrame display wiring for ``dx.install()``.

Two IPython extension points, one for the bundle and one for the bytes:

1. A ``mimebundle_formatter`` for pandas / polars DataFrames serializes
   the frame to parquet, hashes locally, stashes the bytes in a
   thread-local keyed by hash, and returns
   ``{BLOB_REF_MIME: {...}, "text/llm+plain": ...}``. IPython's default
   chain then merges pandas' ``text/html`` and ``text/plain`` so hosts
   that don't understand the ref MIME still render normally.

2. A ``ZMQDisplayPublisher.register_hook`` callback attaches the
   stashed bytes to every outgoing ``display_data`` / ``update_display_data``
   message whose bundle carries the ref MIME. The hook pops the bytes
   by hash and calls ``session.send(..., buffers=[parquet])`` directly,
   returning ``None`` so the default (buffer-less) send is skipped.
   This is why ``h.update(df2)`` on a ``DisplayHandle`` works — the
   hook fires on updates just like initial displays, with
   ``transient.display_id`` already populated on the message.

``register_hook`` is documented public API on
``ipykernel.zmqshell.ZMQDisplayPublisher``.
"""

from __future__ import annotations

import hashlib
import logging
import os
import threading
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

# Pending parquet bytes waiting to be attached to the next IOPub message
# that references them. Keyed by content hash (hex SHA-256) so lookups
# match the ref MIME's ``hash`` field. Thread-local: each execution
# context owns its own pending slot.
_pending = threading.local()


def _pending_buffers() -> dict[str, bytes]:
    if not hasattr(_pending, "buffers"):
        _pending.buffers = {}
    return _pending.buffers


def _get_ipython_for_format() -> Any | None:
    """Extracted for test monkeypatching."""
    try:
        from IPython import get_ipython as _gi
    except ImportError:
        return None
    return _gi()


def _display_pub() -> Any | None:
    """Return the kernel's ``ZMQDisplayPublisher`` instance if we're in a
    kernel, else ``None``. The publisher has ``register_hook`` and
    ``session`` / ``pub_socket`` / ``topic`` attributes we need."""
    ip = _get_ipython_for_format()
    if ip is None:
        return None
    pub = getattr(ip, "display_pub", None)
    if pub is None:
        return None
    # The in-process (plain IPython) DisplayPublisher doesn't have
    # ``register_hook`` or ``session`` / ``pub_socket`` — only the kernel
    # subclass does. Probe for the kernel-specific surface.
    if not all(hasattr(pub, attr) for attr in ("register_hook", "session", "pub_socket")):
        return None
    return pub


def install_formatters() -> None:
    global _INSTALLED
    if _INSTALLED:
        return

    if detect_environment() != Environment.IPYKERNEL:
        log.debug("dx: not running under ipykernel — formatters fall back to default chain.")

    ip = _get_ipython_for_format()
    if ip is None:
        _INSTALLED = True
        return

    # IPython's InteractiveShell exposes DisplayFormatter as an attribute,
    # not a method — do not call it.
    mimebundle = ip.display_formatter.mimebundle_formatter
    ipython_display = ip.display_formatter.ipython_display_formatter

    try:
        import pandas as pd

        mimebundle.for_type(pd.DataFrame, _pandas_mimebundle)
        ipython_display.for_type(pd.DataFrame, _pandas_ipython_display)
    except ImportError:
        pass

    try:
        import polars as pl

        mimebundle.for_type(pl.DataFrame, _polars_mimebundle)
        ipython_display.for_type(pl.DataFrame, _polars_ipython_display)
    except ImportError:
        pass

    _install_display_pub_hook()
    _enable_third_party_nteract_renderers()

    _INSTALLED = True


def _install_display_pub_hook() -> None:
    """Install ``_dx_display_pub_hook`` on the kernel's display publisher.

    The hook fires for every ``display_data`` and ``update_display_data``
    message right before ``session.send`` is called — it's a documented
    public extension point on ``ipykernel.zmqshell.ZMQDisplayPublisher``.

    Idempotent: the hook function is tagged with ``_dx_installed`` so
    repeat ``install()`` calls don't stack duplicates.
    """
    pub = _display_pub()
    if pub is None:
        return
    for existing in getattr(pub, "_hooks", []):
        if getattr(existing, "_dx_installed", False):
            return
    pub.register_hook(_dx_display_pub_hook)


def _dx_display_pub_hook(msg: dict) -> dict | None:
    """Attach buffers to ``display_data`` / ``update_display_data`` messages
    whose data bundle carries our blob-ref MIME.

    Returns:
        - ``msg`` unchanged if the message isn't one of ours (pass-through).
        - ``None`` if we sent the message ourselves with buffers (tells
          ``ZMQDisplayPublisher.publish`` to skip the default send).
    """
    try:
        msg_type = msg.get("header", {}).get("msg_type", "")
        if msg_type not in ("display_data", "update_display_data"):
            return msg
        data = msg.get("content", {}).get("data") or {}
        ref_raw = data.get(BLOB_REF_MIME)
        if ref_raw is None:
            return msg

        # `data` values are JSON-cleaned at this point; the ref MIME
        # is a dict.
        if isinstance(ref_raw, dict):
            ref = ref_raw
        else:
            import json

            ref = json.loads(ref_raw) if isinstance(ref_raw, str) else None
        if not isinstance(ref, dict):
            return msg
        h = ref.get("hash")
        if not isinstance(h, str):
            return msg

        buffers = _pending_buffers().pop(h, None)
        if buffers is None:
            # No stashed payload for this hash — maybe re-publish of a
            # historical message, or a ref emitted by something other
            # than our formatter. Pass through unchanged; the agent can
            # still resolve via BlobStore::exists on the hash.
            return msg

        pub = _display_pub()
        if pub is None:
            return msg
        pub.session.send(
            pub.pub_socket,
            msg,
            ident=pub.topic,
            buffers=[buffers],
        )
        return None
    except Exception as exc:
        log.debug("dx: display_pub hook error: %s — letting default send run", exc)
        return msg


_dx_display_pub_hook._dx_installed = True  # ty: ignore[unresolved-attribute]


def _pandas_mimebundle(df: Any, include=None, exclude=None) -> dict | None:
    return _emit_dataframe(df, total_rows=len(df))


def _polars_mimebundle(df: Any, include=None, exclude=None) -> dict | None:
    return _emit_dataframe(df, total_rows=df.height)


def _pandas_ipython_display(df: Any) -> None:
    """`ipython_display_formatter` handler for `pd.DataFrame`.

    IPython's `DisplayFormatter.format()` checks `ipython_display_formatter`
    before walking mimebundle/per-MIME formatters. If our handler matches,
    `format()` returns `({}, {})` and the displayhook's send is suppressed.
    We publish our own `display_data` message via `publish_display_data`,
    which flows through `ZMQDisplayPublisher.publish` → the existing
    `_dx_display_pub_hook`, which attaches parquet buffers.

    Net effect for a last-expression `df`: one `display_data` message goes
    on the wire (with buffers). No `execute_result` is emitted — the saved
    `.ipynb` records the output as `display_data`, which is valid nbformat.
    `_`, `__`, `___` and `ExecutionResult` bookkeeping still update because
    they run at steps 4–5 of `DisplayHook.__call__`, independently of the
    message send.
    """
    _publish_via_ipython_display(df, total_rows=len(df))


def _polars_ipython_display(df: Any) -> None:
    """`ipython_display_formatter` handler for `pl.DataFrame`."""
    _publish_via_ipython_display(df, total_rows=df.height)


def _publish_via_ipython_display(df: Any, *, total_rows: int) -> None:
    """Shared body for the pandas / polars `ipython_display` handlers."""
    # Lazy import so dx.install() doesn't hard-depend on IPython being
    # importable from the install site (it already is under ipykernel,
    # but stay symmetrical with _emit_dataframe).
    try:
        from IPython.display import publish_display_data
    except ImportError:
        return

    try:
        bundle = _emit_dataframe(df, total_rows=total_rows)
    except Exception as exc:
        log.debug("dx: _emit_dataframe failed: %s — falling back to repr", exc)
        bundle = None

    if bundle:
        publish_display_data(data=bundle, metadata={})
    else:
        # Fallback so a failed formatter doesn't silently eat the output.
        print(repr(df))


def _emit_dataframe(df: Any, *, total_rows: int) -> dict | None:
    """Serialize df → parquet, stash bytes in the pending buffer map, and
    return a mimebundle containing the ref MIME + text/llm+plain.

    IPython's default formatter chain fills in text/html / text/plain
    as a fallback bundle for hosts that don't understand the ref MIME;
    nteract frontends pick the parquet renderer via the ref MIME.

    Returns ``None`` when serialization fails — lets IPython's default
    chain render unchanged.
    """
    try:
        data, content_type = serialize_dataframe(df, max_bytes=_MAX_PAYLOAD_BYTES)
    except Exception as exc:
        log.debug("dx: serialize failed: %s — falling back to default repr", exc)
        return None

    # Detect downsampling by reading parquet metadata (cheap — footer only).
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
    ref_bundle["buffer_index"] = 0

    llm = summarize_dataframe(df, total_rows=total_rows, included_rows=included, sampled=sampled)

    # Stash the parquet bytes for the display_pub hook to pick up on
    # the upcoming publish. Keyed by hash so the hook can match exactly.
    _pending_buffers()[h] = data

    return {BLOB_REF_MIME: ref_bundle, "text/llm+plain": llm}


def _enable_third_party_nteract_renderers() -> None:
    """Flip visualization libraries that ship an 'nteract' renderer to it.

    Each library is guarded by ImportError so install stays a no-op when
    the library isn't present. Logs (debug) which switches fired so a
    curious user can see what dx changed.
    """
    try:
        import altair as alt  # ty: ignore[unresolved-import]

        alt.renderers.enable("nteract")
        log.debug("dx: enabled altair 'nteract' renderer")
    except ImportError:
        pass
    except Exception as exc:  # pragma: no cover — defensive
        log.debug("dx: failed to enable altair 'nteract' renderer: %s", exc)

    try:
        import plotly.io as pio

        pio.renderers.default = "nteract"
        log.debug("dx: set plotly default renderer to 'nteract'")
    except ImportError:
        pass
    except Exception as exc:  # pragma: no cover — defensive
        log.debug("dx: failed to set plotly 'nteract' renderer: %s", exc)


def dx_display(obj: Any) -> None:
    """Upgraded display; hands off to IPython for non-DataFrame types."""
    from IPython.display import display

    display(obj)
