import pandas as pd
import pytest
from dx._summary import summarize_dataframe


def test_summarize_pandas_basic():
    df = pd.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
    out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
    assert "3 rows" in out
    assert "2 columns" in out
    assert "a" in out and "b" in out


def test_summarize_pandas_sampled_mentions_sampling():
    df = pd.DataFrame({"a": list(range(100))})
    out = summarize_dataframe(df, total_rows=1_000_000, included_rows=100, sampled=True)
    assert "sampled" in out.lower()
    assert "1,000,000" in out
    assert "100" in out


def test_summarize_pandas_includes_dtypes():
    df = pd.DataFrame({"i": [1, 2], "s": ["a", "b"]})
    out = summarize_dataframe(df, total_rows=2, included_rows=2, sampled=False)
    assert "int" in out.lower()
    assert "object" in out.lower() or "str" in out.lower()


def test_summarize_pandas_includes_null_counts():
    df = pd.DataFrame({"a": [1.0, None, 3.0], "b": [None, None, "x"]})
    out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
    assert "null" in out.lower()


def test_summarize_polars_basic():
    pl = pytest.importorskip("polars")
    df = pl.DataFrame({"a": [1, 2, 3]})
    out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
    assert "3 rows" in out
    assert "a" in out
