"""Markdown display: mo.md()."""

from __future__ import annotations

import inspect
from typing import Any

from imo._html import Html


def md(text: str) -> Html:
    """Render markdown text for display.

    Uses ``text/markdown`` MIME type (already handled by Jupyter frontends).
    Supports f-string interpolation of other imo objects::

        mo.md(f"# Results\\n{mo.stat(42, label='Users')}")

    Args:
        text: Markdown string. Leading indentation is stripped via
            ``inspect.cleandoc``.

    Returns:
        Html object with ``text/markdown`` in its MIME bundle.
    """
    cleaned = inspect.cleandoc(text)
    return _MarkdownHtml(cleaned)


class _MarkdownHtml(Html):
    """Html subclass that emits text/markdown as its primary MIME type."""

    def __init__(self, text: str) -> None:
        self._markdown_text = text
        # Render HTML fallback using Python's markdown library
        try:
            import markdown as md_lib

            html = md_lib.markdown(text, extensions=["fenced_code", "tables"])
        except ImportError:
            # Minimal fallback: wrap in <pre> if markdown lib not available
            from html import escape

            html = f"<pre>{escape(text)}</pre>"
        super().__init__(html)

    def _repr_mimebundle_(
        self, **kwargs: Any
    ) -> tuple[dict[str, Any], dict[str, Any]]:
        data: dict[str, Any] = {
            "text/markdown": self._markdown_text,
            "text/html": self._html,
            "text/plain": self._markdown_text,
        }
        return data, {}

    def _text_plain(self) -> str:
        return self._markdown_text
