"""Unit tests for the enriched ``text/llm+plain`` summary generation.

Tests verify per-column stats, truncation behavior, polars parity, and
HuggingFace Dataset summaries.
"""

import pandas as pd
import pytest
from dx._summary import (
    _MAX_TEXT_PLAIN_BYTES,
    _truncate_cell,
    summarize_dataframe,
    summarize_dataset,
)


def _pad_pandas(df: pd.DataFrame) -> pd.DataFrame:
    """Add padding columns so the text/plain exceeds the fast-path threshold."""
    while len(df.to_string(index=False).encode("utf-8")) <= _MAX_TEXT_PLAIN_BYTES:
        df[f"_pad{len(df.columns)}"] = ["padding_value"] * len(df)
    return df


_pad_to_force_rich_path = _pad_pandas

# ── Truncation ──────────────────────────────────────────────────────


class TestTruncateCell:
    def test_short_string_unchanged(self):
        assert _truncate_cell("hello", 80) == "hello"

    def test_exact_length_unchanged(self):
        s = "x" * 80
        assert _truncate_cell(s, 80) == s

    def test_long_string_truncated_with_suffix(self):
        s = "a" * 200
        result = _truncate_cell(s, 80)
        assert "…" in result
        assert "+120 chars]" in result or "chars]" in result
        assert len(result) <= 200  # reasonable upper bound

    def test_non_string_value_converted(self):
        assert _truncate_cell(42) == "42"
        assert _truncate_cell(None) == "None"


# ── Pandas numeric range ────────────────────────────────────────────


class TestPandasNumericRange:
    def test_includes_numeric_range(self):
        df = _pad_to_force_rich_path(pd.DataFrame({"score": [0.12, 0.5, 0.99]}))
        out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
        assert "range" in out
        assert "0.120" in out
        assert "0.990" in out

    def test_integer_range(self):
        df = _pad_to_force_rich_path(pd.DataFrame({"id": [1, 500, 1200]}))
        out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
        assert "range" in out
        assert "1" in out
        assert "1,200" in out


# ── Pandas string distinct + top ────────────────────────────────────


class TestPandasStringStats:
    def test_includes_string_distinct_and_top(self):
        df = _pad_to_force_rich_path(
            pd.DataFrame({"name": ["alice", "bob", "carol", "alice", "bob"]})
        )
        n = len(df)
        out = summarize_dataframe(df, total_rows=n, included_rows=n, sampled=False)
        assert "distinct" in out
        assert "top:" in out
        assert '"alice"' in out or '"bob"' in out

    def test_single_value_column(self):
        df = _pad_to_force_rich_path(pd.DataFrame({"status": ["ok"] * 10}))
        out = summarize_dataframe(df, total_rows=10, included_rows=10, sampled=False)
        assert "1 distinct" in out
        assert '"ok"' in out


# ── Truncation integration ──────────────────────────────────────────


class TestTruncationIntegration:
    def test_text_heavy_summary_stays_compact(self):
        """A DataFrame with 200-char text columns should produce a summary
        that stays under 1 KB — verifying that both the head preview and
        column stats are truncated."""
        long_text = "x" * 200
        df = pd.DataFrame(
            {
                "bio": [long_text] * 5,
                "notes": [long_text] * 5,
            }
        )
        out = summarize_dataframe(df, total_rows=5, included_rows=5, sampled=False)
        assert len(out) < 1024, f"Summary is {len(out)} bytes, expected < 1024"


# ── Pandas null handling ────────────────────────────────────────────


class TestNullHandling:
    def test_all_null_column(self):
        df = _pad_to_force_rich_path(pd.DataFrame({"empty": [None, None, None]}))
        out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
        assert "all null" in out

    def test_partial_null_with_percentage(self):
        df = _pad_to_force_rich_path(pd.DataFrame({"a": [1.0, None, 3.0]}))
        out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
        assert "null" in out
        assert "%" in out


# ── Pandas temporal ─────────────────────────────────────────────────


class TestPandasTemporal:
    def test_datetime_range(self):
        df = pd.DataFrame({"ts": pd.to_datetime(["2024-01-01", "2024-06-15", "2024-12-31"])})
        out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
        assert "2024-01-01" in out
        assert "2024-12-31" in out


# ── Wide DataFrame capping ──────────────────────────────────────────


class TestWideDataFrame:
    def test_wide_dataframe_capped(self):
        """DataFrames with 100+ columns should cap at ~40 columns in the
        summary with a ``…[+N more columns]`` suffix."""
        data = {f"col_{i}": [i] for i in range(120)}
        df = pd.DataFrame(data)
        out = summarize_dataframe(df, total_rows=1, included_rows=1, sampled=False)
        assert "more columns]" in out
        assert "120 columns" in out


# ── Polars parity ──────────────────────────────────────────────────


