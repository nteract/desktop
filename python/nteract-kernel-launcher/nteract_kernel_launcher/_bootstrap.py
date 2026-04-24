"""IPython extension — the launcher-hosted bootstrap.

Loaded automatically via ``NteractKernelApp.default_extensions`` before any
user code runs. Three things happen on ``load_ipython_extension``:

1. ``mimebundle_formatter`` entries for pandas / polars / narwhals /
   datasets are registered by **dotted type name** (no eager imports).
   IPython binds them lazily on first encounter — dx's historical
   ``import pandas as pd`` at install time becomes unnecessary.

2. :func:`_buffer_hook.install` registers the single buffer-attachment
   hook on *both* ``ip.display_pub`` and ``ip.displayhook``. With the
   hook chain in place on both seats, ``execute_result`` (bare ``df``
   on last line) and ``display_data`` / ``update_display_data`` all
   pick up parquet buffers from the same pending-bytes stash.

3. Third-party visualization libraries (altair, plotly) are flipped to
   their ``"nteract"`` renderer if they happen to be importable. Each
   is guarded so the bootstrap stays a no-op in minimal envs.

No ``ipython_display_formatter`` handlers. The dx wheel needed one to
divert bare-``df``-on-last-line from the bufferless displayhook path
to ``publish_display_data`` (where the publisher hook could attach
buffers). With the ``NteractShellDisplayHook`` hook chain, the default
displayhook path carries buffers natively — the short-circuit becomes
unnecessary.
"""

from __future__ import annotations

import hashlib
import io
import logging
import os
from typing import Any

from nteract_kernel_launcher import _buffer_hook, _traceback
from nteract_kernel_launcher._buffer_hook import pending_buffers
from nteract_kernel_launcher._format import serialize_dataframe
from nteract_kernel_launcher._refs import BLOB_REF_MIME, BlobRef, build_ref_bundle
from nteract_kernel_launcher._summary import summarize_dataframe, summarize_dataset

log = logging.getLogger("nteract_kernel_launcher")

# Server-side blob ceiling is ~100 MiB; leave ~10 MiB for overhead.
_MAX_PAYLOAD_BYTES = int(os.environ.get("DX_MAX_PAYLOAD_BYTES", str(90 * 1024 * 1024)))


# ─── mimebundle formatters (lazy-bound via for_type_by_name) ──────────────


def _pandas_mimebundle(df: Any, include=None, exclude=None) -> dict | None:
    return _emit_dataframe(df, total_rows=len(df))


def _polars_mimebundle(df: Any, include=None, exclude=None) -> dict | None:
    return _emit_dataframe(df, total_rows=df.height)


def _narwhals_mimebundle(df: Any, include=None, exclude=None) -> dict | None:
    native, total_rows = _unwrap_narwhals(df)
    if native is None:
        return None
    return _emit_dataframe(native, total_rows=total_rows)


def _dataset_mimebundle(ds: Any, include=None, exclude=None) -> dict | None:
    try:
        return {"text/llm+plain": summarize_dataset(ds)}
    except Exception as exc:  # noqa: BLE001
        log.debug("dataset mimebundle failed: %s", exc)
        return None


def _unwrap_narwhals(nw_df: Any) -> tuple[Any, int] | tuple[None, int]:
    """Return ``(native_df, row_count)`` for a ``narwhals.DataFrame``.

    Fast path: native is pandas or polars — pass through. Fallback:
    round-trip via ``.to_pandas()`` for any backend narwhals understands
    (pyarrow, modin, dask, cudf). ``LazyFrame`` isn't handled here —
    displaying a lazy query would force ``.collect()``, which has side
    effects.
    """
    try:
        native = nw_df.to_native()
    except Exception as exc:  # noqa: BLE001
        log.debug("narwhals to_native failed: %s", exc)
        return None, 0

    try:
        import pandas as pd

        if isinstance(native, pd.DataFrame):
            return native, len(native)
    except ImportError:
        pass
    try:
        import polars as pl

        if isinstance(native, pl.DataFrame):
            return native, native.height
    except ImportError:
        pass

    try:
        as_pandas = nw_df.to_pandas()
        return as_pandas, len(as_pandas)
    except Exception as exc:  # noqa: BLE001
        log.debug("narwhals to_pandas failed: %s", exc)
        return None, 0


def _emit_dataframe(df: Any, *, total_rows: int) -> dict | None:
    """Serialize → stash bytes keyed by sha256 → return ref-mime bundle.

    When parquet serialization fails (e.g. pyarrow missing), fall back to
    a summary-only ``text/llm+plain`` bundle so agents still get column
    stats instead of a raw repr. IPython's default formatter chain merges
    pandas' ``text/html`` / ``text/plain`` on top for host-side fallback.
    """
    try:
        data, content_type = serialize_dataframe(df, max_bytes=_MAX_PAYLOAD_BYTES)
    except Exception as exc:  # noqa: BLE001
        log.debug("serialize failed: %s — emitting summary-only bundle", exc)
        try:
            llm = summarize_dataframe(
                df, total_rows=total_rows, included_rows=total_rows, sampled=False
            )
            return {"text/llm+plain": llm}
        except Exception:  # noqa: BLE001
            return None

    # Detect downsampling by reading parquet metadata (footer only, cheap).
    sampled = False
    included = total_rows
    try:
        import pyarrow.parquet as pq

        meta = pq.read_metadata(io.BytesIO(data))
        if meta.num_rows != total_rows:
            sampled = True
            included = meta.num_rows
    except Exception:  # noqa: BLE001
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

    pending_buffers()[h] = data
    return {BLOB_REF_MIME: ref_bundle, "text/llm+plain": llm}


