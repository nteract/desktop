"""Comm client for the ``nteract.dx.blob`` target.

Handles request/response multiplexing by ``req_id`` and timeout behavior.
The underlying :class:`ipykernel.comm.Comm` is injected so this module is
testable without any Jupyter runtime.
"""

from __future__ import annotations

import threading
import uuid
from dataclasses import dataclass, field

from dx._refs import BlobRef

COMM_TARGET = "nteract.dx.blob"


@dataclass
class _Pending:
    event: threading.Event = field(default_factory=threading.Event)
    response: dict | None = None


class BlobClient:
    """Sends ``op: put`` comm_msgs and awaits ``op: ack`` responses by ``req_id``."""

    def __init__(self, comm, default_timeout: float = 30.0) -> None:
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
        timeout: float | None = None,
    ) -> BlobRef:
        req_id = str(uuid.uuid4())
        pending = _Pending()
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
            raise DxError(f"runtime agent error ({code}): {message}")

        from dx import DxError

        raise DxError(f"unexpected response op: {op!r}")

    def _handle_msg(self, msg: dict) -> None:
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
    """Used when no runtime agent is reachable. Every ``put`` raises :class:`DxNoAgentError`."""

    def put(
        self,
        data: bytes,
        content_type: str,
        *,
        blob_base_url: str,
        timeout: float | None = None,
    ) -> BlobRef:
        from dx import DxNoAgentError

        raise DxNoAgentError("nteract.dx.blob comm is not open")


_client: object | None = None
_blob_base_url: str = "http://127.0.0.1"


def set_client(client, *, blob_base_url: str) -> None:
    """Install the module-level client. Used by :func:`dx.install`."""
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
