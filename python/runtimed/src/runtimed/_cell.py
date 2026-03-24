"""CellHandle and CellCollection — the cells API for Notebook."""

from __future__ import annotations

import json
from typing import TYPE_CHECKING, Any

from runtimed._execution import Execution
from runtimed.runtimed import RuntimedError as _RuntimedError

if TYPE_CHECKING:
    from runtimed.runtimed import (
        AsyncSession,
        Cell,
        ExecutionResult,
        Output,
    )


class _HintList(list):
    """List that gives a helpful error if accidentally called."""

    __slots__ = ("_attr",)

    def __init__(self, items, attr: str):
        super().__init__(items)
        self._attr = attr

    def __call__(self, *a, **kw):
        raise TypeError(
            f"'{self._attr}' is a property, not a method — drop the parentheses: .{self._attr}"
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
            return _HintList(cell.outputs, "outputs")
        except _RuntimedError:
            return _HintList([], "outputs")

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
            return _HintList(self._session.get_cell_sync(self._id).tags, "tags")
        except _RuntimedError:
            return _HintList([], "tags")

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

    async def execute(self) -> Execution:
        """Execute this cell and return an Execution handle.

        The handle provides execution-scoped access to status, results,
        and streaming events. Use it to wait, stream, or check status::

            execution = await cell.execute()
            print(execution.status)           # "queued" | "running" | ...
            result = await execution.result()  # wait for completion
            # or: result = await execution     # shorthand

        Returns:
            Execution handle for this specific execution.
        """
        execution_id = await self._session.queue_cell(self._id)
        return Execution(self._session, self._id, execution_id)

    async def run(self, timeout_secs: float = 60.0) -> ExecutionResult:
        """Execute this cell and wait for results.

        Sugar for ``(await cell.execute()).result(timeout_secs)``.
        """
        execution = await self.execute()
        return await execution.result(timeout_secs)

    async def queue(self) -> Execution:
        """Queue this cell for execution without waiting.

        Returns an Execution handle. Unlike :meth:`run`, this returns
        immediately — use the handle to check status or wait later.
        """
        return await self.execute()

    async def delete(self) -> None:
        """Delete this cell from the document."""
        await self._session.delete_cell(self._id)

    async def move_after(self, other: CellHandle | str | None = None) -> CellHandle:
        """Move this cell after another cell (or to the beginning if None)."""
        if isinstance(other, str):
            after_id = other
        elif other is not None:
            after_id = other._id
        else:
            after_id = None
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

    def _repr_markdown_(self) -> str:
        src = self.source
        preview = (src[:60] + "...") if len(src) > 60 else src
        preview = preview.replace("\n", "\\n")
        # Use an indented code block — immune to backticks in source
        indented = "    " + preview
        return (
            f"**Cell** `{self._id[:8]}` ({self.cell_type})\n\n"
            f"{indented}\n\n"
            "| Properties (sync) | Async methods |\n"
            "|-|-|\n"
            "| `source` | `set_source()` `append()` `splice()` |\n"
            "| `cell_type` | `set_type()` |\n"
            "| `outputs` | `execute()` `run()` `queue()` `clear_outputs()` |\n"
            "| `execution_count` | `delete()` `move_after()` |\n"
            "| `metadata` `tags` | `set_tags()` |\n"
            "| `source_hidden` `outputs_hidden` | "
            "`set_source_hidden()` `set_outputs_hidden()` |\n"
            "| `id` `snapshot()` | |\n"
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
        return _HintList(self._session.get_cell_ids_sync(), "ids")

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

    def _repr_markdown_(self) -> str:
        n = len(self)
        lines = [
            f"**Cells** ({n} cell{'s' if n != 1 else ''})\n",
            "| Properties / sync methods | Async methods |",
            "|-|-|",
            "| `ids` `len()` `iter()` | `create()` `insert_at()` |",
            "| `get_by_id()` `get_by_index()` `find()` | |",
            "| `cells['id']` `'id' in cells` | |",
        ]
        return "\n".join(lines) + "\n"

    def __repr__(self) -> str:
        return f"Cells({len(self)})"
