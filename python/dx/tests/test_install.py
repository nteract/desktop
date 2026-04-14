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
    """Minimal stand-in matching IPython's ``InteractiveShell`` API.

    ``display_formatter`` is an *attribute*, not a method. Regression
    guard: dx.install() must access it as ``ip.display_formatter``
    (not ``ip.display_formatter()``), otherwise it raises TypeError
    against a real IPython shell.
    """

    def __init__(self) -> None:
        self.display_formatter = FakeDisplayFormatter()


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
    assert pd.DataFrame in ip.display_formatter.mimebundle_formatter.registrations


def test_install_is_idempotent(monkeypatch):
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._try_open_comm", lambda: None)

    import dx

    dx.install()
    dx.install()
    assert (
        len(ip.display_formatter.mimebundle_formatter.registrations) <= 2
    )  # pandas + optionally polars


def test_install_treats_display_formatter_as_attribute_not_method(monkeypatch):
    """Regression guard: against real IPython, display_formatter is an
    attribute — calling it would raise TypeError.

    Real IPython's ``InteractiveShell.display_formatter`` is a
    ``DisplayFormatter`` instance (not a bound method). Prior versions of
    dx.install() accessed it as ``ip.display_formatter()`` which works
    against a callable test fake but crashes in a real shell.
    """
    _reset_installed(monkeypatch)

    class NonCallableFormatter:
        def __init__(self):
            self.mimebundle_formatter = FakeMimebundleFormatter()

        def __call__(self, *a, **kw):
            raise TypeError("real IPython's display_formatter is not callable — this would crash")

    class StrictFakeIPython:
        def __init__(self):
            self.display_formatter = NonCallableFormatter()

    ip = StrictFakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._try_open_comm", lambda: None)

    import dx

    dx.install()
    assert pd.DataFrame in ip.display_formatter.mimebundle_formatter.registrations


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
    formatter = ip.display_formatter.mimebundle_formatter.registrations[pd.DataFrame]

    df = pd.DataFrame({"a": [1, 2, 3]})
    bundle = formatter(df)

    assert "application/vnd.apache.parquet" in bundle
    assert isinstance(bundle["application/vnd.apache.parquet"], bytes)
    assert "text/llm+plain" in bundle
    assert BLOB_REF_MIME not in bundle
