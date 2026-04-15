import hashlib

import pandas as pd
from dx._refs import BLOB_REF_MIME


class FakeMimebundleFormatter:
    def __init__(self) -> None:
        self.registrations: dict = {}

    def for_type(self, cls, func):
        self.registrations[cls] = func


class FakeDisplayFormatter:
    def __init__(self) -> None:
        self.mimebundle_formatter = FakeMimebundleFormatter()
        self.ipython_display_formatter = FakeMimebundleFormatter()


class FakeSession:
    def __init__(self) -> None:
        self.sent: list[dict] = []

    def send(self, socket, msg, *, ident=None, buffers=None):
        self.sent.append({"msg": msg, "ident": ident, "buffers": list(buffers or [])})


class FakeDisplayPub:
    """Minimal ``ZMQDisplayPublisher`` stand-in exposing the public
    surface our hook uses: ``register_hook`` + ``session`` + ``pub_socket``
    + ``topic``."""

    def __init__(self) -> None:
        self.session = FakeSession()
        self.pub_socket = object()
        self.topic = b"display_data"
        self._hooks: list = []

    def register_hook(self, hook):
        self._hooks.append(hook)


class FakeIPython:
    """Minimal stand-in matching IPython's ``InteractiveShell`` API."""

    def __init__(self, *, with_kernel: bool = True) -> None:
        self.display_formatter = FakeDisplayFormatter()
        if with_kernel:
            self.display_pub = FakeDisplayPub()
        else:
            self.display_pub = None


def _reset_installed(monkeypatch):
    """Each test runs with a fresh install state."""
    import dx._format_install as fi

    monkeypatch.setattr(fi, "_INSTALLED", False)
    # Also clear any leftover pending buffers from prior tests.
    if hasattr(fi._pending, "buffers"):
        fi._pending.buffers.clear()


def test_install_registers_pandas_formatter(monkeypatch):
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx

    dx.install()
    assert pd.DataFrame in ip.display_formatter.mimebundle_formatter.registrations


def test_install_is_idempotent(monkeypatch):
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx

    dx.install()
    dx.install()
    assert (
        len(ip.display_formatter.mimebundle_formatter.registrations) <= 3
    )  # pandas + optional polars + optional narwhals


def test_install_treats_display_formatter_as_attribute_not_method(monkeypatch):
    """Regression: real IPython exposes display_formatter as attribute."""
    _reset_installed(monkeypatch)

    class NonCallableFormatter:
        def __init__(self):
            self.mimebundle_formatter = FakeMimebundleFormatter()
            self.ipython_display_formatter = FakeMimebundleFormatter()

        def __call__(self, *a, **kw):
            raise TypeError("real IPython's display_formatter is not callable")

    class StrictFakeIPython:
        def __init__(self):
            self.display_formatter = NonCallableFormatter()
            self.display_pub = None

    ip = StrictFakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx

    dx.install()
    assert pd.DataFrame in ip.display_formatter.mimebundle_formatter.registrations


def test_install_noop_when_no_ipython(monkeypatch):
    _reset_installed(monkeypatch)
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: None)

    import dx

    dx.install()  # must not raise


def test_install_registers_display_pub_hook(monkeypatch):
    """``install()`` registers a hook on the kernel's display publisher."""
    _reset_installed(monkeypatch)
    ip = FakeIPython(with_kernel=True)
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx

    dx.install()

    assert len(ip.display_pub._hooks) == 1
    assert getattr(ip.display_pub._hooks[0], "_dx_installed", False) is True

    # Idempotent: second install must not stack duplicate hooks.
    dx.install()
    assert len(ip.display_pub._hooks) == 1


def test_install_skips_hook_when_display_pub_lacks_kernel_surface(monkeypatch):
    """Plain IPython's DisplayPublisher has no register_hook / session —
    the hook installer must not raise or register against it."""
    _reset_installed(monkeypatch)
    ip = FakeIPython(with_kernel=False)
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx

    dx.install()  # must not raise
    # Formatters still registered.
    assert pd.DataFrame in ip.display_formatter.mimebundle_formatter.registrations


