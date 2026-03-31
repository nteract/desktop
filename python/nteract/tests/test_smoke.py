"""Smoke tests to verify the package can be imported."""

import asyncio
from unittest.mock import patch


def test_import():
    """Verify the nteract package can be imported."""
    import nteract

    assert hasattr(nteract, "main")


def test_mcp_server_import():
    """Verify the MCP server module can be imported and NteractServer works."""
    from nteract._mcp_server import NteractServer

    server = NteractServer()
    assert server.mcp.name == "nteract"


def test_migration_guide_tool_when_deprecated():
    """deprecated=True registers a migration_guide tool."""
    from nteract._mcp_server import NteractServer

    server = NteractServer(deprecated=True)
    tool_names = [t.name for t in asyncio.run(server.mcp.list_tools())]
    assert "migration_guide" in tool_names


def test_no_migration_guide_tool_when_not_deprecated():
    """deprecated=False does not register migration_guide."""
    from nteract._mcp_server import NteractServer

    server = NteractServer(deprecated=False)
    tool_names = [t.name for t in asyncio.run(server.mcp.list_tools())]
    assert "migration_guide" not in tool_names


def test_instructions_include_deprecation_when_deprecated():
    """Server instructions mention deprecation when deprecated=True."""
    from nteract._mcp_server import NteractServer

    server = NteractServer(deprecated=True)
    assert "deprecated" in server.mcp.instructions.lower()


def test_instructions_no_deprecation_when_not_deprecated():
    """Server instructions do not mention deprecation when deprecated=False."""
    from nteract._mcp_server import NteractServer

    server = NteractServer(deprecated=False)
    assert "deprecated" not in (server.mcp.instructions or "").lower()


def test_fallback_when_runt_not_found():
    """When runt is not found, main() falls back to Python server instead of exiting."""
    from nteract._mcp_server import main

    with (
        patch("nteract._mcp_server._find_runt_binary", return_value=None),
        patch("nteract._mcp_server.NteractServer") as MockServer,
        patch("sys.argv", ["nteract"]),
    ):
        instance = MockServer.return_value
        instance.mcp.run.side_effect = KeyboardInterrupt
        instance.cleanup = lambda: None
        try:
            main()
        except SystemExit as e:
            # Should exit 130 (KeyboardInterrupt), NOT 1
            assert e.code == 130, f"Expected exit code 130 (fallback), got {e.code}"
        MockServer.assert_called_once()
        _, kwargs = MockServer.call_args
        assert kwargs["deprecated"] is True


def test_keyboard_interrupt_exits_130():
    """Ctrl+C should exit with code 130 (Unix SIGINT convention), not dump a traceback."""
    from nteract._mcp_server import main

    with (
        patch("nteract._mcp_server.NteractServer") as MockServer,
        patch("sys.argv", ["nteract", "--legacy"]),
    ):
        instance = MockServer.return_value
        instance.mcp.run.side_effect = KeyboardInterrupt
        instance.cleanup = lambda: None
        try:
            main()
            raised = False
        except SystemExit as e:
            raised = True
            assert e.code == 130, f"Expected exit code 130, got {e.code}"
        assert raised, "main() should have called sys.exit(130)"
