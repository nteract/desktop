"""Tests for imo display renderers."""

import json

import imo as mo
from imo._html import Html


class TestHtml:
    def test_str_returns_html(self):
        h = Html("<b>hello</b>")
        assert str(h) == "<b>hello</b>"

    def test_repr_html(self):
        h = Html("<b>hello</b>")
        assert h._repr_html_() == "<b>hello</b>"

    def test_repr_mimebundle(self):
        h = Html("<b>hello</b>")
        data, meta = h._repr_mimebundle_()
        assert data["text/html"] == "<b>hello</b>"
        assert "text/plain" in data
        assert meta == {}

    def test_format_for_fstring(self):
        h = Html("<b>hello</b>")
        result = f"prefix {h} suffix"
        assert result == "prefix <b>hello</b> suffix"


class TestMd:
    def test_basic_markdown(self):
        result = mo.md("# Hello")
        data, _ = result._repr_mimebundle_()
        assert data["text/markdown"] == "# Hello"
        assert "<h1>" in data["text/html"] or "Hello" in data["text/html"]

    def test_cleandoc(self):
        result = mo.md("""
            # Hello
            World
        """)
        data, _ = result._repr_mimebundle_()
        assert data["text/markdown"] == "# Hello\nWorld"

    def test_fstring_interpolation(self):
        stat = mo.stat(42, label="Users")
        result = mo.md(f"Count: {stat}")
        data, _ = result._repr_mimebundle_()
        assert "42" in data["text/markdown"]


class TestCallout:
    def test_basic_callout(self):
        result = mo.callout("Hello")
        data, _ = result._repr_mimebundle_()

        assert "application/vnd.imo.callout+json" in data
        mime_data = data["application/vnd.imo.callout+json"]
        assert mime_data["content"] == "Hello"
        assert mime_data["kind"] == "neutral"

    def test_callout_kinds(self):
        for kind in ("neutral", "info", "warn", "success", "danger"):
            result = mo.callout("test", kind=kind)
            data, _ = result._repr_mimebundle_()
            assert data["application/vnd.imo.callout+json"]["kind"] == kind

    def test_callout_html_fallback(self):
        result = mo.callout("Hello", kind="warn")
        data, _ = result._repr_mimebundle_()
        html = data["text/html"]
        assert "imo-callout" in html
        assert "Hello" in html

    def test_callout_escapes_strings(self):
        result = mo.callout("<script>alert('xss')</script>")
        data, _ = result._repr_mimebundle_()
        mime_data = data["application/vnd.imo.callout+json"]
        assert "<script>" not in mime_data["content"]
        assert "&lt;script&gt;" in mime_data["content"]

    def test_callout_accepts_html_objects(self):
        inner = mo.md("**bold**")
        result = mo.callout(inner, kind="info")
        data, _ = result._repr_mimebundle_()
        # Inner Html object is rendered as-is (not escaped)
        assert "<strong>" in data["application/vnd.imo.callout+json"]["content"] or "bold" in data["application/vnd.imo.callout+json"]["content"]


class TestStat:
    def test_basic_stat(self):
        result = mo.stat(42)
        data, _ = result._repr_mimebundle_()

        assert "application/vnd.imo.stat+json" in data
        mime_data = data["application/vnd.imo.stat+json"]
        assert mime_data["value"] == "42"

    def test_stat_with_label_and_caption(self):
        result = mo.stat(42, label="Users", caption="active this month")
        data, _ = result._repr_mimebundle_()
        mime_data = data["application/vnd.imo.stat+json"]
        assert mime_data["value"] == "42"
        assert mime_data["label"] == "Users"
        assert mime_data["caption"] == "active this month"

    def test_stat_with_direction(self):
        result = mo.stat(42, direction="increase")
        data, _ = result._repr_mimebundle_()
        mime_data = data["application/vnd.imo.stat+json"]
        assert mime_data["direction"] == "increase"

    def test_stat_bordered(self):
        result = mo.stat(42, bordered=True)
        data, _ = result._repr_mimebundle_()
        mime_data = data["application/vnd.imo.stat+json"]
        assert mime_data["bordered"] is True

    def test_stat_html_fallback(self):
        result = mo.stat(42, label="Users")
        data, _ = result._repr_mimebundle_()
        html = data["text/html"]
        assert "42" in html
        assert "Users" in html
        assert "imo-stat" in html

    def test_stat_text_plain(self):
        result = mo.stat(42, label="Users")
        data, _ = result._repr_mimebundle_()
        assert "Users" in data["text/plain"]
        assert "42" in data["text/plain"]


