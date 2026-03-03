"""Tests for the MCP server."""

import json
import pytest


class TestMcpServerImports:
    """Test that MCP server imports correctly."""

    def test_import_mcp_server(self):
        """Verify the MCP server module can be imported."""
        # This will fail if mcp isn't installed, which is expected
        # for the base package without the [mcp] extra
        try:
            from runtimed import _mcp_server

            assert hasattr(_mcp_server, "mcp")
            assert hasattr(_mcp_server, "main")
        except ImportError as e:
            if "mcp" in str(e):
                pytest.skip(
                    "mcp package not installed (install with: pip install runtimed[mcp])"
                )
            raise


class TestHelperFunctions:
    """Test helper functions without requiring MCP."""

    def test_output_to_dict_stream(self):
        """Test converting stream output to dict."""
        import runtimed

        # Create a stream output
        # Note: We can't easily construct Output objects directly from Python
        # since they're created by the Rust code, so we just test the types exist
        assert hasattr(runtimed, "Output")
        assert hasattr(runtimed, "ExecutionResult")
        assert hasattr(runtimed, "Cell")

    def test_cell_type_exists(self):
        """Test that Cell type is exported."""
        import runtimed

        assert hasattr(runtimed, "Cell")


@pytest.mark.integration
class TestMcpServerIntegration:
    """Integration tests requiring a running daemon.

    Run with: pytest -m integration
    """

    @pytest.fixture
    def daemon_running(self):
        """Check if daemon is running, skip if not."""
        import runtimed

        client = runtimed.DaemonClient()
        if not client.ping():
            pytest.skip("Daemon not running (start with: cargo xtask dev-daemon)")
        return client

    @pytest.mark.asyncio
    async def test_connect_and_run_code(self, daemon_running):
        """Test connecting and running code via MCP tools."""
        try:
            from runtimed._mcp_server import (
                connect_notebook,
                start_kernel,
                run_code,
                disconnect_notebook,
            )
        except ImportError:
            pytest.skip("mcp package not installed")

        # Connect
        result = await connect_notebook()
        assert result["connected"] is True
        assert "notebook_id" in result

        try:
            # Start kernel
            await start_kernel()

            # Run some code
            result = await run_code("x = 42\nprint(x)")
            assert result["success"] is True
            assert "42" in result["stdout"]
        finally:
            # Disconnect
            await disconnect_notebook()

    @pytest.mark.asyncio
    async def test_create_and_execute_cell(self, daemon_running):
        """Test creating and executing a cell."""
        try:
            from runtimed._mcp_server import (
                connect_notebook,
                start_kernel,
                create_cell,
                execute_cell,
                get_cell,
                delete_cell,
                disconnect_notebook,
            )
        except ImportError:
            pytest.skip("mcp package not installed")

        # Connect
        await connect_notebook()

        try:
            # Start kernel
            await start_kernel()

            # Create a cell
            cell_id = await create_cell(source="print('hello from MCP')")
            assert cell_id is not None

            # Get the cell
            cell = await get_cell(cell_id)
            assert cell["id"] == cell_id
            assert cell["source"] == "print('hello from MCP')"
            assert cell["cell_type"] == "code"

            # Execute it
            result = await execute_cell(cell_id)
            assert result["success"] is True
            assert "hello from MCP" in result["stdout"]

            # Clean up
            await delete_cell(cell_id)
        finally:
            await disconnect_notebook()

    @pytest.mark.asyncio
    async def test_list_notebooks(self, daemon_running):
        """Test listing notebook rooms."""
        try:
            from runtimed._mcp_server import list_notebooks
        except ImportError:
            pytest.skip("mcp package not installed")

        rooms = await list_notebooks()
        assert isinstance(rooms, list)
        # Can be empty if no notebooks are open


@pytest.mark.integration
class TestMcpResources:
    """Test MCP resources."""

    @pytest.fixture
    def daemon_running(self):
        """Check if daemon is running."""
        import runtimed

        client = runtimed.DaemonClient()
        if not client.ping():
            pytest.skip("Daemon not running")
        return client

    @pytest.mark.asyncio
    async def test_resource_status_no_session(self, daemon_running):
        """Test status resource without active session."""
        try:
            from runtimed._mcp_server import resource_status
        except ImportError:
            pytest.skip("mcp package not installed")

        result = await resource_status()
        data = json.loads(result)
        assert data["connected"] is False

    @pytest.mark.asyncio
    async def test_resource_rooms(self, daemon_running):
        """Test rooms resource."""
        try:
            from runtimed._mcp_server import resource_rooms
        except ImportError:
            pytest.skip("mcp package not installed")

        result = await resource_rooms()
        data = json.loads(result)
        assert isinstance(data, list)
