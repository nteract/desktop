"""Presence — cursor, selection, and focus tracking for Notebook."""

from __future__ import annotations

from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from runtimed._internals import AsyncSession


class Presence:
    """Presence operations for a notebook.

    Access via ``notebook.presence``. All operations are async
    (synced to peers via the daemon).

    Presence is best-effort — operations silently succeed even if
    the daemon hasn't fully connected yet. This matches the expectation
    that cursor/selection state is ephemeral and non-critical.
    """

    __slots__ = ("_session",)

    def __init__(self, session: AsyncSession) -> None:
        self._session = session

    # ── Write operations ─────────────────────────────────────────────

    async def set_cursor(self, cell_id: str, line: int = 0, column: int = 0) -> None:
        """Set cursor position in a cell."""
        await self._session.set_cursor(cell_id=cell_id, line=line, column=column)

    async def set_selection(
        self,
        cell_id: str,
        anchor_line: int,
        anchor_col: int,
        head_line: int,
        head_col: int,
    ) -> None:
        """Set selection range in a cell."""
        await self._session.set_selection(
            cell_id=cell_id,
            anchor_line=anchor_line,
            anchor_col=anchor_col,
            head_line=head_line,
            head_col=head_col,
        )

    async def focus(self, cell_id: str) -> None:
        """Set focus on a cell (without a specific cursor position)."""
        await self._session.set_focus(cell_id=cell_id)

    async def clear_cursor(self) -> None:
        """Clear cursor presence."""
        await self._session.clear_cursor()

    async def clear_selection(self) -> None:
        """Clear selection presence."""
        await self._session.clear_selection()

    # ── Read operations ──────────────────────────────────────────────

    async def get_remote_cursors(self) -> list[tuple[str, str, str, int, int]]:
        """Get cursor positions from other connected peers.

        Returns a list of (peer_id, peer_label, cell_id, line, column) tuples.
        """
        return await self._session.get_remote_cursors()

    def __repr__(self) -> str:
        return "Presence()"
