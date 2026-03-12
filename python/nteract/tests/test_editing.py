"""Tests for pattern-based editing (resolve_pattern, resolve_regex, apply_edit)."""

from __future__ import annotations

import pytest
from nteract._editing import (
    MAX_REGEX_SOURCE_LEN,
    EditSpan,
    PatternError,
    apply_edit,
    offset_to_line_col,
    replace_match,
    replace_regex,
    resolve_pattern,
    resolve_regex,
)

# ── resolve_pattern ──────────────────────────────────────────────────


class TestResolvePattern:
    def test_simple_literal(self):
        span = resolve_pattern("hello world", "world")
        assert span == EditSpan(start=6, end=11)

    def test_with_context_before(self):
        source = "x = 1\ny = 2\n"
        span = resolve_pattern(source, "2", context_before="y = ")
        assert span == EditSpan(start=10, end=11)

    def test_with_context_after(self):
        source = 'print("hello")\nprint("world")\n'
        span = resolve_pattern(source, "print", context_after='("world")')
        assert span == EditSpan(start=15, end=20)

    def test_with_both_contexts(self):
        source = "a + b\nc + d\na + b\n"
        # Disambiguate the first "a + b" by its trailing newline + "c"
        span = resolve_pattern(source, "a + b", context_after="\nc")
        assert span == EditSpan(start=0, end=5)

    def test_no_match_raises(self):
        with pytest.raises(PatternError, match="no match found") as exc_info:
            resolve_pattern("hello", "missing")
        assert exc_info.value.match_count == 0

    def test_ambiguous_match_raises(self):
        with pytest.raises(PatternError, match="expected exactly 1 match") as exc_info:
            resolve_pattern("x = 1\nx = 2\n", "x")
        assert exc_info.value.match_count == 2

    def test_empty_match_raises(self):
        with pytest.raises(PatternError, match="cannot be empty"):
            resolve_pattern("hello", "")

    def test_special_regex_chars_escaped(self):
        source = "result = a + b\n"
        span = resolve_pattern(source, "a + b")
        assert span == EditSpan(start=9, end=14)

    def test_context_with_special_chars(self):
        source = "func(a, b)\nfunc(c, d)\n"
        span = resolve_pattern(source, "c, d", context_before="func(", context_after=")")
        assert span == EditSpan(start=16, end=20)


# ── resolve_regex ────────────────────────────────────────────────────


class TestResolveRegex:
    def test_simple_regex(self):
        span = resolve_regex("hello world", r"w\w+")
        assert span == EditSpan(start=6, end=11)

    def test_lookbehind(self):
        source = "def calculator(a, b):\n    return a + b\n"
        span = resolve_regex(source, r"(?<=return )a \+ b")
        assert span == EditSpan(start=33, end=38)

    def test_lookahead_insertion(self):
        source = "def calculator(a, b):\n    return a + b\n"
        span = resolve_regex(source, r"(?=def calculator\()")
        assert span == EditSpan(start=0, end=0)

    def test_no_match(self):
        with pytest.raises(PatternError, match="no match found"):
            resolve_regex("hello", r"zzz+")

    def test_ambiguous(self):
        with pytest.raises(PatternError, match="expected exactly 1 match"):
            resolve_regex("x = 1\nx = 2\n", r"x")

    def test_invalid_regex(self):
        with pytest.raises(PatternError, match="invalid pattern"):
            resolve_regex("hello", r"(unclosed")

    def test_empty_pattern(self):
        with pytest.raises(PatternError, match="cannot be empty"):
            resolve_regex("hello", "")

    def test_multiline(self):
        source = "line1\nline2\nline3\n"
        span = resolve_regex(source, r"^line2$")
        assert span == EditSpan(start=6, end=11)


# ── apply_edit ───────────────────────────────────────────────────────


class TestApplyEdit:
    def test_replacement(self):
        result = apply_edit("hello world", EditSpan(0, 5), "goodbye")
        assert result.old_text == "hello"
        assert result.new_source == "goodbye world"

    def test_insertion(self):
        result = apply_edit("hello world", EditSpan(5, 5), " beautiful")
        assert result.old_text == ""
        assert result.new_source == "hello beautiful world"

    def test_deletion(self):
        result = apply_edit("hello beautiful world", EditSpan(5, 15), "")
        assert result.old_text == " beautiful"
        assert result.new_source == "hello world"


# ── replace_match (integration) ──────────────────────────────────────


class TestReplaceMatch:
    def test_heading_rename(self):
        source = "# Classic Rice Crispy Treats\n\nA family recipe.\n"
        result = replace_match(
            source,
            match="Treats",
            content="bars",
            context_before="Classic Rice Crispy ",
        )
        assert result.new_source == "# Classic Rice Crispy bars\n\nA family recipe.\n"
        assert result.old_text == "Treats"

    def test_string_literal(self):
        source = 'message = "nteract-dev MCP is connected"\nprint(message)\n'
        result = replace_match(
            source,
            match="nteract-dev MCP is connected",
            context_before='message = "',
            context_after='"',
            content="agent presence is replayed locally",
        )
        assert (
            result.new_source == 'message = "agent presence is replayed locally"\nprint(message)\n'
        )

    def test_return_expression(self):
        source = "def calculator(a, b):\n    return a + b\n"
        result = replace_match(
            source,
            match="a + b",
            context_before="return ",
            content="a * b",
        )
        assert result.new_source == "def calculator(a, b):\n    return a * b\n"

    def test_argument_default(self):
        source = "def execute_cell(timeout_secs: float = 5.0):\n    return timeout_secs\n"
        result = replace_match(
            source,
            match="5.0",
            context_before="timeout_secs: float = ",
            content="2.0",
        )
        assert (
            result.new_source
            == "def execute_cell(timeout_secs: float = 2.0):\n    return timeout_secs\n"
        )

    def test_wrap_identifier(self):
        source = "print(message)\n"
        result = replace_match(
            source,
            match="message",
            context_before="print(",
            context_after=")",
            content="message.upper()",
        )
        assert result.new_source == "print(message.upper())\n"


