"""Execution handle — tracks a single cell execution by execution_id.

Created by ``cell.execute()`` or ``notebook.execute(cell_id)``.
Reads status from the RuntimeStateDoc's ``executions`` map.
Delegates to existing session methods for waiting and streaming.
"""

from __future__ import annotations

import asyncio
from typing import TYPE_CHECKING, Any

if TYPE_CHECKING:
    from runtimed._internals import (
        AsyncSession,
        ExecutionResult,
    )


class Execution:
    """A handle to a specific cell execution, identified by ``execution_id``.

    The handle is returned by :meth:`CellHandle.execute` and provides
    execution-scoped access to status, results, and streaming events.

    Status is read from the daemon's RuntimeStateDoc (local CRDT replica),
    so it's a cheap sync read — no network round trip.

    Example::

        execution = await cell.execute()
        print(execution.status)          # "queued", "running", "done", "error"
        result = await execution.result()  # wait for completion
        print(result.success, result.stdout)


    """

    __slots__ = ("_session", "_cell_id", "_execution_id")

    def __init__(
        self,
        session: AsyncSession,
        cell_id: str,
        execution_id: str,
    ) -> None:
        self._session = session
        self._cell_id = cell_id
        self._execution_id = execution_id

    @property
    def execution_id(self) -> str:
        """UUID for this execution."""
        return self._execution_id

    @property
    def cell_id(self) -> str:
        """Cell ID being executed."""
        return self._cell_id

    @property
    def status(self) -> str:
        """Current execution status (sync read from local CRDT).

        Returns one of: ``"queued"``, ``"running"``, ``"done"``, ``"error"``,
        or ``"unknown"`` if the execution entry hasn't synced yet.
        """
        try:
            rs = self._session.get_runtime_state_sync()
            entry = rs.executions.get(self._execution_id)
            if entry is not None:
                return entry.status
        except Exception:
            pass
        return "unknown"

    @property
    def success(self) -> bool | None:
        """Whether the execution succeeded (None if still running)."""
        try:
            rs = self._session.get_runtime_state_sync()
            entry = rs.executions.get(self._execution_id)
            if entry is not None:
                return entry.success
        except Exception:
            pass
        return None

    @property
    def execution_count(self) -> int | None:
        """Kernel execution count (None if not yet started)."""
        try:
            rs = self._session.get_runtime_state_sync()
            entry = rs.executions.get(self._execution_id)
            if entry is not None:
                return entry.execution_count
        except Exception:
            pass
        return None

    @property
    def done(self) -> bool:
        """Whether the execution has reached a terminal state."""
        return self.status in ("done", "error")

    async def result(self, timeout_secs: float = 60.0) -> ExecutionResult:
        """Wait for the execution to complete and return collected results.

        Unlike ``execute_cell()``, this does NOT re-queue the cell. It waits
        for this specific ``execution_id`` to finish, then reads outputs from
        the notebook doc with a confirm-sync retry loop.

        If the execution is already done, returns immediately (late consumer).

        Args:
            timeout_secs: Maximum time to wait for completion.

        Returns:
            ExecutionResult with outputs, success flag, error info.
        """
        return await self._session.wait_for_execution(
            self._cell_id, self._execution_id, timeout_secs
        )

    async def cancel(self) -> None:
        """Cancel this execution by interrupting the kernel.

        Note: this interrupts the kernel, which affects all executions —
        not just this one. There is no per-execution cancel yet.
        """
        await self._session.interrupt()

    async def wait(self, timeout_secs: float = 60.0) -> None:
        """Wait for the execution to reach a terminal state.

        Unlike :meth:`result`, this doesn't collect outputs — it just
        polls the RuntimeStateDoc until status is "done" or "error".

        Args:
            timeout_secs: Maximum time to wait.

        Raises:
            asyncio.TimeoutError: If the execution doesn't complete in time.
        """
        deadline = asyncio.get_event_loop().time() + timeout_secs
        while not self.done:
            remaining = deadline - asyncio.get_event_loop().time()
            if remaining <= 0:
                raise asyncio.TimeoutError(
                    f"Execution {self._execution_id[:12]}... did not complete "
                    f"within {timeout_secs}s (status={self.status})"
                )
            await asyncio.sleep(min(0.05, remaining))

    def __await__(self) -> Any:
        """Allow ``await execution`` as shorthand for ``await execution.result()``."""
        return self.result().__await__()

    def __repr__(self) -> str:
        return (
            f"Execution({self._cell_id[:8]}..., "
            f"eid={self._execution_id[:8]}..., "
            f"status={self.status})"
        )
