"""nteract-kernel-launcher — wrapper around ipykernel_launcher with kernel bootstrap.

Run via ``python -m nteract_kernel_launcher -f <connection_file>``.
"""

from __future__ import annotations

import os
import sys


def _bootstrap_dx() -> None:
    try:
        import dx
    except ImportError:
        return
    try:
        dx.install()
    except Exception as exc:
        print(f"[nteract-kernel-launcher] dx.install() failed: {exc!r}", file=sys.stderr)


def bootstrap() -> None:
    """Run all enabled bootstrap steps based on environment variables."""
    if os.environ.get("RUNT_BOOTSTRAP_DX"):
        _bootstrap_dx()


def main() -> None:
    """Run bootstrap, then hand off to ipykernel_launcher."""
    bootstrap()
    from ipykernel import kernelapp

    kernelapp.launch_new_instance()
