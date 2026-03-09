"""Stat display: mo.stat()."""

from __future__ import annotations

from html import escape
from typing import Any, Literal, Union

from imo._html import Html
from imo._style import DIRECTION_STYLES, STAT_CSS

MIME_TYPE = "application/vnd.imo.stat+json"


def stat(
    value: Union[str, int, float],
    label: str | None = None,
    caption: str | None = None,
    direction: Literal["increase", "decrease"] | None = None,
    bordered: bool = False,
    target_direction: Literal["increase", "decrease"] | None = "increase",
) -> Html:
    """Display a statistic card.

    Args:
        value: The metric value to display.
        label: Descriptive label above the value.
        caption: Supplementary text below the value.
        direction: Whether the metric moved "increase" or "decrease".
        bordered: Whether to render a border around the card.
        target_direction: Which direction is "good" (colors direction
            indicator green when matching, red when opposite).

    Returns:
        Html object with custom MIME type for nteract rendering.
    """
    return _StatHtml(
        value=str(value),
        label=label,
        caption=caption,
        direction=direction,
        bordered=bordered,
        target_direction=target_direction,
    )


class _StatHtml(Html):
    def __init__(
        self,
        value: str,
        label: str | None,
        caption: str | None,
        direction: Literal["increase", "decrease"] | None,
        bordered: bool,
        target_direction: Literal["increase", "decrease"] | None,
    ) -> None:
        self._value = value
        self._label = label
        self._caption = caption
        self._direction = direction
        self._bordered = bordered
        self._target_direction = target_direction

        classes = "imo-stat"
        if bordered:
            classes += " imo-stat--bordered"

        parts = [STAT_CSS, f'<div class="{classes}">']

        if label is not None:
            parts.append(
                f'<div class="imo-stat__label">{escape(label)}</div>'
            )

        # Value + optional direction indicator
        parts.append(f'<div class="imo-stat__value">{escape(value)}')
        if direction is not None:
            ds = DIRECTION_STYLES.get(direction)
            if ds:
                # Color based on whether direction matches target
                is_good = direction == target_direction
                color = (
                    DIRECTION_STYLES["increase"]["color"]
                    if is_good
                    else DIRECTION_STYLES["decrease"]["color"]
                )
                parts.append(
                    f'<span class="imo-stat__direction" style="color:{color}">'
                    f'{ds["arrow"]}</span>'
                )
        parts.append("</div>")

        if caption is not None:
            parts.append(
                f'<div class="imo-stat__caption">{escape(caption)}</div>'
            )

        parts.append("</div>")
        super().__init__("".join(parts))

    def _mime_type(self) -> str:
        return MIME_TYPE

    def _mime_data(self) -> dict[str, Any]:
        data: dict[str, Any] = {"value": self._value}
        if self._label is not None:
            data["label"] = self._label
        if self._caption is not None:
            data["caption"] = self._caption
        if self._direction is not None:
            data["direction"] = self._direction
        data["bordered"] = self._bordered
        if self._target_direction is not None:
            data["target_direction"] = self._target_direction
        return data

    def _text_plain(self) -> str:
        parts = [self._value]
        if self._label:
            parts.insert(0, f"{self._label}:")
        return f"Stat({' '.join(parts)})"
