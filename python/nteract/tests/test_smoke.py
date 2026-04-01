"""Smoke tests for the nteract package."""

from unittest.mock import patch

import pytest


def test_import():
    """Verify the nteract package can be imported."""
    import nteract

    assert hasattr(nteract, "main")


def test_main_exits_when_runt_not_found(capsys):
    """When runt is not found, main() exits with code 1 and points to nteract.io."""
    from nteract._mcp_server import main

    with (
        patch("nteract._mcp_server._find_runt_binary", return_value=None),
        patch("sys.argv", ["nteract"]),
        pytest.raises(SystemExit) as exc_info,
    ):
        main()

    assert exc_info.value.code == 1
    captured = capsys.readouterr()
    assert "nteract.io" in captured.err


def test_main_execs_runt_when_found():
    """When runt is found, main() exec's it with the right arguments."""
    from nteract._mcp_server import main

    with (
        patch("nteract._mcp_server._find_runt_binary", return_value="/usr/local/bin/runt"),
        patch("nteract._mcp_server.os.execvp") as mock_execvp,
        patch("sys.argv", ["nteract"]),
    ):
        # execvp never returns in real usage; make it raise so main() stops
        mock_execvp.side_effect = SystemExit(0)
        with pytest.raises(SystemExit):
            main()
        mock_execvp.assert_called_once_with("/usr/local/bin/runt", ["/usr/local/bin/runt", "mcp"])


def test_main_passes_no_show_flag():
    """--no-show is forwarded to runt mcp."""
    from nteract._mcp_server import main

    with (
        patch("nteract._mcp_server._find_runt_binary", return_value="/usr/local/bin/runt"),
        patch("nteract._mcp_server.os.execvp") as mock_execvp,
        patch("sys.argv", ["nteract", "--no-show"]),
    ):
        mock_execvp.side_effect = SystemExit(0)
        with pytest.raises(SystemExit):
            main()
        mock_execvp.assert_called_once_with(
            "/usr/local/bin/runt", ["/usr/local/bin/runt", "mcp", "--no-show"]
        )
