#!/usr/bin/env python3
"""
Summarize CI logs by extracting errors and relevant context.

Reduces ~1000 line GitHub Actions logs to ~30-50 lines while preserving
diagnostic value for agentic workflows.

Usage:
    python scripts/summarize-ci-log.py .context/attachments/*.log
    python scripts/summarize-ci-log.py --stdout .context/attachments/Linux_*.log
    python scripts/summarize-ci-log.py --include-warnings --context 5 *.log
"""

import argparse
import re
import sys
from pathlib import Path

# ANSI escape code pattern
ANSI_PATTERN = re.compile(r"\x1b\[[0-9;]*m")

# Error patterns that indicate failures
ERROR_PATTERNS = [
    # Rust compiler errors
    r"^error(\[E\d+\])?:",
    r"^\s*-->\s+\S+:\d+:\d+",  # Rust file:line:col references
    # Generic errors
    r"##\[error\]",
    r"\bError\b.*failed",
    r"\bFAILED\b",
    r"failed to\b",
    r"error:.*could not compile",
    # npm errors
    r"npm ERR!",
    # pytest errors
    r"^FAILED\s+",
    r"^ERROR\s+",
    # Exit codes
    r"exit code \d+",
    r"exited with code \d+",
    # Process failures
    r"Process completed with exit code [1-9]",
]

# Warning patterns (optional inclusion)
WARNING_PATTERNS = [
    r"^warning(\[W\d+\])?:",
    r"\bWarning\b:",
    r"^\s*Warn\s+",
]

# Patterns to always include (context markers)
CONTEXT_PATTERNS = [
    r"^##\[group\]",
    r"^##\[endgroup\]",
]

# Noise patterns to filter out
NOISE_PATTERNS = [
    r"^Setting up\s+\S+",  # apt package installation
    r"^\s*Compiling\s+\S+\s+v\d",  # Successful Rust compiles
    r"^\s*Downloading\s+",  # Package downloads
    r"^\s*Installing\s+",  # Package installs (successful)
    r"^\[command\]/usr/bin/git",  # Git cleanup commands
    r"^git version",
    r"Temporarily overriding HOME",
    r"Adding repository directory to the temporary git global",
    r"safe\.directory",
]


def strip_ansi(text: str) -> str:
    """Remove ANSI escape codes from text."""
    return ANSI_PATTERN.sub("", text)


def strip_timestamp(line: str) -> str:
    """Remove GitHub Actions timestamp prefix from line."""
    # Format: 2026-02-17T00:32:49.7471217Z
    timestamp_pattern = r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+Z\s*"
    return re.sub(timestamp_pattern, "", line)


def is_error_line(line: str) -> bool:
    """Check if line matches any error pattern."""
    clean_line = strip_timestamp(strip_ansi(line))
    return any(re.search(p, clean_line, re.IGNORECASE) for p in ERROR_PATTERNS)


def is_warning_line(line: str) -> bool:
    """Check if line matches any warning pattern."""
    clean_line = strip_timestamp(strip_ansi(line))
    return any(re.search(p, clean_line, re.IGNORECASE) for p in WARNING_PATTERNS)


def is_context_line(line: str) -> bool:
    """Check if line is a context marker (group start/end)."""
    clean_line = strip_timestamp(strip_ansi(line))
    return any(re.search(p, clean_line) for p in CONTEXT_PATTERNS)


def is_noise_line(line: str) -> bool:
    """Check if line is noise that should be filtered."""
    clean_line = strip_timestamp(strip_ansi(line))
    return any(re.search(p, clean_line) for p in NOISE_PATTERNS)


def find_enclosing_group(lines: list[str], error_idx: int) -> tuple[int | None, int | None]:
    """Find the ##[group]...##[endgroup] range containing an error line."""
    group_start = None
    group_end = None

    # Search backwards for ##[group]
    for i in range(error_idx, -1, -1):
        clean = strip_timestamp(strip_ansi(lines[i]))
        if "##[group]" in clean:
            group_start = i
            break
        if "##[endgroup]" in clean and i != error_idx:
            break  # Hit a different group's end

    # Search forwards for ##[endgroup]
    for i in range(error_idx, len(lines)):
        clean = strip_timestamp(strip_ansi(lines[i]))
        if "##[endgroup]" in clean:
            group_end = i
            break
        if "##[group]" in clean and i != error_idx:
            break  # Hit a different group's start

    return group_start, group_end


