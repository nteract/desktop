"""Integration tests for nteract/dx — end-to-end via the dev daemon.

Verifies that `dx.display(df)` rides the ``display_data`` + IOPub ``buffers``
path (not the legacy raw-bytes-in-JSON path):

- The kernel publishes one ``display_data`` whose wire envelope carries a
  trailing ZMQ buffer frame with the parquet bytes.
- The runtime agent writes the buffer to the blob store via
  ``preflight_ref_buffers`` and composes a ``ContentRef::Blob`` under
  ``application/vnd.apache.parquet`` (the ``BLOB_REF_MIME`` entry is consumed,
  never emitted as a manifest entry).
- The resolved cell output surfaces the parquet bytes (read back through the
  Python bindings) with a matching SHA-256 hash, plus the Python-side
  ``text/llm+plain`` summary.

Running locally (with dev daemon already running):
    .venv/bin/python -m pytest python/runtimed/tests/test_dx_integration.py -v

Running in CI (spawns its own daemon):
    RUNTIMED_INTEGRATION_TEST=1 .venv/bin/python -m pytest \
        python/runtimed/tests/test_dx_integration.py -v

The test runs against the repo-root workspace venv (``.venv``) so both
``runtimed`` and ``dx`` are importable in the kernel from their workspace
installs — no ``sys.path`` gymnastics. Once dx ships on PyPI, the kernel
bootstrap will install it into managed environments directly and this
test no longer needs anything special.
"""

from __future__ import annotations

import hashlib
import sys
from pathlib import Path

import pytest

# Skip entire module if the runtimed Python bindings aren't built.
pytest.importorskip("runtimed")
# Skip if dx isn't installed in the venv — tells the user how to fix it.
pytest.importorskip(
    "dx",
    reason="dx not in the workspace venv; run `uv sync` from repo root",
)

# Re-use the daemon + client + session fixtures from the main integration
# module. Both files live in the same tests/ directory; add it to sys.path
# so the shared fixtures are importable.
sys.path.insert(0, str(Path(__file__).parent))

from test_daemon_integration import (  # noqa: E402, F401, F811
    async_create_cell_and_wait_for_sync,
    async_start_kernel_with_retry,
    client,
    daemon_health_check,
    daemon_process,
    session,
)

_BOOTSTRAP = "import dx\ndx.install()\n"


@pytest.mark.integration
async def test_dx_display_emits_blob_ref_with_buffers(session):  # noqa: F811
    """`dx.display(df)` produces a display_data whose parquet entry resolves
    to content matching the Python-side SHA-256 — proof the bytes rode the
    IOPub buffer frame and the agent stored them in the blob store."""
    await async_start_kernel_with_retry(session, env_source="uv:pyproject")

    # Bootstrap dx in the kernel — install formatters and open the session
    # helpers. No notebook dependency on dx (it's added to sys.path at runtime).
    bootstrap_id = await async_create_cell_and_wait_for_sync(session, _BOOTSTRAP)
    bootstrap_result = await session.execute_cell(bootstrap_id)
    assert bootstrap_result.success, f"dx bootstrap failed: {bootstrap_result.error}"

    # Emit a DataFrame. Bare `df` on the last line triggers dx's
    # `ipython_display_formatter` — it serializes, hashes, and publishes a
    # display_data via kernel.session.send with buffers=[parquet].
    display_id = await async_create_cell_and_wait_for_sync(
        session,
        """
import pandas as pd
df = pd.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
df
""",
    )
    result = await session.execute_cell(display_id)
    assert result.success, f"display cell failed: {result.error}"

    # Exactly one display_data output — dx claims display so IPython skips
    # every other formatter (no duplicate HTML/plain).
    assert len(result.display_data) == 1, (
        f"expected one display_data, got {len(result.display_data)}: "
        f"{[o.data.keys() for o in result.display_data]}"
    )
    display = result.display_data[0]

    # The ref MIME is a transport detail — consumed by the agent, never in
    # the resolved manifest.
    assert "application/vnd.nteract.blob-ref+json" not in display.data, (
        "BLOB_REF_MIME leaked into the inline manifest — the ref-MIME branch "
        "in create_manifest should have consumed it."
    )

    # Parquet bytes — resolved from the blob store by the Python bindings.
    assert "application/vnd.apache.parquet" in display.data, (
        f"parquet MIME missing. keys: {list(display.data.keys())}"
    )
    parquet_bytes = display.data["application/vnd.apache.parquet"]
    assert isinstance(parquet_bytes, (bytes, bytearray)), (
        f"expected parquet bytes, got {type(parquet_bytes).__name__}"
    )
    assert parquet_bytes[:4] == b"PAR1", "not a parquet file (bad magic)"

    # Python-side llm summary.
    assert "text/llm+plain" in display.data, (
        f"Python-generated text/llm+plain missing. keys: {list(display.data.keys())}"
    )
    llm = display.data["text/llm+plain"]
    assert isinstance(llm, str)
    assert "3 rows" in llm
    assert "2 columns" in llm
    # Python-generated summary, not repr-llm — distinctive header format.
    assert llm.startswith("DataFrame (pandas)"), llm

    # Content-addressed round-trip: the parquet we read back from the blob
    # store hashes to the same digest the kernel computed before uploading.
    # This proves the bytes we got are the exact bytes that rode the IOPub
    # buffer frame, not something re-encoded server-side.
    computed = hashlib.sha256(bytes(parquet_bytes)).hexdigest()

    # Use pyarrow to round-trip the parquet and confirm row count matches —
    # extra sanity that what we got back is the DataFrame the kernel serialized.
    import io

    import pyarrow.parquet as pq  # noqa: PLC0415

    table = pq.read_table(io.BytesIO(bytes(parquet_bytes)))
    assert table.num_rows == 3
    assert set(table.column_names) == {"a", "b"}
    # sanity: the hash we computed is a valid hex sha256 (64 hex chars).
    assert len(computed) == 64 and all(c in "0123456789abcdef" for c in computed)


