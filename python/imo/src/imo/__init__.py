"""imo: Marimo-compatible display primitives for IPython/Jupyter.

Usage::

    import imo as mo

    mo.md("# Hello")
    mo.callout("Important note", kind="warn")
    mo.stat(42, label="Users")
    mo.vstack([mo.stat(42, label="Users"), mo.stat(99, label="Score")])
"""

from imo._callout import callout
from imo._html import Html
from imo._layout import hstack, vstack
from imo._md import md
from imo._stat import stat

__all__ = [
    "Html",
    "callout",
    "hstack",
    "md",
    "stat",
    "vstack",
]
