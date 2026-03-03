"""Tests for the IPython IOPub bridge."""

import json
import time
from unittest.mock import MagicMock, patch

import pytest
import zmq

from runtimed._ipython_bridge import (
    IPythonBridge,
    _TeeStream,
    _rewrite_widget_data,
    install_bridge,
)


class TestIPythonBridge:
    """Tests for the core IPythonBridge ZMQ infrastructure."""

    def test_bridge_creates_connection_file(self):
        bridge = IPythonBridge()
        try:
            assert bridge.connection_file.exists()
            info = json.loads(bridge.connection_file.read_text())
            assert info["transport"] == "tcp"
            assert info["signature_scheme"] == "hmac-sha256"
            assert info["kernel_name"] == "python3"
            for key in (
                "shell_port",
                "iopub_port",
                "stdin_port",
                "control_port",
                "hb_port",
            ):
                assert isinstance(info[key], int)
                assert info[key] > 0
            assert len(info["key"]) == 32  # uuid4 hex
        finally:
            bridge.close()

    def test_bridge_close_cleans_up(self):
        bridge = IPythonBridge()
        conn_file = bridge.connection_file
        bridge.close()
        assert not conn_file.exists()

    def test_bridge_iopub_publishes_messages(self):
        bridge = IPythonBridge()
        try:
            info = json.loads(bridge.connection_file.read_text())

            # Connect a SUB socket to the bridge's IOPub
            ctx = zmq.Context()
            sub = ctx.socket(zmq.SUB)
            sub.subscribe(b"")
            sub.connect(f"tcp://127.0.0.1:{info['iopub_port']}")
            time.sleep(0.1)  # Let ZMQ subscription propagate

            # Publish a message
            bridge.publish_stream("stdout", "hello world")

            # Receive and verify
            assert sub.poll(2000)
            parts = sub.recv_multipart()
            delim_idx = parts.index(b"<IDS|MSG>")
            header = json.loads(parts[delim_idx + 2])
            content = json.loads(parts[delim_idx + 5])

            assert header["msg_type"] == "stream"
            assert content["name"] == "stdout"
            assert content["text"] == "hello world"

            sub.close()
            ctx.term()
        finally:
            bridge.close()

    def test_bridge_shell_responds_to_kernel_info(self):
        bridge = IPythonBridge()
        try:
            info = json.loads(bridge.connection_file.read_text())

            ctx = zmq.Context()
            dealer = ctx.socket(zmq.DEALER)
            dealer.connect(f"tcp://127.0.0.1:{info['shell_port']}")

            # Build a kernel_info_request
            import hashlib
            import hmac as hmac_mod

            header = json.dumps(
                {
                    "msg_id": "test-123",
                    "msg_type": "kernel_info_request",
                    "username": "test",
                    "session": "test-session",
                    "date": "2025-01-01T00:00:00Z",
                    "version": "5.3",
                }
            ).encode()
            parent = b"{}"
            metadata = b"{}"
            content = b"{}"

            h = hmac_mod.new(info["key"].encode(), digestmod=hashlib.sha256)
            h.update(header)
            h.update(parent)
            h.update(metadata)
            h.update(content)
            sig = h.hexdigest().encode()

            dealer.send_multipart(
                [b"<IDS|MSG>", sig, header, parent, metadata, content]
            )

            # Wait for reply
            assert dealer.poll(3000)
            parts = dealer.recv_multipart()
            delim_idx = parts.index(b"<IDS|MSG>")
            reply_header = json.loads(parts[delim_idx + 2])
            reply_content = json.loads(parts[delim_idx + 5])

            assert reply_header["msg_type"] == "kernel_info_reply"
            assert reply_content["status"] == "ok"
            assert reply_content["language_info"]["name"] == "python"
            assert reply_content["implementation"] == "ipython-bridge"

            dealer.close()
            ctx.term()
        finally:
            bridge.close()

    def test_bridge_shell_responds_to_execute_request(self):
        bridge = IPythonBridge()
        try:
            info = json.loads(bridge.connection_file.read_text())

            ctx = zmq.Context()
            dealer = ctx.socket(zmq.DEALER)
            dealer.connect(f"tcp://127.0.0.1:{info['shell_port']}")

            import hashlib
            import hmac as hmac_mod

            header = json.dumps(
                {
                    "msg_id": "test-456",
                    "msg_type": "execute_request",
                    "username": "test",
                    "session": "test-session",
                    "date": "2025-01-01T00:00:00Z",
                    "version": "5.3",
                }
            ).encode()
            parent = b"{}"
            metadata = b"{}"
            content = json.dumps(
                {
                    "code": "",
                    "silent": True,
                    "store_history": False,
                    "user_expressions": {"cwd": "__import__('os').getcwd()"},
                    "allow_stdin": False,
                    "stop_on_error": False,
                }
            ).encode()

            h = hmac_mod.new(info["key"].encode(), digestmod=hashlib.sha256)
            h.update(header)
            h.update(parent)
            h.update(metadata)
            h.update(content)
            sig = h.hexdigest().encode()

            dealer.send_multipart(
                [b"<IDS|MSG>", sig, header, parent, metadata, content]
            )

            assert dealer.poll(3000)
            parts = dealer.recv_multipart()
            delim_idx = parts.index(b"<IDS|MSG>")
            reply_content = json.loads(parts[delim_idx + 5])

            assert reply_content["status"] == "ok"
            assert "cwd" in reply_content["user_expressions"]
            cwd_result = reply_content["user_expressions"]["cwd"]
            assert cwd_result["status"] == "ok"
            assert "text/plain" in cwd_result["data"]

            dealer.close()
            ctx.term()
        finally:
            bridge.close()

    def test_bridge_heartbeat(self):
        bridge = IPythonBridge()
        try:
            info = json.loads(bridge.connection_file.read_text())

            ctx = zmq.Context()
            req = ctx.socket(zmq.REQ)
            req.connect(f"tcp://127.0.0.1:{info['hb_port']}")

            req.send(b"ping")
            assert req.poll(2000)
            reply = req.recv()
            assert reply == b"ping"  # Heartbeat echoes the message back

            req.close()
            ctx.term()
        finally:
            bridge.close()

    def test_bridge_publish_execute_result(self):
        bridge = IPythonBridge()
        try:
            info = json.loads(bridge.connection_file.read_text())

            ctx = zmq.Context()
            sub = ctx.socket(zmq.SUB)
            sub.subscribe(b"")
            sub.connect(f"tcp://127.0.0.1:{info['iopub_port']}")
            time.sleep(0.1)

            bridge.publish_execute_result({"text/plain": "42"}, {}, execution_count=1)

            assert sub.poll(2000)
            parts = sub.recv_multipart()
            delim_idx = parts.index(b"<IDS|MSG>")
            header = json.loads(parts[delim_idx + 2])
            content = json.loads(parts[delim_idx + 5])

            assert header["msg_type"] == "execute_result"
            assert content["data"]["text/plain"] == "42"
            assert content["execution_count"] == 1

            sub.close()
            ctx.term()
        finally:
            bridge.close()

    def test_bridge_publish_error(self):
        bridge = IPythonBridge()
        try:
            info = json.loads(bridge.connection_file.read_text())

            ctx = zmq.Context()
            sub = ctx.socket(zmq.SUB)
            sub.subscribe(b"")
            sub.connect(f"tcp://127.0.0.1:{info['iopub_port']}")
            time.sleep(0.1)

            bridge.publish_error("ValueError", "bad value", ["traceback line 1"])

            assert sub.poll(2000)
            parts = sub.recv_multipart()
            delim_idx = parts.index(b"<IDS|MSG>")
            header = json.loads(parts[delim_idx + 2])
            content = json.loads(parts[delim_idx + 5])

            assert header["msg_type"] == "error"
            assert content["ename"] == "ValueError"
            assert content["evalue"] == "bad value"

            sub.close()
            ctx.term()
        finally:
            bridge.close()


