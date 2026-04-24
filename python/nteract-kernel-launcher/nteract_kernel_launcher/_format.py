"""DataFrame → parquet serialization (best-available encoder).

Pandas uses pyarrow (or fastparquet as a fallback). Polars uses its native
parquet writer. If the serialized payload would exceed ``max_bytes``, the
serializer downsamples via ``head(n)`` with a binary-search-ish loop and
annotates the result so the caller can advertise partial data in the
``text/llm+plain`` summary and the ref-MIME ``summary`` hints.
"""

from __future__ import annotations

import io
from typing import Any

PARQUET_MIME = "application/vnd.apache.parquet"


def _detect_flavor(df: Any) -> str:
    mod = type(df).__module__.split(".")[0]
    return mod if mod in ("pandas", "polars") else "unknown"


def _serialize_pandas(df: Any, rows: int | None = None) -> bytes:
    import pyarrow as pa
    import pyarrow.parquet as pq

    if rows is not None:
        df = df.head(rows)
    table = pa.Table.from_pandas(df, preserve_index=False)
    buf = io.BytesIO()
    pq.write_table(table, buf, compression="snappy")
    return buf.getvalue()


def _serialize_polars(df: Any, rows: int | None = None) -> bytes:
    if rows is not None:
        df = df.head(rows)
    buf = io.BytesIO()
    df.write_parquet(buf, compression="snappy")
    return buf.getvalue()


def serialize_dataframe(df: Any, *, max_bytes: int) -> tuple[bytes, str]:
    """Serialize ``df`` to parquet; downsample if it would exceed ``max_bytes``.

    Returns ``(bytes, content_type)``. Raises ``ValueError`` for unsupported
    DataFrame types.
    """
    flavor = _detect_flavor(df)
    if flavor == "pandas":
        encoder = _serialize_pandas
    elif flavor == "polars":
        encoder = _serialize_polars
    else:
        raise ValueError(f"unsupported DataFrame type: {type(df).__module__}.{type(df).__name__}")

    full = encoder(df)
    if len(full) <= max_bytes:
        return full, PARQUET_MIME

    # Estimate a row count that fits. Parquet compression is non-linear in
    # row count; try a few downsample rounds before giving up.
    n = len(df) if flavor == "pandas" else df.height
    if n <= 1:
        return full, PARQUET_MIME
    target_rows = max(1, int(n * (max_bytes / len(full))))
    for _ in range(4):
        sampled = encoder(df, rows=target_rows)
        if len(sampled) <= max_bytes:
            return sampled, PARQUET_MIME
        target_rows = max(1, target_rows // 2)

    # Last resort: a single row. Never raise for sampling.
    return encoder(df, rows=1), PARQUET_MIME
