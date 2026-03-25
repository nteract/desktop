"""Notebook — high-level wrapper around AsyncSession."""

from __future__ import annotations

from typing import TYPE_CHECKING

from runtimed._cell import CellCollection, _HintList
from runtimed._presence import Presence

if TYPE_CHECKING:
    from runtimed._internals import (
        AsyncSession,
        SyncEnvironmentResult,
    )


class Notebook:
    """A connected notebook backed by a local Automerge CRDT replica.

    Created by ``Client.open_notebook()``, ``Client.create_notebook()``,
    or ``Client.join_notebook()``.

    Properties read directly from the local replica and return instantly.
    Methods go through the daemon to mutate the document or manage the
    runtime, so they must be awaited.
    """

    __slots__ = ("_session", "_cells", "_presence")

    def __init__(self, async_session: AsyncSession) -> None:
        self._session = async_session
        self._cells: CellCollection | None = None
        self._presence: Presence | None = None

    @property
    def notebook_id(self) -> str:
        """File path or UUID identifying this notebook."""
        return self._session.notebook_id

    @property
    def cells(self) -> CellCollection:
        """Cells in this notebook. Iterate, index, or search without awaiting."""
        if self._cells is None:
            self._cells = CellCollection(self._session)
        return self._cells

    @property
    def presence(self) -> Presence:
        """Presence operations (cursor, selection, focus)."""
        if self._presence is None:
            self._presence = Presence(self._session)
        return self._presence

    @property
    def runtime(self):
        """Current runtime state read from the local replica.

        Returns a ``RuntimeState`` with ``.kernel``, ``.queue``, ``.env``,
        and ``.executions`` — useful for polling kernel status or queue depth.
        """
        return self._session.get_runtime_state_sync()

    @property
    def peers(self) -> list[tuple[str, str]]:
        """Connected peers as ``(peer_id, peer_label)`` tuples, read from the local replica."""
        return _HintList(self._session.get_peers_sync(), "peers")

    @property
    def is_connected(self) -> bool:
        """Whether the session is connected to the daemon."""
        return self._session.is_connected_sync()

    # ── Async operations ─────────────────────────────────────────────

    async def save(self) -> str:
        """Save the notebook to its current path. Returns the path saved to."""
        return await self._session.save(None)

    async def save_as(self, path: str) -> str:
        """Save the notebook to a new path. Returns the path saved to."""
        return await self._session.save(path)

    async def start(
        self,
        runtime: str = "python",
        env_source: str = "auto",
        notebook_path: str | None = None,
    ) -> None:
        """Start a runtime for this notebook.

        Args:
            runtime: Runtime type (e.g. "python", "deno").
            env_source: Environment source (e.g. "auto", "uv:inline").
            notebook_path: Optional path for project file detection.
        """
        await self._session.start_kernel(runtime, env_source, notebook_path)

    async def stop_runtime(self) -> None:
        """Shut down the kernel. The notebook session stays connected."""
        await self._session.shutdown_kernel()

    async def restart(self, wait_for_ready: bool = True) -> list[str]:
        """Restart the runtime. Returns progress messages."""
        return await self._session.restart_kernel(wait_for_ready)

    async def interrupt(self) -> None:
        """Interrupt the currently executing cell."""
        await self._session.interrupt()

    async def run_all(self) -> int:
        """Queue every code cell for execution. Returns the number queued."""
        return await self._session.run_all_cells()

    async def disconnect(self) -> None:
        """Disconnect from the notebook session."""
        await self._session.close()

    # ── Dependency management ────────────────────────────────────────

    async def _package_manager(self) -> str:
        """Auto-detect the package manager (uv or conda)."""
        env = await self._session.env_source()
        if env:
            return "conda" if env.startswith("conda:") else "uv"
        env_type = await self._session.get_metadata_env_type()
        if env_type:
            return env_type
        settings = self._session.get_settings()
        if settings:
            return settings.get("default_python_env", "uv")
        return "uv"

    async def add_dependency(self, package: str) -> list[str]:
        """Add a package dependency and return the updated list."""
        pm = await self._package_manager()
        if pm == "conda":
            await self._session.add_conda_dependency(package)
            return await self._session.get_conda_dependencies()
        else:
            await self._session.add_uv_dependency(package)
            return await self._session.get_uv_dependencies()

    async def add_dependencies(self, packages: list[str]) -> list[str]:
        """Add multiple dependencies at once and return the updated list."""
        pm = await self._package_manager()
        if pm == "conda":
            await self._session.add_conda_dependencies(packages)
            return await self._session.get_conda_dependencies()
        else:
            await self._session.add_uv_dependencies(packages)
            return await self._session.get_uv_dependencies()

    async def remove_dependency(self, package: str) -> list[str]:
        """Remove a package dependency and return the updated list."""
        pm = await self._package_manager()
        if pm == "conda":
            await self._session.remove_conda_dependency(package)
            return await self._session.get_conda_dependencies()
        else:
            await self._session.remove_uv_dependency(package)
            return await self._session.get_uv_dependencies()

    async def get_dependencies(self) -> list[str]:
        """Get current package dependencies."""
        pm = await self._package_manager()
        if pm == "conda":
            return await self._session.get_conda_dependencies()
        else:
            return await self._session.get_uv_dependencies()

    async def sync_environment(self) -> SyncEnvironmentResult:
        """Install pending dependency changes into the running kernel."""
        return await self._session.sync_environment()

    # ── Context manager ──────────────────────────────────────────────

    async def __aenter__(self) -> Notebook:
        return self

    async def __aexit__(self, *args) -> None:
        await self.disconnect()

    def _repr_markdown_(self) -> str:
        nid = self.notebook_id[:12]
        n_cells = len(self.cells)
        peers = self.peers
        return (
            f"**Notebook** `{nid}` — "
            f"{n_cells} cell{'s' if n_cells != 1 else ''}, "
            f"{len(peers)} peer{'s' if len(peers) != 1 else ''}\n\n"
            "| Properties (sync) | Async methods |\n"
            "|-|-|\n"
            "| `cells` `peers` | `save()` `save_as()` |\n"
            "| `presence` `runtime` | `start()` `stop_runtime()` `restart()` |\n"
            "| `notebook_id` `is_connected` | `interrupt()` `run_all()` |\n"
            "| | `add_dependency()` `remove_dependency()` |\n"
            "| | `get_dependencies()` `sync_environment()` |\n"
            "| | `disconnect()` |\n"
        )

    def __repr__(self) -> str:
        return f"Notebook({self.notebook_id[:12]})"
