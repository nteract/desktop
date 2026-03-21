"""Notebook — high-level wrapper around AsyncSession."""

from __future__ import annotations

from typing import TYPE_CHECKING

from runtimed._cell import CellCollection

if TYPE_CHECKING:
    from runtimed.runtimed import AsyncSession


class Notebook:
    """A connected notebook with sync reads and async writes.

    Created by ``Client.open()``, ``Client.create()``, or ``Client.join()``.

    Properties read from the local Automerge replica (sync).
    Mutation methods are async (synced to peers).
    """

    __slots__ = ("_session", "_cells")

    def __init__(self, async_session: AsyncSession) -> None:
        self._session = async_session
        self._cells: CellCollection | None = None

    @property
    def notebook_id(self) -> str:
        return self._session.notebook_id

    @property
    def cells(self) -> CellCollection:
        """The cell collection (sync reads, async writes)."""
        if self._cells is None:
            self._cells = CellCollection(self._session)
        return self._cells

    @property
    def runtime(self):
        """Runtime state snapshot (sync read from local RuntimeStateDoc)."""
        return self._session.get_runtime_state_sync()

    @property
    def peers(self) -> list[tuple[str, str]]:
        """Connected peers as (peer_id, peer_label) tuples (sync read)."""
        return self._session.get_peers_sync()

    # ── Async operations ─────────────────────────────────────────────

    async def save(self, path: str | None = None) -> str:
        """Save the notebook to disk. Returns the path saved to."""
        return await self._session.save(path)

    async def start_kernel(
        self,
        kernel_type: str = "python",
        env_source: str = "auto",
        notebook_path: str | None = None,
    ) -> None:
        """Start a kernel in this notebook.

        Args:
            kernel_type: Kernel runtime type (e.g. "python", "deno").
            env_source: Environment source (e.g. "auto", "uv:inline").
            notebook_path: Optional path for project file detection.
        """
        await self._session.start_kernel(kernel_type, env_source, notebook_path)

    async def shutdown_kernel(self) -> None:
        """Shut down the kernel (daemon manages cleanup on last peer disconnect)."""
        await self._session.shutdown_kernel()

    async def restart_kernel(self, wait_for_ready: bool = True) -> list[str]:
        """Restart the kernel. Returns progress messages."""
        return await self._session.restart_kernel(wait_for_ready)

    async def interrupt(self) -> None:
        """Interrupt the currently executing cell."""
        await self._session.interrupt()

    async def close(self) -> None:
        """Close the notebook session."""
        await self._session.close()

    @property
    def session(self) -> AsyncSession:
        """The underlying AsyncSession (for advanced/direct use)."""
        return self._session

    # ── Context manager ──────────────────────────────────────────────

    async def __aenter__(self) -> Notebook:
        return self

    async def __aexit__(self, *args) -> None:
        await self.close()

    def __repr__(self) -> str:
        return f"Notebook({self.notebook_id[:12]})"
