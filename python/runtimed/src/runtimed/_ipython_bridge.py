"""IOPub bridge for plain IPython.

Creates a minimal set of ZMQ sockets that speak the Jupyter wire protocol,
hooks into IPython's execution events, and publishes outputs on IOPub so
the runt sidecar can display them.
"""

from __future__ import annotations

import hashlib
import hmac
import json
import sys
import tempfile
import threading
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

import zmq


class IPythonBridge:
    """Bridges plain IPython outputs to a Jupyter-protocol IOPub channel.

    Creates ZMQ sockets that mimic a Jupyter kernel's server-side channels:
    - PUB on IOPub (sidecar subscribes here)
    - ROUTER on shell (handles kernel_info_request and execute_request)
    - REP on heartbeat (responds to pings)
    - ROUTER on control and stdin (bound but idle)

    Registers IPython hooks to publish execution outputs on IOPub.
    """

    def __init__(self, ip: str = "127.0.0.1") -> None:
        self._ip = ip
        self._key = uuid.uuid4().hex
        self._session_id = uuid.uuid4().hex
        self._execution_count = 0
        self._running = True
        self._in_displayhook = False

        self._ctx = zmq.Context()

        # Bind all sockets to random ports
        self._iopub_socket = self._ctx.socket(zmq.PUB)
        self._iopub_port = self._iopub_socket.bind_to_random_port(f"tcp://{ip}")

        self._shell_socket = self._ctx.socket(zmq.ROUTER)
        self._shell_port = self._shell_socket.bind_to_random_port(f"tcp://{ip}")

        self._hb_socket = self._ctx.socket(zmq.REP)
        self._hb_port = self._hb_socket.bind_to_random_port(f"tcp://{ip}")

        self._control_socket = self._ctx.socket(zmq.ROUTER)
        self._control_port = self._control_socket.bind_to_random_port(f"tcp://{ip}")

        self._stdin_socket = self._ctx.socket(zmq.ROUTER)
        self._stdin_port = self._stdin_socket.bind_to_random_port(f"tcp://{ip}")

        self._connection_file = self._write_connection_file()

        # Start background responder threads
        self._shell_thread = threading.Thread(target=self._shell_loop, daemon=True)
        self._shell_thread.start()

        self._hb_thread = threading.Thread(target=self._heartbeat_loop, daemon=True)
        self._hb_thread.start()

    @property
    def connection_file(self) -> Path:
        return self._connection_file

    def _write_connection_file(self) -> Path:
        info = {
            "ip": self._ip,
            "transport": "tcp",
            "shell_port": self._shell_port,
            "iopub_port": self._iopub_port,
            "stdin_port": self._stdin_port,
            "control_port": self._control_port,
            "hb_port": self._hb_port,
            "key": self._key,
            "signature_scheme": "hmac-sha256",
            "kernel_name": "python3",
        }
        runtime_dir = tempfile.mkdtemp(prefix="runtimed-bridge-")
        path = Path(runtime_dir) / f"kernel-bridge-{uuid.uuid4().hex[:8]}.json"
        path.write_text(json.dumps(info))
        return path

    # --- Jupyter wire protocol ---

    def _make_header(self, msg_type: str) -> dict:
        return {
            "msg_id": uuid.uuid4().hex,
            "msg_type": msg_type,
            "username": "ipython-bridge",
            "session": self._session_id,
            "date": datetime.now(timezone.utc).isoformat(),
            "version": "5.3",
        }

    def _sign(self, *parts: bytes) -> bytes:
        h = hmac.new(self._key.encode(), digestmod=hashlib.sha256)
        for part in parts:
            h.update(part)
        return h.hexdigest().encode()

    def _send(
        self,
        socket: zmq.Socket,
        identities: list[bytes],
        msg_type: str,
        parent_header: dict | None,
        content: dict,
        metadata: dict | None = None,
    ) -> None:
        header = self._make_header(msg_type)
        header_b = json.dumps(header).encode()
        parent_b = json.dumps(parent_header or {}).encode()
        meta_b = json.dumps(metadata or {}).encode()
        content_b = json.dumps(content).encode()
        sig = self._sign(header_b, parent_b, meta_b, content_b)

        parts = identities + [b"<IDS|MSG>", sig, header_b, parent_b, meta_b, content_b]
        socket.send_multipart(parts)

    def _send_iopub(
        self,
        msg_type: str,
        content: dict,
        parent_header: dict | None = None,
    ) -> None:
        self._send(self._iopub_socket, [], msg_type, parent_header, content)

    # --- Background responders ---

    def _shell_loop(self) -> None:
        while self._running:
            try:
                if not self._shell_socket.poll(1000):
                    continue
                parts = self._shell_socket.recv_multipart()
                try:
                    delim_idx = parts.index(b"<IDS|MSG>")
                except ValueError:
                    continue
                identities = parts[:delim_idx]
                header = json.loads(parts[delim_idx + 2])

                if header["msg_type"] == "kernel_info_request":
                    self._handle_kernel_info(identities, header)
                elif header["msg_type"] == "execute_request":
                    content = json.loads(parts[delim_idx + 5])
                    self._handle_execute(identities, header, content)
            except zmq.ZMQError:
                break
            except Exception:
                continue

    def _handle_kernel_info(self, identities: list[bytes], parent: dict) -> None:
        reply = {
            "status": "ok",
            "protocol_version": "5.3",
            "implementation": "ipython-bridge",
            "implementation_version": "0.1.0",
            "language_info": {
                "name": "python",
                "version": (
                    f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
                ),
                "mimetype": "text/x-python",
                "file_extension": ".py",
                "pygments_lexer": "ipython3",
                "codemirror_mode": {"name": "ipython", "version": 3},
                "nbconvert_exporter": "python",
            },
            "banner": "IPython Bridge for runtimed sidecar",
            "help_links": [],
            "debugger": False,
        }
        self._send(self._shell_socket, identities, "kernel_info_reply", parent, reply)
        # Also publish kernel info on IOPub so the sidecar sees it
        self._send_iopub("status", {"execution_state": "idle"}, parent)

    def _handle_execute(self, identities: list[bytes], parent: dict, content: dict) -> None:
        user_expressions = content.get("user_expressions", {})
        results: dict[str, Any] = {}
        for key, expr in user_expressions.items():
            try:
                val = eval(expr)  # noqa: S307
                results[key] = {
                    "status": "ok",
                    "data": {"text/plain": repr(val)},
                    "metadata": {},
                }
            except Exception as e:
                results[key] = {
                    "status": "error",
                    "ename": type(e).__name__,
                    "evalue": str(e),
                    "traceback": [],
                }

        reply = {
            "status": "ok",
            "execution_count": self._execution_count,
            "user_expressions": results,
        }
        self._send(self._shell_socket, identities, "execute_reply", parent, reply)

    def _heartbeat_loop(self) -> None:
        while self._running:
            try:
                if not self._hb_socket.poll(1000):
                    continue
                msg = self._hb_socket.recv()
                self._hb_socket.send(msg)
            except zmq.ZMQError:
                break

    # --- IPython hook methods (called from main thread) ---

    def publish_execute_result(self, data: dict, metadata: dict, execution_count: int) -> None:
        content = {
            "execution_count": execution_count,
            "data": data,
            "metadata": metadata,
        }
        self._send_iopub("execute_result", content)

    def publish_stream(self, name: str, text: str) -> None:
        self._send_iopub("stream", {"name": name, "text": text})

    def publish_display_data(
        self, data: dict, metadata: dict | None = None, transient: dict | None = None
    ) -> None:
        self._send_iopub(
            "display_data",
            {"data": data, "metadata": metadata or {}, "transient": transient or {}},
        )

    def publish_error(self, ename: str, evalue: str, traceback_list: list[str]) -> None:
        self._send_iopub("error", {"ename": ename, "evalue": evalue, "traceback": traceback_list})

    def publish_status(self, execution_state: str) -> None:
        self._send_iopub("status", {"execution_state": execution_state})

    def close(self) -> None:
        self._running = False
        # Wait for background threads to notice _running=False and exit
        self._shell_thread.join(timeout=2)
        self._hb_thread.join(timeout=2)
        # Now safe to tear down sockets and context
        self._ctx.destroy(linger=0)
        try:
            self._connection_file.unlink()
            self._connection_file.parent.rmdir()
        except OSError:
            pass


