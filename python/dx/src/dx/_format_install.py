"""Install-time wiring: open the comm, register IPython formatters, emit displays.

This module glues together :mod:`dx._env`, :mod:`dx._comm`, :mod:`dx._format`,
:mod:`dx._summary`, and :mod:`dx._refs` into the public ``dx.install()`` entry
point.
"""

from __future__ import annotations

import logging
import os
from typing import Any

from dx._comm import COMM_TARGET, BlobClient, FallbackClient, get_client, set_client
from dx._env import Environment, detect_environment
from dx._format import serialize_dataframe
from dx._refs import BLOB_REF_MIME, BlobRef, build_ref_bundle
from dx._summary import summarize_dataframe

log = logging.getLogger("dx")

_INSTALLED = False

# Payload ceiling enforced on the kernel side. Server-side MAX_BLOB_SIZE is
# 100 MiB; leave ~10 MiB for overhead and safety.
_MAX_PAYLOAD_BYTES = int(os.environ.get("DX_MAX_PAYLOAD_BYTES", str(90 * 1024 * 1024)))


def _get_ipython_for_format() -> object | None:
    """Extracted for test monkeypatching."""
    try:
        from IPython import get_ipython as _gi
    except ImportError:
        return None
    return _gi()


def _publish_display_data(data: dict, metadata: dict | None = None) -> None:
    """Indirection over :func:`IPython.display.publish_display_data` for tests."""
    from IPython.display import publish_display_data

    publish_display_data(data, metadata or {})


def _try_open_comm() -> BlobClient | None:
    """Open the ``nteract.dx.blob`` comm if we're under ipykernel."""
    if detect_environment() != Environment.IPYKERNEL:
        return None
    try:
        from ipykernel.comm import Comm
    except ImportError:
        return None
    try:
        comm = Comm(target_name=COMM_TARGET, data={})
    except Exception as exc:  # pragma: no cover — defensive
        log.debug("dx: failed to open %s comm: %s", COMM_TARGET, exc)
        return None
    return BlobClient(comm)


def install_formatters() -> None:
    global _INSTALLED
    if _INSTALLED:
        return

    client = _try_open_comm()
    if client is None:
        log.debug("dx: no runtime agent comm — using fallback (raw-bytes display).")
        client = FallbackClient()
    set_client(client)

    ip = _get_ipython_for_format()
    if ip is None:
        _INSTALLED = True
        return

    formatter = ip.display_formatter().mimebundle_formatter

    try:
        import pandas as pd

        formatter.for_type(pd.DataFrame, _pandas_formatter)
    except ImportError:
        pass

    try:
        import polars as pl

        formatter.for_type(pl.DataFrame, _polars_formatter)
    except ImportError:
        pass

    _INSTALLED = True


def _pandas_formatter(df: Any) -> dict:
    return _df_to_bundle(df, total_rows=len(df))


def _polars_formatter(df: Any) -> dict:
    return _df_to_bundle(df, total_rows=df.height)


def _df_to_bundle(df: Any, *, total_rows: int) -> dict:
    """Serialize, upload, and build the display bundle. Falls back on failure."""
    try:
        data, content_type = serialize_dataframe(df, max_bytes=_MAX_PAYLOAD_BYTES)
    except Exception as exc:
        log.debug("dx: serialize failed: %s — falling back to repr", exc)
        return {"text/plain": repr(df)}

    sampled = False
    included = total_rows
    # Read parquet metadata to detect downsampling (cheap — footer only).
    try:
        import io

        import pyarrow.parquet as pq

        meta = pq.read_metadata(io.BytesIO(data))
        if meta.num_rows != total_rows:
            sampled = True
            included = meta.num_rows
    except Exception:
        pass

    client = get_client()
    try:
        ref = client.put(data, content_type)
    except Exception as exc:
        log.debug("dx: upload failed: %s — falling back to raw-bytes display", exc)
        llm = summarize_dataframe(
            df, total_rows=total_rows, included_rows=included, sampled=sampled
        )
        return {content_type: data, "text/llm+plain": llm}

    summary = {
        "total_rows": total_rows,
        "included_rows": included,
        "sampled": sampled,
        "sample_strategy": "head" if sampled else "none",
    }
    bundle = build_ref_bundle(ref, content_type=content_type, summary=summary)
    llm = summarize_dataframe(df, total_rows=total_rows, included_rows=included, sampled=sampled)
    return {BLOB_REF_MIME: bundle, "text/llm+plain": llm}


def dx_display(obj: Any) -> None:
    """Upgraded display; hands off to IPython for non-DataFrame types."""
    from IPython.display import display

    display(obj)


def publish_ref_with_type(
    ref: BlobRef,
    *,
    content_type: str,
    summary: dict | None = None,
) -> None:
    bundle = build_ref_bundle(ref, content_type=content_type, summary=summary)
    _publish_display_data({BLOB_REF_MIME: bundle})
