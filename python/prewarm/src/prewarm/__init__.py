"""Warm up Python environments by importing packages and running IPython's init.

This module is both a standalone CLI tool (``python -m prewarm``) and is embedded
into the Rust daemon via ``include_str!`` for environments where the package
can't be pip-installed (conda/pixi pools).

The warmup has two phases:
1. ``compileall`` — pre-compile all ``.pyc`` files in site-packages
2. Imports — trigger expensive first-run side effects (font caches, C extensions, BLAS)
"""

from __future__ import annotations

import argparse
import compileall
import contextlib
import importlib

# Base packages always imported during warmup — these are the core
# notebook runtime dependencies whose first-import is expensive.
BASE_MODULES = [
    "ipykernel",
    "IPython",
    "ipywidgets",
    "anywidget",
    "nbformat",
]

# Additional imports for conda/pixi environments that bundle the
# full Jupyter runtime (traitlets config, zmq transport, comms).
CONDA_MODULES = [
    "traitlets",
    "zmq",
]

CONDA_DEEP_IMPORTS = [
    ("ipykernel.kernelbase", "Kernel"),
    ("ipykernel.ipkernel", "IPythonKernel"),
    ("ipykernel.comm", "CommManager"),
]


def warm(
    modules: list[str],
    *,
    ipython: bool = True,
    include_conda: bool = False,
    site_packages: str | None = None,
) -> None:
    """Import *modules* and optionally boot IPython to warm caches.

    Each module is imported inside a ``try``/``except`` so a single broken
    package never blocks the rest of the warmup.

    When *site_packages* is given, ``compileall.compile_dir()`` is run first
    to pre-compile all ``.pyc`` files in that directory.

    When *ipython* is ``True`` (the default) the function starts an embedded
    IPython session that runs the imports and exits immediately.  This warms
    IPython's own startup path (traitlets, magics, completer, display hooks)
    in addition to the requested packages.

    When *include_conda* is ``True``, additional conda-runtime imports
    (traitlets, zmq, CommManager) are included in the warmup.
    """
    if site_packages:
        _compile_site_packages(site_packages)

    all_modules = _collect_modules(modules, include_conda=include_conda)

    if ipython:
        _warm_via_ipython(all_modules, include_conda=include_conda)
    else:
        _warm_directly(all_modules, include_conda=include_conda)


def _compile_site_packages(path: str) -> None:
    """Pre-compile all .py files in site-packages to .pyc."""
    compileall.compile_dir(path, quiet=2, workers=0)


def _collect_modules(extra: list[str], *, include_conda: bool = False) -> list[str]:
    """Assemble the full module list: base + conda (optional) + user extras."""
    modules = list(BASE_MODULES)
    if include_conda:
        modules.extend(CONDA_MODULES)
    modules.extend(extra)
    # Deduplicate while preserving order
    seen: set[str] = set()
    result: list[str] = []
    for m in modules:
        if m not in seen:
            seen.add(m)
            result.append(m)
    return result


def _warm_directly(modules: list[str], *, include_conda: bool = False) -> None:
    """Import modules directly without IPython."""
    for m in modules:
        with contextlib.suppress(Exception):
            importlib.import_module(m)
    if include_conda:
        _import_conda_deep()


def _import_conda_deep() -> None:
    """Import conda-specific deep imports (CommManager, etc.)."""
    for mod, attr in CONDA_DEEP_IMPORTS:
        with contextlib.suppress(Exception):
            m = importlib.import_module(mod)
            getattr(m, attr)


def _warm_via_ipython(modules: list[str], *, include_conda: bool = False) -> None:
    """Warm modules by running them inside an IPython session."""
    try:
        import IPython
    except ImportError:
        _warm_directly(modules, include_conda=include_conda)
        return

    script = build_warmup_script(modules, include_conda=include_conda)
    IPython.start_ipython(argv=["--quick", "--no-banner", "-c", script])


def build_warmup_script(
    extra_modules: list[str],
    *,
    include_conda: bool = False,
    site_packages: str | None = None,
) -> str:
    """Build a self-contained Python script string for warmup.

    This is the script that gets embedded via ``include_str!`` in Rust
    and run via ``python -c``. It must be a single string that works
    standalone — no imports from the prewarm package itself.
    """
    lines: list[str] = []

    # Phase 1: compileall (always import, conditionally run)
    lines.append("import compileall")
    if site_packages:
        lines.append(f"compileall.compile_dir({site_packages!r}, quiet=2, workers=0)")

    # Phase 2: imports
    lines.append("import importlib")

    all_modules = _collect_modules(extra_modules, include_conda=include_conda)
    for m in all_modules:
        lines.append(f"try:\n    importlib.import_module({m!r})\nexcept Exception:\n    pass")

    # Deep imports (always — ipykernel classes)
    lines.append(
        "try:\n"
        "    from ipykernel.kernelbase import Kernel\n"
        "    from ipykernel.ipkernel import IPythonKernel\n"
        "except Exception:\n"
        "    pass"
    )

    if include_conda:
        for mod, attr in CONDA_DEEP_IMPORTS:
            lines.append(f"try:\n    from {mod} import {attr}\nexcept Exception:\n    pass")

    lines.append('print("warmup complete")')
    return "\n".join(lines)


def main(argv: list[str] | None = None) -> None:
    parser = argparse.ArgumentParser(
        prog="prewarm",
        description="Warm up a Python environment by importing packages.",
    )
    parser.add_argument(
        "modules",
        nargs="*",
        help="Module names to import (e.g. matplotlib pandas numpy).",
    )
    parser.add_argument(
        "--no-ipython",
        action="store_true",
        help="Skip IPython startup, just import modules directly.",
    )
    parser.add_argument(
        "--include-conda",
        action="store_true",
        help="Include conda-runtime imports (traitlets, zmq, CommManager).",
    )
    parser.add_argument(
        "--site-packages",
        default=None,
        help="Path to site-packages directory for compileall pre-compilation.",
    )
    args = parser.parse_args(argv)
    warm(
        args.modules,
        ipython=not args.no_ipython,
        include_conda=args.include_conda,
        site_packages=args.site_packages,
    )


if __name__ == "__main__":
    main()