class TestPolars:
    @pytest.fixture(autouse=True)
    def _require_polars(self):
        pytest.importorskip("polars")

    def test_polars_numeric_range(self):
        import polars as pl

        data: dict = {"val": [10, 20, 30]}
        for i in range(20):
            data[f"_pad{i}"] = ["padding_value"] * 3
        df = pl.DataFrame(data)
        out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
        assert "range" in out
        assert "10" in out
        assert "30" in out

    def test_polars_string_distinct(self):
        import polars as pl

        data: dict = {"name": ["alice", "bob", "carol"]}
        for i in range(20):
            data[f"_pad{i}"] = ["padding_value"] * 3
        df = pl.DataFrame(data)
        out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
        assert "distinct" in out

    def test_polars_temporal(self):
        from datetime import date

        import polars as pl

        df = pl.DataFrame({"d": [date(2024, 1, 1), date(2024, 6, 15), date(2024, 12, 31)]})
        out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
        assert "2024-01-01" in out
        assert "2024-12-31" in out

    def test_polars_datetime(self):
        from datetime import datetime

        import polars as pl

        df = pl.DataFrame({"ts": [datetime(2024, 1, 1), datetime(2024, 12, 31)]})
        out = summarize_dataframe(df, total_rows=2, included_rows=2, sampled=False)
        assert "2024-01-01" in out
        assert "2024-12-31" in out

    def test_polars_duration(self):
        from datetime import timedelta

        import polars as pl

        df = pl.DataFrame({"dur": [timedelta(seconds=10), timedelta(hours=1)]})
        out = summarize_dataframe(df, total_rows=2, included_rows=2, sampled=False)
        # Duration should be handled as temporal without crashing
        assert "dur" in out

    def test_polars_all_null(self):
        import polars as pl

        schema: dict = {"x": pl.Int64}
        data: dict = {"x": [None, None, None]}
        for i in range(20):
            data[f"_pad{i}"] = ["padding_value"] * 3
            schema[f"_pad{i}"] = pl.Utf8
        df = pl.DataFrame(data, schema=schema)
        out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
        assert "all null" in out

    def test_polars_summary_matches_pandas_shape(self):
        """Polars and pandas summaries for the same data should contain
        equivalent structural elements."""
        import polars as pl

        pd_data: dict = {"a": [1, 2, 3], "b": ["x", "y", "z"]}
        pl_data: dict = {"a": [1, 2, 3], "b": ["x", "y", "z"]}
        for i in range(20):
            pd_data[f"_pad{i}"] = ["padding_value"] * 3
            pl_data[f"_pad{i}"] = ["padding_value"] * 3
        pd_df = pd.DataFrame(pd_data)
        pl_df = pl.DataFrame(pl_data)

        pd_out = summarize_dataframe(pd_df, total_rows=3, included_rows=3, sampled=False)
        pl_out = summarize_dataframe(pl_df, total_rows=3, included_rows=3, sampled=False)

        # Both should have the same structural sections
        for section in ["Columns:", "Head (", "range", "distinct"]:
            assert section in pd_out, f"pandas missing section: {section}"
            assert section in pl_out, f"polars missing section: {section}"


# ── Dataset summary ─────────────────────────────────────────────────


class TestDatasetSummary:
    @pytest.fixture(autouse=True)
    def _require_datasets(self):
        pytest.importorskip("datasets")

    def test_dataset_summary_from_features_only(self):
        from datasets import Dataset

        ds = Dataset.from_dict({"text": ["hello", "world"], "label": [0, 1]})
        out = summarize_dataset(ds)
        assert "HuggingFace Dataset" in out
        assert "2 rows" in out
        assert "2 features" in out
        assert "text" in out
        assert "label" in out

    def test_dataset_summary_includes_sample_row(self):
        from datasets import Dataset

        ds = Dataset.from_dict({"name": ["alice"], "score": [0.95]})
        out = summarize_dataset(ds)
        assert "Sample (row 0):" in out
        assert "alice" in out

    def test_dataset_summary_empty(self):
        from datasets import Dataset

        ds = Dataset.from_dict({"a": [], "b": []})
        out = summarize_dataset(ds)
        assert "0 rows" in out
        # No sample row for empty dataset
        assert "Sample" not in out

    def test_dataset_summary_truncates_long_values(self):
        from datasets import Dataset

        ds = Dataset.from_dict({"bio": ["x" * 200]})
        out = summarize_dataset(ds)
        assert "…" in out
        assert "chars]" in out


# ── Small DataFrame text/plain fast path ───────────────────────────


class TestSmallDataFrameTextPlainPath:
    """When a DataFrame's native text/plain fits under 1 KB, summarize_dataframe
    should return it directly instead of running the stat-rich path."""

    def test_small_pandas_uses_text_plain(self):
        df = pd.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
        out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
        assert out.startswith("DataFrame (pandas): 3 rows × 2 columns")
        assert "Columns:" not in out
        assert "Head (" not in out
        # Should contain the actual cell values
        assert "x" in out and "y" in out and "z" in out

    def test_small_polars_uses_text_plain(self):
        pl = pytest.importorskip("polars")
        df = pl.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
        out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
        assert out.startswith("DataFrame (polars): 3 rows × 2 columns")
        assert "Columns:" not in out
        assert "Head (" not in out

    def test_large_pandas_uses_rich_path(self):
        df = pd.DataFrame({"text": ["word " * 40] * 100})
        out = summarize_dataframe(df, total_rows=100, included_rows=100, sampled=False)
        assert "Columns:" in out
        assert "Head (" in out

    def test_sampled_frame_uses_rich_path(self):
        df = pd.DataFrame({"a": [1, 2, 3]})
        out = summarize_dataframe(df, total_rows=1000, included_rows=3, sampled=True)
        assert "Columns:" in out
        assert "(sampled from" in out

    def test_wide_frame_uses_rich_path(self):
        data = {f"c{i}": [i] for i in range(50)}
        df = pd.DataFrame(data)
        out = summarize_dataframe(df, total_rows=1, included_rows=1, sampled=False)
        assert "Columns:" in out

    def test_sampled_but_equal_totals_uses_text_plain(self):
        df = pd.DataFrame({"a": [1, 2]})
        out = summarize_dataframe(df, total_rows=2, included_rows=2, sampled=True)
        assert "Columns:" not in out
        assert "Head (" not in out