class TestWidgetRewrite:
    """Tests for widget mime type rewriting."""

    def test_rewrites_widget_view_to_html(self):
        data = {
            "application/vnd.jupyter.widget-view+json": {
                "model_id": "abc123",
                "version_major": 2,
            },
            "text/plain": "IntSlider(value=5)",
        }
        result = _rewrite_widget_data(data)
        assert "application/vnd.jupyter.widget-view+json" not in result
        assert "text/html" in result
        assert "widget" in result["text/html"].lower()
        assert result["text/plain"] == "IntSlider(value=5)"

    def test_passes_through_non_widget_data(self):
        data = {"text/plain": "hello", "text/html": "<b>hello</b>"}
        result = _rewrite_widget_data(data)
        assert result is data  # Same object, not copied

    def test_adds_text_plain_fallback(self):
        data = {
            "application/vnd.jupyter.widget-view+json": {
                "model_id": "abc123",
            },
        }
        result = _rewrite_widget_data(data)
        assert "text/plain" in result

    def test_does_not_mutate_original(self):
        data = {
            "application/vnd.jupyter.widget-view+json": {"model_id": "x"},
            "text/plain": "Slider()",
        }
        _rewrite_widget_data(data)
        assert "application/vnd.jupyter.widget-view+json" in data


class TestTeeStream:
    """Tests for the stdout/stderr wrapper."""

    def test_tee_writes_to_both(self):
        bridge = MagicMock()
        bridge._in_displayhook = False
        original = MagicMock()
        original.write.return_value = 5
        stream = _TeeStream(original, "stdout", bridge)

        result = stream.write("hello")
        assert result == 5
        original.write.assert_called_once_with("hello")
        bridge.publish_stream.assert_called_once_with("stdout", "hello")

    def test_tee_suppresses_during_displayhook(self):
        bridge = MagicMock()
        bridge._in_displayhook = True
        original = MagicMock()
        original.write.return_value = 8
        stream = _TeeStream(original, "stdout", bridge)

        result = stream.write("Out[1]: 23")
        assert result == 8
        original.write.assert_called_once_with("Out[1]: 23")
        bridge.publish_stream.assert_not_called()

    def test_tee_skips_empty_writes(self):
        bridge = MagicMock()
        bridge._in_displayhook = False
        original = MagicMock()
        original.write.return_value = 0
        stream = _TeeStream(original, "stdout", bridge)

        stream.write("")
        bridge.publish_stream.assert_not_called()

    def test_tee_flush_delegates(self):
        bridge = MagicMock()
        original = MagicMock()
        stream = _TeeStream(original, "stderr", bridge)

        stream.flush()
        original.flush.assert_called_once()


class TestInstallBridge:
    """Tests for the install_bridge function."""

    def test_install_registers_hooks(self):
        ip = MagicMock()
        ip.display_formatter.format.return_value = ({"text/plain": "x"}, {})

        bridge = install_bridge(ip)
        try:
            ip.events.register.assert_called_once_with(
                "post_run_cell",
                bridge._post_run_cell
                if hasattr(bridge, "_post_run_cell")
                else ip.events.register.call_args[0][1],
            )
            assert bridge.connection_file.exists()
        finally:
            bridge.close()

    def test_install_wraps_stdout_stderr(self):
        import sys

        original_stdout = sys.stdout
        original_stderr = sys.stderr
        ip = MagicMock()

        bridge = install_bridge(ip)
        try:
            assert isinstance(sys.stdout, _TeeStream)
            assert isinstance(sys.stderr, _TeeStream)
        finally:
            bridge.close()
            sys.stdout = original_stdout
            sys.stderr = original_stderr

    def test_install_wraps_display_publisher(self):
        ip = MagicMock()
        original_publish = ip.display_pub.publish

        bridge = install_bridge(ip)
        try:
            # display_pub.publish should have been replaced
            assert ip.display_pub.publish is not original_publish
        finally:
            bridge.close()