class _TeeStream:
    """Wraps a stream to tee writes to both the original and the bridge."""

    def __init__(self, original: Any, name: str, bridge: IPythonBridge) -> None:
        self._original = original
        self._name = name
        self._bridge = bridge

    def write(self, text: str) -> int:
        result = self._original.write(text)
        if text and not self._bridge._in_displayhook:
            self._bridge.publish_stream(self._name, text)
        return result

    def flush(self) -> None:
        self._original.flush()

    def __getattr__(self, attr: str) -> Any:
        return getattr(self._original, attr)


_WIDGET_VIEW_MIMETYPE = "application/vnd.jupyter.widget-view+json"

_WIDGET_PLACEHOLDER_HTML = (
    '<div style="padding: 8px 12px; border: 1px solid #e0e0e0; '
    "border-radius: 4px; background: #f8f8f8; color: #666; "
    'font-size: 13px;">'
    "&#9432; Widgets are not supported in the IPython Sidecar Bridge<br>"
    "Use <code>jupyter console</code> for full widget support."
    "</div>"
)


def _rewrite_widget_data(data: dict) -> dict:
    """Replace widget view mime type with a helpful HTML placeholder."""
    if _WIDGET_VIEW_MIMETYPE not in data:
        return data
    data = dict(data)
    del data[_WIDGET_VIEW_MIMETYPE]
    data["text/html"] = _WIDGET_PLACEHOLDER_HTML
    # Keep text/plain if present, otherwise add one
    if "text/plain" not in data:
        data["text/plain"] = "(widget not available in bridged mode)"
    return data


