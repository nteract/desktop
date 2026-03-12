"""Pattern-based editing for agentic notebook interactions.

Resolves edit targets using text matching (literal or regex) rather than
line:column positions. LLMs are good at describing context in language;
they're bad at counting lines. This module bridges that gap.

The core operation:

    resolve_pattern(source, match, context_before, context_after) → (start, end)

The resolved span can be used to:
1. Apply the edit via runtimed (CRDT splice)
2. Derive synthetic presence (cursor, selection, typing replay)

All pattern resolution happens against the agent's synced copy of the
document. If the source has diverged (human typing concurrently), the
pattern won't match and the caller gets a clear error to retry.
"""

from __future__ import annotations

import re
from dataclasses import dataclass


@dataclass(frozen=True)
class EditSpan:
    """Resolved edit location within a cell source."""

    start: int
    end: int


@dataclass(frozen=True)
class EditResult:
    """Result of applying a pattern-based edit."""

    span: EditSpan
    old_text: str
    new_source: str


class PatternError(Exception):
    """Raised when a pattern cannot be uniquely resolved."""

    def __init__(self, message: str, match_count: int = 0):
        super().__init__(message)
        self.match_count = match_count


def resolve_pattern(
    source: str,
    match: str,
    context_before: str = "",
    context_after: str = "",
) -> EditSpan:
    """Resolve a context+match+context pattern to a character span.

    The match text is treated as a literal string (escaped for regex).
    context_before and context_after are also literal strings used as
    lookbehind/lookahead anchors for disambiguation.

    Args:
        source: The cell source text to search.
        match: Literal text to find. Must resolve to exactly one location.
        context_before: Text that must appear immediately before the match.
        context_after: Text that must appear immediately after the match.

    Returns:
        EditSpan with start/end character offsets.

    Raises:
        PatternError: If the match is empty, not found, or ambiguous.
    """
    if not match and not context_before and not context_after:
        raise PatternError("match text cannot be empty")

    escaped_match = re.escape(match)

    if context_before or context_after:
        parts = []
        if context_before:
            parts.append(f"(?<={re.escape(context_before)})")
        parts.append(escaped_match)
        if context_after:
            parts.append(f"(?={re.escape(context_after)})")
        pattern = "".join(parts)
    else:
        pattern = escaped_match

    return _resolve_compiled(source, pattern, match)


def resolve_regex(
    source: str,
    pattern: str,
) -> EditSpan:
    """Resolve a regex pattern to a character span.

    The pattern must match exactly one location in the source.
    Supports the full Python regex syntax including lookahead/lookbehind
    for zero-width insertions.

    Args:
        source: The cell source text to search.
        pattern: Regex pattern (compiled with re.MULTILINE).

    Returns:
        EditSpan with start/end character offsets.

    Raises:
        PatternError: If the pattern is invalid, not found, or ambiguous.
    """
    if not pattern:
        raise PatternError("pattern cannot be empty")

    return _resolve_compiled(source, pattern, pattern)


# Maximum source length for regex mode (guard against catastrophic backtracking)
MAX_REGEX_SOURCE_LEN = 1_000_000  # 1MB — well beyond any reasonable cell


def _resolve_compiled(source: str, pattern: str, display: str) -> EditSpan:
    """Internal: compile and resolve a regex pattern to exactly one span."""
    if len(source) > MAX_REGEX_SOURCE_LEN:
        raise PatternError(
            f"source too large for regex resolution ({len(source)} chars, "
            f"max {MAX_REGEX_SOURCE_LEN})",
            match_count=0,
        )

    try:
        compiled = re.compile(pattern, re.MULTILINE)
    except re.error as e:
        raise PatternError(f"invalid pattern {display!r}: {e}") from e

    # Early exit after 2nd match — no need to materialize all matches
    first_match = None
    locations: list[str] = []
    match_count = 0

    for m in compiled.finditer(source):
        match_count += 1
        if first_match is None:
            first_match = m
        if len(locations) < 5:
            locations.append(f"offset {m.start()}")

    if match_count == 0:
        raise PatternError(
            f"no match found for {display!r} in source ({len(source)} chars)",
            match_count=0,
        )

    if match_count > 1:
        suffix = f" (and {match_count - 5} more)" if match_count > 5 else ""
        raise PatternError(
            f"expected exactly 1 match for {display!r}, found {match_count} "
            f"at {', '.join(locations)}{suffix}. "
            f"Use context_before/context_after to disambiguate.",
            match_count=match_count,
        )

    assert first_match is not None
    return EditSpan(start=first_match.start(), end=first_match.end())


def offset_to_line_col(source: str, offset: int) -> tuple[int, int]:
    """Convert a character offset to a (line, column) tuple (both 0-based)."""
    line = source[:offset].count("\n")
    last_newline = source.rfind("\n", 0, offset)
    col = offset if last_newline == -1 else offset - last_newline - 1
    return line, col


def apply_edit(
    source: str,
    span: EditSpan,
    content: str,
) -> EditResult:
    """Apply a replacement at a resolved span.

    Args:
        source: Original cell source text.
        span: Resolved edit location from resolve_pattern/resolve_regex.
        content: Replacement text.

    Returns:
        EditResult with the old text, new source, and span.
    """
    old_text = source[span.start : span.end]
    new_source = source[: span.start] + content + source[span.end :]
    return EditResult(span=span, old_text=old_text, new_source=new_source)


def replace_match(
    source: str,
    match: str,
    content: str,
    context_before: str = "",
    context_after: str = "",
) -> EditResult:
    """Resolve a literal match and apply a replacement in one step.

    Convenience wrapper around resolve_pattern + apply_edit.
    """
    span = resolve_pattern(source, match, context_before, context_after)
    return apply_edit(source, span, content)


def replace_regex(
    source: str,
    pattern: str,
    content: str,
) -> EditResult:
    """Resolve a regex pattern and apply a replacement in one step.

    Convenience wrapper around resolve_regex + apply_edit.
    """
    span = resolve_regex(source, pattern)
    return apply_edit(source, span, content)
