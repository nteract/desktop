"""Client — the primary entry point for the runtimed Python API."""

from __future__ import annotations

from runtimed._internals import NativeAsyncClient
from runtimed._notebook import Notebook
from runtimed._notebook_info import NotebookInfo


class Client:
    """Async client for the runtimed daemon.

    Primary entry point for the runtimed Python API. Returns ``Notebook``
    objects with sync reads and async writes.

    Example::

        async with Client() as client:
            notebook = await client.create_notebook()
            cell = await notebook.cells.create("print('hello')")
            print(cell.source)   # sync read
            result = await cell.run()  # async
    """

    def __init__(
        self,
        socket_path: str | None = None,
        peer_label: str | None = None,
    ) -> None:
        self._native = NativeAsyncClient(socket_path, peer_label)

    async def list_active_notebooks(self) -> list[NotebookInfo]:
        """List all active notebook rooms on the daemon."""
        raw = await self._native.list_active_notebooks()
        return [NotebookInfo._from_dict(d) for d in raw]

    async def open_notebook(self, path: str, peer_label: str | None = None) -> Notebook:
        """Open an existing notebook file and return a connected Notebook."""
        session = await self._native.open_notebook(path, peer_label)
        return Notebook(session)

    async def create_notebook(
        self,
        runtime: str = "python",
        working_dir: str | None = None,
        peer_label: str | None = None,
        dependencies: list[str] | None = None,
    ) -> Notebook:
        """Create a new notebook and return a connected Notebook.

        If *dependencies* are provided they are written to the notebook
        metadata in a single bulk operation.  The environment is **not**
        synced automatically — call ``sync_environment()`` or ``restart()``
        on the returned notebook to apply them.
        """
        session = await self._native.create_notebook(runtime, working_dir, peer_label)
        notebook = Notebook(session)
        if dependencies:
            await notebook.add_dependencies(dependencies)
        return notebook

    async def join_notebook(self, notebook_id: str, peer_label: str | None = None) -> Notebook:
        """Join an existing notebook room by ID.

        Relative paths (e.g. ``"notebook.ipynb"``) are resolved to absolute
        paths so they match the canonical room keys used by the daemon.
        """
        session = await self._native.join_notebook(notebook_id, peer_label)
        return Notebook(session)

    async def ping(self) -> bool:
        """Check if the daemon is alive."""
        return await self._native.ping()

    async def is_running(self) -> bool:
        """Check if the daemon is running."""
        return await self._native.is_running()

    async def status(self) -> dict:
        """Get daemon pool statistics."""
        return await self._native.status()

    async def flush_pool(self) -> None:
        """Flush prewarmed environment pool."""
        await self._native.flush_pool()

    async def close(self) -> None:
        """Close the client connection.

        Releases local resources without affecting the daemon.
        """

    async def _shutdown_daemon(self) -> None:
        """Request the daemon process to shut down.

        Stops the *entire* daemon, disconnecting all peers and notebooks.
        You almost certainly want ``close()`` instead.
        """
        await self._native.shutdown()

    async def __aenter__(self) -> Client:
        return self

    async def __aexit__(self, *exc: object) -> None:
        await self.close()

    def _repr_markdown_(self) -> str:
        return (
            "**Client** — async interface to the runtimed daemon\n\n"
            "| Async methods |\n"
            "|-|\n"
            "| `open_notebook()` `create_notebook()` `join_notebook()` |\n"
            "| `list_active_notebooks()` |\n"
            "| `ping()` `is_running()` `status()` |\n"
            "| `flush_pool()` `close()` |\n"
        )

    def __repr__(self) -> str:
        return "Client()"
