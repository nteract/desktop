import io

import pandas as pd
import pytest
from dx._format import PARQUET_MIME, serialize_dataframe


def test_serialize_pandas_to_parquet():
    df = pd.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
    data, content_type = serialize_dataframe(df, max_bytes=10_000_000)
    assert content_type == PARQUET_MIME
    assert isinstance(data, bytes)
    assert data[:4] == b"PAR1"


def test_serialize_pandas_round_trip():
    import pyarrow.parquet as pq

    df = pd.DataFrame({"a": [1, 2, 3]})
    data, _ = serialize_dataframe(df, max_bytes=10_000_000)
    table = pq.read_table(io.BytesIO(data))
    assert table.column("a").to_pylist() == [1, 2, 3]


def test_serialize_downsamples_when_oversized():
    """When the full payload would exceed max_bytes, downsample."""
    big = pd.DataFrame({"a": list(range(200_000))})
    data, content_type = serialize_dataframe(big, max_bytes=2_000)
    assert content_type == PARQUET_MIME
    # Parquet has a fixed footer overhead; allow generous slack.
    assert len(data) <= 8_000


def test_serialize_polars_when_available():
    pl = pytest.importorskip("polars")
    df = pl.DataFrame({"a": [1, 2, 3]})
    data, content_type = serialize_dataframe(df, max_bytes=10_000_000)
    assert content_type == PARQUET_MIME
    assert data[:4] == b"PAR1"


def test_serialize_rejects_unsupported():
    with pytest.raises(ValueError):
        serialize_dataframe([1, 2, 3], max_bytes=10_000)