class TestLayout:
    def test_hstack_basic(self):
        result = mo.hstack([mo.stat(1), mo.stat(2)])
        data, _ = result._repr_mimebundle_()

        assert "application/vnd.imo.hstack+json" in data
        mime_data = data["application/vnd.imo.hstack+json"]
        assert len(mime_data["items"]) == 2
        assert mime_data["gap"] == 0.5
        assert mime_data["justify"] == "space-between"

    def test_vstack_basic(self):
        result = mo.vstack([mo.stat(1), mo.stat(2)])
        data, _ = result._repr_mimebundle_()

        assert "application/vnd.imo.vstack+json" in data
        mime_data = data["application/vnd.imo.vstack+json"]
        assert len(mime_data["items"]) == 2
        assert mime_data["justify"] == "start"

    def test_hstack_params(self):
        result = mo.hstack(
            [mo.stat(1)],
            justify="center",
            align="stretch",
            wrap=True,
            gap=1.0,
        )
        data, _ = result._repr_mimebundle_()
        mime_data = data["application/vnd.imo.hstack+json"]
        assert mime_data["justify"] == "center"
        assert mime_data["align"] == "stretch"
        assert mime_data["wrap"] is True
        assert mime_data["gap"] == 1.0

    def test_vstack_params(self):
        result = mo.vstack(
            [mo.stat(1)],
            align="center",
            justify="space-between",
            gap=2.0,
        )
        data, _ = result._repr_mimebundle_()
        mime_data = data["application/vnd.imo.vstack+json"]
        assert mime_data["align"] == "center"
        assert mime_data["justify"] == "space-between"
        assert mime_data["gap"] == 2.0

    def test_nested_mime_bundles(self):
        result = mo.vstack([
            mo.callout("hello", kind="info"),
            mo.stat(42, label="Users"),
        ])
        data, _ = result._repr_mimebundle_()
        items = data["application/vnd.imo.vstack+json"]["items"]

        # Each item should have its own MIME bundle
        assert "application/vnd.imo.callout+json" in items[0]
        assert "application/vnd.imo.stat+json" in items[1]

    def test_hstack_widths_equal(self):
        result = mo.hstack([mo.stat(1), mo.stat(2)], widths="equal")
        data, _ = result._repr_mimebundle_()
        mime_data = data["application/vnd.imo.hstack+json"]
        assert mime_data["widths"] == [1.0, 1.0]

    def test_hstack_widths_custom(self):
        result = mo.hstack([mo.stat(1), mo.stat(2)], widths=[1, 2])
        data, _ = result._repr_mimebundle_()
        mime_data = data["application/vnd.imo.hstack+json"]
        assert mime_data["widths"] == [1.0, 2.0]

    def test_vstack_heights(self):
        result = mo.vstack([mo.stat(1), mo.stat(2)], heights="equal")
        data, _ = result._repr_mimebundle_()
        mime_data = data["application/vnd.imo.vstack+json"]
        assert mime_data["heights"] == [1.0, 1.0]

    def test_layout_html_fallback(self):
        result = mo.hstack([mo.stat(1), mo.stat(2)])
        data, _ = result._repr_mimebundle_()
        html = data["text/html"]
        assert "display:flex" in html
        assert "flex-direction:row" in html

    def test_plain_string_items(self):
        result = mo.hstack(["hello", "world"])
        data, _ = result._repr_mimebundle_()
        items = data["application/vnd.imo.hstack+json"]["items"]
        assert items[0]["text/plain"] == "hello"
        assert items[1]["text/plain"] == "world"

    def test_string_items_escaped(self):
        result = mo.hstack(["<script>alert(1)</script>"])
        data, _ = result._repr_mimebundle_()
        html = data["text/html"]
        assert "<script>" not in html


class TestComposability:
    def test_nested_layouts(self):
        inner = mo.hstack([mo.stat(1), mo.stat(2)])
        outer = mo.vstack([mo.md("# Title"), inner])
        data, _ = outer._repr_mimebundle_()

        items = data["application/vnd.imo.vstack+json"]["items"]
        assert "text/markdown" in items[0]
        assert "application/vnd.imo.hstack+json" in items[1]

    def test_callout_with_layout(self):
        inner = mo.hstack([mo.stat(1), mo.stat(2)])
        result = mo.callout(inner, kind="info")
        data, _ = result._repr_mimebundle_()
        # The callout content contains the rendered HTML of the hstack
        content = data["application/vnd.imo.callout+json"]["content"]
        assert "display:flex" in content

    def test_md_with_stat_fstring(self):
        result = mo.md(f"""
            # Dashboard
            {mo.stat(42, label="Users")}
        """)
        data, _ = result._repr_mimebundle_()
        assert "42" in data["text/markdown"]

    def test_deeply_nested(self):
        result = mo.vstack([
            mo.callout(
                mo.hstack([
                    mo.stat(1, label="A"),
                    mo.stat(2, label="B"),
                ]),
                kind="success",
            ),
            mo.md("Footer"),
        ])
        data, _ = result._repr_mimebundle_()
        assert "application/vnd.imo.vstack+json" in data
        items = data["application/vnd.imo.vstack+json"]["items"]
        assert len(items) == 2
