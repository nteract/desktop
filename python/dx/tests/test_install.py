import hashlib

import pandas as pd
from dx._refs import BLOB_REF_MIME


class FakeTypeFormatter:
    def __init__(self) -> None:
        self.registrations: dict = {}

    def for_type(self, cls, func):
        self.registrations[cls] = func


class FakeDisplayFormatter:
    def __init__(self) -> None:
        self.ipython_display_formatter = FakeTypeFormatter()


class FakeIPython:
    """Minimal stand-in matching IPython's ``InteractiveShell`` API.

    Regression guard: ``display_formatter`` is an attribute, not a method.
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
    monkeypatch.setattr("dx._format_install._kernel_session_and_socket", lambda: None)

    import dx

    dx.install()
    assert pd.DataFrame in ip.display_formatter.ipython_display_formatter.registrations


def test_install_is_idempotent(monkeypatch):
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._kernel_session_and_socket", lambda: None)

    import dx

    dx.install()
    dx.install()
    # pandas + optionally polars — but no double registration.
    assert len(ip.display_formatter.ipython_display_formatter.registrations) <= 2


def test_install_treats_display_formatter_as_attribute_not_method(monkeypatch):
    """Regression: real IPython exposes display_formatter as attribute."""
    _reset_installed(monkeypatch)

    class NonCallableFormatter:
        def __init__(self):
            self.ipython_display_formatter = FakeTypeFormatter()

        def __call__(self, *a, **kw):
            raise TypeError("real IPython's display_formatter is not callable")

    class StrictFakeIPython:
        def __init__(self):
            self.display_formatter = NonCallableFormatter()

    ip = StrictFakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._kernel_session_and_socket", lambda: None)

    import dx

    dx.install()
    assert pd.DataFrame in ip.display_formatter.ipython_display_formatter.registrations


def test_install_noop_when_no_ipython(monkeypatch):
    _reset_installed(monkeypatch)
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: None)
    monkeypatch.setattr("dx._format_install._kernel_session_and_socket", lambda: None)

    import dx

    dx.install()  # must not raise


def test_formatter_publishes_display_data_with_buffers_and_claims_display(monkeypatch):
    """Under ipykernel: formatter fires session.send with buffers=[parquet_bytes]
    and returns True so IPython skips every other formatter for the DataFrame."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    captured: dict = {}

    def fake_send(*, session, iopub_socket, data, buffers):
        captured["data"] = data
        captured["buffers"] = buffers

    # Pretend we're under ipykernel by stubbing the helper that probes for it.
    monkeypatch.setattr(
        "dx._format_install._kernel_session_and_socket",
        lambda: (object(), object()),
    )
    monkeypatch.setattr("dx._format_install._send_display_data_with_buffers", fake_send)

    import dx

    dx.install()
    formatter = ip.display_formatter.ipython_display_formatter.registrations[pd.DataFrame]

    df = pd.DataFrame({"a": [1, 2, 3]})
    result = formatter(df)

    # True means "we took over display"; IPython skips the rest of the chain.
    assert result is True

    # The published message has the ref MIME + llm summary, and one buffer.
    assert BLOB_REF_MIME in captured["data"]
    assert "text/llm+plain" in captured["data"]
    ref = captured["data"][BLOB_REF_MIME]
    assert ref["content_type"] == "application/vnd.apache.parquet"
    assert ref["buffer_index"] == 0
    assert len(captured["buffers"]) == 1
    parquet_bytes = captured["buffers"][0]
    assert parquet_bytes[:4] == b"PAR1"
    # Hash in the ref matches the buffer bytes — content-addressed.
    assert ref["hash"] == hashlib.sha256(parquet_bytes).hexdigest()
    assert ref["size"] == len(parquet_bytes)


def test_install_enables_altair_nteract_renderer_when_present(monkeypatch):
    """If altair is importable, dx.install() flips its renderer registry to 'nteract'."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._kernel_session_and_socket", lambda: None)

    import sys
    import types

    enabled = []

    class FakeRegistry:
        def enable(self, name):
            enabled.append(name)

    fake_alt = types.ModuleType("altair")
    fake_alt.renderers = FakeRegistry()
    monkeypatch.setitem(sys.modules, "altair", fake_alt)

    import dx

    dx.install()
    assert enabled == ["nteract"], f"expected altair renderer flipped, got {enabled}"


def test_install_sets_plotly_default_renderer_when_present(monkeypatch):
    """If plotly.io is importable, dx.install() assigns the default to 'nteract'."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._kernel_session_and_socket", lambda: None)

    import sys
    import types

    class FakeRenderers:
        def __init__(self):
            self.default = "plotly_mimetype"

    fake_pio = types.ModuleType("plotly.io")
    fake_pio.renderers = FakeRenderers()
    fake_plotly = types.ModuleType("plotly")
    fake_plotly.io = fake_pio
    monkeypatch.setitem(sys.modules, "plotly", fake_plotly)
    monkeypatch.setitem(sys.modules, "plotly.io", fake_pio)

    import dx

    dx.install()
    assert fake_pio.renderers.default == "nteract"


def test_install_third_party_renderer_activation_is_noop_when_absent(monkeypatch):
    """Missing altair/plotly must not break install — guarded by ImportError."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._kernel_session_and_socket", lambda: None)

    import sys

    # Hide altair / plotly even if they're installed in the test env.
    monkeypatch.setitem(sys.modules, "altair", None)
    monkeypatch.setitem(sys.modules, "plotly", None)
    monkeypatch.setitem(sys.modules, "plotly.io", None)

    import dx

    dx.install()  # must not raise


def test_formatter_returns_none_when_no_ipykernel(monkeypatch):
    """No ipykernel: returning None lets IPython's default HTML/plain chain run."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr("dx._format_install._kernel_session_and_socket", lambda: None)

    import dx

    dx.install()
    formatter = ip.display_formatter.ipython_display_formatter.registrations[pd.DataFrame]

    df = pd.DataFrame({"a": [1, 2, 3]})
    result = formatter(df)
    # None => IPython's default formatter chain proceeds.
    assert result is None
