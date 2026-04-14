import threading
import time

import pytest
from dx import BlobRef, DxNoAgentError, DxPayloadTooLargeError, DxTimeoutError
from dx._comm import BlobClient, FallbackClient


class FakeComm:
    """Stand-in for ipykernel.comm.Comm."""

    def __init__(self) -> None:
        self.sent: list[tuple[dict, list[bytes]]] = []
        self._handler = None
        self.closed = False

    def on_msg(self, handler):
        self._handler = handler

    def send(self, data, buffers=None):
        self.sent.append((data, list(buffers or [])))

    def close(self):
        self.closed = True

    # Test helper: simulate an incoming message from the agent.
    def incoming(self, data, buffers=None):
        assert self._handler is not None
        self._handler({"content": {"data": data}, "buffers": buffers or []})


def _wait_for_send(comm: FakeComm, timeout: float = 2.0) -> None:
    start = time.monotonic()
    while not comm.sent:
        if time.monotonic() - start > timeout:
            raise AssertionError("comm.send was never called")
        time.sleep(0.001)


def test_put_blob_sends_comm_msg_with_buffer():
    comm = FakeComm()
    client = BlobClient(comm, default_timeout=5.0)

    def ack_on_send():
        _wait_for_send(comm)
        req_id = comm.sent[0][0]["req_id"]
        comm.incoming(
            {
                "op": "ack",
                "req_id": req_id,
                "hash": "sha256:abc",
                "size": 3,
            }
        )

    t = threading.Thread(target=ack_on_send, daemon=True)
    t.start()

    ref = client.put(b"abc", "image/png", blob_base_url="http://localhost:9999")
    t.join(timeout=2)
    assert isinstance(ref, BlobRef)
    assert ref.hash == "sha256:abc"
    assert ref.size == 3
    assert ref.url == "http://localhost:9999/blob/sha256:abc"

    data, buffers = comm.sent[0]
    assert data["op"] == "put"
    assert data["content_type"] == "image/png"
    assert len(buffers) == 1 and buffers[0] == b"abc"


def test_put_blob_timeout_raises():
    comm = FakeComm()
    client = BlobClient(comm, default_timeout=0.05)
    with pytest.raises(DxTimeoutError):
        client.put(b"abc", "image/png", blob_base_url="http://localhost:9999")


def test_put_blob_error_response_too_large():
    comm = FakeComm()
    client = BlobClient(comm, default_timeout=2.0)

    def err_on_send():
        _wait_for_send(comm)
        req_id = comm.sent[0][0]["req_id"]
        comm.incoming(
            {
                "op": "err",
                "req_id": req_id,
                "code": "too_large",
                "message": "exceeds MAX_BLOB_SIZE",
            }
        )

    threading.Thread(target=err_on_send, daemon=True).start()

    with pytest.raises(DxPayloadTooLargeError):
        client.put(b"abc", "image/png", blob_base_url="http://localhost:9999")


def test_put_blob_stale_req_id_ignored():
    """A late ack for a timed-out request should not break subsequent calls."""
    comm = FakeComm()
    client = BlobClient(comm, default_timeout=0.05)

    with pytest.raises(DxTimeoutError):
        client.put(b"abc", "image/png", blob_base_url="http://x")

    # Now deliver the stale ack — should be ignored silently.
    req_id = comm.sent[0][0]["req_id"]
    comm.incoming({"op": "ack", "req_id": req_id, "hash": "sha256:abc", "size": 3})

    # And a fresh call should still work.
    def ack_on_second():
        _wait_for_send(comm, timeout=2.0)
        while len(comm.sent) < 2:
            time.sleep(0.001)
        req_id2 = comm.sent[1][0]["req_id"]
        comm.incoming({"op": "ack", "req_id": req_id2, "hash": "sha256:def", "size": 3})

    client2 = BlobClient(comm, default_timeout=2.0)
    threading.Thread(target=ack_on_second, daemon=True).start()
    ref = client2.put(b"def", "image/png", blob_base_url="http://x")
    assert ref.hash == "sha256:def"


def test_fallback_client_raises_no_agent():
    client = FallbackClient()
    with pytest.raises(DxNoAgentError):
        client.put(b"abc", "image/png", blob_base_url="http://localhost:9999")
