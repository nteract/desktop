"""Tests for the bootstrap step. The hand-off to ipykernel is exercised by
integration tests; here we only check the feature-flagged bootstrap logic.
"""

from __future__ import annotations

import sys
import types
from unittest.mock import patch

import nteract_kernel_launcher


def test_bootstrap_noop_without_flag(monkeypatch):
    monkeypatch.delenv("RUNT_BOOTSTRAP_DX", raising=False)
    fake_dx = types.SimpleNamespace(
        install=lambda: (_ for _ in ()).throw(AssertionError("should not run"))
    )
    with patch.dict(sys.modules, {"dx": fake_dx}):
        nteract_kernel_launcher.bootstrap()


def test_bootstrap_calls_dx_install_when_flag_set(monkeypatch):
    monkeypatch.setenv("RUNT_BOOTSTRAP_DX", "1")
    calls: list[bool] = []
    fake_dx = types.SimpleNamespace(install=lambda: calls.append(True))
    with patch.dict(sys.modules, {"dx": fake_dx}):
        nteract_kernel_launcher.bootstrap()
    assert calls == [True]


def test_bootstrap_survives_missing_dx(monkeypatch):
    monkeypatch.setenv("RUNT_BOOTSTRAP_DX", "1")
    # Ensure dx is not importable
    monkeypatch.setitem(sys.modules, "dx", None)
    nteract_kernel_launcher.bootstrap()


def test_bootstrap_survives_dx_install_error(monkeypatch, capsys):
    monkeypatch.setenv("RUNT_BOOTSTRAP_DX", "1")

    def boom() -> None:
        raise RuntimeError("boom")

    fake_dx = types.SimpleNamespace(install=boom)
    with patch.dict(sys.modules, {"dx": fake_dx}):
        nteract_kernel_launcher.bootstrap()
    assert "dx.install() failed" in capsys.readouterr().err
