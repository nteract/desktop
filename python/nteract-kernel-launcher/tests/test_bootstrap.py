"""Unit tests for the bootstrap wiring.

These cover the argv rewriting and feature-flag gating. The hand-off to
ipykernel itself is exercised by integration tests.
"""

from __future__ import annotations

import sys
from pathlib import Path

# The module is a single file at the package root, not under src/.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
import nteract_kernel_launcher as nkl  # noqa: E402


def test_no_exec_lines_without_flag(monkeypatch):
    monkeypatch.delenv("RUNT_BOOTSTRAP_DX", raising=False)
    assert nkl.enabled_exec_lines() == []


def test_dx_exec_line_when_flag_set(monkeypatch):
    monkeypatch.setenv("RUNT_BOOTSTRAP_DX", "1")
    lines = nkl.enabled_exec_lines()
    assert len(lines) == 1
    assert "dx" in lines[0]
    assert "install" in lines[0]
    assert "\n" not in lines[0]


def test_inject_exec_lines_appends_args():
    argv = ["nteract_kernel_launcher", "-f", "/tmp/conn.json"]
    nkl._inject_exec_lines(argv, ["import dx; dx.install()"])
    assert argv[:3] == ["nteract_kernel_launcher", "-f", "/tmp/conn.json"]
    assert argv[3] == "--IPKernelApp.exec_lines=import dx; dx.install()"


def test_inject_exec_lines_noop_on_empty():
    argv = ["nteract_kernel_launcher", "-f", "/tmp/conn.json"]
    before = list(argv)
    nkl._inject_exec_lines(argv, [])
    assert argv == before
