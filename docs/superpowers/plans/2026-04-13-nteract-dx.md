# nteract/dx Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a Python library `dx` that pushes bytes from a kernel to the daemon's blob store via a Jupyter comm (`nteract.dx.blob`), bypassing IOPub for binary payloads, with a new `application/vnd.nteract.blob-ref+json` ref MIME that the runtime agent resolves into ContentRefs in the CRDT.

**Architecture:** Kernel ↔ agent ZMQ carries a dedicated comm target for blob uploads (bytes in `buffers`, not base64 JSON). Agent calls `BlobStore::put` directly, acks with a hash, and when a subsequent IOPub `display_data` carries the ref MIME, composes a ContentRef in the inline manifest without re-uploading. The `nteract.dx.*` comm namespace is filtered out of `RuntimeStateDoc` persistence so large buffers never land in the CRDT. A query field is reserved in the ref MIME for a future interactive query backend.

**Tech Stack:** Python (pyarrow, polars-optional, ipykernel), Rust (runtimed, notebook-doc, notebook-protocol), pytest, `cargo test`, WebdriverIO for E2E, uv workspace.

**Spec:** `docs/superpowers/specs/2026-04-13-nteract-dx-design.md` *(revised 2026-04-14; see top of spec)*

### Plan status — SUPERSEDED in part

Tasks 1–8 (MIME constant, python/dx scaffold, refs, env, summary, format, install) landed as originally planned.

Tasks 9–10 were redesigned mid-implementation after smoke-testing revealed a deadlock:
the original comm-with-ack upload path cannot work because ipykernel dispatches shell
`comm_msg`s on the same asyncio loop that runs cells, so `dx.put()` blocking on an ack
event starves the dispatcher and the ack never arrives. Details in the spec under
"Why fire-and-forget".

**What actually shipped for Tasks 9–10:**

- Upload rides the Jupyter messaging envelope's `buffers` field attached to `display_data`,
  not a comm. Python calls `kernel.session.send(...)` directly with `buffers=[parquet_bytes]`.
- Agent adds `preflight_ref_buffers` in `crates/runtimed/src/output_store.rs` — called
  from the IOPub DisplayData/ExecuteResult arms before `create_manifest`. Walks the data
  bundle for `BLOB_REF_MIME` entries with `buffer_index`, writes each to `BlobStore::put`,
  verifies hash.
- `create_manifest`'s ref-MIME branch (still present) composes `ContentRef::from_hash` under
  the target content_type. Ref MIME does not appear in the inline manifest.
- `nteract.dx.*` comm namespace filter stays in the IOPub handler — all dx comm targets log
  a `warn!` and drop. `DxTarget` enum collapsed to `Unknown(String)` only; future query /
  stream / attach variants will grow back live-dispatch branches.
- Python `dx._comm` module + `BlobClient` / `FallbackClient` / `_Pending` plumbing:
  **deleted**. Exceptions reduced to `DxError`. Public API: `install`, `display`, `BlobRef`,
  `BLOB_REF_MIME`, `DxError`.
- Display ownership: dx registers on `ipython_display_formatter` (not `mimebundle_formatter`)
  and returns `True` when it publishes a `display_data`. This tells IPython to skip every
  other formatter for the DataFrame, so bare `df` on the last cell line emits one output,
  not a duplicate HTML/plain alongside.

Earlier drafts threaded a `blob_base_url` through `BlobRef` and used a `RUNTIMED_BLOB_BASE_URL`
env var. Both removed. `BlobRef` is `(hash, size)`. Kernel-side never needs a URL; frontend
WASM derives the current blob URL from the hash at render time.

Task 11 (Python integration test) and Task 13 (WebdriverIO E2E) are still pending and now
target the new buffers-on-display path.

Task 12 reworked to a publishing-prep checklist — auto-install of `dx` is deferred to v1.1.
v1 UX is an explicit `import dx; dx.install()` in the first cell (or in the bootstrap
startup once dx ships on PyPI).

**Manual smoke test completed 2026-04-14**: bare `df` on last cell line emits exactly
one `display_data` with `application/vnd.apache.parquet` resolved to a blob URL +
`text/llm+plain` Python-side summary. Verified by intercepting `session.send` to confirm
the actual wire payload carries `BLOB_REF_MIME` + `buffers=[parquet_bytes]` (not raw
bytes inside JSON). 50,000-row DataFrame follows the same path with no IOPub JSON
carrying the megabytes.

The rest of this document describes the original task decomposition with the
pre-redesign transport; it is retained as historical context. For the current
implementation, refer to the spec and the commits listed under "Implementation trail"
at the bottom of the spec.

---

## File Structure

### New files

- `crates/notebook-doc/src/mime.rs` — extend with blob-ref MIME constant.
- `python/dx/pyproject.toml` — new uv workspace member.
- `python/dx/src/dx/__init__.py` — public API re-exports, `install`, exceptions.
- `python/dx/src/dx/_refs.py` — `BlobRef` dataclass, ref-MIME factory.
- `python/dx/src/dx/_env.py` — environment detection.
- `python/dx/src/dx/_summary.py` — `text/llm+plain` generator.
- `python/dx/src/dx/_format.py` — DataFrame → parquet serializer chain + IPython formatter registration.
- `python/dx/src/dx/_comm.py` — comm client (open, multiplex, timeout, fallback).
- `python/dx/tests/test_refs.py`
- `python/dx/tests/test_env.py`
- `python/dx/tests/test_summary.py`
- `python/dx/tests/test_format.py`
- `python/dx/tests/test_comm.py`
- `python/dx/tests/test_install.py`
- `python/runtimed/tests/test_dx_integration.py` — end-to-end against dev daemon.
- `crates/runtimed/tests/dx_comm_filter.rs` — asserts `nteract.dx.*` comms don't hit `RuntimeStateDoc`.

### Modified files

- `pyproject.toml` (repo root) — add `python/dx` to workspace members.
- `crates/runtimed/src/jupyter_kernel.rs` — target-name filter on comm_open / comm_msg, dx blob comm handler, shell-socket send of ack.
- `crates/runtimed/src/output_store.rs` — recognize ref MIME in `create_manifest`, compose ContentRef without re-upload.
- `crates/kernel-launch/src/lib.rs` (or equivalent) — add `import dx; dx.install()` to ipykernel startup + include `dx` as a bootstrap dep.
- `docs/python-bindings.md` — brief "Using dx" note linking to the spec.

---

## Task 1: Add blob-ref MIME constant (notebook-doc)

**Files:**
- Modify: `crates/notebook-doc/src/mime.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/notebook-doc/src/mime.rs`:

