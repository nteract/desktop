"""Unit tests for Session and AsyncSession classes.

These tests don't require a running daemon - they test construction,
properties, and error handling for disconnected sessions.
"""

import pytest

import runtimed


class TestSessionConstruction:
    """Test Session construction and properties."""

    def test_session_with_auto_id(self):
        """Session generates unique ID if not provided."""
        session = runtimed.Session()
        assert session.notebook_id.startswith("agent-session-")
        assert len(session.notebook_id) > 20  # UUID adds significant length

    def test_session_with_custom_id(self):
        """Session uses provided notebook_id."""
        session = runtimed.Session(notebook_id="my-custom-notebook")
        assert session.notebook_id == "my-custom-notebook"

    def test_session_unique_ids(self):
        """Each Session gets a unique ID."""
        s1 = runtimed.Session()
        s2 = runtimed.Session()
        assert s1.notebook_id != s2.notebook_id

    def test_session_repr_disconnected(self):
        """Session repr shows disconnected status."""
        session = runtimed.Session(notebook_id="test-repr")
        r = repr(session)
        assert "Session" in r
        assert "test-repr" in r
        assert "disconnected" in r

    def test_session_is_connected_initially_false(self):
        """Session is not connected immediately after construction."""
        session = runtimed.Session()
        assert not session.is_connected

    def test_session_kernel_started_initially_false(self):
        """Kernel is not started immediately after construction."""
        session = runtimed.Session()
        assert not session.kernel_started

    def test_session_env_source_initially_none(self):
        """env_source is None when no kernel is running."""
        session = runtimed.Session()
        assert session.env_source is None


class TestAsyncSessionConstruction:
    """Test AsyncSession construction and properties."""

    def test_async_session_with_auto_id(self):
        """AsyncSession generates unique ID if not provided."""
        session = runtimed.AsyncSession()
        assert session.notebook_id.startswith("agent-session-")
        assert len(session.notebook_id) > 20

    def test_async_session_with_custom_id(self):
        """AsyncSession uses provided notebook_id."""
        session = runtimed.AsyncSession(notebook_id="my-async-notebook")
        assert session.notebook_id == "my-async-notebook"

    def test_async_session_unique_ids(self):
        """Each AsyncSession gets a unique ID."""
        s1 = runtimed.AsyncSession()
        s2 = runtimed.AsyncSession()
        assert s1.notebook_id != s2.notebook_id

    def test_async_session_repr(self):
        """AsyncSession repr shows notebook ID."""
        session = runtimed.AsyncSession(notebook_id="test-async-repr")
        r = repr(session)
        assert "AsyncSession" in r
        assert "test-async-repr" in r


class TestAsyncSessionProperties:
    """Test AsyncSession async property methods."""

    @pytest.mark.asyncio
    async def test_async_is_connected_initially_false(self):
        """AsyncSession is not connected immediately after construction."""
        session = runtimed.AsyncSession()
        assert not await session.is_connected()

    @pytest.mark.asyncio
    async def test_async_kernel_started_initially_false(self):
        """Kernel is not started immediately after construction."""
        session = runtimed.AsyncSession()
        assert not await session.kernel_started()

    @pytest.mark.asyncio
    async def test_async_env_source_initially_none(self):
        """env_source is None when no kernel is running."""
        session = runtimed.AsyncSession()
        assert await session.env_source() is None


class TestSessionErrorHandling:
    """Test error handling for disconnected sessions."""

    def test_set_source_without_connection(self):
        """set_source raises error when not connected."""
        session = runtimed.Session()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            session.set_source("cell-123", "x = 1")

    def test_get_cell_without_connection(self):
        """get_cell raises error when not connected."""
        session = runtimed.Session()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            session.get_cell("cell-123")

    def test_get_cells_without_connection(self):
        """get_cells raises error when not connected."""
        session = runtimed.Session()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            session.get_cells()

    def test_delete_cell_without_connection(self):
        """delete_cell raises error when not connected."""
        session = runtimed.Session()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            session.delete_cell("cell-123")

    def test_interrupt_without_connection(self):
        """interrupt raises error when not connected."""
        session = runtimed.Session()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            session.interrupt()

    def test_shutdown_kernel_without_connection(self):
        """shutdown_kernel raises error when not connected."""
        session = runtimed.Session()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            session.shutdown_kernel()

    def test_queue_cell_without_connection(self):
        """queue_cell raises error when not connected."""
        session = runtimed.Session()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            session.queue_cell("cell-123")


