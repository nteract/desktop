"""Detect the runtime environment dx is operating in."""

from __future__ import annotations

from enum import Enum
from typing import Any


class Environment(str, Enum):
    PLAIN_PYTHON = "plain_python"
    IPYTHON_NO_KERNEL = "ipython_no_kernel"
    IPYKERNEL = "ipykernel"


def _get_ipython() -> Any | None:
    """Return the active IPython instance, or ``None``.

    Extracted for test monkeypatching.
    """
    try:
        from IPython import get_ipython as _gi
    except ImportError:
        return None
    return _gi()


def detect_environment() -> Environment:
    """Classify the current runtime."""
    ip = _get_ipython()
    if ip is None:
        return Environment.PLAIN_PYTHON
    if getattr(ip, "kernel", None) is None:
        return Environment.IPYTHON_NO_KERNEL
    return Environment.IPYKERNEL
