"""Layout display: mo.vstack() and mo.hstack()."""

from __future__ import annotations

from html import escape
from typing import Any, Literal, Sequence, Union

from imo._html import Html

HSTACK_MIME = "application/vnd.imo.hstack+json"
VSTACK_MIME = "application/vnd.imo.vstack+json"

JustifyValue = Literal["start", "center", "end", "space-between", "space-around"]
AlignValue = Literal["start", "end", "center", "stretch"]

_JUSTIFY_MAP = {
    "start": "flex-start",
    "center": "center",
    "end": "flex-end",
    "space-between": "space-between",
    "space-around": "space-around",
}
_ALIGN_MAP = {
    "start": "flex-start",
    "end": "flex-end",
    "center": "center",
    "stretch": "stretch",
}


def _item_to_html(item: object) -> str:
    """Convert an item to HTML string."""
    if isinstance(item, Html):
        return str(item)
    return f"<span>{escape(str(item))}</span>"


def _item_to_mime_bundle(item: object) -> dict[str, Any]:
    """Get the MIME bundle for an item, for nested rendering."""
    if isinstance(item, Html):
        bundle, _meta = item._repr_mimebundle_()
        return bundle
    text = str(item)
    return {"text/plain": text, "text/html": f"<span>{escape(text)}</span>"}


def hstack(
    items: Sequence[object],
    *,
    justify: JustifyValue = "space-between",
    align: AlignValue | None = None,
    wrap: bool = False,
    gap: float = 0.5,
    widths: Union[Literal["equal"], Sequence[float], None] = None,
) -> Html:
    """Arrange items horizontally in a flex row.

    Args:
        items: Objects to lay out. Html objects render as-is; others are
            stringified and HTML-escaped.
        justify: Horizontal distribution. Default "space-between".
        align: Vertical alignment. Default None (CSS normal).
        wrap: Whether items wrap to the next line. Default False.
        gap: Spacing between items in rem. Default 0.5.
        widths: "equal" for uniform widths, a list of relative flex values,
            or None for natural sizing.

    Returns:
        Html object with custom MIME type for nteract rendering.
    """
    child_flexes = _resolve_sizes(widths, len(items))
    return _LayoutHtml(
        items=list(items),
        direction="row",
        justify=justify,
        align=align,
        wrap=wrap,
        gap=gap,
        child_flexes=child_flexes,
        mime_type=HSTACK_MIME,
    )


def vstack(
    items: Sequence[object],
    *,
    align: AlignValue | None = None,
    justify: JustifyValue = "start",
    gap: float = 0.5,
    heights: Union[Literal["equal"], Sequence[float], None] = None,
) -> Html:
    """Arrange items vertically in a flex column.

    Args:
        items: Objects to lay out. Html objects render as-is; others are
            stringified and HTML-escaped.
        align: Horizontal alignment. Default None (CSS normal).
        justify: Vertical distribution. Default "start".
        gap: Spacing between items in rem. Default 0.5.
        heights: "equal" for uniform heights, a list of relative flex values,
            or None for natural sizing.

    Returns:
        Html object with custom MIME type for nteract rendering.
    """
    child_flexes = _resolve_sizes(heights, len(items))
    return _LayoutHtml(
        items=list(items),
        direction="column",
        justify=justify,
        align=align,
        wrap=False,
        gap=gap,
        child_flexes=child_flexes,
        mime_type=VSTACK_MIME,
    )


def _resolve_sizes(
    sizes: Union[Literal["equal"], Sequence[float], None],
    count: int,
) -> list[float] | None:
    if sizes is None:
        return None
    if sizes == "equal":
        return [1.0] * count
    return list(sizes)


class _LayoutHtml(Html):
    def __init__(
        self,
        items: list[object],
        direction: str,
        justify: JustifyValue,
        align: AlignValue | None,
        wrap: bool,
        gap: float,
        child_flexes: list[float] | None,
        mime_type: str,
    ) -> None:
        self._items = items
        self._direction = direction
        self._justify = justify
        self._align = align
        self._wrap = wrap
        self._gap = gap
        self._child_flexes = child_flexes
        self._custom_mime = mime_type

        # Build fallback HTML
        justify_css = _JUSTIFY_MAP.get(justify, "flex-start")
        align_css = _ALIGN_MAP.get(align, "normal") if align else "normal"
        wrap_css = "wrap" if wrap else "nowrap"

        style = (
            f"display:flex;flex-direction:{direction};"
            f"justify-content:{justify_css};align-items:{align_css};"
            f"flex-wrap:{wrap_css};gap:{gap}rem"
        )

        children_html = []
        for i, item in enumerate(items):
            item_html = _item_to_html(item)
            if child_flexes and i < len(child_flexes):
                item_html = (
                    f'<div style="flex:{child_flexes[i]}">{item_html}</div>'
                )
            children_html.append(item_html)

        html = f'<div style="{style}">{"".join(children_html)}</div>'
        super().__init__(html)

    def _mime_type(self) -> str:
        return self._custom_mime

    def _mime_data(self) -> dict[str, Any]:
        data: dict[str, Any] = {
            "items": [_item_to_mime_bundle(item) for item in self._items],
            "gap": self._gap,
            "justify": self._justify,
            "align": self._align,
        }
        if self._direction == "row":
            data["wrap"] = self._wrap
            if self._child_flexes is not None:
                data["widths"] = self._child_flexes
        else:
            if self._child_flexes is not None:
                data["heights"] = self._child_flexes
        return data

    def _text_plain(self) -> str:
        name = "hstack" if self._direction == "row" else "vstack"
        return f"{name}({len(self._items)} items)"
