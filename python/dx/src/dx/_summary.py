"""Generate ``text/llm+plain`` summaries for DataFrames, computed Python-side.

Python has direct access to the schema, dtypes, and null counts; deriving the
summary in-place is far simpler than reparsing parquet server-side. The
``repr-llm`` crate remains the fallback when dx is not in the loop.
"""

from __future__ import annotations

from typing import Any


def _format_int(n: int) -> str:
    return f"{n:,}"


def _detect_flavor(df: Any) -> str:
    mod = type(df).__module__.split(".")[0]
    if mod in ("pandas", "polars"):
        return mod
    return "unknown"


def _pandas_dtypes(df: Any) -> list[tuple[str, str]]:
    return [(str(col), str(df[col].dtype)) for col in df.columns]


def _pandas_null_counts(df: Any) -> list[tuple[str, int]]:
    counts = df.isna().sum()
    return [(str(col), int(counts[col])) for col in df.columns]


def _polars_dtypes(df: Any) -> list[tuple[str, str]]:
    return [(str(col), str(dtype)) for col, dtype in zip(df.columns, df.dtypes, strict=True)]


def _polars_null_counts(df: Any) -> list[tuple[str, int]]:
    counts = df.null_count().row(0)
    return [(str(col), int(n)) for col, n in zip(df.columns, counts, strict=True)]


def summarize_dataframe(
    df: Any,
    *,
    total_rows: int,
    included_rows: int,
    sampled: bool,
    head_n: int = 10,
) -> str:
    """Produce a ``text/llm+plain`` summary for ``df``.

    The summary includes shape, per-column dtype + null count, and a small
    head sample. If the serialized parquet was downsampled, the header
    explicitly calls that out.
    """
    flavor = _detect_flavor(df)

    if flavor == "pandas":
        dtypes = _pandas_dtypes(df)
        nulls = _pandas_null_counts(df)
        try:
            head_repr = df.head(head_n).to_string()
        except Exception:  # pragma: no cover — defensive
            head_repr = repr(df.head(head_n))
    elif flavor == "polars":
        dtypes = _polars_dtypes(df)
        nulls = _polars_null_counts(df)
        head_repr = str(df.head(head_n))
    else:
        dtypes = []
        nulls = []
        head_repr = repr(df)

    n_cols = len(dtypes)
    lines: list[str] = []
    header = f"DataFrame ({flavor}): {_format_int(included_rows)} rows × {n_cols} columns"
    if sampled and total_rows != included_rows:
        header += f" (sampled from {_format_int(total_rows)} total rows)"
    lines.append(header)

    if dtypes:
        lines.append("Columns:")
        null_map = dict(nulls)
        for name, dtype in dtypes:
            null_n = null_map.get(name, 0)
            if null_n:
                lines.append(f"  - {name}: {dtype} ({null_n} null)")
            else:
                lines.append(f"  - {name}: {dtype}")

    lines.append("")
    lines.append(f"Head ({head_n}):")
    lines.append(head_repr)
    return "\n".join(lines)
