"""Callout display: mo.callout()."""

from __future__ import annotations

from html import escape
from typing import Any, Literal

from imo._html import Html
from imo._style import CALLOUT_CSS, CALLOUT_STYLES

CalloutKind = Literal["neutral", "warn", "success", "info", "danger"]

MIME_TYPE = "application/vnd.imo.callout+json"


def callout(
    value: object,
    kind: CalloutKind = "neutral",
) -> Html:
    """Display content in a styled callout box.

    Args:
        value: Content to display. Html objects are rendered as-is;
            strings are HTML-escaped.
        kind: Style variant — "neutral", "info", "warn", "success", or "danger".

    Returns:
        Html object with custom MIME type for nteract rendering.
    """
    content_html = str(value) if isinstance(value, Html) else escape(str(value))
    return _CalloutHtml(content_html, kind)


class _CalloutHtml(Html):
    def __init__(self, content_html: str, kind: CalloutKind) -> None:
        self._content_html = content_html
        self._kind = kind
        style = CALLOUT_STYLES.get(kind, CALLOUT_STYLES["neutral"])
        html = (
            f"{CALLOUT_CSS}"
            f'<div class="imo-callout" style="'
            f"border-left-color: {style['border']}; "
            f"--imo-callout-bg: {style['bg']}; "
            f"--imo-callout-dark-bg: {style['dark_bg']}; "
            f"--imo-callout-border: {style['border']}"
            f'">'
            f"{content_html}"
            f"</div>"
        )
        super().__init__(html)

    def _mime_type(self) -> str:
        return MIME_TYPE

    def _mime_data(self) -> dict[str, Any]:
        return {
            "content": self._content_html,
            "kind": self._kind,
        }

    def _text_plain(self) -> str:
        return f"Callout({self._kind})"
