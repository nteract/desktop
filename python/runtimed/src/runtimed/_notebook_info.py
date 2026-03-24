"""NotebookInfo — structured metadata for active notebook rooms."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from runtimed._client import Client
    from runtimed._notebook import Notebook


@dataclass
class NotebookInfo:
    """Read-only metadata about an active notebook room.

    Returned by ``Client.list_active_notebooks()``. Inspect before joining.
    """

    notebook_id: str
    runtime_type: str | None = None
    status: str | None = None
    active_peers: int = 0
    has_runtime: bool = False
    env_source: str | None = None
    had_peers: bool = False

    @property
    def is_draining(self) -> bool:
        """True if the room previously had peers and is now in keep-alive countdown."""
        return self.active_peers == 0 and self.had_peers

    @property
    def name(self) -> str:
        """Path stem for file-backed notebooks, short ID for ephemeral."""
        p = self.path
        return p.stem if p else self.notebook_id[:8]

    @property
    def path(self) -> Path | None:
        """Filesystem path, or None for ephemeral notebooks."""
        p = Path(self.notebook_id)
        if p.is_absolute():
            return p
        return None

    @property
    def is_ephemeral(self) -> bool:
        """True if this notebook has no backing file."""
        return self.path is None

    async def join(self, client: Client, peer_label: str | None = None) -> Notebook:
        """Join this room and return a Notebook."""
        return await client.join_notebook(self.notebook_id, peer_label=peer_label)

    @classmethod
    def _from_dict(cls, d: dict[str, Any]) -> NotebookInfo:
        """Construct from the dict returned by NativeAsyncClient.list_active_notebooks()."""
        return cls(
            notebook_id=d["notebook_id"],
            runtime_type=d.get("kernel_type"),
            status=d.get("kernel_status"),
            active_peers=int(d.get("active_peers", 0)),
            has_runtime=bool(d.get("has_kernel", False)),
            env_source=d.get("env_source"),
            had_peers=bool(d.get("had_peers", False)),
        )

    def __repr__(self) -> str:
        status_str = f" [{self.status}]" if self.status else ""
        peers = f" ({self.active_peers} peers)" if self.active_peers else ""
        return f"NotebookInfo({self.name!r}{status_str}{peers})"
