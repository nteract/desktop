"""Tests for the prewarm package."""

from __future__ import annotations

import subprocess
import sys


def test_warm_directly_with_stdlib_module():
    """warm() with ipython=False should import stdlib modules without error."""
    from prewarm import warm

    # json is always available — should not raise
    warm(["json", "os"], ipython=False)


def test_warm_directly_skips_missing_modules():
    """Missing modules should be silently skipped."""
    from prewarm import warm

    # Should not raise even though this module doesn't exist
    warm(["__nonexistent_module_xyz__"], ipython=False)


def test_collect_modules_base_only():
    """Base modules are always included."""
    from prewarm import BASE_MODULES, _collect_modules

    result = _collect_modules([], include_conda=False)
    for m in BASE_MODULES:
        assert m in result


def test_collect_modules_with_conda():
    """--include-conda adds traitlets and zmq."""
    from prewarm import CONDA_MODULES, _collect_modules

    result = _collect_modules([], include_conda=True)
    for m in CONDA_MODULES:
        assert m in result


def test_collect_modules_deduplicates():
    """Duplicate modules should be removed while preserving order."""
    from prewarm import _collect_modules

    result = _collect_modules(["ipykernel", "numpy"], include_conda=False)
    assert result.count("ipykernel") == 1


def test_build_warmup_script_basic():
    """build_warmup_script produces valid Python with module imports."""
    from prewarm import build_warmup_script

    script = build_warmup_script(["numpy", "pandas"], include_conda=False)
    assert "numpy" in script
    assert "pandas" in script
    assert "compileall" in script
    assert "warmup complete" in script


def test_build_warmup_script_include_conda():
    """--include-conda adds traitlets, zmq, and CommManager imports."""
    from prewarm import build_warmup_script

    script = build_warmup_script([], include_conda=True)
    assert "traitlets" in script
    assert "zmq" in script
    assert "CommManager" in script


def test_build_warmup_script_no_conda():
    """Without --include-conda, conda-specific imports are absent."""
    from prewarm import build_warmup_script

    script = build_warmup_script([], include_conda=False)
    assert "CommManager" not in script


def test_build_warmup_script_with_site_packages():
    """--site-packages adds a compileall.compile_dir() call."""
    from prewarm import build_warmup_script

    script = build_warmup_script([], include_conda=False, site_packages="/fake/path")
    assert "compileall" in script
    assert "/fake/path" in script


def test_cli_no_ipython_flag():
    """prewarm --no-ipython json should exit 0."""
    result = subprocess.run(
        [sys.executable, "-m", "prewarm", "--no-ipython", "json"],
        capture_output=True,
        timeout=10,
    )
    assert result.returncode == 0


def test_cli_help():
    """prewarm --help should exit 0."""
    result = subprocess.run(
        [sys.executable, "-m", "prewarm", "--help"],
        capture_output=True,
        timeout=10,
    )
    assert result.returncode == 0
