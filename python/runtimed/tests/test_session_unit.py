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
            # Typed string constants mirroring the Rust daemon enums
            "KERNEL_ERROR_REASON",
            "KERNEL_STATUS",
            "KernelErrorReasonKey",
            "KernelStatusKey",
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


class TestSyncGuards:
    """Test __await__ guards on sync return types."""

    def test_hint_list_await(self):
        """_HintList raises TypeError on __await__."""
        from runtimed._cell import _HintList

        v = _HintList([1, 2, 3], "outputs")
        with pytest.raises(TypeError, match="sync property"):
            v.__await__()

    def test_hint_list_call(self):
        """_HintList raises TypeError on __call__."""
        from runtimed._cell import _HintList

        v = _HintList([1, 2, 3], "outputs")
        with pytest.raises(TypeError, match="not a method"):
            v()

    def test_runtime_state_has_await_guard(self):
        """RuntimeState has __await__ guard method."""
        assert hasattr(runtimed.RuntimeState, "__await__")

    def test_kernel_state_has_await_guard(self):
        """KernelState has __await__ guard method."""
        assert hasattr(runtimed.KernelState, "__await__")

    def test_env_state_has_await_guard(self):
        """EnvState has __await__ guard method."""
        assert hasattr(runtimed.EnvState, "__await__")


class TestKernelStatusConstants:
    """Typed kernel-status constants mirror the Rust daemon strings."""

    def test_kernel_status_values(self):
        """Each constant matches the exact wire string the daemon writes."""
        assert runtimed.KERNEL_STATUS.NOT_STARTED == "not_started"
        assert runtimed.KERNEL_STATUS.AWAITING_TRUST == "awaiting_trust"
        assert runtimed.KERNEL_STATUS.STARTING == "starting"
        assert runtimed.KERNEL_STATUS.IDLE == "idle"
        assert runtimed.KERNEL_STATUS.BUSY == "busy"
        assert runtimed.KERNEL_STATUS.ERROR == "error"
        assert runtimed.KERNEL_STATUS.SHUTDOWN == "shutdown"

    def test_kernel_status_comparable_to_strings(self):
        """Constants compare equal to the bare strings they replace."""
        assert runtimed.KERNEL_STATUS.IDLE == "idle"
        assert "busy" in (runtimed.KERNEL_STATUS.IDLE, runtimed.KERNEL_STATUS.BUSY)


class TestKernelErrorReasonConstants:
    """Typed error-reason constants mirror ``KernelErrorReason::as_str()``."""

    def test_missing_ipykernel_value(self):
        """``MISSING_IPYKERNEL`` matches the Rust enum's wire string."""
        assert runtimed.KERNEL_ERROR_REASON.MISSING_IPYKERNEL == "missing_ipykernel"

    def test_missing_ipykernel_matches_ts_mirror(self):
        """Value matches the TypeScript ``KERNEL_ERROR_REASON.MISSING_IPYKERNEL``."""
        # The TS mirror lives in packages/runtimed/src/runtime-state.ts;
        # both ends must serialise to the same CRDT value.
        assert runtimed.KERNEL_ERROR_REASON.MISSING_IPYKERNEL == "missing_ipykernel"


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
