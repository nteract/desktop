"""NotebookInfo — structured metadata for active notebook rooms."""

from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from runtimed._notebook import Notebook


@dataclass
class NotebookInfo:
    """Read-only metadata about an active notebook room.

    Returned by ``Client.list_active_notebooks()``. Inspect before joining.
    """

    notebook_id: str
    kernel_type: str | None = None
    kernel_status: str | None = None
    active_peers: int = 0
    has_kernel: bool = False
    env_source: str | None = None

    @property
    def name(self) -> str:
        """Path stem for file-backed notebooks, short ID for ephemeral."""
        p = self.path
        return p.stem if p else self.notebook_id[:8]

    @property
    def path(self) -> Path | None:
        """Filesystem path, or None for ephemeral notebooks."""
        if "/" in self.notebook_id:
            return Path(self.notebook_id)
        return None

    @property
    def is_ephemeral(self) -> bool:
        """True if this notebook has no backing file."""
        return self.path is None

    async def join(self, client: Any, peer_label: str | None = None) -> Notebook:
        """Join this room and return a Notebook."""
        return await client.join(self.notebook_id, peer_label=peer_label)

    @classmethod
    def _from_dict(cls, d: dict[str, Any]) -> NotebookInfo:
        """Construct from the dict returned by NativeAsyncClient.list_active_notebooks()."""
        return cls(
            notebook_id=d["notebook_id"],
            kernel_type=d.get("kernel_type"),
            kernel_status=d.get("kernel_status"),
            active_peers=int(d.get("active_peers", 0)),
            has_kernel=bool(d.get("has_kernel", False)),
            env_source=d.get("env_source"),
        )

    def __repr__(self) -> str:
        status = f" [{self.kernel_status}]" if self.kernel_status else ""
        peers = f" ({self.active_peers} peers)" if self.active_peers else ""
        return f"NotebookInfo({self.name!r}{status}{peers})"
