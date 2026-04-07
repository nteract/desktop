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

    result = _collect_modules(["ipywidgets", "numpy"], include_conda=False)
    assert result.count("ipywidgets") == 1


def test_collect_modules_normalizes_specs():
    """Version specifiers and hyphens are normalized to import names."""
    from prewarm import _collect_modules

    result = _collect_modules(["numpy>=1.24", "scikit-learn>=1.0"], include_conda=False)
    assert "numpy" in result
    assert "scikit_learn" in result
    assert "numpy>=1.24" not in result


def test_normalize_module_name():
    """normalize_module_name strips specs and converts hyphens."""
    from prewarm import normalize_module_name

    assert normalize_module_name("numpy>=1.24") == "numpy"
    assert normalize_module_name("scikit-learn>=1.0") == "scikit_learn"
    assert normalize_module_name("pandas") == "pandas"
    assert normalize_module_name("Pillow[extra]") == "Pillow"
    assert normalize_module_name("") is None


def test_build_warmup_script_critical_imports_not_wrapped():
    """Critical imports (ipykernel, IPython) must NOT be in try/except."""
    from prewarm import build_warmup_script

    script = build_warmup_script([], include_conda=False)
    # Critical imports should be bare import statements
    assert "import ipykernel" in script
    assert "import IPython" in script
    # They should NOT be wrapped in try/except
    lines = script.split("\n")
    for i, line in enumerate(lines):
        if line.strip() == "import ipykernel":
            assert i == 0 or "try" not in lines[i - 1]
        if line.strip() == "import IPython":
            assert i == 0 or "try" not in lines[i - 1]


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