# ─── Install functions — called from load_ipython_extension ───────────────


def _install_dataframe_formatters(ip: Any) -> None:
    """Register mimebundle formatters lazily via ``for_type_by_name``.

    IPython's ``for_type_by_name`` keys on ``type.__module__`` + class
    name. Library-by-library that's:

    - pandas: ``pandas`` (pandas sets ``DataFrame.__module__ = "pandas"``)
    - polars: ``polars.dataframe.frame``
    - narwhals: ``narwhals.dataframe``
    - datasets: ``datasets.arrow_dataset``

    We register the definitive module each library stamps on the class,
    plus a short-name fallback for pandas/polars/narwhals. Short names
    cover both conservative stamping by the library and any future
    flattening. Registration is cheap and idempotent, so the spray
    costs nothing.
    """
    mimebundle = ip.display_formatter.mimebundle_formatter

    # (module_path, type_name, formatter). Keep definitive entries first so
    # a first-dispatch lookup short-circuits before the fallbacks matter.
    for module_path, type_name, fn in (
        # pandas — public DataFrame ships with __module__ == "pandas".
        ("pandas", "DataFrame", _pandas_mimebundle),
        ("pandas.core.frame", "DataFrame", _pandas_mimebundle),
        # polars — class module is the deep path; register both.
        ("polars.dataframe.frame", "DataFrame", _polars_mimebundle),
        ("polars", "DataFrame", _polars_mimebundle),
        # narwhals — class lives in narwhals.dataframe.
        ("narwhals.dataframe", "DataFrame", _narwhals_mimebundle),
        ("narwhals", "DataFrame", _narwhals_mimebundle),
        # datasets — single definitive path.
        ("datasets.arrow_dataset", "Dataset", _dataset_mimebundle),
    ):
        try:
            mimebundle.for_type_by_name(module_path, type_name, fn)
        except Exception as exc:  # noqa: BLE001
            log.debug("for_type_by_name(%s.%s) failed: %s", module_path, type_name, exc)


def _install_buffer_hooks(ip: Any) -> None:
    """Delegate to :mod:`_buffer_hook` — registers on both seats."""
    _buffer_hook.install(ip)


def _enable_third_party_renderers() -> None:
    """Flip altair / plotly to their ``"nteract"`` renderer if present.

    ImportError-guarded; safe in a minimal env. Non-ImportError failures
    get logged at debug level — a missing renderer shouldn't crash the
    kernel startup.
    """
    try:
        import altair as alt  # ty: ignore[unresolved-import]

        alt.renderers.enable("nteract")
        log.debug("enabled altair 'nteract' renderer")
    except ImportError:
        pass
    except Exception as exc:  # noqa: BLE001
        log.debug("altair 'nteract' renderer enable failed: %s", exc)

    try:
        import plotly.io as pio

        pio.renderers.default = "nteract"
        log.debug("set plotly default renderer to 'nteract'")
    except ImportError:
        pass
    except Exception as exc:  # noqa: BLE001
        log.debug("plotly 'nteract' renderer set failed: %s", exc)


# ─── IPython extension contract ────────────────────────────────────────────


def load_ipython_extension(ip: Any) -> None:
    """Entry point invoked by ``ExtensionManager.load_extension``.

    Failures here are log-warned by IPython rather than raised, so a
    missing dependency never presents as a traceback to the user. We
    further guard each install step so a single failure doesn't abort
    the others.
    """
    try:
        _install_dataframe_formatters(ip)
    except Exception as exc:  # noqa: BLE001
        log.warning("dataframe formatter install failed: %s", exc)

    try:
        _install_buffer_hooks(ip)
    except Exception as exc:  # noqa: BLE001
        log.warning("buffer hook install failed: %s", exc)

    try:
        _enable_third_party_renderers()
    except Exception as exc:  # noqa: BLE001
        log.warning("third-party renderer enable failed: %s", exc)

    # Traceback install goes last so earlier failures can't prevent it.
    # The wrapper itself is bulletproof — see `_traceback.install`.
    try:
        _traceback.install(ip)
    except Exception as exc:  # noqa: BLE001
        log.warning("traceback install failed: %s", exc)


def unload_ipython_extension(ip: Any) -> None:
    """Best-effort symmetry with ``load``. Kernel teardown tears the
    process down, so precise reversal isn't load-bearing — but leaving
    this defined keeps ``%reload_ext nteract_kernel_launcher._bootstrap``
    well-behaved during dev iteration.
    """
    try:
        for pub in (ip.display_pub, ip.displayhook):
            hooks = list(getattr(pub, "_hooks", []))
            for h in hooks:
                if getattr(h, "_nteract_installed", False):
                    unregister = getattr(pub, "unregister_hook", None)
                    if unregister is not None:
                        unregister(h)
    except Exception as exc:  # noqa: BLE001
        log.debug("unload cleanup: %s", exc)