def install_bridge(ip: Any) -> IPythonBridge:
    """Install the IOPub bridge into an IPython shell instance.

    Registers post_run_cell hooks, wraps stdout/stderr, and wraps
    the display publisher to forward all outputs to the bridge's
    IOPub channel.

    Args:
        ip: The IPython InteractiveShell instance.

    Returns:
        The running IPythonBridge.
    """
    bridge = IPythonBridge()

    # --- Enable rich formatters ---
    # Terminal IPython disables formatters like text/html, image/png, etc.
    # since the terminal can't render them. But the sidecar can, so we
    # enable them so display objects produce rich output.
    _rich_mime_types = (
        "text/html",
        "text/latex",
        "text/markdown",
        "image/png",
        "image/jpeg",
        "image/svg+xml",
        "application/json",
        "application/javascript",
        "application/pdf",
    )
    for mime_type in _rich_mime_types:
        if mime_type in ip.display_formatter.formatters:
            ip.display_formatter.formatters[mime_type].enabled = True

    # --- Suppress displayhook stdout from bridge ---
    # IPython's displayhook writes the result repr to stdout (e.g. "Out[3]: 23").
    # We already publish results as execute_result with rich mime types, so
    # suppress the displayhook's stdout from being forwarded to the bridge
    # (it would show up as a duplicate plain-text stream message).
    original_displayhook = ip.displayhook.__call__

    def _displayhook_wrapper(result: Any) -> None:
        bridge._in_displayhook = True
        try:
            original_displayhook(result)
        finally:
            bridge._in_displayhook = False

    ip.displayhook.__call__ = _displayhook_wrapper

    # --- post_run_cell: publish execute_result or error ---
    def post_run_cell(result: Any) -> None:
        bridge._execution_count += 1
        bridge.publish_status("busy")

        if result.error_in_exec is not None:
            exc = result.error_in_exec
            ename = type(exc).__name__
            evalue = str(exc)
            # Use IPython's formatted traceback if available
            tb_lines: list[str] = []
            if hasattr(result, "error_before_exec") and result.error_before_exec:
                tb_lines = [str(result.error_before_exec)]
            bridge.publish_error(ename, evalue, tb_lines)
        elif result.result is not None:
            try:
                format_dict, md_dict = ip.display_formatter.format(result.result)
            except Exception:
                format_dict = {"text/plain": repr(result.result)}
                md_dict = {}
            bridge.publish_execute_result(
                _rewrite_widget_data(format_dict), md_dict, bridge._execution_count
            )

        bridge.publish_status("idle")

    ip.events.register("post_run_cell", post_run_cell)

    # --- Wrap stdout/stderr ---
    sys.stdout = _TeeStream(sys.stdout, "stdout", bridge)
    sys.stderr = _TeeStream(sys.stderr, "stderr", bridge)

    # --- Wrap display publisher ---
    original_publish = ip.display_pub.publish

    def patched_publish(
        data: Any = None,
        metadata: Any = None,
        source: Any = None,
        *,
        transient: Any = None,
        update: bool = False,
        **kwargs: Any,
    ) -> None:
        original_publish(data, metadata, source, transient=transient, update=update, **kwargs)
        if data:
            msg_type = "update_display_data" if update else "display_data"
            bridge._send_iopub(
                msg_type,
                {
                    "data": _rewrite_widget_data(data) if isinstance(data, dict) else data,
                    "metadata": metadata or {},
                    "transient": transient or {},
                },
            )

    ip.display_pub.publish = patched_publish

    # Publish an initial idle status so the sidecar knows we're alive
    bridge.publish_status("idle")

    return bridge
