"""Smoke tests to verify the package can be imported."""

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



def test_keyboard_interrupt_exits_130():
    """Ctrl+C should exit with code 130 (Unix SIGINT convention), not dump a traceback."""
    from nteract._mcp_server import main

    with patch("nteract._mcp_server.NteractServer") as MockServer, patch("sys.argv", ["nteract"]):
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