def test_mimebundle_returns_ref_mime_and_stashes_bytes(monkeypatch):
    """Formatter returns {BLOB_REF_MIME: ..., text/llm+plain: ...} and
    stashes the parquet bytes in the pending buffer map keyed by hash."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx
    import dx._format_install as fi

    dx.install()
    formatter = ip.display_formatter.mimebundle_formatter.registrations[pd.DataFrame]

    df = pd.DataFrame({"a": [1, 2, 3]})
    bundle = formatter(df)

    assert BLOB_REF_MIME in bundle
    assert "text/llm+plain" in bundle
    ref = bundle[BLOB_REF_MIME]
    assert ref["content_type"] == "application/vnd.apache.parquet"
    assert ref["buffer_index"] == 0

    # Bytes were stashed at the ref's hash.
    pending = fi._pending_buffers()
    assert ref["hash"] in pending
    buf = pending[ref["hash"]]
    assert buf[:4] == b"PAR1"
    assert hashlib.sha256(buf).hexdigest() == ref["hash"]


def test_mimebundle_returns_none_on_serialize_failure(monkeypatch):
    """When parquet serialization raises, formatter returns None so the
    default HTML/plain chain runs."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)
    monkeypatch.setattr(
        "dx._format_install.serialize_dataframe",
        lambda df, **kw: (_ for _ in ()).throw(RuntimeError("boom")),
    )

    import dx

    dx.install()
    formatter = ip.display_formatter.mimebundle_formatter.registrations[pd.DataFrame]

    df = pd.DataFrame({"a": [1]})
    assert formatter(df) is None


def test_display_pub_hook_attaches_buffers_on_display_data(monkeypatch):
    """Hook: sees BLOB_REF_MIME in a display_data message → pops buffers
    from the pending map, calls session.send with buffers=[bytes],
    returns None (suppress default send)."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx
    import dx._format_install as fi

    dx.install()
    hook = ip.display_pub._hooks[0]

    payload = b"PAR1parquet-bytes"
    h = hashlib.sha256(payload).hexdigest()
    fi._pending_buffers()[h] = payload

    msg = {
        "header": {"msg_type": "display_data"},
        "content": {
            "data": {
                BLOB_REF_MIME: {
                    "hash": h,
                    "content_type": "application/vnd.apache.parquet",
                    "size": len(payload),
                    "buffer_index": 0,
                },
                "text/llm+plain": "summary",
            },
            "metadata": {},
            "transient": {},
        },
    }

    result = hook(msg)
    assert result is None, "hook must return None to suppress default send"
    assert len(ip.display_pub.session.sent) == 1
    sent = ip.display_pub.session.sent[0]
    assert sent["buffers"] == [payload]
    # The pending slot is consumed so a later publish for the same hash
    # doesn't accidentally re-attach the bytes.
    assert h not in fi._pending_buffers()


def test_display_pub_hook_fires_for_update_display_data(monkeypatch):
    """``h.update(df)`` produces an ``update_display_data`` message with
    ``transient.display_id`` set. Our hook must fire for that msg_type
    too and publish with buffers, so the update carries the binary
    payload just like the initial display."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx
    import dx._format_install as fi

    dx.install()
    hook = ip.display_pub._hooks[0]

    payload = b"PAR1updated"
    h = hashlib.sha256(payload).hexdigest()
    fi._pending_buffers()[h] = payload

    msg = {
        "header": {"msg_type": "update_display_data"},
        "content": {
            "data": {
                BLOB_REF_MIME: {
                    "hash": h,
                    "content_type": "application/vnd.apache.parquet",
                    "size": len(payload),
                    "buffer_index": 0,
                },
            },
            "metadata": {},
            "transient": {"display_id": "h1"},
        },
    }

    result = hook(msg)
    assert result is None
    assert len(ip.display_pub.session.sent) == 1
    sent = ip.display_pub.session.sent[0]
    assert sent["buffers"] == [payload]
    # The sent msg preserves transient.display_id — this is what makes
    # update_display_data work: the frontend matches the display_id and
    # updates the existing output in place.
    assert sent["msg"]["content"]["transient"] == {"display_id": "h1"}