class TestAsyncSessionErrorHandling:
    """Test error handling for disconnected async sessions."""

    @pytest.mark.asyncio
    async def test_async_set_source_without_connection(self):
        """set_source raises error when not connected."""
        session = runtimed.AsyncSession()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            await session.set_source("cell-123", "x = 1")

    @pytest.mark.asyncio
    async def test_async_get_cell_without_connection(self):
        """get_cell raises error when not connected."""
        session = runtimed.AsyncSession()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            await session.get_cell("cell-123")

    @pytest.mark.asyncio
    async def test_async_get_cells_without_connection(self):
        """get_cells raises error when not connected."""
        session = runtimed.AsyncSession()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            await session.get_cells()

    @pytest.mark.asyncio
    async def test_async_delete_cell_without_connection(self):
        """delete_cell raises error when not connected."""
        session = runtimed.AsyncSession()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            await session.delete_cell("cell-123")

    @pytest.mark.asyncio
    async def test_async_interrupt_without_connection(self):
        """interrupt raises error when not connected."""
        session = runtimed.AsyncSession()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            await session.interrupt()

    @pytest.mark.asyncio
    async def test_async_shutdown_kernel_without_connection(self):
        """shutdown_kernel raises error when not connected."""
        session = runtimed.AsyncSession()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            await session.shutdown_kernel()

    @pytest.mark.asyncio
    async def test_async_create_cell_without_connection(self):
        """create_cell raises error when not connected."""
        session = runtimed.AsyncSession()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            await session.create_cell("x = 1")

    @pytest.mark.asyncio
    async def test_async_queue_cell_without_connection(self):
        """queue_cell raises error when not connected."""
        session = runtimed.AsyncSession()
        with pytest.raises(runtimed.RuntimedError, match="[Nn]ot connected"):
            await session.queue_cell("cell-123")


class TestOutputTypes:
    """Test Output and ExecutionResult classes."""

    def test_output_class_exists(self):
        """Output class is exported."""
        assert hasattr(runtimed, "Output")

    def test_execution_result_class_exists(self):
        """ExecutionResult class is exported."""
        assert hasattr(runtimed, "ExecutionResult")

    def test_runtimed_error_class_exists(self):
        """RuntimedError class is exported."""
        assert hasattr(runtimed, "RuntimedError")


class TestCreateNotebookValidation:
    """Test create_notebook working_dir validation."""

    def test_create_notebook_rejects_nonexistent_path(self):
        """create_notebook raises FileNotFoundError for non-existent working_dir."""
        with pytest.raises(FileNotFoundError, match="working_dir does not exist"):
            runtimed.Session.create_notebook(working_dir="/sessions/fake-path")

    def test_create_notebook_rejects_file_as_working_dir(self, tmp_path):
        """create_notebook raises NotADirectoryError when working_dir is a file."""
        test_file = tmp_path / "test_file.txt"
        test_file.write_text("test")
        with pytest.raises(NotADirectoryError, match="working_dir is not a directory"):
            runtimed.Session.create_notebook(working_dir=str(test_file))


class TestAsyncCreateNotebookValidation:
    """Test AsyncSession.create_notebook working_dir validation."""

    def test_async_create_notebook_rejects_nonexistent_path(self):
        """AsyncSession.create_notebook raises FileNotFoundError for non-existent working_dir."""
        with pytest.raises(FileNotFoundError, match="working_dir does not exist"):
            runtimed.AsyncSession.create_notebook(working_dir="/sessions/fake-path")

    def test_async_create_notebook_rejects_file_as_working_dir(self, tmp_path):
        """AsyncSession.create_notebook raises NotADirectoryError when working_dir is a file."""
        test_file = tmp_path / "test_file.txt"
        test_file.write_text("test")
        with pytest.raises(NotADirectoryError, match="working_dir is not a directory"):
            runtimed.AsyncSession.create_notebook(working_dir=str(test_file))


class TestModuleExports:
    """Test that all expected classes are exported."""

    def test_session_exported(self):
        """Session is exported from runtimed."""
        assert hasattr(runtimed, "Session")

    def test_async_session_exported(self):
        """AsyncSession is exported from runtimed."""
        assert hasattr(runtimed, "AsyncSession")

    def test_daemon_client_exported(self):
        """DaemonClient is exported from runtimed."""
        assert hasattr(runtimed, "DaemonClient")

    def test_all_exports(self):
        """Check __all__ exports the expected items."""
        expected = [
            "Session",
            "AsyncSession",
            "DaemonClient",
            "ExecutionResult",
            "Output",
            "RuntimedError",
        ]
        for name in expected:
            assert name in runtimed.__all__, f"{name} not in __all__"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
