"""Shared CSS utilities for imo HTML fallbacks."""

from __future__ import annotations

# Callout kind → (border color, background color, dark bg, icon)
CALLOUT_STYLES: dict[str, dict[str, str]] = {
    "neutral": {
        "border": "#9ca3af",
        "bg": "#f9fafb",
        "dark_bg": "#1f2937",
        "icon": "&#x2139;&#xFE0F;",
    },
    "info": {
        "border": "#3b82f6",
        "bg": "#eff6ff",
        "dark_bg": "#1e3a5f",
        "icon": "&#x2139;&#xFE0F;",
    },
    "warn": {
        "border": "#f59e0b",
        "bg": "#fffbeb",
        "dark_bg": "#422006",
        "icon": "&#x26A0;&#xFE0F;",
    },
    "success": {
        "border": "#10b981",
        "bg": "#ecfdf5",
        "dark_bg": "#064e3b",
        "icon": "&#x2705;",
    },
    "danger": {
        "border": "#ef4444",
        "bg": "#fef2f2",
        "dark_bg": "#450a0a",
        "icon": "&#x26D4;",
    },
}

# Direction → (color, dark color, arrow)
DIRECTION_STYLES: dict[str, dict[str, str]] = {
    "increase": {"color": "#10b981", "dark_color": "#34d399", "arrow": "&#x25B2;"},
    "decrease": {"color": "#ef4444", "dark_color": "#f87171", "arrow": "&#x25BC;"},
}


def _style_tag(css: str, style_id: str) -> str:
    """Wrap CSS in a <style> tag with deduplication ID."""
    return f'<style id="imo-{style_id}">{css}</style>'


CALLOUT_CSS = _style_tag(
    """
.imo-callout {
    border-left: 4px solid var(--imo-callout-border, #9ca3af);
    background: var(--imo-callout-bg, #f9fafb);
    padding: 12px 16px;
    border-radius: 4px;
    margin: 4px 0;
    font-size: 14px;
    line-height: 1.5;
}
@media (prefers-color-scheme: dark) {
    .imo-callout {
        background: var(--imo-callout-dark-bg, #1f2937);
        color: #e5e7eb;
    }
}
""",
    "callout",
)

STAT_CSS = _style_tag(
    """
.imo-stat {
    display: inline-flex;
    flex-direction: column;
    padding: 12px 16px;
    min-width: 100px;
}
.imo-stat--bordered {
    border: 1px solid #e5e7eb;
    border-radius: 8px;
}
@media (prefers-color-scheme: dark) {
    .imo-stat--bordered {
        border-color: #374151;
    }
}
.imo-stat__label {
    font-size: 12px;
    font-weight: 500;
    color: #6b7280;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin-bottom: 4px;
}
.imo-stat__value {
    font-size: 24px;
    font-weight: 700;
    line-height: 1.2;
}
.imo-stat__caption {
    font-size: 12px;
    color: #9ca3af;
    margin-top: 4px;
}
.imo-stat__direction {
    font-size: 12px;
    margin-left: 6px;
}
@media (prefers-color-scheme: dark) {
    .imo-stat__label { color: #9ca3af; }
    .imo-stat__caption { color: #6b7280; }
    .imo-stat__value { color: #f3f4f6; }
}
""",
    "stat",
)