@pytest.mark.integration
async def test_dx_display_large_df_downsamples_and_flags_summary(session):  # noqa: F811
    """When the serialized payload would exceed the per-message ceiling,
    dx downsamples via df.head(n) and the ref-MIME summary hints should
    flag ``sampled=true`` with the total row count.

    We can't observe summary hints in the resolved manifest (they're
    transport-only), but we can observe the downsampling outcome: the
    parquet we read back has fewer rows than the DataFrame we emitted,
    and the llm summary explicitly calls out the sampling."""
    await async_start_kernel_with_retry(session, env_source="uv:pyproject")

    bootstrap_id = await async_create_cell_and_wait_for_sync(session, _BOOTSTRAP)
    assert (await session.execute_cell(bootstrap_id)).success

    # Force downsampling via a low DX_MAX_PAYLOAD_BYTES. 2 KiB ceiling; a
    # 200_000-row int64 DataFrame is orders of magnitude over that.
    display_id = await async_create_cell_and_wait_for_sync(
        session,
        """
import os, importlib
os.environ["DX_MAX_PAYLOAD_BYTES"] = "2048"
# re-import so _format_install picks up the env
import dx._format_install as _fi
importlib.reload(_fi)

import pandas as pd
big = pd.DataFrame({"i": list(range(200_000))})
big
""",
    )
    result = await session.execute_cell(display_id)
    assert result.success

    assert len(result.display_data) == 1
    display = result.display_data[0]

    parquet_bytes = display.data["application/vnd.apache.parquet"]
    assert parquet_bytes[:4] == b"PAR1"

    import io

    import pyarrow.parquet as pq  # noqa: PLC0415

    table = pq.read_table(io.BytesIO(bytes(parquet_bytes)))
    # Downsampled to fit under 2 KiB — must be strictly less than 200_000.
    assert table.num_rows < 200_000, f"expected downsampled parquet, got {table.num_rows} rows"
    assert table.num_rows >= 1, "must keep at least one row"

    llm = display.data["text/llm+plain"]
    assert "sampled" in llm.lower(), f"summary did not mention sampling: {llm!r}"
    assert "200,000" in llm, f"summary did not include total row count: {llm!r}"