def summarize_log(content: str, context_lines: int = 3, include_warnings: bool = False) -> str:
    """
    Summarize a CI log by extracting errors and context.

    Args:
        content: Full log content
        context_lines: Number of lines to include before/after errors
        include_warnings: Whether to include warning lines

    Returns:
        Summarized log content
    """
    lines = content.splitlines()

    # Track which line indices to include
    include_ranges: list[tuple[int, int]] = []

    # Always include header (first few lines before === LOGS ===)
    header_end = 0
    for i, line in enumerate(lines):
        if "=== LOGS" in line:
            header_end = i + 1
            break
    if header_end > 0:
        include_ranges.append((0, header_end))

    # Find all error lines and their context
    for i, line in enumerate(lines):
        if is_error_line(line):
            # Include context around error
            start = max(0, i - context_lines)
            end = min(len(lines), i + context_lines + 1)
            include_ranges.append((start, end))

            # Also include enclosing group markers
            group_start, group_end = find_enclosing_group(lines, i)
            if group_start is not None:
                # Just the group header line
                include_ranges.append((group_start, group_start + 1))
            if group_end is not None:
                include_ranges.append((group_end, group_end + 1))

        elif include_warnings and is_warning_line(line):
            start = max(0, i - context_lines)
            end = min(len(lines), i + context_lines + 1)
            include_ranges.append((start, end))

    # Merge overlapping ranges
    if not include_ranges:
        return "No errors found in log.\n"

    include_ranges.sort()
    merged: list[tuple[int, int]] = [include_ranges[0]]
    for start, end in include_ranges[1:]:
        if start <= merged[-1][1]:
            merged[-1] = (merged[-1][0], max(merged[-1][1], end))
        else:
            merged.append((start, end))

    # Build output
    output_lines: list[str] = []
    last_end = 0

    for start, end in merged:
        # Add separator if there's a gap
        if output_lines and start > last_end:
            output_lines.append("...")

        for i in range(start, end):
            line = lines[i]
            # Skip noise lines unless they're in the header
            if start >= header_end and is_noise_line(line):
                continue

            # Strip ANSI codes for cleaner output
            clean_line = strip_ansi(line)
            output_lines.append(clean_line)

        last_end = end

    return "\n".join(output_lines) + "\n"


def process_file(
    input_path: Path,
    output_path: Path | None,
    context_lines: int,
    include_warnings: bool,
    to_stdout: bool,
) -> None:
    """Process a single log file."""
    content = input_path.read_text(encoding="utf-8", errors="replace")
    summary = summarize_log(content, context_lines, include_warnings)

    if to_stdout:
        print(f"=== {input_path.name} ===")
        print(summary)
    else:
        if output_path is None:
            output_path = input_path.with_suffix(input_path.suffix + ".summary")
        output_path.write_text(summary, encoding="utf-8")

        # Report size reduction
        original_lines = len(content.splitlines())
        summary_lines = len(summary.splitlines())
        reduction = (1 - summary_lines / original_lines) * 100 if original_lines > 0 else 0
        print(
            f"{input_path.name}: {original_lines} -> {summary_lines} lines"
            f" ({reduction:.0f}% reduction)"
        )


def main():
    parser = argparse.ArgumentParser(
        description="Summarize CI logs by extracting errors and context.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__,
    )
    parser.add_argument("files", nargs="+", type=Path, help="Log files to summarize")
    parser.add_argument(
        "--stdout", action="store_true", help="Print summaries to stdout instead of creating files"
    )
    parser.add_argument(
        "--include-warnings", action="store_true", help="Include warning lines in summary"
    )
    parser.add_argument(
        "--context", type=int, default=3, help="Lines of context around errors (default: 3)"
    )
    parser.add_argument(
        "-o", "--output", type=Path, help="Output file (only valid with single input file)"
    )

    args = parser.parse_args()

    if args.output and len(args.files) > 1:
        parser.error("--output can only be used with a single input file")

    for input_path in args.files:
        if not input_path.exists():
            print(f"Warning: {input_path} does not exist, skipping", file=sys.stderr)
            continue

        output_path = args.output if len(args.files) == 1 else None
        process_file(input_path, output_path, args.context, args.include_warnings, args.stdout)


if __name__ == "__main__":
    main()
