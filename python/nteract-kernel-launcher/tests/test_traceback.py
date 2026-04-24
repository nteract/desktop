"""Unit tests for the bulletproof traceback emitter.

The short-circuit path is easy to test. The critical assertion is the
safety invariant: the user ALWAYS gets a traceback, even when our own
code blows up in creative ways.
"""

from __future__ import annotations

from types import SimpleNamespace

import pytest
from nteract_kernel_launcher import _traceback
from nteract_kernel_launcher._traceback import TRACEBACK_MIME, build_rich_payload, install

# ─── build_rich_payload ────────────────────────────────────────────────────


def _capture_exc() -> BaseException:
    try:
        raise KeyError("missing_key")
    except BaseException as exc:
        return exc


def test_build_payload_shape():
    exc = _capture_exc()
    payload = build_rich_payload(type(exc), exc, exc.__traceback__)
    assert payload["ename"] == "KeyError"
    assert payload["evalue"] == "'missing_key'"
    assert payload["language"] == "python"
    assert payload["text"].startswith("Traceback (most recent call last):")
    assert len(payload["frames"]) >= 1
    top = payload["frames"][-1]
    assert "filename" in top and "lineno" in top and "name" in top
    assert isinstance(top["library"], bool)


def test_build_payload_marks_highlight_on_fail_line():
    exc = _capture_exc()
    payload = build_rich_payload(type(exc), exc, exc.__traceback__)
    highlights = [
        line
        for frame in payload["frames"]
        for line in (frame.get("lines") or [])
        if line.get("highlight")
    ]
    # Each frame that has any lines should have exactly one highlighted entry
    # at its failing lineno.
    assert len(highlights) >= 1


# ─── leading-library-frame strip ───────────────────────────────────────────


def test_strip_leading_library_frames_removes_ipython_run_code():
    # Simulate the real-world shape: [IPython.run_code, user <module>]
    raw = [
        {
            "filename": "/opt/python/site-packages/IPython/core/interactiveshell.py",
            "lineno": 3747,
            "name": "run_code",
            "lines": [],
            "library": True,
        },
        {
            "filename": "/tmp/ipykernel_1/abc.py",
            "lineno": 1,
            "name": "<module>",
            "lines": [],
            "library": False,
        },
    ]
    out = _traceback._strip_leading_library_frames(raw)
    assert len(out) == 1
    assert out[0]["name"] == "<module>"


def test_strip_leading_library_frames_keeps_intermediate_library():
    raw = [
        {
            "filename": "/opt/py/site-packages/ipy.py",
            "lineno": 1,
            "name": "run_code",
            "library": True,
        },
        {"filename": "/tmp/ipykernel_1/abc.py", "lineno": 1, "name": "<module>", "library": False},
        {
            "filename": "/opt/py/site-packages/pandas/x.py",
            "lineno": 9,
            "name": "helper",
            "library": True,
        },
    ]
    out = _traceback._strip_leading_library_frames(raw)
    # Only the leading library frame is dropped; the pandas frame stays.
    assert [f["name"] for f in out] == ["<module>", "helper"]


def test_strip_leading_library_frames_keeps_everything_when_all_library():
    raw = [
        {"filename": "/opt/py/site-packages/a.py", "lineno": 1, "name": "load", "library": True},
        {"filename": "/opt/py/site-packages/b.py", "lineno": 2, "name": "parse", "library": True},
    ]
    out = _traceback._strip_leading_library_frames(raw)
    assert out == raw


# ─── install: wrapping + idempotency ───────────────────────────────────────


class _FakeShell:
    """Minimal stand-in for ZMQInteractiveShell that exercises the hook."""

    def __init__(self):
        self.original_calls = []

        def _original(_self, etype, evalue, stb):
            self.original_calls.append((etype, evalue, stb))

        # Bind as a bound method so MethodType can replicate the real shape.
        import types as _t

        self._showtraceback = _t.MethodType(_original, self)


