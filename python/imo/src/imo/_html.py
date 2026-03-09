"""Base Html class for imo display objects."""

from __future__ import annotations

from typing import Any


class Html:
    """Base display object wrapping HTML content.

    All imo display functions return an Html instance. These objects are
    composable: ``str(html_obj)`` returns raw HTML, so Html objects can be
    nested inside other imo calls or embedded in f-strings.

    Display integration works via ``_repr_mimebundle_``, which returns a
    custom MIME type (when subclasses override ``_mime_type`` and
    ``_mime_data``) alongside ``text/html`` and ``text/plain`` fallbacks.
    """

    def __init__(self, html: str) -> None:
        self._html = html

    # -- IPython display protocol ------------------------------------------

    def _repr_mimebundle_(
        self, **kwargs: Any
    ) -> tuple[dict[str, Any], dict[str, Any]]:
        """Return MIME bundle for IPython display.

        Subclasses that define ``_mime_type()`` and ``_mime_data()`` will
        include a custom MIME type alongside the HTML fallback.
        """
        data: dict[str, Any] = {
            "text/html": self._html,
            "text/plain": self._text_plain(),
        }
        mime_type = self._mime_type()
        if mime_type is not None:
            data[mime_type] = self._mime_data()
        metadata: dict[str, Any] = {}
        return data, metadata

    def _repr_html_(self) -> str:
        """Fallback for environments that don't support _repr_mimebundle_."""
        return self._html

    # -- Composability -----------------------------------------------------

    def __str__(self) -> str:
        """Return raw HTML for embedding inside other imo objects."""
        return self._html

    def __format__(self, format_spec: str) -> str:
        """Support f-string interpolation: f'{mo.stat(42)}'."""
        return self._html

    # -- Subclass hooks ----------------------------------------------------

    def _mime_type(self) -> str | None:
        """Override to provide a custom MIME type."""
        return None

    def _mime_data(self) -> Any:
        """Override to provide structured data for the custom MIME type."""
        return None

    def _text_plain(self) -> str:
        """Override to provide a plain text representation."""
        return f"Html({len(self._html)} chars)"
