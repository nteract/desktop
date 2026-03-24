"""CellHandle and CellCollection — the cells API for Notebook."""

from __future__ import annotations

import json
from collections.abc import AsyncIterator
from typing import TYPE_CHECKING, Any

from runtimed.runtimed import RuntimedError as _RuntimedError

if TYPE_CHECKING:
    from runtimed.runtimed import (
        AsyncSession,
        Cell,
        ExecutionEvent,
        ExecutionResult,
        Output,
    )


class CellHandle:
    """A live reference to a cell in the notebook document.

    Reads are sync (local Automerge replica). Writes are async (synced to peers).
    """

    __slots__ = ("_id", "_session")

    def __init__(self, cell_id: str, session: AsyncSession) -> None:
        self._id = cell_id
        self._session = session

    @property
    def id(self) -> str:
        return self._id

    @property
    def source(self) -> str:
        """Cell source text (sync read from local doc)."""
        return self._session.get_cell_source_sync(self._id) or ""

    @property
    def cell_type(self) -> str:
        """Cell type: 'code', 'markdown', or 'raw' (sync read)."""
        return self._session.get_cell_type_sync(self._id) or "code"

    @property
    def outputs(self) -> list[Output]:
        """Resolved outputs (sync — may do disk I/O for blob resolution)."""
        try:
            cell = self._session.get_cell_sync(self._id)
            return cell.outputs
        except _RuntimedError:
            return []

    @property
    def execution_count(self) -> int | None:
        """Execution count, or None if never executed (sync read)."""
        raw = self._session.get_cell_execution_count_sync(self._id)
        if raw is None:
            return None
        try:
            return int(raw)
        except (ValueError, TypeError):
            return None

    @property
    def metadata(self) -> Any:
        """Parsed metadata dict (sync read)."""
        raw = self._session.get_cell_metadata_sync(self._id)
        if raw is None:
            return {}
        try:
            return json.loads(raw)
        except (json.JSONDecodeError, TypeError):
            return {}

    @property
    def tags(self) -> list[str]:
        """Cell tags (sync read). Uses Rust Cell helpers for key resolution."""
        try:
            return self._session.get_cell_sync(self._id).tags
        except _RuntimedError:
            return []

    @property
    def source_hidden(self) -> bool:
        """Whether cell source is hidden (sync read). Uses Rust Cell helpers."""
        try:
            return self._session.get_cell_sync(self._id).is_source_hidden
        except _RuntimedError:
            return False

    @property
    def outputs_hidden(self) -> bool:
        """Whether cell outputs are hidden (sync read). Uses Rust Cell helpers."""
        try:
            return self._session.get_cell_sync(self._id).is_outputs_hidden
        except _RuntimedError:
            return False

    def snapshot(self) -> Cell:
        """Return the full Cell object (sync — includes resolved outputs)."""
        return self._session.get_cell_sync(self._id)

    # ── Async mutations ──────────────────────────────────────────────

    async def set_source(self, source: str) -> CellHandle:
        """Replace the cell's source text."""
        await self._session.set_source(self._id, source)
        return self

    async def append(self, text: str) -> CellHandle:
        """Append text to the cell's source."""
        await self._session.append_source(self._id, text)
        return self

    async def splice(self, index: int, delete_count: int, text: str = "") -> CellHandle:
        """Splice text at a character position (no diff overhead)."""
        await self._session.splice_source(self._id, index, delete_count, text)
        return self

    async def set_type(self, cell_type: str) -> CellHandle:
        """Change cell type ('code', 'markdown', 'raw')."""
        await self._session.set_cell_type(self._id, cell_type)
        return self

    async def run(self, timeout_secs: float = 60.0) -> ExecutionResult:
        """Execute this cell and wait for results."""
        return await self._session.execute_cell(self._id, timeout_secs)

    async def queue(self) -> str:
        """Queue this cell for execution without waiting.

        Returns the execution_id (UUID) for this execution.
        """
        return await self._session.queue_cell(self._id)

    async def delete(self) -> None:
        """Delete this cell from the document."""
        await self._session.delete_cell(self._id)

    async def move_after(self, other: CellHandle | None = None) -> CellHandle:
        """Move this cell after another cell (or to the beginning if None)."""
        after_id = other._id if other else None
        await self._session.move_cell(self._id, after_id)
        return self

    async def clear_outputs(self) -> CellHandle:
        """Clear this cell's outputs."""
        await self._session.clear_outputs(self._id)
        return self

    async def set_tags(self, tags: list[str]) -> CellHandle:
        """Set the cell's tags."""
        await self._session.set_cell_tags(self._id, tags)
        return self

    async def set_source_hidden(self, hidden: bool) -> CellHandle:
        """Show or hide the cell's source."""
        await self._session.set_cell_source_hidden(self._id, hidden)
        return self

    async def set_outputs_hidden(self, hidden: bool) -> CellHandle:
        """Show or hide the cell's outputs."""
        await self._session.set_cell_outputs_hidden(self._id, hidden)
        return self

    async def stream(
        self,
        timeout_secs: float = 60.0,
        signal_only: bool = False,
    ) -> AsyncIterator[ExecutionEvent]:
        """Execute and stream events as an async iterator.

        Yields ``ExecutionEvent`` objects until execution completes.
        Use ``signal_only=True`` to receive only start/done signals
        without output payloads.

        Events are automatically scoped to the execution triggered by
        this call — concurrent or subsequent executions of the same cell
        will not leak into this stream.
        """
        return await self._session.stream_execute(
            self._id,
            timeout_secs,
            signal_only,
        )

    def __repr__(self) -> str:
        return f"Cell({self._id[:8]}, {self.cell_type})"


