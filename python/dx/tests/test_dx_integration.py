"""Integration tests for dx display enrichment.

Tests verify that the full pipeline (summary + formatting) produces the
expected output shapes. These run without a live kernel — they test the
Python-side logic that would run inside a kernel.
"""

import pandas as pd
import pytest
from dx._summary import summarize_dataframe


class TestTextHeavyDataFrameSummaryStaysInline:
    """Regression guard: text-heavy DataFrames used to produce summaries
    that exceeded the 1 KB inline threshold and got blob-stored, showing
    up as a URL rather than inline text in MCP structured content."""

    def test_summary_stays_under_1kb(self):
        long_text = "x" * 200
        df = pd.DataFrame(
            {
                "bio": [long_text] * 5,
                "notes": [long_text] * 5,
                "comment": [long_text] * 5,
            }
        )
        out = summarize_dataframe(df, total_rows=5, included_rows=5, sampled=False)
        assert len(out) < 1024, (
            f"Summary is {len(out)} bytes, must stay under 1 KB for inline display"
        )

    def test_summary_contains_truncation_markers(self):
        long_text = "a" * 200
        df = pd.DataFrame({"text": [long_text] * 5})
        out = summarize_dataframe(df, total_rows=5, included_rows=5, sampled=False)
        # The head preview should contain truncation markers from max_colwidth
        assert "…" in out or "..." in out or len(out) < 1024


class TestEmitDataFrameFallbackWithoutPyarrow:
    """When pyarrow is missing, _emit_dataframe should still return a
    text/llm+plain summary instead of None."""

    def test_emit_returns_summary_only_on_serialize_failure(self, monkeypatch):
        import dx._format_install as fi

        # Make serialize_dataframe raise to simulate missing pyarrow
        monkeypatch.setattr(
            fi,
            "serialize_dataframe",
            lambda df, max_bytes: (_ for _ in ()).throw(ImportError("No module named 'pyarrow'")),
        )

        df = pd.DataFrame({"score": [0.1, 0.5, 0.9], "name": ["a", "b", "c"]})
        bundle = fi._emit_dataframe(df, total_rows=3)

        assert bundle is not None
        assert "text/llm+plain" in bundle
        # No blob ref — parquet serialization failed
        from dx._refs import BLOB_REF_MIME

        assert BLOB_REF_MIME not in bundle
        # Summary should still contain useful stats
        summary = bundle["text/llm+plain"]
        assert "score" in summary
        assert "name" in summary
        assert "range" in summary


class TestDatasetEmitsDisplayDataWithSummary:
    """Verify that a datasets.Dataset produces display_data with
    text/llm+plain mentioning feature names and num_rows."""

    @pytest.fixture(autouse=True)
    def _require_datasets(self):
        pytest.importorskip("datasets")

    def test_dataset_mimebundle_returns_llm_plain(self, monkeypatch):
        import dx._format_install as fi
        from datasets import Dataset

        monkeypatch.setattr(fi, "_INSTALLED", False)
        if hasattr(fi._pending, "buffers"):
            fi._pending.buffers.clear()

        ds = Dataset.from_dict({"text": ["hello", "world"], "label": [0, 1]})

        bundle = fi._dataset_mimebundle(ds)
        assert bundle is not None
        assert "text/llm+plain" in bundle
        summary = bundle["text/llm+plain"]
        assert "2 rows" in summary
        assert "text" in summary
        assert "label" in summary

    def test_dataset_mimebundle_no_parquet_ref(self, monkeypatch):
        """Dataset handler must NOT emit parquet ref MIME — keeps data lazy."""
        import dx._format_install as fi
        from datasets import Dataset
        from dx._refs import BLOB_REF_MIME

        ds = Dataset.from_dict({"a": [1]})
        bundle = fi._dataset_mimebundle(ds)
        assert bundle is not None
        assert BLOB_REF_MIME not in bundle

    def test_dataset_handler_registered_on_install(self, monkeypatch):
        import dx._format_install as fi
        from datasets import Dataset

        monkeypatch.setattr(fi, "_INSTALLED", False)
        if hasattr(fi._pending, "buffers"):
            fi._pending.buffers.clear()

        # Fake IPython
        class FakeMimebundleFormatter:
            def __init__(self):
                self.registrations = {}

            def for_type(self, cls, func):
                self.registrations[cls] = func

        class FakeDisplayFormatter:
            def __init__(self):
                self.mimebundle_formatter = FakeMimebundleFormatter()
                self.ipython_display_formatter = FakeMimebundleFormatter()

        class FakeIPython:
            def __init__(self):
                self.display_formatter = FakeDisplayFormatter()
                self.display_pub = None

        ip = FakeIPython()
        monkeypatch.setattr(fi, "_get_ipython_for_format", lambda: ip)

        fi.install_formatters()
        assert Dataset in ip.display_formatter.mimebundle_formatter.registrations
        assert Dataset in ip.display_formatter.ipython_display_formatter.registrations

    def test_dataset_ipython_display_publishes(self, monkeypatch):
        import dx._format_install as fi
        from datasets import Dataset

        ds = Dataset.from_dict({"text": ["hello"], "label": [1]})

        published = []

        def fake_publish_display_data(data, metadata=None, **kwargs):
            published.append({"data": data, "metadata": metadata})

        import IPython.display as ipd

        monkeypatch.setattr(ipd, "publish_display_data", fake_publish_display_data)

        fi._dataset_ipython_display(ds)

        assert len(published) == 1
        bundle = published[0]["data"]
        assert "text/llm+plain" in bundle
        assert "hello" in bundle["text/llm+plain"] or "text" in bundle["text/llm+plain"]