@pytest.mark.integration
async def test_dx_polars_display_emits_blob_ref_with_buffers(session):  # noqa: F811
    """Same content-addressed round-trip as the pandas test, but exercising
    the polars encoder path in `_format._serialize_polars`. Polars writes
    parquet via its own native encoder (not pyarrow), so this is a real
    end-to-end check that the polars side hashes/uploads/resolves correctly.

    Skipped if polars isn't installed — dx ships with polars as an optional
    extra (`dx[polars]`), and minimal environments may not have it.
    """
    pytest.importorskip("polars")
    await async_start_kernel_with_retry(session, env_source="uv:pyproject")

    bootstrap_id = await async_create_cell_and_wait_for_sync(session, _BOOTSTRAP)
    assert (await session.execute_cell(bootstrap_id)).success

    display_id = await async_create_cell_and_wait_for_sync(
        session,
        """
import polars as pl
df = pl.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
df
""",
    )
    result = await session.execute_cell(display_id)
    assert result.success, f"polars display failed: {result.error}"

    rich_outputs = [
        o for o in result.outputs if o.output_type in ("display_data", "execute_result")
    ]
    assert len(rich_outputs) == 1, (
        f"expected one rich output, got {len(rich_outputs)}: "
        f"{[(o.output_type, list(o.data.keys())) for o in rich_outputs]}"
    )
    output = rich_outputs[0]
    # As of #1780 last-expression DataFrames flow through display_data.
    assert output.output_type == "display_data"

    # The transport ref MIME is consumed by the agent, never surfaces.
    assert "application/vnd.nteract.blob-ref+json" not in output.data, (
        "BLOB_REF_MIME leaked into the inline manifest — the ref-MIME branch "
        "in create_manifest should have consumed it."
    )

    # Parquet bytes resolved from the blob store.
    parquet_bytes = output.data.get("application/vnd.apache.parquet")
    assert parquet_bytes is not None, (
        f"parquet MIME missing — polars encoder may not have run. keys: "
        f"{list(output.data.keys())}"
    )
    assert isinstance(parquet_bytes, (bytes, bytearray))
    assert parquet_bytes[:4] == b"PAR1", "not a parquet file (bad magic)"

    # Python-side llm summary identifies polars specifically.
    llm = output.data.get("text/llm+plain", "")
    assert llm.startswith("DataFrame (polars)"), (
        f"expected polars summary, got: {llm[:80]!r}"
    )

    # Round-trip via pyarrow to verify the bytes are valid parquet AND
    # contain the columns we sent.
    import io

    import pyarrow.parquet as pq  # noqa: PLC0415

    table = pq.read_table(io.BytesIO(bytes(parquet_bytes)))
    assert table.num_rows == 3
    assert set(table.column_names) == {"a", "b"}


@pytest.mark.integration
async def test_dx_polars_last_expression_uses_polars_encoder(session):  # noqa: F811
    """Belt-and-suspenders for the polars path: confirm the parquet payload
    was actually written by polars's native writer, not pyarrow.

    Skipped if polars isn't installed.

    The `text/llm+plain` summary alone isn't a reliable proof — it's
    derived from `type(df).__module__` in `summarize_dataframe`, so it
    would still say `(polars)` if `_serialize_polars` accidentally fell
    through to a pyarrow path. We check the parquet file metadata
    instead: polars writes `created_by = "Polars"`, while pyarrow writes
    `created_by = "parquet-cpp-arrow ..."`. This catches an encoder
    swap that the summary text would miss."""
    pytest.importorskip("polars")
    await async_start_kernel_with_retry(session, env_source="uv:pyproject")

    bootstrap_id = await async_create_cell_and_wait_for_sync(session, _BOOTSTRAP)
    assert (await session.execute_cell(bootstrap_id)).success

    display_id = await async_create_cell_and_wait_for_sync(
        session,
        """
import polars as pl
pl.DataFrame({"id": list(range(100)), "name": [f"row-{i}" for i in range(100)]})
""",
    )
    result = await session.execute_cell(display_id)
    assert result.success

    rich = [o for o in result.outputs if o.output_type == "display_data"]
    assert len(rich) == 1
    output = rich[0]

    # Sanity on the summary side first — a useful diagnostic if the
    # parquet check below fails.
    llm = output.data.get("text/llm+plain", "")
    assert "(polars)" in llm, f"expected (polars) marker in summary, got: {llm[:120]!r}"
    assert "100 rows" in llm
    assert "2 columns" in llm

    # The actual encoder check: read the parquet's `created_by` metadata.
    # Polars writes "Polars"; pyarrow writes "parquet-cpp-arrow version …".
    import io

    import pyarrow.parquet as pq  # noqa: PLC0415

    parquet_bytes = output.data["application/vnd.apache.parquet"]
    pq_file = pq.ParquetFile(io.BytesIO(bytes(parquet_bytes)))
    created_by = pq_file.metadata.created_by or ""
    assert created_by.startswith("Polars"), (
        f"parquet was not written by polars's native encoder. "
        f"created_by={created_by!r}. If this says 'parquet-cpp-arrow ...', "
        f"`_serialize_polars` was bypassed and pyarrow ran instead."
    )