class CellCollection:
    """The cells in a notebook. Sync reads, async mutations.

    Access via ``notebook.cells``.
    """

    __slots__ = ("_session",)

    def __init__(self, session: AsyncSession) -> None:
        self._session = session

    def _handle(self, cell_id: str) -> CellHandle:
        return CellHandle(cell_id, self._session)

    # ── Sync reads ───────────────────────────────────────────────────

    def get_by_id(self, cell_id: str) -> CellHandle:
        """Get a cell by its exact ID (sync)."""
        ids = self._session.get_cell_ids_sync()
        if cell_id not in ids:
            raise KeyError(f"No cell with ID {cell_id!r}")
        return self._handle(cell_id)

    def get_by_index(self, index: int) -> CellHandle:
        """Get a cell by position (sync, supports negative indexing)."""
        ids = self._session.get_cell_ids_sync()
        return self._handle(ids[index])

    def find(self, substring: str) -> list[CellHandle]:
        """Find cells whose source contains a substring (sync)."""
        result = []
        for cell_id in self._session.get_cell_ids_sync():
            source = self._session.get_cell_source_sync(cell_id) or ""
            if substring in source:
                result.append(self._handle(cell_id))
        return result

    @property
    def ids(self) -> list[str]:
        """All cell IDs in document order (sync)."""
        return self._session.get_cell_ids_sync()

    def __getitem__(self, cell_id: str) -> CellHandle:
        """cells['cell-id'] — sugar for get_by_id."""
        if not isinstance(cell_id, str):
            raise TypeError(f"Cell access requires a string ID, got {type(cell_id).__name__}")
        return self.get_by_id(cell_id)

    def __iter__(self):
        for cell_id in self._session.get_cell_ids_sync():
            yield self._handle(cell_id)

    def __len__(self) -> int:
        return len(self._session.get_cell_ids_sync())

    def __contains__(self, cell_id: str) -> bool:
        return cell_id in self._session.get_cell_ids_sync()

    # ── Async mutations ──────────────────────────────────────────────

    async def create(
        self,
        source: str = "",
        cell_type: str = "code",
    ) -> CellHandle:
        """Create a new cell at the end of the document."""
        cell_id = await self._session.create_cell(source, cell_type)
        return self._handle(cell_id)

    async def insert_at(
        self,
        index: int,
        source: str = "",
        cell_type: str = "code",
    ) -> CellHandle:
        """Insert a new cell at a specific position."""
        cell_id = await self._session.create_cell(source, cell_type, index)
        return self._handle(cell_id)

    def __repr__(self) -> str:
        return f"Cells({len(self)})"