def test_display_pub_hook_passes_through_unrelated_messages(monkeypatch):
    """Hook returns the msg unchanged when it doesn't carry our ref MIME."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx

    dx.install()
    hook = ip.display_pub._hooks[0]

    msg = {
        "header": {"msg_type": "display_data"},
        "content": {"data": {"text/html": "<p>hi</p>"}},
    }
    assert hook(msg) is msg
    assert ip.display_pub.session.sent == []


def test_display_pub_hook_passes_through_when_no_pending_payload(monkeypatch):
    """If the ref MIME references a hash we don't have bytes for — maybe
    a re-publish or a handcrafted bundle — leave the message untouched
    so default send runs (the agent can still resolve via BlobStore on
    that hash if it's already stored)."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx

    dx.install()
    hook = ip.display_pub._hooks[0]

    msg = {
        "header": {"msg_type": "display_data"},
        "content": {
            "data": {
                BLOB_REF_MIME: {
                    "hash": "deadbeef",
                    "content_type": "image/png",
                    "size": 0,
                    "buffer_index": 0,
                },
            },
        },
    }
    result = hook(msg)
    assert result is msg
    assert ip.display_pub.session.sent == []


def test_install_enables_altair_nteract_renderer_when_present(monkeypatch):
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

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
    assert enabled == ["nteract"]


def test_install_sets_plotly_default_renderer_when_present(monkeypatch):
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

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
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import sys

    monkeypatch.setitem(sys.modules, "altair", None)
    monkeypatch.setitem(sys.modules, "plotly", None)
    monkeypatch.setitem(sys.modules, "plotly.io", None)

    import dx

    dx.install()  # must not raise


def test_install_registers_ipython_display_handler_for_pandas(monkeypatch):
    """dx.install() must register a handler on ipython_display_formatter
    alongside the existing mimebundle_formatter registration, so last-
    expression `df` short-circuits the execute_result path."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx

    dx.install()
    assert pd.DataFrame in ip.display_formatter.ipython_display_formatter.registrations


def test_ipython_display_handler_publishes_and_stashes_buffers(monkeypatch):
    """The handler builds a mimebundle via _emit_dataframe (which stashes
    parquet bytes in _pending_buffers keyed by hash) and publishes a
    display_data message carrying BLOB_REF_MIME. The publisher hook is
    the piece that later attaches the buffers — this test covers just
    the formatter side."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx
    import dx._format_install as fi

    dx.install()
    handler = ip.display_formatter.ipython_display_formatter.registrations[pd.DataFrame]

    published: list[dict] = []

    def fake_publish_display_data(data, metadata=None, **kwargs):
        published.append({"data": data, "metadata": metadata})

    # Patch the real publish_display_data *inside* the function scope where
    # the handler imports it. The handler does `from IPython.display import
    # publish_display_data` lazily, so patch IPython.display.
    import IPython.display as ipd

    monkeypatch.setattr(ipd, "publish_display_data", fake_publish_display_data)

    df = pd.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
    handler(df)

    assert len(published) == 1, f"expected one publish call, got {len(published)}"
    bundle = published[0]["data"]
    assert BLOB_REF_MIME in bundle, f"expected BLOB_REF_MIME in bundle keys: {list(bundle)}"
    ref = bundle[BLOB_REF_MIME]
    assert ref["content_type"] == "application/vnd.apache.parquet"
    assert isinstance(ref["hash"], str) and len(ref["hash"]) == 64  # sha256 hex

    # The parquet bytes must be in the pending stash so the publisher hook
    # can pop them on the subsequent IOPub send.
    assert ref["hash"] in fi._pending_buffers(), "buffer not stashed for hash"


# ── Narwhals wrapping ──────────────────────────────────────────────


def test_install_registers_ipython_display_handler_for_narwhals(monkeypatch):
    """narwhals.DataFrame wraps pandas/polars/pyarrow/modin/dask/cudf.
    dx.install() should register for the wrapper type too so a
    last-expression nw.DataFrame doesn't fall back to narwhals's own repr."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx
    import narwhals as nw

    dx.install()
    assert nw.DataFrame in ip.display_formatter.ipython_display_formatter.registrations


def test_narwhals_handler_unwraps_pandas_and_publishes(monkeypatch):
    """Fast path: nw.DataFrame backed by pandas — unwrap via to_native()
    and emit via the pandas encoder directly (no to_pandas() round-trip)."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx
    import dx._format_install as fi
    import narwhals as nw

    dx.install()
    handler = ip.display_formatter.ipython_display_formatter.registrations[nw.DataFrame]

    published: list[dict] = []

    def fake_publish_display_data(data, metadata=None, **kwargs):
        published.append({"data": data})

    import IPython.display as ipd

    monkeypatch.setattr(ipd, "publish_display_data", fake_publish_display_data)

    df = pd.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
    nw_df = nw.from_native(df)
    handler(nw_df)

    assert len(published) == 1
    bundle = published[0]["data"]
    assert BLOB_REF_MIME in bundle
    ref = bundle[BLOB_REF_MIME]
    assert ref["content_type"] == "application/vnd.apache.parquet"
    assert ref["hash"] in fi._pending_buffers()


def test_narwhals_handler_unwraps_polars_and_publishes(monkeypatch):
    """Fast path: nw.DataFrame backed by polars — unwrap + polars encoder."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx
    import dx._format_install as fi
    import narwhals as nw
    import polars as pl

    dx.install()
    handler = ip.display_formatter.ipython_display_formatter.registrations[nw.DataFrame]

    published: list[dict] = []

    def fake_publish_display_data(data, metadata=None, **kwargs):
        published.append({"data": data})

    import IPython.display as ipd

    monkeypatch.setattr(ipd, "publish_display_data", fake_publish_display_data)

    df = pl.DataFrame({"a": [1, 2, 3], "b": ["x", "y", "z"]})
    nw_df = nw.from_native(df)
    handler(nw_df)

    assert len(published) == 1
    bundle = published[0]["data"]
    assert BLOB_REF_MIME in bundle
    assert bundle[BLOB_REF_MIME]["hash"] in fi._pending_buffers()


def test_narwhals_handler_falls_back_to_pandas_for_pyarrow_table(monkeypatch):
    """pyarrow Table isn't supported by serialize_dataframe directly.
    The narwhals handler should round-trip via .to_pandas() so the fast
    path in _emit_dataframe still hits the pandas encoder."""
    _reset_installed(monkeypatch)
    ip = FakeIPython()
    monkeypatch.setattr("dx._format_install._get_ipython_for_format", lambda: ip)

    import dx
    import dx._format_install as fi
    import narwhals as nw
    import pyarrow as pa

    dx.install()
    handler = ip.display_formatter.ipython_display_formatter.registrations[nw.DataFrame]

    published: list[dict] = []

    def fake_publish_display_data(data, metadata=None, **kwargs):
        published.append({"data": data})

    import IPython.display as ipd

    monkeypatch.setattr(ipd, "publish_display_data", fake_publish_display_data)

    table = pa.table({"a": [1, 2, 3], "b": ["x", "y", "z"]})
    nw_df = nw.from_native(table, eager_only=True)
    handler(nw_df)

    assert len(published) == 1
    bundle = published[0]["data"]
    assert BLOB_REF_MIME in bundle
    assert bundle[BLOB_REF_MIME]["hash"] in fi._pending_buffers()
