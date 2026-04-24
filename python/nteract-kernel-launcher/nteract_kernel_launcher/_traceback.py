"""Rich traceback short-circuit for the nteract-kernel-launcher.

Replaces ``ZMQInteractiveShell._showtraceback`` with a wrapper that
builds a structured payload (file/line/name per frame with a source
window and highlight) and publishes a ``display_data`` carrying
``application/vnd.nteract.traceback+json``. One output per exception,
same shape the in-session prototype used.

**Safety invariant: this code MUST NEVER prevent a user from seeing a
traceback.** Every code path is wrapped so the original
``_showtraceback`` runs if anything goes wrong. The user would rather
see plain ANSI output than nothing.

Why this lives in a tiny module of its own: it has to be dead-simple
to audit. Reviewers should be able to read the file end-to-end and
convince themselves that no user-triggered exception can sabotage
error reporting.
"""

from __future__ import annotations

import contextlib
import linecache
import logging
import os
import traceback as _pytraceback
import types
from typing import Any

log = logging.getLogger("nteract_kernel_launcher")

TRACEBACK_MIME = "application/vnd.nteract.traceback+json"
"""Matches `src/components/outputs/traceback-output.tsx` on the frontend."""

_LIBRARY_PATH_MARKERS = (
    "site-packages",
    "dist-packages",
    "lib/python",
    "lib\\python",
    "python.framework",
)

_CONTEXT_BEFORE = 2
_CONTEXT_AFTER = 2


# ─── Payload construction ───────────────────────────────────────────────────


def _is_library_frame(filename: str) -> bool:
    if not filename:
        return True
    norm = os.path.normpath(filename).lower()
    return any(m in norm for m in _LIBRARY_PATH_MARKERS)


def _source_window(filename: str, lineno: int) -> list[dict[str, Any]]:
    """Return the source-context lines around ``lineno`` (inclusive range)."""
    out: list[dict[str, Any]] = []
    start = max(1, lineno - _CONTEXT_BEFORE)
    end = lineno + _CONTEXT_AFTER
    for n in range(start, end + 1):
        src = linecache.getline(filename, n)
        if not src:
            continue
        entry: dict[str, Any] = {"lineno": n, "source": src.rstrip("\n")}
        if n == lineno:
            entry["highlight"] = True
        out.append(entry)
    return out


def _strip_leading_library_frames(frames: list[dict[str, Any]]) -> list[dict[str, Any]]:
    """Drop library frames above the first user frame.

    IPython's ``run_code`` wraps every cell execution, so a cell raising
    a bare ``NameError`` ends up with two frames: the IPython wrapper and
    the user's cell. The wrapper is pure ceremony — no user code lives
    there — so we strip it.

    Any library frames *after* a user frame are kept: if a user call into
    a library raised inside that library, those frames carry real info.

    If *every* frame is library (e.g. a failing ``import`` inside a
    worker thread before any user code runs), keep everything so we
    don't emit an empty stack.
    """
    if not any(not f.get("library") for f in frames):
        return frames
    for i, f in enumerate(frames):
        if not f.get("library"):
            return frames[i:]
    return frames


def build_rich_payload(etype: Any, evalue: Any, tb: Any) -> dict[str, Any]:
    """Structure an exception into the rich traceback payload.

    Assumes the caller protects against exceptions from this function —
    see `_safe_showtraceback` below.
    """
    raw_frames = []
    for f in _pytraceback.extract_tb(tb):
        # `FrameSummary.lineno` is typed as `int | None`; treat missing
        # as 0 so the manifest stays numeric. `linecache.getline` with
        # lineno=0 returns "" which is what we want (no context).
        lineno = f.lineno or 0
        raw_frames.append(
            {
                "filename": f.filename,
                "lineno": lineno,
                "name": f.name,
                "lines": _source_window(f.filename, lineno),
                "library": _is_library_frame(f.filename),
            }
        )
    frames = _strip_leading_library_frames(raw_frames)
    text = "".join(_pytraceback.format_exception(etype, evalue, tb))
    ename = etype.__name__ if isinstance(etype, type) else str(etype)
    return {
        "ename": ename,
        "evalue": str(evalue),
        "frames": frames,
        "language": "python",
        "text": text,
    }


# ─── Safe showtraceback wrapper ─────────────────────────────────────────────


def install(ip: Any) -> None:
    """Install a safe, short-circuiting ``_showtraceback`` on *ip*.

    Tagged with ``_nteract_installed`` so re-installs (e.g. dev
    hot-reload) don't stack.
    """
    existing = getattr(ip, "_showtraceback", None)
    if existing is not None and getattr(existing, "_nteract_installed", False):
        return

    original = existing

    def _safe_showtraceback(self: Any, etype: Any, evalue: Any, stb: Any) -> None:
        """Emit the rich payload, falling back to *original* on any error.

        Catches ``BaseException`` rather than ``Exception`` so nothing a
        user can trigger inside payload construction or the publish call
        can take down the error path. ``SystemExit`` and
        ``KeyboardInterrupt`` are re-raised — those are intentional
        control flow.
        """
        try:
            # Lazy import inside the function body so a missing IPython
            # at extension-load time never strands us without a traceback
            # path.
            from IPython.display import publish_display_data

            tb = evalue.__traceback__ if isinstance(evalue, BaseException) else None
            payload = build_rich_payload(etype, evalue, tb)
            publish_display_data(data={TRACEBACK_MIME: payload}, metadata={})
        except (SystemExit, KeyboardInterrupt):
            # Intentional control flow — propagate.
            raise
        except BaseException as err:  # noqa: BLE001
            # Anything else — including MemoryError, RecursionError,
            # OSError from IPython internals — must not starve the
            # user of a traceback. Log at debug (we're literally inside
            # the error path; loud logging is worse than silent fallback).
            log.debug("rich traceback fallback: %r", err)
            if original is not None:
                # If the *original* also fails, there's nothing more we
                # can usefully do. Swallow to avoid obscuring the root
                # exception with a meta-error.
                with contextlib.suppress(BaseException):
                    original(etype, evalue, stb)

    # Tag for idempotency.
    _safe_showtraceback._nteract_installed = True  # type: ignore[attr-defined]

    ip._showtraceback = types.MethodType(_safe_showtraceback, ip)
