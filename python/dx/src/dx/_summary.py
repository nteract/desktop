"""Generate ``text/llm+plain`` summaries for DataFrames, computed Python-side.

Python has direct access to the schema, dtypes, and null counts; deriving the
summary in-place is far simpler than reparsing parquet server-side. The
``repr-llm`` crate remains the fallback when dx is not in the loop.
"""

from __future__ import annotations

from typing import Any

# Cap the number of columns in the summary to keep it compact.
_MAX_SUMMARY_COLUMNS = 40


def _format_int(n: int) -> str:
    return f"{n:,}"


def _truncate_cell(value: Any, max_chars: int = 80) -> str:
    """Render *value* as a string, truncating with ``…[+N chars]`` when longer
    than *max_chars*."""
    s = str(value)
    if len(s) <= max_chars:
        return s
    # Upper-bound suffix length to size the prefix, then compute the true suffix.
    suffix_upper = f"…[+{len(s)} chars]"
    prefix_len = max(max_chars - len(suffix_upper), 1)
    suffix = f"…[+{len(s) - prefix_len} chars]"
    return s[:prefix_len] + suffix


def _detect_flavor(df: Any) -> str:
    mod = type(df).__module__.split(".")[0]
    if mod in ("pandas", "polars"):
        return mod
    return "unknown"


# ── Per-column stat extractors ──────────────────────────────────────


def _pandas_column_stats(df: Any) -> list[dict]:
    """Extract per-column stats from a pandas DataFrame.

    Returns a list of dicts with keys: name, dtype, null_count, kind, stats.
    """
    import numpy as np

    results: list[dict] = []
    for col in df.columns:
        series = df[col]
        dtype_str = str(series.dtype)
        null_count = int(series.isna().sum())
        entry: dict = {
            "name": str(col),
            "dtype": dtype_str,
            "null_count": null_count,
        }

        if null_count == len(series):
            entry["kind"] = "all_null"
            results.append(entry)
            continue

        kind = series.dtype.kind if hasattr(series.dtype, "kind") else ""

        # Numeric (int, uint, float)
        if kind in ("i", "u", "f"):
            try:
                vmin = series.min()
                vmax = series.max()
                if isinstance(vmin, (int, float, np.integer, np.floating)):
                    entry["kind"] = "numeric"
                    entry["stats"] = {"min": _format_numeric(vmin), "max": _format_numeric(vmax)}
                else:
                    entry["kind"] = "other"
            except Exception:
                entry["kind"] = "other"
        # Datetime / timedelta
        elif kind in ("M", "m"):
            try:
                vmin = series.dropna().min()
                vmax = series.dropna().max()
                entry["kind"] = "temporal"
                entry["stats"] = {"min": str(vmin), "max": str(vmax)}
            except Exception:
                entry["kind"] = "other"
        # Boolean
        elif kind == "b":
            entry["kind"] = "other"
        # String / object / categorical
        elif dtype_str in ("object", "string", "string[python]", "string[pyarrow]") or kind == "O":
            try:
                non_null = series.dropna()
                nunique = int(non_null.nunique())
                top_counts = non_null.value_counts(dropna=True).head(3)
                top = [(str(idx), int(cnt)) for idx, cnt in top_counts.items()]
                entry["kind"] = "string"
                entry["stats"] = {"distinct": nunique, "top": top}
            except Exception:
                entry["kind"] = "other"
        else:
            entry["kind"] = "other"

        results.append(entry)
    return results