def test_install_replaces_showtraceback_and_tags_for_idempotency(monkeypatch):
    captured = []

    def _fake_publish(data=None, metadata=None):
        captured.append({"data": data, "metadata": metadata})

    monkeypatch.setattr("IPython.display.publish_display_data", _fake_publish)

    ip = _FakeShell()
    install(ip)
    assert getattr(ip._showtraceback, "_nteract_installed", False) is True

    # Trigger via a real exception.
    try:
        raise ValueError("boom")
    except BaseException as exc:
        ip._showtraceback(type(exc), exc, ["traceback-stb"])

    assert len(captured) == 1
    assert TRACEBACK_MIME in captured[0]["data"]
    payload = captured[0]["data"][TRACEBACK_MIME]
    assert payload["ename"] == "ValueError"
    assert payload["evalue"] == "boom"

    # Idempotent re-install must not wrap-the-wrapper.
    install(ip)
    assert getattr(ip._showtraceback, "_nteract_installed", False) is True
    assert ip._showtraceback.__func__ is not None  # still bound


def test_fallback_when_build_payload_fails(monkeypatch):
    """If payload construction raises, the ORIGINAL shell must be called."""

    def _boom(*_a, **_kw):
        raise RuntimeError("kaboom")

    monkeypatch.setattr(_traceback, "build_rich_payload", _boom)

    captured = []
    monkeypatch.setattr(
        "IPython.display.publish_display_data",
        lambda *_a, **_kw: captured.append("should-not-be-called"),
    )

    ip = _FakeShell()
    install(ip)

    try:
        raise ValueError("seen-by-user")
    except BaseException as exc:
        ip._showtraceback(type(exc), exc, ["stb-line-1"])

    # Our publish path was aborted, original ran.
    assert captured == []
    assert len(ip.original_calls) == 1
    et, ev, stb = ip.original_calls[0]
    assert et is ValueError
    assert str(ev) == "seen-by-user"
    assert stb == ["stb-line-1"]


def test_fallback_when_publish_fails(monkeypatch):
    """publish_display_data raising must still hand off to the original."""

    def _boom(*_a, **_kw):
        raise OSError("ipub broken")

    monkeypatch.setattr("IPython.display.publish_display_data", _boom)

    ip = _FakeShell()
    install(ip)
    try:
        raise ValueError("seen")
    except BaseException as exc:
        ip._showtraceback(type(exc), exc, ["stb"])

    assert len(ip.original_calls) == 1


def test_systemexit_is_not_swallowed(monkeypatch):
    """SystemExit from payload path must propagate (not be caught)."""

    def _exit(*_a, **_kw):
        raise SystemExit(0)

    monkeypatch.setattr(_traceback, "build_rich_payload", _exit)

    ip = _FakeShell()
    install(ip)
    with pytest.raises(SystemExit):
        try:
            raise ValueError("x")
        except BaseException as exc:
            ip._showtraceback(type(exc), exc, [])


def test_keyboardinterrupt_is_not_swallowed(monkeypatch):
    def _int(*_a, **_kw):
        raise KeyboardInterrupt()

    monkeypatch.setattr(_traceback, "build_rich_payload", _int)

    ip = _FakeShell()
    install(ip)
    with pytest.raises(KeyboardInterrupt):
        try:
            raise ValueError("x")
        except BaseException as exc:
            ip._showtraceback(type(exc), exc, [])


def test_original_also_failing_does_not_reraise(monkeypatch):
    """If BOTH our path AND the original path fail, we must still not
    raise out to the user — there's nothing more we can usefully do."""

    def _boom(*_a, **_kw):
        raise RuntimeError("payload broke")

    monkeypatch.setattr(_traceback, "build_rich_payload", _boom)

    ip = SimpleNamespace()

    def _bad_original(_self, _etype, _evalue, _stb):
        raise OSError("original also broke")

    import types as _t

    ip._showtraceback = _t.MethodType(_bad_original, ip)

    install(ip)
    # Must not raise.
    try:
        raise ValueError("x")
    except BaseException as exc:
        ip._showtraceback(type(exc), exc, [])