# ── replace_regex (integration) ──────────────────────────────────────


class TestReplaceRegex:
    def test_insert_after_heading(self):
        source = "# Notes\n\n- alpha\n- beta\n"
        result = replace_regex(source, r"(?<=# Notes\n)", "\nEdited by agent.\n")
        assert result.new_source == "# Notes\n\nEdited by agent.\n\n- alpha\n- beta\n"
        assert result.old_text == ""

    def test_insert_decorator(self):
        source = "def calculator(a, b):\n    return a + b\n"
        result = replace_regex(source, r"(?=def calculator\()", "@trace\n")
        assert result.new_source == "@trace\ndef calculator(a, b):\n    return a + b\n"

    def test_insert_import(self):
        source = "import math\n\ndef compute(x):\n    return math.sqrt(x)\n"
        result = replace_regex(source, r"(?<=import math\n)", "import statistics\n")
        assert result.new_source == (
            "import math\nimport statistics\n\ndef compute(x):\n    return math.sqrt(x)\n"
        )

    def test_ambiguous_fails(self):
        with pytest.raises(PatternError):
            replace_regex("x = 1\nx = 2\n", r"x", "value")


# ── Edge cases ───────────────────────────────────────────────────────


class TestEdgeCases:
    def test_empty_source(self):
        with pytest.raises(PatternError, match="no match found"):
            resolve_pattern("", "anything")

    def test_match_entire_source(self):
        result = replace_match("hello", match="hello", content="goodbye")
        assert result.new_source == "goodbye"
        assert result.span == EditSpan(start=0, end=5)

    def test_newlines_in_match(self):
        source = "def foo():\n    pass\n\ndef bar():\n    pass\n"
        result = replace_match(
            source,
            match="def foo():\n    pass",
            content="def foo():\n    return 42",
        )
        assert result.new_source == "def foo():\n    return 42\n\ndef bar():\n    pass\n"

    def test_unicode_content(self):
        source = "greeting = 'hello'\n"
        result = replace_match(
            source,
            match="hello",
            context_before="greeting = '",
            context_after="'",
            content="こんにちは",
        )
        assert result.new_source == "greeting = 'こんにちは'\n"

    def test_zero_width_at_start(self):
        result = replace_regex("hello", r"(?=hello)", ">>> ")
        assert result.new_source == ">>> hello"
        assert result.span == EditSpan(start=0, end=0)

    def test_zero_width_at_end(self):
        result = replace_regex("hello", r"(?<=hello)", " world")
        assert result.new_source == "hello world"
        assert result.span == EditSpan(start=5, end=5)

    def test_pattern_error_includes_match_count(self):
        try:
            resolve_pattern("aaa", "a")
        except PatternError as e:
            assert e.match_count == 3
        else:
            pytest.fail("expected PatternError")

    def test_large_source_performance(self):
        """Pattern resolution on a large cell shouldn't be pathological."""
        source = "x = 1\n" * 10_000 + "unique_marker = 42\n"
        span = resolve_pattern(source, "unique_marker = 42")
        assert span.start == 60_000

    def test_source_too_large_for_regex(self):
        """Regex resolution rejects sources beyond MAX_REGEX_SOURCE_LEN."""
        source = "a" * (MAX_REGEX_SOURCE_LEN + 1)
        with pytest.raises(PatternError, match="source too large"):
            resolve_regex(source, "a")

    def test_source_too_large_for_pattern(self):
        """Literal pattern resolution also rejects oversized sources."""
        source = "a" * (MAX_REGEX_SOURCE_LEN + 1)
        with pytest.raises(PatternError, match="source too large"):
            resolve_pattern(source, "a")


# ── offset_to_line_col ───────────────────────────────────────────────


class TestOffsetToLineCol:
    def test_start_of_file(self):
        assert offset_to_line_col("hello\nworld", 0) == (0, 0)

    def test_middle_of_first_line(self):
        assert offset_to_line_col("hello\nworld", 3) == (0, 3)

    def test_start_of_second_line(self):
        assert offset_to_line_col("hello\nworld", 6) == (1, 0)

    def test_middle_of_second_line(self):
        assert offset_to_line_col("hello\nworld", 8) == (1, 2)

    def test_end_of_file(self):
        assert offset_to_line_col("hello\nworld", 11) == (1, 5)

    def test_newline_char(self):
        assert offset_to_line_col("hello\nworld", 5) == (0, 5)

    def test_multiline(self):
        source = "line0\nline1\nline2\n"
        assert offset_to_line_col(source, 12) == (2, 0)
        assert offset_to_line_col(source, 15) == (2, 3)

    def test_empty_string(self):
        assert offset_to_line_col("", 0) == (0, 0)

    def test_single_line(self):
        assert offset_to_line_col("hello", 3) == (0, 3)
