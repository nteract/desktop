"""Warm up Python environments by importing packages and running IPython's init."""

from __future__ import annotations

import argparse
import importlib
import sys


def warm(modules: list[str], *, ipython: bool = True) -> None:
    """Import *modules* and optionally boot IPython to warm its caches.

    Each module is imported inside a ``try``/``except`` so a single broken
    package never blocks the rest of the warmup.

    When *ipython* is ``True`` (the default) the function starts an embedded
    IPython session that runs the imports and exits immediately.  This warms
    IPython's own startup path (traitlets, magics, completer, display hooks)
    in addition to the requested packages.
    """
    if ipython:
        _warm_via_ipython(modules)
    else:
        _warm_directly(modules)


def _warm_directly(modules: list[str]) -> None:
    for m in modules:
        try:
            importlib.import_module(m)
        except Exception:
            pass


def _warm_via_ipython(modules: list[str]) -> None:
    try:
        import IPython
    except ImportError:
        _warm_directly(modules)
        return

    code = "\n".join(
        [
            "import importlib",
            *(
                f"try:\n    importlib.import_module({m!r})\nexcept Exception:\n    pass"
                for m in modules
            ),
        ]
    )

    IPython.start_ipython(argv=["--quick", "--no-banner", "-c", code])


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
    args = parser.parse_args(argv)
    warm(args.modules, ipython=not args.no_ipython)


if __name__ == "__main__":
    main()