```rust
#[cfg(test)]
mod blob_ref_tests {
    use super::*;

    #[test]
    fn blob_ref_mime_is_text_not_binary() {
        // The ref MIME is a tiny JSON bundle, not binary.
        assert!(!is_binary_mime(BLOB_REF_MIME));
        assert_eq!(mime_kind(BLOB_REF_MIME), MimeKind::Json);
    }

    #[test]
    fn blob_ref_mime_constant_value() {
        assert_eq!(BLOB_REF_MIME, "application/vnd.nteract.blob-ref+json");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

```bash
cargo test -p notebook-doc blob_ref_tests
```

Expected: compile error — `BLOB_REF_MIME` not found.

- [ ] **Step 3: Implement**

Add near the top of `crates/notebook-doc/src/mime.rs` (after existing constants):

```rust
/// MIME type for a blob reference bundle.
///
/// Emitted by `dx.display(...)` in place of raw binary bytes. The payload is a
/// small JSON object carrying a content hash and the target `content_type`;
/// the runtime agent composes a `ContentRef` in the inline output manifest from it.
///
/// Schema:
/// ```json
/// { "hash": "sha256:...", "content_type": "application/vnd.apache.parquet",
///   "size": 104857600, "summary": {...}?, "query": null }
/// ```
pub const BLOB_REF_MIME: &str = "application/vnd.nteract.blob-ref+json";
```

No changes to `is_binary_mime` or `mime_kind` needed — they already route `application/*+json` to `MimeKind::Json` (text). If they don't, add the `+json` suffix handling in the same patch.

Verify the existing `+json` suffix handling:

```bash
cargo test -p notebook-doc mime
```

- [ ] **Step 4: Run test to verify it passes**

```bash
cargo test -p notebook-doc blob_ref_tests
```

Expected: 2 passed.

- [ ] **Step 5: Commit**

```bash
git add crates/notebook-doc/src/mime.rs
git commit -m "feat(notebook-doc): add application/vnd.nteract.blob-ref+json MIME constant"
```

---

## Task 2: Scaffold python/dx workspace member

**Files:**
- Create: `python/dx/pyproject.toml`
- Create: `python/dx/src/dx/__init__.py`
- Create: `python/dx/tests/__init__.py`
- Create: `python/dx/tests/conftest.py`
- Modify: `pyproject.toml` (repo root)

- [ ] **Step 1: Add workspace member**

Inspect the repo root `pyproject.toml`:

```bash
grep -A 20 "\[tool.uv.workspace\]" pyproject.toml
```

Add `python/dx` to the `members` list.

- [ ] **Step 2: Create `python/dx/pyproject.toml`**

```toml
[project]
name = "dx"
version = "0.1.0"
description = "nteract/dx — efficient display and blob-store uploads from Python kernels"
requires-python = ">=3.10"
dependencies = [
    "ipykernel>=6.0",
]

[project.optional-dependencies]
pandas = ["pandas>=2.0", "pyarrow>=14"]
polars = ["polars>=0.20"]
test = [
    "pytest>=8",
    "pytest-asyncio>=0.23",
    "pandas>=2.0",
    "pyarrow>=14",
    "polars>=0.20",
]

[build-system]
requires = ["hatchling"]
build-backend = "hatchling.build"

[tool.hatch.build.targets.wheel]
packages = ["src/dx"]
```

- [ ] **Step 3: Create `python/dx/src/dx/__init__.py`**

```python
"""nteract/dx — efficient Python → blob store display.

Public API:
- install(): register IPython formatters + open the runtime agent comm channel
- display(obj): upgraded display routed through the blob store when possible
- put(data, content_type): low-level upload primitive
- display_blob_ref(ref): emit a display_data bundle referencing an existing blob
- BlobRef: dataclass returned by put()
- DxError, DxNoAgentError, DxTimeoutError, DxPayloadTooLargeError
"""

from dx._refs import BlobRef  # noqa: F401

__version__ = "0.1.0"


class DxError(Exception):
    """Base class for dx exceptions."""


class DxNoAgentError(DxError):
    """Raised when a blob-store operation is requested but no agent is reachable."""


class DxTimeoutError(DxError):
    """Raised when an agent comm request does not ack within the configured timeout."""


class DxPayloadTooLargeError(DxError):
    """Raised when an upload exceeds the runtime agent's MAX_BLOB_SIZE."""


def install() -> None:
    """Install IPython formatters and open the nteract.dx.blob comm.

    Idempotent. Safe to call in vanilla Jupyter or plain python — in those
    environments it installs a no-op fallback and returns silently.
    """
    from dx._format import install_formatters

    install_formatters()


def display(obj) -> None:
    """Display `obj` using dx's upgraded path when available.

    Falls back to IPython.display.display for objects dx does not handle.
    """
    from dx._format import dx_display

    dx_display(obj)


def put(data: bytes, content_type: str) -> BlobRef:
    """Upload `data` to the blob store and return a BlobRef."""
    from dx._comm import put_blob

    return put_blob(data, content_type)


def display_blob_ref(ref: BlobRef, summary: dict | None = None) -> None:
    """Emit a display_data bundle for an existing blob ref."""
    from dx._format import publish_ref

    publish_ref(ref, summary=summary)
```

- [ ] **Step 4: Create empty test harness**

`python/dx/tests/__init__.py` — empty file.

`python/dx/tests/conftest.py`:

```python
"""Shared fixtures for dx tests."""

import pytest


@pytest.fixture
def no_ipykernel(monkeypatch):
    """Pretend we're not running under ipykernel."""
    import sys

    monkeypatch.setitem(sys.modules, "ipykernel", None)
    yield
```

- [ ] **Step 5: Install and verify**

```bash
cd python/dx && uv sync --extra test
```

Expected: resolves, installs, no errors.

Smoke-check import:

```bash
cd /path/to/repo && uv run --package dx python -c "import dx; print(dx.__version__)"
```

Expected output: `0.1.0`.

- [ ] **Step 6: Commit**

```bash
git add python/dx pyproject.toml
git commit -m "feat(dx): scaffold python/dx workspace member"
```

---

## Task 3: `dx._refs` — BlobRef + ref-MIME factory

**Files:**
- Create: `python/dx/src/dx/_refs.py`
- Create: `python/dx/tests/test_refs.py`

- [ ] **Step 1: Write the failing tests**

`python/dx/tests/test_refs.py`:

```python
from dx._refs import BlobRef, BLOB_REF_MIME, build_ref_bundle


def test_blob_ref_dataclass_fields():
    ref = BlobRef(hash="sha256:abc", url="http://127.0.0.1:1234/blob/sha256/abc", size=42)
    assert ref.hash == "sha256:abc"
    assert ref.url == "http://127.0.0.1:1234/blob/sha256/abc"
    assert ref.size == 42


def test_ref_mime_constant():
    assert BLOB_REF_MIME == "application/vnd.nteract.blob-ref+json"


def test_build_ref_bundle_minimal():
    ref = BlobRef(hash="sha256:abc", url="http://x/blob/abc", size=10)
    bundle = build_ref_bundle(ref, content_type="image/png")
    assert bundle == {
        "hash": "sha256:abc",
        "content_type": "image/png",
        "size": 10,
        "query": None,
    }


def test_build_ref_bundle_with_summary():
    ref = BlobRef(hash="sha256:abc", url="http://x/blob/abc", size=10)
    summary = {"total_rows": 100, "included_rows": 50, "sampled": True, "sample_strategy": "head"}
    bundle = build_ref_bundle(ref, content_type="application/vnd.apache.parquet", summary=summary)
    assert bundle["summary"] == summary
    assert bundle["query"] is None


def test_build_ref_bundle_no_url_leak():
    """URLs are session-ephemeral; they must not end up in the ref bundle."""
    ref = BlobRef(hash="sha256:abc", url="http://127.0.0.1:9999/blob/sha256/abc", size=10)
    bundle = build_ref_bundle(ref, content_type="image/png")
    assert "url" not in bundle
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cd python/dx && uv run pytest tests/test_refs.py -v
```

Expected: import error — `_refs` module not found.

- [ ] **Step 3: Implement**

`python/dx/src/dx/_refs.py`:

```python
"""BlobRef dataclass and ref-MIME bundle construction."""

from __future__ import annotations

from dataclasses import dataclass
from typing import Optional

BLOB_REF_MIME = "application/vnd.nteract.blob-ref+json"


@dataclass(frozen=True)
class BlobRef:
    """A content-addressed reference to a blob in the daemon's blob store.

    `hash` is the persistent identity (e.g. "sha256:...").
    `url` is a current-session URL; it is NOT persisted in the CRDT and
    should not be stored anywhere durable.
    """

    hash: str
    url: str
    size: int


def build_ref_bundle(
    ref: BlobRef,
    *,
    content_type: str,
    summary: Optional[dict] = None,
    query: Optional[dict] = None,
) -> dict:
    """Build the JSON bundle for `application/vnd.nteract.blob-ref+json`.

    Note: the URL is intentionally omitted. The frontend derives the current
    blob-server URL from the hash at render time.
    """
    bundle: dict = {
        "hash": ref.hash,
        "content_type": content_type,
        "size": ref.size,
        "query": query,
    }
    if summary is not None:
        bundle["summary"] = summary
    return bundle
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd python/dx && uv run pytest tests/test_refs.py -v
```

Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
git add python/dx/src/dx/_refs.py python/dx/tests/test_refs.py
git commit -m "feat(dx): add BlobRef and build_ref_bundle"
```

---

## Task 4: `dx._env` — environment detection

**Files:**
- Create: `python/dx/src/dx/_env.py`
- Create: `python/dx/tests/test_env.py`

- [ ] **Step 1: Write the failing tests**

`python/dx/tests/test_env.py`:

```python
from dx._env import detect_environment, Environment


def test_detect_plain_python_when_no_ipython(monkeypatch):
    monkeypatch.setattr("dx._env._get_ipython", lambda: None)
    assert detect_environment() == Environment.PLAIN_PYTHON


def test_detect_ipython_without_kernel(monkeypatch):
    class FakeIPython:
        kernel = None

    monkeypatch.setattr("dx._env._get_ipython", lambda: FakeIPython())
    assert detect_environment() == Environment.IPYTHON_NO_KERNEL


def test_detect_ipykernel(monkeypatch):
    class FakeKernel:
        pass

    class FakeIPython:
        kernel = FakeKernel()

    monkeypatch.setattr("dx._env._get_ipython", lambda: FakeIPython())
    assert detect_environment() == Environment.IPYKERNEL
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cd python/dx && uv run pytest tests/test_env.py -v
```

Expected: ImportError on `dx._env`.

- [ ] **Step 3: Implement**

`python/dx/src/dx/_env.py`:

```python
"""Detect the runtime environment dx is operating in."""

from __future__ import annotations

from enum import Enum
from typing import Optional


class Environment(str, Enum):
    PLAIN_PYTHON = "plain_python"
    IPYTHON_NO_KERNEL = "ipython_no_kernel"
    IPYKERNEL = "ipykernel"


def _get_ipython():
    """Return the active IPython instance, or None.

    Extracted for test monkeypatching.
    """
    try:
        from IPython import get_ipython as _gi  # type: ignore
    except ImportError:
        return None
    return _gi()


def detect_environment() -> Environment:
    ip = _get_ipython()
    if ip is None:
        return Environment.PLAIN_PYTHON
    if getattr(ip, "kernel", None) is None:
        return Environment.IPYTHON_NO_KERNEL
    return Environment.IPYKERNEL
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd python/dx && uv run pytest tests/test_env.py -v
```

Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
git add python/dx/src/dx/_env.py python/dx/tests/test_env.py
git commit -m "feat(dx): environment detection"
```

---

## Task 5: `dx._summary` — text/llm+plain generator

**Files:**
- Create: `python/dx/src/dx/_summary.py`
- Create: `python/dx/tests/test_summary.py`

- [ ] **Step 1: Write the failing tests**

`python/dx/tests/test_summary.py`:

```python
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
    assert "1,000,000" in out or "1000000" in out
    assert "100" in out


def test_summarize_pandas_includes_dtypes():
    df = pd.DataFrame({"i": [1, 2], "s": ["a", "b"]})
    out = summarize_dataframe(df, total_rows=2, included_rows=2, sampled=False)
    assert "int" in out.lower()  # dtype for i
    assert "object" in out.lower() or "str" in out.lower()  # dtype for s


def test_summarize_pandas_includes_null_counts():
    df = pd.DataFrame({"a": [1, None, 3], "b": [None, None, "x"]})
    out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
    assert "null" in out.lower() or "nan" in out.lower()


@pytest.mark.skipif(
    pytest.importorskip("polars", reason="polars not installed") is None,
    reason="polars not installed",
)
def test_summarize_polars_basic():
    import polars as pl

    df = pl.DataFrame({"a": [1, 2, 3]})
    out = summarize_dataframe(df, total_rows=3, included_rows=3, sampled=False)
    assert "3 rows" in out
    assert "a" in out
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cd python/dx && uv run pytest tests/test_summary.py -v
```

Expected: ImportError on `dx._summary`.

- [ ] **Step 3: Implement**

`python/dx/src/dx/_summary.py`:

```python
"""Generate text/llm+plain summaries for DataFrames, computed in Python."""

from __future__ import annotations

from typing import Any


def _format_int(n: int) -> str:
    return f"{n:,}"


def _pandas_dtypes(df: Any) -> list[tuple[str, str]]:
    return [(str(col), str(df[col].dtype)) for col in df.columns]


def _pandas_null_counts(df: Any) -> list[tuple[str, int]]:
    counts = df.isna().sum()
    return [(str(col), int(counts[col])) for col in df.columns]


def _polars_dtypes(df: Any) -> list[tuple[str, str]]:
    return [(str(col), str(dtype)) for col, dtype in zip(df.columns, df.dtypes)]


def _polars_null_counts(df: Any) -> list[tuple[str, int]]:
    counts = df.null_count().row(0)
    return [(str(col), int(n)) for col, n in zip(df.columns, counts)]


def _detect_flavor(df: Any) -> str:
    mod = type(df).__module__.split(".")[0]
    if mod == "pandas":
        return "pandas"
    if mod == "polars":
        return "polars"
    return mod


def summarize_dataframe(
    df: Any,
    *,
    total_rows: int,
    included_rows: int,
    sampled: bool,
    head_n: int = 10,
) -> str:
    """Produce a text/llm+plain summary string."""
    flavor = _detect_flavor(df)
    lines: list[str] = []

    if flavor == "pandas":
        dtypes = _pandas_dtypes(df)
        nulls = _pandas_null_counts(df)
        head_repr = df.head(head_n).to_string()
    elif flavor == "polars":
        dtypes = _polars_dtypes(df)
        nulls = _polars_null_counts(df)
        head_repr = str(df.head(head_n))
    else:
        dtypes = []
        nulls = []
        head_repr = repr(df)

    n_cols = len(dtypes) if dtypes else 0
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
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd python/dx && uv run pytest tests/test_summary.py -v
```

Expected: 5 passed (1 skipped if polars not installed).

- [ ] **Step 5: Commit**

```bash
git add python/dx/src/dx/_summary.py python/dx/tests/test_summary.py
git commit -m "feat(dx): text/llm+plain DataFrame summaries"
```

---

## Task 6: `dx._format` — serialization chain (Python-only, formatter hook in Task 8)

**Files:**
- Create: `python/dx/src/dx/_format.py`
- Create: `python/dx/tests/test_format.py`

- [ ] **Step 1: Write the failing tests**

`python/dx/tests/test_format.py`:

```python
import io

import pandas as pd
import pytest

from dx._format import serialize_dataframe


def test_serialize_pandas_to_parquet():
    df = pd.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
    data, content_type = serialize_dataframe(df, max_bytes=10_000_000)
    assert content_type == "application/vnd.apache.parquet"
    assert isinstance(data, bytes)
    # Parquet files start with "PAR1"
    assert data[:4] == b"PAR1"


def test_serialize_pandas_round_trip():
    import pyarrow.parquet as pq

    df = pd.DataFrame({"a": [1, 2, 3]})
    data, _ = serialize_dataframe(df, max_bytes=10_000_000)
    table = pq.read_table(io.BytesIO(data))
    assert table.column("a").to_pylist() == [1, 2, 3]


def test_serialize_downsamples_when_oversized():
    """When the full payload would exceed max_bytes, downsample and annotate."""
    big = pd.DataFrame({"a": list(range(200_000))})
    data, content_type = serialize_dataframe(big, max_bytes=2_000)
    assert content_type == "application/vnd.apache.parquet"
    assert len(data) <= 2_000 + 4_096  # allow parquet footer slack


def test_serialize_polars_when_available():
    polars = pytest.importorskip("polars")
    df = polars.DataFrame({"a": [1, 2, 3]})
    data, content_type = serialize_dataframe(df, max_bytes=10_000_000)
    assert content_type == "application/vnd.apache.parquet"
    assert data[:4] == b"PAR1"
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cd python/dx && uv run pytest tests/test_format.py -v
```

Expected: ImportError.

- [ ] **Step 3: Implement**

`python/dx/src/dx/_format.py`:

```python
"""DataFrame → parquet serialization (best-available encoder) + IPython formatter hooks."""

from __future__ import annotations

import io
from typing import Any, Tuple

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


def serialize_dataframe(df: Any, *, max_bytes: int) -> Tuple[bytes, str]:
    """Serialize `df` to parquet. Downsamples if the full payload exceeds max_bytes.

    Returns (bytes, content_type). content_type is always PARQUET_MIME on success.
    Raises ValueError for unsupported DataFrame types.
    """
    flavor = _detect_flavor(df)
    if flavor == "pandas":
        encoder = _serialize_pandas
    elif flavor == "polars":
        encoder = _serialize_polars
    else:
        raise ValueError(f"unsupported DataFrame type: {type(df).__module__}.{type(df).__name__}")

    # Try the full DataFrame first.
    full = encoder(df)
    if len(full) <= max_bytes:
        return full, PARQUET_MIME

    # Estimate a row count that fits. Binary-search-ish: try halving.
    n = len(df)
    target_rows = max(1, int(n * (max_bytes / len(full))))
    # Try a couple of rounds; parquet compression is row-count-nonlinear.
    for _ in range(4):
        sampled = encoder(df, rows=target_rows)
        if len(sampled) <= max_bytes:
            return sampled, PARQUET_MIME
        target_rows = max(1, target_rows // 2)

    # Last resort: 1-row sample. Never raise for sampling.
    return encoder(df, rows=1), PARQUET_MIME


def summarize_for_df(df: Any, *, total_rows: int, included_rows: int, sampled: bool) -> str:
    """Thin indirection so callers don't depend on dx._summary directly."""
    from dx._summary import summarize_dataframe

    return summarize_dataframe(
        df,
        total_rows=total_rows,
        included_rows=included_rows,
        sampled=sampled,
    )


# Formatter and display functions are stubbed here; wired up in Task 8.
def install_formatters() -> None:
    """Install IPython formatters + open the comm. See Task 8 for full wiring."""
    # Real implementation lands in Task 8.
    from dx._format_install import install_formatters as _impl

    _impl()


def dx_display(obj: Any) -> None:
    from dx._format_install import dx_display as _impl

    _impl(obj)


def publish_ref(ref, *, summary):
    from dx._format_install import publish_ref as _impl

    _impl(ref, summary=summary)
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd python/dx && uv run pytest tests/test_format.py -v
```

Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add python/dx/src/dx/_format.py python/dx/tests/test_format.py
git commit -m "feat(dx): DataFrame → parquet serialization with downsampling"
```

---

## Task 7: `dx._comm` — comm client with multiplexing

**Files:**
- Create: `python/dx/src/dx/_comm.py`
- Create: `python/dx/tests/test_comm.py`

- [ ] **Step 1: Write the failing tests**

`python/dx/tests/test_comm.py`:

```python
import threading

import pytest

from dx import DxNoAgentError, DxTimeoutError, BlobRef
from dx._comm import BlobClient, FallbackClient


class FakeComm:
    """Stand-in for ipykernel.comm.Comm."""

    def __init__(self):
        self.sent = []
        self._handler = None
        self.closed = False

    def on_msg(self, handler):
        self._handler = handler

    def send(self, data, buffers=None):
        self.sent.append((data, list(buffers or [])))

    def close(self):
        self.closed = True

    # test helper: simulate an incoming message from the runtime agent
    def incoming(self, data, buffers=None):
        assert self._handler is not None
        self._handler({"content": {"data": data}, "buffers": buffers or []})


def test_put_blob_sends_comm_msg_with_buffer():
    comm = FakeComm()
    client = BlobClient(comm, default_timeout=5.0)

    # Schedule an ack to fire once send() is called.
    def ack_on_send():
        while not comm.sent:
            pass
        req_id = comm.sent[0][0]["req_id"]
        comm.incoming({
            "op": "ack",
            "req_id": req_id,
            "hash": "sha256:abc",
            "size": 3,
        })

    t = threading.Thread(target=ack_on_send, daemon=True)
    t.start()

    ref = client.put(b"abc", "image/png", blob_base_url="http://localhost:9999")
    t.join(timeout=2)
    assert isinstance(ref, BlobRef)
    assert ref.hash == "sha256:abc"
    assert ref.size == 3
    assert ref.url == "http://localhost:9999/blob/sha256/abc"

    data, buffers = comm.sent[0]
    assert data["op"] == "put"
    assert data["content_type"] == "image/png"
    assert len(buffers) == 1 and buffers[0] == b"abc"


def test_put_blob_timeout_raises():
    comm = FakeComm()
    client = BlobClient(comm, default_timeout=0.05)
    with pytest.raises(DxTimeoutError):
        client.put(b"abc", "image/png", blob_base_url="http://localhost:9999")


def test_put_blob_agent_error_response():
    comm = FakeComm()
    client = BlobClient(comm, default_timeout=2.0)

    def err_on_send():
        while not comm.sent:
            pass
        req_id = comm.sent[0][0]["req_id"]
        comm.incoming({
            "op": "err",
            "req_id": req_id,
            "code": "too_large",
            "message": "exceeds MAX_BLOB_SIZE",
        })

    threading.Thread(target=err_on_send, daemon=True).start()

    from dx import DxPayloadTooLargeError

    with pytest.raises(DxPayloadTooLargeError):
        client.put(b"abc", "image/png", blob_base_url="http://localhost:9999")


def test_fallback_client_raises_no_agent():
    client = FallbackClient()
    with pytest.raises(DxNoAgentError):
        client.put(b"abc", "image/png", blob_base_url="http://localhost:9999")
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cd python/dx && uv run pytest tests/test_comm.py -v
```

Expected: ImportError.

- [ ] **Step 3: Implement**

`python/dx/src/dx/_comm.py`:

```python
"""Comm client for the nteract.dx.blob target.

The Comm itself (ipykernel.comm.Comm) is opened elsewhere and injected; this
module handles request/response multiplexing by req_id and timeout behavior.
"""

from __future__ import annotations

import threading
import uuid
from dataclasses import dataclass
from typing import Optional

from dx._refs import BlobRef


COMM_TARGET = "nteract.dx.blob"


@dataclass
class _Pending:
    event: threading.Event
    response: Optional[dict] = None


class BlobClient:
    """Sends `op: put` comm_msgs, awaits `op: ack` responses by req_id."""

    def __init__(self, comm, default_timeout: float = 30.0):
        self._comm = comm
        self._default_timeout = default_timeout
        self._pending: dict[str, _Pending] = {}
        self._lock = threading.Lock()
        comm.on_msg(self._handle_msg)

    def put(
        self,
        data: bytes,
        content_type: str,
        *,
        blob_base_url: str,
        timeout: Optional[float] = None,
    ) -> BlobRef:
        req_id = str(uuid.uuid4())
        pending = _Pending(event=threading.Event())
        with self._lock:
            self._pending[req_id] = pending

        self._comm.send(
            {"op": "put", "req_id": req_id, "content_type": content_type},
            buffers=[data],
        )

        wait_s = timeout if timeout is not None else self._default_timeout
        if not pending.event.wait(wait_s):
            with self._lock:
                self._pending.pop(req_id, None)
            from dx import DxTimeoutError

            raise DxTimeoutError(f"no ack for req_id {req_id} within {wait_s}s")

        with self._lock:
            response = self._pending.pop(req_id).response
        assert response is not None

        op = response.get("op")
        if op == "ack":
            return BlobRef(
                hash=response["hash"],
                url=f"{blob_base_url.rstrip('/')}/blob/{response['hash']}",
                size=int(response["size"]),
            )
        if op == "err":
            code = response.get("code", "unknown")
            message = response.get("message", "")
            from dx import DxError, DxPayloadTooLargeError

            if code == "too_large":
                raise DxPayloadTooLargeError(message)
            raise DxError(f"agent error ({code}): {message}")

        from dx import DxError

        raise DxError(f"unexpected response op: {op}")

    def _handle_msg(self, msg):
        data = msg.get("content", {}).get("data", {})
        req_id = data.get("req_id")
        if req_id is None:
            return
        with self._lock:
            pending = self._pending.get(req_id)
            if pending is None:
                return
            pending.response = data
            pending.event.set()


class FallbackClient:
    """Used when no agent is reachable.

    Every `put` raises DxNoAgentError so callers can decide whether to fall
    back to raw-bytes display or re-raise.
    """

    def put(self, data: bytes, content_type: str, *, blob_base_url: str, timeout=None):
        from dx import DxNoAgentError

        raise DxNoAgentError("nteract.dx.blob comm is not open")


# Module-level singleton holder set by install() in Task 8.
_client: Optional[object] = None
_blob_base_url: str = "http://127.0.0.1"


def set_client(client, *, blob_base_url: str) -> None:
    global _client, _blob_base_url
    _client = client
    _blob_base_url = blob_base_url


def get_client():
    global _client
    if _client is None:
        _client = FallbackClient()
    return _client


def put_blob(data: bytes, content_type: str) -> BlobRef:
    return get_client().put(data, content_type, blob_base_url=_blob_base_url)
```

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd python/dx && uv run pytest tests/test_comm.py -v
```

Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
git add python/dx/src/dx/_comm.py python/dx/tests/test_comm.py
git commit -m "feat(dx): comm client with req_id multiplexing"
```

---

## Task 8: `dx` install + formatters + display plumbing

**Files:**
- Create: `python/dx/src/dx/_format_install.py`
- Create: `python/dx/tests/test_install.py`

- [ ] **Step 1: Write the failing tests**

`python/dx/tests/test_install.py`:

```python
import pandas as pd
import pytest

from dx._refs import BlobRef, BLOB_REF_MIME


class FakeIPython:
    def __init__(self):
        self.formatters = {}

    def display_formatter(self):
        return self

    @property
    def mimebundle_formatter(self):
        # pretend to be IPython's MimeBundle formatter with `.for_type`
        class _Fmt:
            def __init__(self, outer):
                self.outer = outer

            def for_type(self, cls, func):
                self.outer.formatters[cls] = func

        return _Fmt(self)


def test_install_registers_pandas_formatter(monkeypatch):
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._try_open_comm", lambda: None)  # no agent

    import dx

    dx.install()
    assert pd.DataFrame in ip.formatters


def test_install_is_idempotent(monkeypatch):
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._try_open_comm", lambda: None)

    import dx

    dx.install()
    dx.install()
    assert len(ip.formatters) == 1  # only one registration


def test_publish_ref_emits_blob_ref_mime(monkeypatch):
    published = []

    def fake_publish(data, **kwargs):
        published.append(data)

    monkeypatch.setattr(
        "dx._format_install._publish_display_data",
        fake_publish,
    )

    import dx

    ref = BlobRef(hash="sha256:abc", url="http://x/blob/sha256/abc", size=42)
    dx.display_blob_ref(ref, summary={"total_rows": 100})

    assert len(published) == 1
    bundle = published[0]
    assert BLOB_REF_MIME in bundle
    body = bundle[BLOB_REF_MIME]
    assert body["hash"] == "sha256:abc"
    assert body["summary"] == {"total_rows": 100}
```

- [ ] **Step 2: Run tests to verify failure**

```bash
cd python/dx && uv run pytest tests/test_install.py -v
```

Expected: ImportError on `dx._format_install`.

- [ ] **Step 3: Implement**

`python/dx/src/dx/_format_install.py`:

```python
"""Install-time wiring: comm open, IPython formatter registration, display emit."""

from __future__ import annotations

import logging
import os
from typing import Any, Optional

from dx._env import detect_environment, Environment
from dx._refs import BLOB_REF_MIME, BlobRef, build_ref_bundle
from dx._format import PARQUET_MIME, serialize_dataframe, summarize_for_df
from dx._comm import COMM_TARGET, BlobClient, FallbackClient, set_client

log = logging.getLogger("dx")

_INSTALLED = False
_MAX_PAYLOAD_BYTES = int(os.environ.get("DX_MAX_PAYLOAD_BYTES", str(90 * 1024 * 1024)))
_COMM_OPEN_TIMEOUT_S = 0.1


def _get_ipython_for_format():  # extracted for test monkeypatching
    try:
        from IPython import get_ipython as _gi  # type: ignore
    except ImportError:
        return None
    return _gi()


def _publish_display_data(data: dict, metadata: Optional[dict] = None) -> None:
    """Indirection over IPython.display.publish_display_data for test patching."""
    from IPython.display import publish_display_data  # type: ignore

    publish_display_data(data, metadata or {})


def _try_open_comm():
    """Open the nteract.dx.blob comm, if we're in an ipykernel. Return a BlobClient or None."""
    if detect_environment() != Environment.IPYKERNEL:
        return None
    try:
        from ipykernel.comm import Comm  # type: ignore
    except ImportError:
        return None
    try:
        comm = Comm(target_name=COMM_TARGET, data={})
    except Exception as exc:  # pragma: no cover — defensive
        log.debug("dx: failed to open %s comm: %s", COMM_TARGET, exc)
        return None
    return BlobClient(comm)


def _resolve_blob_base_url() -> str:
    # The agent supplies this via RUNTIMED_BLOB_BASE_URL at kernel spawn.
    return os.environ.get("RUNTIMED_BLOB_BASE_URL", "http://127.0.0.1")


def install_formatters() -> None:
    global _INSTALLED
    if _INSTALLED:
        return

    client = _try_open_comm()
    if client is None:
        client = FallbackClient()
        log.debug("dx: no agent comm — using fallback (raw-bytes display).")
    set_client(client, blob_base_url=_resolve_blob_base_url())

    ip = _get_ipython_for_format()
    if ip is None:
        _INSTALLED = True
        return

    # Register IPython formatters for pandas and polars DataFrames.
    try:
        import pandas as pd  # type: ignore

        ip.display_formatter().mimebundle_formatter.for_type(
            pd.DataFrame, _pandas_formatter
        )
    except ImportError:
        pass

    try:
        import polars as pl  # type: ignore

        ip.display_formatter().mimebundle_formatter.for_type(
            pl.DataFrame, _polars_formatter
        )
    except ImportError:
        pass

    _INSTALLED = True


def _pandas_formatter(df: Any) -> dict:
    return _df_to_bundle(df, total_rows=len(df))


def _polars_formatter(df: Any) -> dict:
    return _df_to_bundle(df, total_rows=df.height)


def _df_to_bundle(df: Any, *, total_rows: int) -> dict:
    """Serialize df, upload, build the display bundle. Fallback path on failure."""
    from dx._comm import get_client

    try:
        data, content_type = serialize_dataframe(df, max_bytes=_MAX_PAYLOAD_BYTES)
    except Exception as exc:
        log.debug("dx: serialize failed: %s — falling back to repr", exc)
        return {"text/plain": repr(df)}

    sampled = False
    included = total_rows
    # If serialize_dataframe downsampled, we can't always tell from bytes alone;
    # compare the parquet row count (read back). Cheap: parquet metadata only.
    try:
        import pyarrow.parquet as pq
        import io

        meta = pq.read_metadata(io.BytesIO(data))
        if meta.num_rows != total_rows:
            sampled = True
            included = meta.num_rows
    except Exception:
        pass

    client = get_client()
    try:
        ref = client.put(data, content_type)
    except Exception as exc:
        log.debug("dx: upload failed: %s — falling back to raw-bytes display", exc)
        # Fallback: emit the raw-bytes bundle so the existing agent path handles it.
        llm = summarize_for_df(df, total_rows=total_rows, included_rows=included, sampled=sampled)
        return {content_type: data, "text/llm+plain": llm}

    summary = {
        "total_rows": total_rows,
        "included_rows": included,
        "sampled": sampled,
        "sample_strategy": "head" if sampled else "none",
    }
    bundle = build_ref_bundle(ref, content_type=content_type, summary=summary)
    llm = summarize_for_df(df, total_rows=total_rows, included_rows=included, sampled=sampled)
    return {BLOB_REF_MIME: bundle, "text/llm+plain": llm}


def dx_display(obj: Any) -> None:
    """Upgraded display; hands off to IPython for non-DataFrame types."""
    from IPython.display import display  # type: ignore

    display(obj)


def publish_ref(ref: BlobRef, *, summary: Optional[dict] = None) -> None:
    bundle = build_ref_bundle(
        ref,
        content_type=_guess_content_type_from_hash(ref),
        summary=summary,
    )
    # `publish_ref` is for callers who already have a BlobRef and know the content type.
    # To support that cleanly, accept content_type directly — see API adjustment below.
    # (Forwarding kept minimal here; callers should use publish_ref_with_type.)
    _publish_display_data({BLOB_REF_MIME: bundle})


def publish_ref_with_type(
    ref: BlobRef,
    *,
    content_type: str,
    summary: Optional[dict] = None,
) -> None:
    bundle = build_ref_bundle(ref, content_type=content_type, summary=summary)
    _publish_display_data({BLOB_REF_MIME: bundle})


def _guess_content_type_from_hash(_ref: BlobRef) -> str:
    # Placeholder for a future metadata lookup. v1 requires callers to use
    # publish_ref_with_type when they know the content type.
    return "application/octet-stream"
```

Update `python/dx/src/dx/__init__.py` to expose `publish_ref_with_type` too:

```python
def display_blob_ref(ref: BlobRef, *, content_type: str | None = None, summary: dict | None = None) -> None:
    """Emit a display_data bundle for an existing blob ref."""
    from dx._format_install import publish_ref, publish_ref_with_type

    if content_type is not None:
        publish_ref_with_type(ref, content_type=content_type, summary=summary)
    else:
        publish_ref(ref, summary=summary)
```

Update the failing test accordingly — tests call `dx.display_blob_ref(ref, summary={...})` without a content_type, which uses the placeholder. Update `test_publish_ref_emits_blob_ref_mime` to pass `content_type="image/png"` and assert `body["content_type"] == "image/png"`.

- [ ] **Step 4: Run tests to verify they pass**

```bash
cd python/dx && uv run pytest tests/test_install.py -v
```

Expected: 3 passed.

- [ ] **Step 5: Run the full dx test suite**

```bash
cd python/dx && uv run pytest -v
```

Expected: all previous tests still green.

- [ ] **Step 6: Commit**

```bash
git add python/dx
git commit -m "feat(dx): install(), IPython formatters, display_blob_ref"
```

---

## Task 9: Runtime agent — `nteract.dx.*` namespace filter + blob comm handler

**Files:**
- Modify: `crates/runtimed/src/jupyter_kernel.rs` (~lines 1240–1410 for comm_open/comm_msg)
- Create: `crates/runtimed/src/dx_blob_comm.rs` — handler module
- Modify: `crates/runtimed/src/lib.rs` — `mod dx_blob_comm;`

Context: the comm_open handler at `crates/runtimed/src/jupyter_kernel.rs:1244` currently runs `store_widget_buffers`, `blob_store_large_state_values`, then `put_comm` into `RuntimeStateDoc`. We need a **prefix check at the very top** so dx comms short-circuit before any widget-buffer processing. The `CommMsg` arm at ~1335 needs the same filter, looked up via a new local `comm_id → target_name` map.

### Outbound reply path (concrete)

The kernel's shell socket is owned by `JupyterKernel::send_comm_message` (`jupyter_kernel.rs:1959`), which takes a raw JSON message and writes on `shell: DealerSendConnection`. That method holds `&mut self`, so the IOPub task (spawned elsewhere) can't call it directly.

**Minimal addition:** introduce a new `tokio::sync::mpsc::UnboundedSender<serde_json::Value>` — name it `shell_comm_tx` — that the IOPub task holds a clone of. The kernel's main `select!` loop drains the receiver and forwards each message to `send_comm_message`. This reuses the existing serialization path (`send_comm_message` already knows how to reconstruct a `JupyterMessage` from raw JSON) and does not touch `SendComm` or the daemon ↔ agent protocol.

The existing `comm_coalesce_tx` (line 525) is **not** a fit — it writes to `RuntimeStateDoc` via `merge_comm_state_delta`, which is the exact CRDT path dx must avoid.

### comm_id → target_name map (concrete)

`RuntimeStateDoc` already tracks target_name in `CommDocEntry` (`notebook-doc/src/runtime_state.rs:192`), but reading from the CRDT inside the hot IOPub path would be heavy. Add a local `HashMap<String, String>` owned by the IOPub task (same scope as `capture_cache`), populated on `comm_open` before the short-circuit and cleared on `comm_close`.

- [ ] **Step 1: Write the failing test**

Create `crates/runtimed/tests/dx_comm_filter.rs`:

```rust
//! Asserts that comm traffic on `nteract.dx.*` target names does not land in RuntimeStateDoc.

use notebook_doc::runtime_state::RuntimeStateDoc;
use runtimed::dx_blob_comm::is_dx_target;

#[test]
fn dx_blob_target_is_filtered() {
    assert!(is_dx_target("nteract.dx.blob"));
    assert!(is_dx_target("nteract.dx.query"));
    assert!(is_dx_target("nteract.dx.stream"));
    assert!(!is_dx_target("nteract.dx"));
    assert!(!is_dx_target("something.else"));
    assert!(!is_dx_target("jupyter.widget"));
}

#[tokio::test]
async fn dx_comm_open_does_not_write_comm_doc_entry() {
    // This is an integration-style test. We construct a minimal state doc,
    // invoke the comm-open path for target="nteract.dx.blob", and assert
    // that state_doc.comms remains empty.
    //
    // (Full wiring is in the runtime agent's IOPub handler; here we test the filter
    // function directly and rely on Task 9 Step 3 to enforce it at the call
    // site.)
    let mut doc = RuntimeStateDoc::new();
    // Simulate the check the handler performs:
    let target = "nteract.dx.blob";
    if !is_dx_target(target) {
        doc.put_comm("fake-id", target, "", "", &serde_json::json!({}), 0);
    }
    assert!(doc.comms().is_empty());
}
```

- [ ] **Step 2: Run to verify failure**

```bash
cargo test -p runtimed --test dx_comm_filter
```

Expected: compile error — `dx_blob_comm::is_dx_target` not found.

- [ ] **Step 3: Implement `dx_blob_comm.rs`**

`crates/runtimed/src/dx_blob_comm.rs`:

```rust
//! Handler for the `nteract.dx.blob` comm target.
//!
//! The comm carries byte uploads from the Python kernel's `dx` library.
//! Buffers go straight to the `BlobStore`; nothing from this comm target
//! (open, msg, or close) is written into `RuntimeStateDoc::comms` — that
//! persistence path is reserved for ipywidgets/anywidget state.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::blob_store::{BlobStore, MAX_BLOB_SIZE};

/// The reserved comm-target namespace prefix.
pub const DX_NAMESPACE_PREFIX: &str = "nteract.dx.";

pub const DX_BLOB_TARGET: &str = "nteract.dx.blob";

/// Returns true if the comm target is part of the reserved dx namespace.
///
/// Targets in this namespace are NOT persisted to RuntimeStateDoc.
pub fn is_dx_target(target_name: &str) -> bool {
    // Require at least one character after the prefix (i.e., reject
    // "nteract.dx" and "nteract.dx.").
    target_name.starts_with(DX_NAMESPACE_PREFIX)
        && target_name.len() > DX_NAMESPACE_PREFIX.len()
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op")]
#[serde(rename_all = "snake_case")]
pub enum DxBlobRequest {
    Put {
        req_id: String,
        content_type: String,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "op")]
#[serde(rename_all = "snake_case")]
pub enum DxBlobResponse {
    Ack {
        req_id: String,
        hash: String,
        size: u64,
    },
    Err {
        req_id: String,
        code: String,
        message: String,
    },
}

/// Handle a single dx.blob comm_msg. Returns the response to send back on
/// the kernel's shell socket as a comm_msg for the same comm_id.
pub async fn handle_blob_msg(
    blob_store: &Arc<BlobStore>,
    request: DxBlobRequest,
    buffer: &[u8],
) -> DxBlobResponse {
    match request {
        DxBlobRequest::Put { req_id, content_type } => {
            if buffer.len() > MAX_BLOB_SIZE {
                return DxBlobResponse::Err {
                    req_id,
                    code: "too_large".to_string(),
                    message: format!(
                        "payload {} bytes exceeds MAX_BLOB_SIZE {}",
                        buffer.len(),
                        MAX_BLOB_SIZE
                    ),
                };
            }
            match blob_store.put(buffer, &content_type).await {
                Ok(hash) => DxBlobResponse::Ack {
                    req_id,
                    hash,
                    size: buffer.len() as u64,
                },
                Err(err) => {
                    warn!("[dx] blob store put failed: {}", err);
                    DxBlobResponse::Err {
                        req_id,
                        code: "blob_store_error".to_string(),
                        message: err.to_string(),
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_deserialize() {
        let req: DxBlobRequest = serde_json::from_value(serde_json::json!({
            "op": "put",
            "req_id": "r1",
            "content_type": "image/png",
        }))
        .unwrap();
        match req {
            DxBlobRequest::Put { req_id, content_type } => {
                assert_eq!(req_id, "r1");
                assert_eq!(content_type, "image/png");
            }
        }
    }

    #[test]
    fn response_serialize_ack() {
        let resp = DxBlobResponse::Ack {
            req_id: "r1".into(),
            hash: "sha256:abc".into(),
            size: 3,
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(v["op"], "ack");
        assert_eq!(v["hash"], "sha256:abc");
    }
}
```

- [ ] **Step 4: Register module**

Add to `crates/runtimed/src/lib.rs` (next to existing `pub mod ...;` declarations):

```rust
pub mod dx_blob_comm;
```

- [ ] **Step 5: Add the shell outbound mpsc + kernel main-loop drainer**

In `crates/runtimed/src/jupyter_kernel.rs`, near where `comm_coalesce_tx` is created (line 525):

```rust
let (shell_comm_tx, mut shell_comm_rx) =
    tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();
```

Clone `shell_comm_tx` into the IOPub task's moved captures (alongside `comm_coalesce_tx`).

In the kernel's main `select!` loop — the same one that dispatches `RuntimeAgentRequest::SendComm` to `self.send_comm_message(...)` — add a branch:

```rust
Some(msg) = shell_comm_rx.recv() => {
    if let Err(e) = self.send_comm_message(msg).await {
        warn!("[dx] shell comm_msg send failed: {e}");
    }
}
```

This reuses `send_comm_message` (line 1959) — which already knows how to build a `JupyterMessage` from raw JSON and write it on the shell socket — without adding a second serialization path.

- [ ] **Step 6: Add the local comm_id → target_name map in the IOPub task**

Near `capture_cache` in the IOPub task scope, add:

```rust
let mut comm_targets: std::collections::HashMap<String, String> =
    std::collections::HashMap::new();
```

Populate on `comm_open` (before any other processing):

```rust
comm_targets.insert(open.comm_id.0.clone(), open.target_name.clone());
```

Remove on `comm_close`:

```rust
comm_targets.remove(&close.comm_id.0);
```

- [ ] **Step 7: Wire the filter into `CommOpen`**

At the very top of the `JupyterMessageContent::CommOpen(open)` arm (~line 1230) — before `store_widget_buffers`, before any CRDT writes:

```rust
use crate::dx_blob_comm::is_dx_target;
comm_targets.insert(open.comm_id.0.clone(), open.target_name.clone());
if is_dx_target(&open.target_name) {
    debug!("[dx] comm_open {} target={} (not persisted)", open.comm_id.0, open.target_name);
    continue;
}
```

- [ ] **Step 8: Wire the filter into `CommMsg`**

In `JupyterMessageContent::CommMsg(msg)` (~line 1335), after the existing `msg` binding but before the widget-update coalesce logic:

```rust
if let Some(target) = comm_targets.get(&msg.comm_id.0) {
    if is_dx_target(target) {
        let request_value = serde_json::to_value(&msg.data).unwrap_or_default();
        let request: crate::dx_blob_comm::DxBlobRequest =
            match serde_json::from_value(request_value) {
                Ok(r) => r,
                Err(e) => {
                    warn!("[dx] bad request on {}: {}", msg.comm_id.0, e);
                    continue;
                }
            };
        let buffer = buffers.first().cloned().unwrap_or_default();
        let response = crate::dx_blob_comm::handle_blob_msg(
            &blob_store, request, &buffer,
        ).await;
        let response_data = serde_json::to_value(&response).unwrap_or_default();
        // Build a raw Jupyter comm_msg envelope for send_comm_message.
        let envelope = serde_json::json!({
            "header": {"msg_type": "comm_msg"},
            "content": {"comm_id": msg.comm_id.0, "data": response_data},
            "buffers": [],
        });
        let _ = shell_comm_tx.send(envelope);
        continue;
    }
}
```

Confirm the envelope shape matches what `send_comm_message` expects at line 1959 (header/parent_header/metadata/content/buffers). Adapt field names if the actual parser is stricter.

- [ ] **Step 9: Wire `CommClose` cleanup**

In `JupyterMessageContent::CommClose(close)` (~line 1409), at the top:

```rust
if let Some(target) = comm_targets.remove(&close.comm_id.0) {
    if is_dx_target(&target) {
        debug!("[dx] comm_close {} target={}", close.comm_id.0, target);
        continue;
    }
}
```

- [ ] **Step 10: Run filter tests**

```bash
cargo test -p runtimed --test dx_comm_filter
cargo test -p runtimed dx_blob_comm
```

Expected: all green.

- [ ] **Step 11: Lint + full build**

```bash
cargo xtask lint --fix
cargo build -p runtimed
```

Expected: clean.

- [ ] **Step 12: Commit**

```bash
git add crates/runtimed/src/dx_blob_comm.rs crates/runtimed/src/lib.rs \
        crates/runtimed/src/jupyter_kernel.rs crates/runtimed/tests/dx_comm_filter.rs
git commit -m "feat(runtimed): nteract.dx.blob comm handler + namespace filter"
```

---

## Task 10: Runtime agent — recognize ref MIME in `create_manifest`

**Files:**
- Modify: `crates/runtimed/src/output_store.rs`

Goal: when a display_data bundle contains `application/vnd.nteract.blob-ref+json`, compose a `ContentRef::Blob` for the wrapped `content_type` directly, skipping `BlobStore::put` (the blob is already stored by the dx blob-comm handler).

### Current shape (reference)

- `crates/runtimed/src/output_store.rs:276` — `pub async fn create_manifest(output: &Value, blob_store: &BlobStore, threshold: usize) -> io::Result<OutputManifest>`. Takes an immutable `&Value` for the outer output envelope; walks its `data` bundle entries internally and builds MIME → `ContentRef` entries in the returned `OutputManifest` variant (DisplayData / ExecuteResult / etc.).
- `crates/runtimed/src/jupyter_kernel.rs:131` — `ContentRef::from_binary(data: &[u8], media_type: &str, blob_store: &BlobStore) -> io::Result<Self>` hashes + writes + returns `ContentRef::Blob { blob: hash, size }`.
- `crates/runtimed/src/blob_store.rs:194` — `BlobStore::exists(&self, hash: &str) -> bool` (sync).

For dx we already have the hash, so we want a sibling constructor — no bytes, no write, just the variant:

```rust
// In whichever module ContentRef lives (same file as from_binary):
impl ContentRef {
    pub fn from_hash(hash: String, size: u64) -> Self {
        ContentRef::Blob { blob: hash, size }
    }
}
```

This is a thin wrapper over the existing enum variant. No refactor of `from_binary` needed.

- [ ] **Step 2: Write the failing test**

Append to `crates/runtimed/src/output_store.rs` (or its existing tests file):

```rust
#[cfg(test)]
mod dx_ref_tests {
    use super::*;
    use notebook_doc::mime::BLOB_REF_MIME;
    use serde_json::json;
    use tempfile::tempdir;

    #[tokio::test]
    async fn ref_mime_composes_content_ref_without_blob_put() {
        let dir = tempdir().unwrap();
        let blob_store = std::sync::Arc::new(BlobStore::new(dir.path().to_path_buf()));

        // Pre-populate the blob store so the hash is resolvable.
        let raw = b"PAR1-fake-parquet-body";
        let hash = blob_store.put(raw, "application/vnd.apache.parquet").await.unwrap();

        let bundle = json!({
            BLOB_REF_MIME: {
                "hash": hash,
                "content_type": "application/vnd.apache.parquet",
                "size": raw.len(),
                "query": null,
            },
            "text/llm+plain": "DataFrame (pandas): 3 rows × 2 columns",
        });

        let manifest = create_manifest(&bundle, &blob_store).await.unwrap();

        // Assert the manifest contains a parquet ContentRef keyed by the hash
        // and NOT a blob-ref MIME entry.
        assert!(manifest.contains_key("application/vnd.apache.parquet"));
        assert!(manifest.contains_key("text/llm+plain"));
        assert!(!manifest.contains_key(BLOB_REF_MIME));
        // and the ContentRef hash matches
        let ct_ref = &manifest["application/vnd.apache.parquet"];
        let hash_in_ref = ct_ref.get("hash").and_then(|v| v.as_str()).unwrap();
        assert_eq!(hash_in_ref, hash);
    }

    #[tokio::test]
    async fn ref_mime_with_missing_hash_logs_and_skips() {
        let dir = tempdir().unwrap();
        let blob_store = std::sync::Arc::new(BlobStore::new(dir.path().to_path_buf()));

        let bundle = json!({
            BLOB_REF_MIME: {"hash": "sha256:does-not-exist", "content_type": "image/png", "size": 0},
        });
        // Should not panic. Manifest may be empty or include a debug note.
        let _ = create_manifest(&bundle, &blob_store).await.unwrap();
    }
}
```

The test calls `create_manifest(&output_envelope, &blob_store, threshold)` where `output_envelope` wraps the bundle in a `data` field matching what IOPub delivers. Adapt the exact shape to `OutputManifest`'s variant (DisplayData vs. ExecuteResult). Assertions target the composed `ContentRef` for the wrapped content_type and the absence of any `BLOB_REF_MIME` entry in the result.

- [ ] **Step 3: Run test to verify failure**

```bash
cargo test -p runtimed dx_ref_tests
```

Expected: logic error — the bundle with BLOB_REF_MIME is not recognized; result contains the ref MIME verbatim rather than a composed ContentRef.

- [ ] **Step 4: Implement**

Inside `create_manifest`, during (or immediately after) the bundle walk, detect `BLOB_REF_MIME` entries and substitute them:

```rust
use notebook_doc::mime::BLOB_REF_MIME;

// When a ref MIME is encountered, compose a ContentRef for the wrapped
// content_type without re-uploading. The underlying blob was stored by the
// dx blob-comm handler (crates/runtimed/src/dx_blob_comm.rs).
if mime == BLOB_REF_MIME {
    let hash = value.get("hash").and_then(|v| v.as_str());
    let target_ct = value.get("content_type").and_then(|v| v.as_str());
    let size = value.get("size").and_then(|v| v.as_u64()).unwrap_or(0);

    match (hash, target_ct) {
        (Some(h), Some(ct)) => {
            if blob_store.exists(h) {
                let content_ref = ContentRef::from_hash(h.to_string(), size);
                entries.insert(ct.to_string(), content_ref);
            } else {
                warn!("[dx] ref MIME points at missing blob {}", h);
            }
        }
        _ => {
            warn!("[dx] ref MIME missing hash or content_type");
        }
    }
    // Do not emit an entry under BLOB_REF_MIME — the target_ct entry is what
    // consumers read. The ref MIME is only a transport detail.
    continue;
}
```

`BlobStore::exists(&str)` is already present at `crates/runtimed/src/blob_store.rs:194` (synchronous file check). `ContentRef::from_hash(hash, size)` is the trivial wrapper shown earlier — add it next to `from_binary`.

- [ ] **Step 5: Run tests to verify they pass**

```bash
cargo test -p runtimed dx_ref_tests
cargo test -p runtimed output_store
```

Expected: all green.

- [ ] **Step 6: Lint**

```bash
cargo xtask lint --fix
```

- [ ] **Step 7: Commit**

```bash
git add crates/runtimed/src/output_store.rs crates/runtimed/src/blob_store.rs
git commit -m "feat(runtimed): compose ContentRef from blob-ref MIME in display bundles"
```

---

## Task 11: Python integration test against the dev daemon

**Files:**
- Create: `python/runtimed/tests/test_dx_integration.py`

- [ ] **Step 1: Write the integration test**

`python/runtimed/tests/test_dx_integration.py`:

```python
"""End-to-end: dx.display(df) in a kernel → ContentRef in CRDT → blob present.

Requires a running dev daemon. See the runtimed test harness in conftest.py
for the socket discovery pattern.
"""

import json
import os

import pandas as pd
import pytest

runtimed = pytest.importorskip("runtimed")


@pytest.mark.integration
async def test_dx_display_writes_content_ref_not_raw_bytes(daemon_client, tmp_notebook):
    """dx.display(df) should emit a blob-ref MIME; the runtime agent should compose a ContentRef."""
    notebook = await daemon_client.open_notebook(tmp_notebook)
    await notebook.add_dependency("pandas")
    await notebook.add_dependency("pyarrow")

    # Install dx from the workspace.
    cell_install = await notebook.create_cell(
        cell_type="code",
        source="import subprocess; subprocess.check_call(['pip', 'install', '-e', '/path/to/python/dx'])",
    )
    await cell_install.run()

    cell = await notebook.create_cell(
        cell_type="code",
        source=(
            "import dx\n"
            "dx.install()\n"
            "import pandas as pd\n"
            "df = pd.DataFrame({'a': [1, 2, 3], 'b': ['x', 'y', 'z']})\n"
            "dx.display(df)\n"
        ),
    )
    await cell.run()

    outputs = cell.outputs
    assert len(outputs) >= 1
    display_out = next(o for o in outputs if o.output_type == "display_data")

    # The manifest must NOT contain the ref MIME literally — it's resolved.
    assert "application/vnd.nteract.blob-ref+json" not in display_out.data
    # It MUST contain a parquet ContentRef.
    parquet = display_out.data.get("application/vnd.apache.parquet")
    assert parquet is not None
    assert parquet.get("hash", "").startswith("sha256:")
    # And a text/llm+plain sibling.
    assert "text/llm+plain" in display_out.data
    llm = display_out.data["text/llm+plain"]
    assert "3 rows" in llm
    assert "2 columns" in llm
```

Adapt fixture names (`daemon_client`, `tmp_notebook`) to match the actual runtimed test harness in `python/runtimed/tests/conftest.py`.

- [ ] **Step 2: Run the test**

```bash
# Start the dev daemon in another terminal (or via supervisor_restart target=daemon)
RUNTIMED_SOCKET_PATH="$(RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH=$(pwd) ./target/debug/runt daemon status --json | python3 -c 'import sys,json; print(json.load(sys.stdin)["socket_path"])')" \
  python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_dx_integration.py -v
```

Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add python/runtimed/tests/test_dx_integration.py
git commit -m "test(dx): end-to-end integration against dev daemon"
```

---

## Task 12: Publishing `dx` and deferred kernel bootstrap

Kernel bootstrap auto-installation of `dx` is **deferred**. Rationale:

- Today, the kernel-launch path does not inject any ipykernel startup lines — adding that plumbing is net-new surface area that doesn't need to ship with v1.
- Before auto-bootstrap, `dx` needs to exist somewhere a managed Python environment can install it from. The workspace path works locally but is fragile across machines; publishing `dx` to PyPI is a prerequisite.

### v1 UX

Users (or us, in notebooks) call:

```python
import dx
dx.install()
```

as the first cell in a notebook. `dx.install()` opens the `nteract.dx.blob` comm and registers the IPython formatters; subsequent cells get the upgraded display automatically. In vanilla Jupyter the same two lines are a safe no-op (raw-bytes display fallback).

### Publishing prep checklist (land before v1.1 auto-bootstrap)

- [ ] Claim the PyPI project name `dx` (or, if unavailable, rename to `nteract-dx` and update all references).
- [ ] Add `dx` to the release workflow that publishes the nteract Python packages.
- [ ] Cut a 0.1.0 release once the agent-side work (Tasks 1, 9, 10) has landed and is tested.

### v1.1 auto-bootstrap (future task, out of scope for this plan)

When we circle back, the design is:

- Add `dx` to the managed Python environment's dep list (wherever `ipykernel`, `uv`, etc. are pinned during environment setup).
- Inject `import dx; dx.install()` via `--IPKernelApp.exec_lines` or a startup `.py` in the managed profile directory.
- No env var plumbing needed — the kernel discovers the comm target via ipykernel; the runtime agent is already on the other end. We ruled out `RUNTIMED_BLOB_BASE_URL`: kernel-side code never needs the URL, and the ref MIME carries only the hash.

---

## Task 13: End-to-end smoke in the notebook app

**Files:**
- Create: a fixture notebook under `apps/notebook/tests/fixtures/dx_smoke.ipynb` (or wherever E2E fixtures live; check `contributing/e2e.md`)
- Create: WebdriverIO spec `apps/notebook/tests/e2e/dx.spec.ts`

- [ ] **Step 1: Read the existing E2E conventions**

```bash
sed -n '1,80p' contributing/e2e.md
ls apps/notebook/tests/e2e/
```

- [ ] **Step 2: Fixture notebook**

Create a notebook (via `create_notebook` MCP tool or JSON file) containing one code cell:

```python
import dx
dx.install()
import pandas as pd
df = pd.DataFrame({'a': list(range(1000)), 'b': list(range(1000))})
df
```

- [ ] **Step 3: WebdriverIO spec**

`apps/notebook/tests/e2e/dx.spec.ts`:

```ts
import { $, expect, browser } from "@wdio/globals";

describe("dx end-to-end", () => {
  it("displays a pandas DataFrame via blob-ref path", async () => {
    await browser.url("/fixtures/dx_smoke.ipynb");
    const runAll = await $('[data-testid="run-all"]');
    await runAll.click();

    // Wait for the sift/parquet renderer to materialize.
    const table = await $('[data-testid="sift-parquet-table"]');
    await table.waitForExist({ timeout: 15_000 });

    // The cell's underlying output should reference a ContentRef hash, not
    // carry raw bytes. Probe via a test-only hook or via CRDT inspection if
    // available.
    const manifest = await browser.execute(() => {
      return (window as any).__nteract_debug?.lastOutputManifest?.();
    });
    expect(manifest).toBeDefined();
    expect(manifest["application/vnd.apache.parquet"]).toBeDefined();
    expect(manifest["application/vnd.apache.parquet"].hash).toMatch(/^sha256:/);
    expect(manifest["application/vnd.nteract.blob-ref+json"]).toBeUndefined();
  });
});
```

Adapt the `data-testid`s to whatever the sift renderer and cell output actually expose. If no `__nteract_debug` hook exists, add a thin one gated on `import.meta.env.DEV` in `apps/notebook/src/` that returns the last rendered cell's manifest.

- [ ] **Step 4: Run**

```bash
cargo xtask e2e test -- dx.spec.ts
```

Expected: pass.

- [ ] **Step 5: Commit**

```bash
git add apps/notebook
git commit -m "test(dx): e2e smoke via WebdriverIO"
```

---

## Task 14: Docs + cleanup

**Files:**
- Modify: `docs/python-bindings.md` or `contributing/python-bindings.md`

- [ ] **Step 1: Add a brief "Using dx" section**

```markdown
## Using `dx`

The `dx` package lives at `python/dx/` and is auto-installed into managed
Python environments. In a cell:

```python
import pandas as pd
df = pd.read_parquet("somefile.parquet")
df  # rendered via the sift/parquet renderer via a blob-ref, not raw bytes
```

For low-level use:

```python
import dx
ref = dx.put(open("image.png", "rb").read(), content_type="image/png")
dx.display_blob_ref(ref, content_type="image/png")
```

See `docs/superpowers/specs/2026-04-13-nteract-dx-design.md` for the protocol.
```

- [ ] **Step 2: Run lint on everything**

```bash
cargo xtask lint --fix
```

- [ ] **Step 3: Commit**

```bash
git add docs/
git commit -m "docs(dx): add brief usage note"
```

---

## Verification Checklist

- [ ] `cargo test -p notebook-doc` — ref MIME constant test green.
- [ ] `cargo test -p runtimed --test dx_comm_filter` — namespace filter + CRDT exclusion.
- [ ] `cargo test -p runtimed dx_blob_comm output_store::dx_ref_tests` — handler + manifest composition.
- [ ] `cd python/dx && uv run pytest -v` — all dx unit tests green.
- [ ] Python integration test against dev daemon — green.
- [ ] WebdriverIO E2E — green.
- [ ] `cargo xtask lint` — clean.
- [ ] Manual: open a notebook, execute a cell with a large pandas DataFrame, confirm:
  - Sift parquet renderer shows the table.
  - Daemon `runt ps` / logs show blob uploaded at expected hash.
  - No raw-bytes parquet visible in IOPub capture (inspect `runt daemon logs` with debug level).

## Follow-ups (not in scope)

- `PutBlob` frame on the runtime-agent↔daemon notebook-protocol socket for remote agents (part of #1334).
- Renderer UX for `summary` hints ("showing N of M rows" banner).
- `dx.attach(path)` with chunked upload (likely arrives with streaming).
- Interactive query backend (see spec Future section).
- Arrow IPC stream / ADBC research before designing streaming.