def _polars_column_stats(df: Any) -> list[dict]:
    """Extract per-column stats from a polars DataFrame.

    Returns a list of dicts with keys: name, dtype, null_count, kind, stats.
    """
    import polars as pl

    results: list[dict] = []
    for col_name, dtype in zip(df.columns, df.dtypes, strict=True):
        series = df[col_name]
        dtype_str = str(dtype)
        null_count = int(series.null_count())
        entry: dict = {
            "name": str(col_name),
            "dtype": dtype_str,
            "null_count": null_count,
        }

        if null_count == series.len():
            entry["kind"] = "all_null"
            results.append(entry)
            continue

        # Numeric types
        if dtype.is_numeric() and not dtype.is_(pl.Boolean):
            try:
                vmin = series.min()
                vmax = series.max()
                entry["kind"] = "numeric"
                entry["stats"] = {"min": _format_numeric(vmin), "max": _format_numeric(vmax)}
            except Exception:
                entry["kind"] = "other"
        # Temporal types (Datetime, Date, Time, Duration)
        elif dtype.is_temporal():
            try:
                vmin = series.drop_nulls().min()
                vmax = series.drop_nulls().max()
                entry["kind"] = "temporal"
                entry["stats"] = {"min": str(vmin), "max": str(vmax)}
            except Exception:
                entry["kind"] = "other"
        # String / Categorical / Utf8
        elif dtype == pl.Utf8 or dtype == pl.String or dtype == pl.Categorical:
            try:
                non_null = series.drop_nulls()
                nunique = int(non_null.n_unique())
                vc = non_null.value_counts(sort=True)
                # value_counts returns a DataFrame with columns [col_name, "count"]
                top_n = vc.head(3)
                top = [(str(top_n[col_name][i]), int(top_n["count"][i])) for i in range(len(top_n))]
                entry["kind"] = "string"
                entry["stats"] = {"distinct": nunique, "top": top}
            except Exception:
                entry["kind"] = "other"
        else:
            entry["kind"] = "other"

        results.append(entry)
    return results


def _format_numeric(v: Any) -> str:
    """Format a numeric value for display in stats."""
    if isinstance(v, float):
        if v == int(v) and abs(v) < 1e15:
            return _format_int(int(v))
        return f"{v:.3f}"
    return _format_int(int(v))


def _format_column_line(col: dict, total_rows: int) -> str:
    """Format a single column's summary line."""
    name = col["name"]
    dtype = col["dtype"]
    null_count = col["null_count"]
    kind = col.get("kind", "other")

    parts: list[str] = []

    # Null info
    if kind == "all_null":
        parts.append("all null")
    elif null_count > 0:
        if total_rows > 0:
            pct = round(null_count / total_rows * 100)
            parts.append(f"{_format_int(null_count)} null ({pct}%)")
        else:
            parts.append(f"{_format_int(null_count)} null")

    stats = col.get("stats")
    if stats and kind == "numeric":
        parts.append(f"range {stats['min']} – {stats['max']}")
    elif stats and kind == "temporal":
        if stats["min"] and stats["max"]:
            parts.append(f"{stats['min']} to {stats['max']}")
    elif stats and kind == "string":
        s = f"{_format_int(stats['distinct'])} distinct"
        if stats["top"]:
            top_str = ", ".join(
                f'"{_truncate_cell(label, 32)}" ({_format_int(cnt)})' for label, cnt in stats["top"]
            )
            s += f", top: {top_str}"
        parts.append(s)

    suffix = " · ".join(parts)
    if suffix:
        return f"  - {name} ({dtype}) · {suffix}"
    return f"  - {name} ({dtype})"


