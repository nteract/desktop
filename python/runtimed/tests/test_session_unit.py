"""Unit tests for the runtimed public API surface.

These tests don't require a running daemon — they test construction,
exports, and working_dir validation.
"""

import pytest

import runtimed


class TestModuleExports:
    """Test that all expected classes are exported."""

    def test_client_exported(self):
        """Client is exported from runtimed."""
        assert hasattr(runtimed, "Client")

    def test_notebook_exported(self):
        """Notebook is exported from runtimed."""
        assert hasattr(runtimed, "Notebook")

    def test_notebook_info_exported(self):
        """NotebookInfo is exported from runtimed."""
        assert hasattr(runtimed, "NotebookInfo")

    def test_cell_handle_exported(self):
        """CellHandle is exported from runtimed."""
        assert hasattr(runtimed, "CellHandle")

    def test_internal_types_not_exported(self):
        """Internal native types are not re-exported from the package."""
        assert not hasattr(runtimed, "NativeAsyncClient")
        assert not hasattr(runtimed, "AsyncSession")

    def test_runtime_state_types_exported(self):
        """Runtime state types use clean names."""
        assert hasattr(runtimed, "RuntimeState")
        assert hasattr(runtimed, "KernelState")
        assert hasattr(runtimed, "EnvState")

    def test_deprecated_types_removed(self):
        """Removed types are no longer exported."""
        assert not hasattr(runtimed, "DaemonClient")
        assert not hasattr(runtimed, "NativeClient")
        assert not hasattr(runtimed, "Session")

    def test_all_exports(self):
        """Check __all__ exports match expected items exactly."""
        expected = {
            # Primary API
            "Client",
            "Execution",
            "Notebook",
            "NotebookInfo",
            "CellHandle",
            "CellCollection",
            "Presence",
            # Error type
            "RuntimedError",
            # Standalone functions
            "default_socket_path",
            "show_notebook_app",
            "show_notebook_app_for_channel",
            "socket_path_for_channel",
        }
        assert set(runtimed.__all__) == expected


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


class TestClientConstruction:
    """Test Client construction."""

    def test_client_creates(self):
        """Client can be instantiated without a daemon."""
        client = runtimed.Client()
        assert repr(client) == "Client()"


class TestNotebookInfo:
    """Test NotebookInfo dataclass."""

    def test_from_dict_file_backed(self):
        info = runtimed.NotebookInfo._from_dict(
            {
                "notebook_id": "/Users/test/notebook.ipynb",
                "active_peers": 2,
                "has_kernel": True,
                "kernel_type": "python",
                "kernel_status": "idle",
                "env_source": "uv:prewarmed",
            }
        )
        assert info.notebook_id == "/Users/test/notebook.ipynb"
        assert info.name == "notebook"
        assert info.path is not None
        assert not info.is_ephemeral
        assert info.active_peers == 2
        assert info.has_runtime is True

    def test_from_dict_ephemeral(self):
        info = runtimed.NotebookInfo._from_dict(
            {
                "notebook_id": "abc123",
                "active_peers": 0,
                "has_kernel": False,
            }
        )
        assert info.name == "abc123"
        assert info.path is None
        assert info.is_ephemeral is True
        assert info.has_runtime is False

    def test_repr(self):
        info = runtimed.NotebookInfo(
            notebook_id="/test/gremlins.ipynb",
            status="idle",
            active_peers=3,
        )
        r = repr(info)
        assert "gremlins" in r
        assert "idle" in r
        assert "3 peers" in r


class TestCreateNotebookValidation:
    """Test create_notebook working_dir validation on NativeAsyncClient."""

    def test_create_notebook_rejects_nonexistent_path(self):
        """create_notebook raises FileNotFoundError for non-existent working_dir."""
        from runtimed._internals import NativeAsyncClient

        client = NativeAsyncClient()
        with pytest.raises(FileNotFoundError, match="working_dir does not exist"):
            client.create_notebook(working_dir="/sessions/fake-path")

    def test_create_notebook_rejects_file_as_working_dir(self, tmp_path):
        """create_notebook raises NotADirectoryError when working_dir is a file."""
        from runtimed._internals import NativeAsyncClient

        test_file = tmp_path / "test_file.txt"
        test_file.write_text("test")
        client = NativeAsyncClient()
        with pytest.raises(NotADirectoryError, match="working_dir is not a directory"):
            client.create_notebook(working_dir=str(test_file))


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
