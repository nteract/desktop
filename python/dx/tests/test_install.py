import pandas as pd
from dx._refs import BLOB_REF_MIME, BlobRef


class FakeMimebundleFormatter:
    def __init__(self) -> None:
        self.registrations: dict = {}

    def for_type(self, cls, func):
        self.registrations[cls] = func


class FakeDisplayFormatter:
    def __init__(self) -> None:
        self.mimebundle_formatter = FakeMimebundleFormatter()


class FakeIPython:
    def __init__(self) -> None:
        self._formatter = FakeDisplayFormatter()

    def display_formatter(self):
        return self._formatter


def _reset_installed(monkeypatch):
    """Each test runs with a fresh install state."""
    import dx._format_install as fi

    monkeypatch.setattr(fi, "_INSTALLED", False)


def test_install_registers_pandas_formatter(monkeypatch):
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._try_open_comm", lambda: None)

    import dx

    dx.install()
    assert pd.DataFrame in ip._formatter.mimebundle_formatter.registrations


def test_install_is_idempotent(monkeypatch):
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._try_open_comm", lambda: None)

    import dx

    dx.install()
    dx.install()
    assert len(ip._formatter.mimebundle_formatter.registrations) <= 2  # pandas + optionally polars


def test_install_noop_when_no_ipython(monkeypatch):
    _reset_installed(monkeypatch)
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: None)
    monkeypatch.setattr("dx._format_install._try_open_comm", lambda: None)

    import dx

    dx.install()  # should not raise


def test_display_blob_ref_emits_blob_ref_mime(monkeypatch):
    _reset_installed(monkeypatch)
    published = []

    def fake_publish(data, metadata=None):
        published.append(data)

    monkeypatch.setattr("dx._format_install._publish_display_data", fake_publish)

    import dx

    ref = BlobRef(hash="sha256:abc", size=42)
    dx.display_blob_ref(ref, content_type="image/png", summary={"total_rows": 100})

    assert len(published) == 1
    bundle = published[0]
    assert BLOB_REF_MIME in bundle
    body = bundle[BLOB_REF_MIME]
    assert body["hash"] == "sha256:abc"
    assert body["content_type"] == "image/png"
    assert body["summary"] == {"total_rows": 100}
    assert body["query"] is None
    assert "url" not in body


def test_pandas_formatter_falls_back_when_no_agent(monkeypatch):
    """No agent → bundle carries raw parquet bytes + summary, not ref MIME."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._try_open_comm", lambda: None)

    import dx

    dx.install()
    formatter = ip._formatter.mimebundle_formatter.registrations[pd.DataFrame]

    df = pd.DataFrame({"a": [1, 2, 3]})
    bundle = formatter(df)

    assert "application/vnd.apache.parquet" in bundle
    assert isinstance(bundle["application/vnd.apache.parquet"], bytes)
    assert "text/llm+plain" in bundle
    assert BLOB_REF_MIME not in bundle