def _build_head_preview(df: Any, flavor: str, head_n: int) -> str:
    """Build a truncated head preview of the DataFrame.

    Uses manual per-cell truncation to keep the output compact regardless
    of the underlying library's formatting defaults.
    """
    # Reduce head rows to keep output compact — the column stats carry
    # the heavy analytical signal, the head is just a sample peek.
    effective_n = min(head_n, 3)

    if flavor == "pandas":
        head_df = df.head(effective_n)
        lines: list[str] = []
        # Column headers
        col_names = [str(c) for c in head_df.columns]
        # Build rows with truncated cells
        rows: list[list[str]] = []
        for _, row in head_df.iterrows():
            rows.append([_truncate_cell(row[c], 40) for c in head_df.columns])
        # Compute column widths
        widths = [
            max(len(col_names[i]), *(len(r[i]) for r in rows)) if rows else len(col_names[i])
            for i in range(len(col_names))
        ]
        # Format header
        header = "  ".join(col_names[i].ljust(widths[i]) for i in range(len(col_names)))
        lines.append(header)
        # Format rows
        for row in rows:
            lines.append("  ".join(row[i].ljust(widths[i]) for i in range(len(row))))
        return "\n".join(lines)
    elif flavor == "polars":
        head_df = df.head(effective_n)
        lines_out: list[str] = []
        col_names = list(head_df.columns)
        rows_data: list[list[str]] = []
        for i in range(len(head_df)):
            row_cells: list[str] = []
            for c in col_names:
                val = head_df[c][i]
                row_cells.append(_truncate_cell(val, 40))
            rows_data.append(row_cells)
        widths = [
            max(len(col_names[j]), *(len(r[j]) for r in rows_data))
            if rows_data
            else len(col_names[j])
            for j in range(len(col_names))
        ]
        header = "  ".join(col_names[j].ljust(widths[j]) for j in range(len(col_names)))
        lines_out.append(header)
        for row in rows_data:
            lines_out.append("  ".join(row[j].ljust(widths[j]) for j in range(len(row))))
        return "\n".join(lines_out)
    else:
        return repr(df)


def summarize_dataframe(
    df: Any,
    *,
    total_rows: int,
    included_rows: int,
    sampled: bool,
    head_n: int = 10,
) -> str:
    """Produce a ``text/llm+plain`` summary for ``df``.

    The summary includes shape, per-column dtype + stats + null count, and a
    small head sample. If the serialized parquet was downsampled, the header
    explicitly calls that out.
    """
    flavor = _detect_flavor(df)

    # Extract per-column stats
    if flavor == "pandas":
        col_stats = _pandas_column_stats(df)
    elif flavor == "polars":
        col_stats = _polars_column_stats(df)
    else:
        col_stats = []

    n_cols = len(col_stats) if col_stats else (len(df.columns) if hasattr(df, "columns") else 0)
    lines: list[str] = []

    # Header
    header = f"DataFrame ({flavor}): {_format_int(included_rows)} rows × {n_cols} columns"
    if sampled and total_rows != included_rows:
        header += f" (sampled from {_format_int(total_rows)} total rows)"
    lines.append(header)

    # Column stats
    if col_stats:
        lines.append("Columns:")
        capped = len(col_stats) > _MAX_SUMMARY_COLUMNS
        display_stats = col_stats[:_MAX_SUMMARY_COLUMNS] if capped else col_stats
        for col in display_stats:
            lines.append(_format_column_line(col, included_rows))
        if capped:
            remaining = len(col_stats) - _MAX_SUMMARY_COLUMNS
            lines.append(f"  …[+{remaining} more columns]")

    # Head preview
    lines.append("")
    head_preview = _build_head_preview(df, flavor, head_n)
    lines.append(f"Head ({head_n}):")
    lines.append(head_preview)
    return "\n".join(lines)


def summarize_dataset(ds: Any) -> str:
    """Produce a ``text/llm+plain`` summary for a HuggingFace ``Dataset``.

    Does NOT call ``.to_pandas()`` or any materializing operation beyond
    reading ``ds[0]`` for a sample row.
    """
    features = ds.features
    num_rows = ds.num_rows
    n_features = len(features) if features else 0

    lines: list[str] = []
    lines.append(f"HuggingFace Dataset: {_format_int(num_rows)} rows × {n_features} features")

    if features:
        lines.append("Features:")
        for name, feat in features.items():
            lines.append(f"  - {name}: {feat}")

    # Dataset description
    if hasattr(ds, "info") and ds.info and getattr(ds.info, "description", None):
        desc = ds.info.description
        if desc.strip():
            excerpt = desc[:200].strip()
            if len(desc) > 200:
                excerpt += "…"
            lines.append("")
            lines.append(f"Description: {excerpt}")

    # Sample row
    if num_rows > 0:
        try:
            row = ds[0]
            lines.append("")
            lines.append("Sample (row 0):")
            for key, value in row.items():
                lines.append(f"  {key}: {_truncate_cell(value, 80)}")
        except Exception:
            pass

    return "\n".join(lines)
