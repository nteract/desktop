"""Smoke tests to verify the package can be imported."""


def test_import():
    """Verify the nteract package can be imported."""
    import nteract

    assert hasattr(nteract, "main")


def test_mcp_server_import():
    """Verify the MCP server module can be imported."""
    from nteract._mcp_server import mcp

    assert mcp.name == "nteract"
