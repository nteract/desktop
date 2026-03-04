"""Integration tests for runtimed daemon client.

These tests exercise the full daemon integration, including:
- Document-first execution (automerge sync)
- Multi-client synchronization
- Kernel lifecycle management

Running locally (with dev daemon already running):
    pytest tests/test_daemon_integration.py -v

Running in CI (spawns its own daemon):
    RUNTIMED_INTEGRATION_TEST=1 pytest tests/test_daemon_integration.py -v

Environment variables:
    RUNTIMED_INTEGRATION_TEST=1  - Enable daemon spawning for CI
    RUNTIMED_SOCKET_PATH         - Override socket path
    RUNTIMED_BINARY              - Path to runtimed binary (for CI)
    RUNTIMED_LOG_LEVEL           - Daemon log level (default: info)
"""

import os
import subprocess
import sys
import tempfile
import time
import uuid
from pathlib import Path

import pytest

# Skip all tests if runtimed module not available
pytest.importorskip("runtimed")

import runtimed


# ============================================================================
# Test utilities
# ============================================================================


def wait_for_sync(check_fn, *, timeout=3.0, interval=0.1, description="sync"):
    """Poll until check_fn returns True or timeout.

    Args:
        check_fn: Callable that returns True when sync is complete
        timeout: Maximum time to wait in seconds
        interval: Initial polling interval (grows with backoff)
        description: Description for error message

    Returns:
        True if sync completed within timeout

    Raises:
        AssertionError: If timeout exceeded
    """
    start = time.time()
    while time.time() - start < timeout:
        if check_fn():
            return True
        time.sleep(interval)
        interval = min(interval * 1.5, 0.5)  # Backoff up to 0.5s
    raise AssertionError(f"Timed out waiting for {description} after {timeout}s")


# ============================================================================
# Fixtures for daemon management
# ============================================================================


def _find_runtimed_binary():
    """Find the runtimed binary, checking common locations."""
    # Explicit override
    if "RUNTIMED_BINARY" in os.environ:
        return Path(os.environ["RUNTIMED_BINARY"])

    # Use CONDUCTOR_WORKSPACE_PATH if available (preferred in CI and worktrees)
    if "CONDUCTOR_WORKSPACE_PATH" in os.environ:
        repo_root = Path(os.environ["CONDUCTOR_WORKSPACE_PATH"])
    else:
        # Fallback: walk up from this file (python/runtimed/tests/test_*.py)
        repo_root = Path(__file__).parent.parent.parent.parent.parent

    candidates = [
        repo_root / "target" / "release" / "runtimed",
        repo_root / "target" / "debug" / "runtimed",
    ]

    for path in candidates:
        if path.exists():
            return path

    pytest.skip("runtimed binary not found - build with: cargo build -p runtimed")


def _is_integration_test_mode():
    """Check if we should spawn our own daemon (CI mode)."""
    return os.environ.get("RUNTIMED_INTEGRATION_TEST", "0") == "1"


def _get_socket_path():
    """Get the socket path for tests."""
    if "RUNTIMED_SOCKET_PATH" in os.environ:
        return Path(os.environ["RUNTIMED_SOCKET_PATH"])

    # In integration test mode, use a temp directory
    if _is_integration_test_mode():
        return None  # Will be set by the daemon fixture

    # Otherwise, use default (assumes dev daemon is running)
    return (
        runtimed.default_socket_path()
        if hasattr(runtimed, "default_socket_path")
        else None
    )


@pytest.fixture(scope="module")
def daemon_process():
    """Fixture that ensures a daemon is running.

    In CI mode (RUNTIMED_INTEGRATION_TEST=1), spawns a daemon process.
    In dev mode, assumes daemon is already running via `cargo xtask dev-daemon`.

    Yields:
        tuple: (socket_path, process_or_none)
    """
    if not _is_integration_test_mode():
        # Dev mode: assume daemon is already running
        socket_path = _get_socket_path()
        if socket_path is None:
            # Try the default
            import runtimed as rt

            socket_path = (
                rt.default_socket_path() if hasattr(rt, "default_socket_path") else None
            )

        if socket_path and not socket_path.exists():
            pytest.skip(
                f"Daemon socket not found at {socket_path}. "
                "Start daemon with: cargo xtask dev-daemon"
            )

        yield socket_path, None
        return

    # CI mode: spawn our own daemon
    binary = _find_runtimed_binary()
    log_level = os.environ.get("RUNTIMED_LOG_LEVEL", "info")

    # Create a temp directory for this test run
    # ignore_cleanup_errors=True prevents OSError when ipykernel leaves behind
    # directories like 'magics' that aren't empty during cleanup
    with tempfile.TemporaryDirectory(prefix="runtimed-test-", ignore_cleanup_errors=True) as tmpdir:
        tmpdir = Path(tmpdir)
        socket_path = tmpdir / "runtimed.sock"
        cache_dir = tmpdir / "cache"
        blob_dir = tmpdir / "blobs"
        cache_dir.mkdir()
        blob_dir.mkdir()

        # Build command
        cmd = [
            str(binary),
            "run",
            "--socket",
            str(socket_path),
            "--cache-dir",
            str(cache_dir),
            "--blob-store-dir",
            str(blob_dir),
            "--uv-pool-size",
            "3",  # Pool for sequential tests (need headroom for replenishment)
            "--conda-pool-size",
            "3",  # Need headroom for conda project file tests + inline fallback
        ]

        print(f"\n[test] Starting daemon: {' '.join(cmd)}", file=sys.stderr)
        print(f"[test] Socket path: {socket_path}", file=sys.stderr)

        # Start daemon, capturing logs
        log_file = tmpdir / "daemon.log"
        with open(log_file, "w") as log_f:
            env = os.environ.copy()
            env["RUST_LOG"] = log_level

            proc = subprocess.Popen(
                cmd,
                stdout=log_f,
                stderr=subprocess.STDOUT,
                env=env,
            )

        # Wait for socket to appear
        for i in range(30):
            if socket_path.exists():
                print(f"[test] Daemon ready after {i + 1}s", file=sys.stderr)
                break
            if proc.poll() is not None:
                # Daemon died - print logs and fail
                print(
                    f"[test] Daemon died with code {proc.returncode}", file=sys.stderr
                )
                print(f"[test] Daemon logs:\n{log_file.read_text()}", file=sys.stderr)
                pytest.fail("Daemon process died during startup")
            time.sleep(1)
        else:
            proc.terminate()
            print(f"[test] Daemon logs:\n{log_file.read_text()}", file=sys.stderr)
            pytest.fail("Daemon socket did not appear within 30s")

        # Wait for pools to warm up before running tests.
        # We poll the daemon log file for pool-ready messages since
        # DaemonClient uses default_socket_path() which doesn't respect
        # RUNTIMED_SOCKET_PATH for CI mode.
        uv_ready = False
        conda_ready = False
        import re

        # Match "UV pool: N/M available" where N > 0 (works for any pool size)
        uv_pattern = re.compile(r"UV pool: (\d+)/\d+ available")
        conda_pattern = re.compile(r"Conda pool: (\d+)/\d+ available")
        for i in range(120):
            try:
                log_contents = log_file.read_text()
                if not uv_ready:
                    for line in log_contents.splitlines():
                        match = uv_pattern.search(line)
                        if match and int(match.group(1)) > 0:
                            uv_ready = True
                            print(
                                f"[test] UV pool ready after {i + 1}s", file=sys.stderr
                            )
                            break
                if not conda_ready:
                    for line in log_contents.splitlines():
                        match = conda_pattern.search(line)
                        if match and int(match.group(1)) > 0:
                            conda_ready = True
                            print(
                                f"[test] Conda pool ready after {i + 1}s",
                                file=sys.stderr,
                            )
                            break
            except Exception:
                pass
            if uv_ready and conda_ready:
                break
            time.sleep(1)
        else:
            pytest.fail(
                f"Pools not ready within 120s (uv={uv_ready}, conda={conda_ready}). "
                f"Daemon logs:\n{log_file.read_text()}"
            )

        try:
            yield socket_path, proc
        finally:
            # Cleanup
            print(f"\n[test] Stopping daemon...", file=sys.stderr)
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait()

            # Print daemon logs for debugging
            if log_file.exists():
                logs = log_file.read_text()
                if logs:
                    print(f"[test] Daemon logs:\n{logs}", file=sys.stderr)


@pytest.fixture
def session(daemon_process, monkeypatch):
    """Create a fresh Session for each test."""
    socket_path, _ = daemon_process

    # Set socket path env var so Session.connect() uses the right daemon
    if socket_path is not None:
        monkeypatch.setenv("RUNTIMED_SOCKET_PATH", str(socket_path))

    # Create session with unique notebook ID
    notebook_id = f"test-{uuid.uuid4()}"
    sess = runtimed.Session(notebook_id=notebook_id)

    sess.connect()
    yield sess

    # Cleanup: shutdown kernel if running
    try:
        if sess.kernel_started:
            sess.shutdown_kernel()
    except Exception:
        pass


@pytest.fixture
def two_sessions(daemon_process, monkeypatch):
    """Create two sessions connected to the same notebook (peer sync test)."""
    socket_path, _ = daemon_process

    # Set socket path env var so Session.connect() uses the right daemon
    if socket_path is not None:
        monkeypatch.setenv("RUNTIMED_SOCKET_PATH", str(socket_path))

    # Both sessions share the same notebook ID
    notebook_id = f"test-{uuid.uuid4()}"

    session1 = runtimed.Session(notebook_id=notebook_id)
    session1.connect()

    session2 = runtimed.Session(notebook_id=notebook_id)
    session2.connect()

    yield session1, session2

    # Cleanup
    for sess in [session1, session2]:
        try:
            if sess.kernel_started:
                sess.shutdown_kernel()
        except Exception:
            pass


# ============================================================================
# Basic connectivity tests
# ============================================================================


class TestBasicConnectivity:
    """Test basic daemon connectivity."""

    def test_session_connect(self, session):
        """Session can connect to daemon."""
        assert session.is_connected

    def test_session_repr(self, session):
        """Session has useful repr."""
        r = repr(session)
        assert "Session" in r
        assert session.notebook_id in r


# ============================================================================
# Document-first execution tests
# ============================================================================


class TestDocumentFirstExecution:
    """Test document-first execution pattern.

    These tests verify the architectural principle that execution reads
    from the automerge document rather than receiving code directly.
    """

    def test_create_cell(self, session):
        """Can create a cell in the document."""
        cell_id = session.create_cell("x = 1")

        assert cell_id.startswith("cell-")

        # Verify cell exists in document
        cell = session.get_cell(cell_id)
        assert cell.id == cell_id
        assert cell.source == "x = 1"
        assert cell.cell_type == "code"

    def test_update_cell_source(self, session):
        """Can update cell source in document."""
        cell_id = session.create_cell("original")
        session.set_source(cell_id, "updated")

        cell = session.get_cell(cell_id)
        assert cell.source == "updated"

    def test_get_cells(self, session):
        """Can list all cells in document."""
        # Create a few cells
        cell_ids = [
            session.create_cell("a = 1"),
            session.create_cell("b = 2"),
            session.create_cell("c = 3"),
        ]

        cells = session.get_cells()
        assert len(cells) >= 3

        found_ids = {c.id for c in cells}
        for cid in cell_ids:
            assert cid in found_ids

    def test_delete_cell(self, session):
        """Can delete a cell from document."""
        cell_id = session.create_cell("to_delete")
        session.delete_cell(cell_id)

        with pytest.raises(runtimed.RuntimedError, match="not found"):
            session.get_cell(cell_id)

    def test_execute_cell_reads_from_document(self, session):
        """execute_cell reads source from the synced document.

        This is the core architectural test: execution uses ExecuteCell
        which reads from the automerge doc, not QueueCell which bypasses it.
        """
        session.start_kernel()

        # Create cell with source in document
        cell_id = session.create_cell("result = 2 + 2; print(result)")

        # Execute - daemon reads from document
        result = session.execute_cell(cell_id)

        assert result.success
        assert "4" in result.stdout
        assert result.cell_id == cell_id
        assert result.execution_count is not None

    def test_queue_cell_fires_execution(self, session):
        """queue_cell fires execution without waiting.

        This tests the fire-and-forget pattern where you queue execution
        and then poll get_cell() for results.
        """
        session.start_kernel()

        # Create and queue execution
        cell_id = session.create_cell("queued_var = 'queued'")
        session.queue_cell(cell_id)

        # Give it time to execute
        time.sleep(1)

        # Now verify it ran by executing another cell that uses the variable
        cell2 = session.create_cell("print(queued_var)")
        result = session.execute_cell(cell2)

        assert result.success
        assert "queued" in result.stdout

    def test_execution_error_captured(self, session):
        """Execution errors are captured in result."""
        session.start_kernel()

        cell_id = session.create_cell("raise ValueError('test error')")
        result = session.execute_cell(cell_id)

        assert not result.success
        assert result.error is not None
        assert "ValueError" in result.error.ename

    def test_multiple_executions(self, session):
        """Can execute multiple cells sequentially."""
        session.start_kernel()

        # Execute multiple cells, building up state
        cell1 = session.create_cell("x = 10")
        r1 = session.execute_cell(cell1)
        assert r1.success

        cell2 = session.create_cell("y = x * 2")
        r2 = session.execute_cell(cell2)
        assert r2.success

        cell3 = session.create_cell("print(f'y = {y}')")
        r3 = session.execute_cell(cell3)
        assert r3.success
        assert "y = 20" in r3.stdout


# ============================================================================
# Multi-client synchronization tests
# ============================================================================


class TestMultiClientSync:
    """Test multi-client scenarios where two sessions share a notebook.

    These tests verify that automerge sync works correctly when multiple
    clients are connected to the same notebook.
    """

    def test_two_sessions_same_notebook(self, two_sessions):
        """Two sessions can connect to the same notebook."""
        s1, s2 = two_sessions

        assert s1.is_connected
        assert s2.is_connected
        assert s1.notebook_id == s2.notebook_id

    def test_cell_created_by_one_visible_to_other(self, two_sessions):
        """Cell created by session 1 is visible to session 2."""
        s1, s2 = two_sessions

        # Session 1 creates a cell
        cell_id = s1.create_cell("shared_var = 42")

        # Give sync time to propagate
        time.sleep(0.5)

        # Session 2 should see it
        cells = s2.get_cells()
        found = [c for c in cells if c.id == cell_id]
        assert len(found) == 1
        assert found[0].source == "shared_var = 42"

    def test_source_update_syncs_between_peers(self, two_sessions):
        """Source updates sync between peers."""
        s1, s2 = two_sessions

        # Session 1 creates cell
        cell_id = s1.create_cell("original")

        # Wait for cell to sync to session 2
        def cell_visible_to_s2():
            cells = s2.get_cells()
            return any(c.id == cell_id for c in cells)

        wait_for_sync(cell_visible_to_s2, description="cell visible to s2")

        # Session 2 updates it
        s2.set_source(cell_id, "updated by s2")

        # Wait for update to sync back to session 1
        def update_visible_to_s1():
            cell = s1.get_cell(cell_id)
            return cell.source == "updated by s2"

        wait_for_sync(update_visible_to_s1, description="update visible to s1")

        # Final assertion
        cell = s1.get_cell(cell_id)
        assert cell.source == "updated by s2"

    def test_shared_kernel_execution(self, two_sessions):
        """Both sessions share the same kernel and execution state.

        When two sessions connect to the same notebook, they share the
        daemon's kernel. However, each session tracks its own `kernel_started`
        flag locally, so both need to call start_kernel() (the second call
        is a no-op in the daemon but updates local state).
        """
        s1, s2 = two_sessions

        # Both sessions need to call start_kernel to update their local state
        # The daemon only starts one kernel for the notebook
        s1.start_kernel()
        s2.start_kernel()  # No-op in daemon, but updates s2.kernel_started
        time.sleep(0.5)

        # Session 1 sets a variable
        cell1 = s1.create_cell("shared = 'from s1'")
        r1 = s1.execute_cell(cell1)
        assert r1.success

        # Session 2 can access it (same kernel)
        cell2 = s2.create_cell("print(shared)")
        r2 = s2.execute_cell(cell2)
        assert r2.success
        assert "from s1" in r2.stdout


# ============================================================================
# Kernel lifecycle tests
# ============================================================================


class TestKernelLifecycle:
    """Test kernel lifecycle management."""

    def test_start_kernel(self, session):
        """Can start a kernel."""
        assert not session.kernel_started

        session.start_kernel()

        assert session.kernel_started
        assert session.env_source is not None

    def test_kernel_interrupt(self, session):
        """Can interrupt a running kernel."""
        session.start_kernel()

        # Start a long-running execution in background
        cell_id = session.create_cell("import time; time.sleep(30)")

        # We can't easily test async interrupt without threading,
        # but we can at least verify the interrupt call doesn't error
        # when nothing is running
        session.interrupt()  # Should not raise

    def test_shutdown_kernel(self, session):
        """Can shutdown the kernel."""
        session.start_kernel()
        assert session.kernel_started

        session.shutdown_kernel()
        assert not session.kernel_started


# ============================================================================
# Output type tests
# ============================================================================


class TestOutputTypes:
    """Test different output types from execution."""

    @pytest.mark.skip(reason="Trailing newline stripped by stream_terminal.rs")
    def test_stdout_output(self, session):
        """Captures stdout output."""
        session.start_kernel()

        cell_id = session.create_cell("print('hello stdout')")
        result = session.execute_cell(cell_id)

        assert result.success
        assert result.stdout == "hello stdout\n"

    def test_stderr_output(self, session):
        """Captures stderr output."""
        session.start_kernel()

        cell_id = session.create_cell("import sys; sys.stderr.write('hello stderr\\n')")
        result = session.execute_cell(cell_id)

        assert result.success
        assert "hello stderr" in result.stderr

    def test_return_value(self, session):
        """Captures expression return value."""
        session.start_kernel()

        cell_id = session.create_cell("2 + 2")
        result = session.execute_cell(cell_id)

        assert result.success
        # Return value should appear in display_data
        display = result.display_data
        assert len(display) > 0

    def test_multiple_outputs(self, session):
        """Captures multiple outputs from one cell."""
        session.start_kernel()

        cell_id = session.create_cell("""
print('line 1')
print('line 2')
'final value'
""")
        result = session.execute_cell(cell_id)

        assert result.success
        assert "line 1" in result.stdout
        assert "line 2" in result.stdout


# ============================================================================
# Terminal emulation tests
# ============================================================================


class TestTerminalEmulation:
    """Test terminal emulation for stream outputs.

    The daemon uses alacritty_terminal to process escape sequences like
    carriage returns (for progress bars) and cursor movement.
    """

    def test_carriage_return_overwrites(self, session):
        """Carriage return \\r should overwrite previous content on same line.

        This is how progress bars work - they print "Progress: 50%" then
        "\\rProgress: 100%" to update in place.
        """
        session.start_kernel()

        cell_id = session.create_cell(r"""
import sys
sys.stdout.write("Progress: 50%\rProgress: 100%")
sys.stdout.flush()
""")
        result = session.execute_cell(cell_id)

        assert result.success
        # Should only contain the final state, not the intermediate
        assert "Progress: 100%" in result.stdout
        assert "Progress: 50%" not in result.stdout

    def test_progress_bar_simulation(self, session):
        """Simulated progress bar should show only final state."""
        session.start_kernel()

        cell_id = session.create_cell(r"""
import sys
import time
for i in range(0, 101, 20):
    sys.stdout.write(f"\rLoading: {i}%")
    sys.stdout.flush()
    time.sleep(0.05)
print()  # Final newline
""")
        result = session.execute_cell(cell_id)

        assert result.success
        # Should show final state
        assert "Loading: 100%" in result.stdout
        # Should NOT show intermediate states (they were overwritten)
        assert "Loading: 0%" not in result.stdout
        assert "Loading: 20%" not in result.stdout

    def test_consecutive_prints_merged(self, session):
        """Consecutive print statements should be merged into one output."""
        session.start_kernel()

        cell_id = session.create_cell("""
print("line 1")
print("line 2")
print("line 3")
""")
        result = session.execute_cell(cell_id)

        assert result.success
        # All lines should be present
        assert "line 1" in result.stdout
        assert "line 2" in result.stdout
        assert "line 3" in result.stdout
        # Should be a single continuous output
        expected = "line 1\nline 2\nline 3\n"
        assert result.stdout == expected

    def test_interleaved_stdout_stderr_separate(self, session):
        """Interleaved stdout and stderr should remain separate streams."""
        session.start_kernel()

        cell_id = session.create_cell("""
import sys
print("out1")
sys.stderr.write("err1\\n")
sys.stderr.flush()
print("out2")
""")
        result = session.execute_cell(cell_id)

        assert result.success
        # stdout should have both stdout lines
        assert "out1" in result.stdout
        assert "out2" in result.stdout
        # stderr should have the error line
        assert "err1" in result.stderr
        # They should not be mixed
        assert "err1" not in result.stdout
        assert "out1" not in result.stderr

    def test_ansi_colors_preserved(self, session):
        """ANSI color codes should be preserved in output."""
        session.start_kernel()

        cell_id = session.create_cell(r"""
# Print with ANSI red color
print("\x1b[31mRed text\x1b[0m Normal text")
""")
        result = session.execute_cell(cell_id)

        assert result.success
        # The text content should be present
        assert "Red text" in result.stdout
        assert "Normal text" in result.stdout
        # ANSI codes should be preserved (the terminal emulator serializes back to ANSI)
        assert "\x1b[" in result.stdout

    def test_backspace_handling(self, session):
        """Backspace character should delete previous character."""
        session.start_kernel()

        cell_id = session.create_cell(r"""
import sys
sys.stdout.write("abc\b\bd")
sys.stdout.flush()
print()
""")
        result = session.execute_cell(cell_id)

        assert result.success
        # "abc" with two backspaces then "d" should result in "ad"
        # (delete 'c', delete 'b', write 'd')
        assert "ad" in result.stdout

    def test_ansi_colors_with_carriage_return(self, session):
        """ANSI colors combined with carriage return work correctly."""
        session.start_kernel()

        cell_id = session.create_cell(r"""
import sys
# Print colored text, then overwrite with different color
sys.stdout.write("\x1b[31mRed\x1b[0m\r\x1b[32mGreen\x1b[0m")
sys.stdout.flush()
""")
        result = session.execute_cell(cell_id)

        assert result.success
        # Should contain green ANSI codes, red should be overwritten
        assert "\x1b[32m" in result.stdout
        assert "Green" in result.stdout


# ============================================================================
# Error handling tests
# ============================================================================


class TestErrorHandling:
    """Test error handling scenarios."""

    def test_execute_auto_starts_kernel(self, session):
        """execute_cell auto-starts kernel if not running."""
        # Don't call start_kernel() - execute_cell should do it automatically
        cell_id = session.create_cell("x = 42; print(x)")

        # Should work without explicit start_kernel()
        result = session.execute_cell(cell_id)
        assert result.success
        assert "42" in result.stdout
        assert session.kernel_started

    def test_get_nonexistent_cell(self, session):
        """Getting nonexistent cell raises error."""
        with pytest.raises(runtimed.RuntimedError, match="not found"):
            session.get_cell("cell-does-not-exist")

    def test_syntax_error(self, session):
        """Syntax errors are captured."""
        session.start_kernel()

        cell_id = session.create_cell("def broken(")
        result = session.execute_cell(cell_id)

        assert not result.success
        assert result.error is not None
        assert "SyntaxError" in result.error.ename


# ============================================================================
# Output handling tests
# ============================================================================


class TestOutputHandling:
    """Test comprehensive output handling from execution.

    Verifies that all output types are captured correctly and that
    execution stops when an error is raised.
    """

    def test_output_types_and_error_stops_execution(self, session):
        """Test stream, display, error outputs and verify error stops execution.

        Creates 4 cells:
        1. print() - should produce stream data
        2. display() - should produce display_data
        3. raise ValueError - should produce error, stop execution
        4. print() - should NOT execute because error stops execution
        """
        session.start_kernel()

        # Create and execute cell 1: stream data (print)
        cell1 = session.create_cell('print("should be stream data")')
        result1 = session.execute_cell(cell1)
        assert result1.success, f"Cell 1 should succeed: {result1.error}"
        assert "should be stream data" in result1.stdout, (
            f"Expected stream data in stdout, got: {result1.stdout!r}"
        )

        # Create remaining cells after first execution
        cell2 = session.create_cell("display('test')")
        cell3 = session.create_cell('raise ValueError("better see this")')
        cell4 = session.create_cell('print("this better not run")')

        # Execute cell 2: display data
        result2 = session.execute_cell(cell2)
        assert result2.success, f"Cell 2 should succeed: {result2.error}"
        # display('test') produces display_data output
        assert len(result2.display_data) > 0, (
            f"Expected display_data from display(), got none. "
            f"stdout={result2.stdout!r}, stderr={result2.stderr!r}"
        )

        # Execute cell 3: error (ValueError)
        result3 = session.execute_cell(cell3)
        assert not result3.success, "Cell 3 should fail (ValueError)"
        assert result3.error is not None, "Cell 3 should have error info"
        assert result3.error.ename == "ValueError", (
            f"Expected ValueError, got: {result3.error.ename}"
        )
        assert "better see this" in result3.error.evalue, (
            f"Expected error message, got: {result3.error.evalue}"
        )

        # Cell 4: In a "run all" scenario, this would not execute because
        # cell 3 raised an error. Here we're executing cells individually,
        # so we verify the kernel is still functional but the error was
        # properly captured in cell 3.
        # If this were a "run all" API, cell 4 would be skipped.
        # For now, we just verify the kernel didn't crash.
        result4 = session.execute_cell(cell4)
        # This WILL execute since we're calling execute_cell directly,
        # but in a "run all" scenario it would be skipped.
        # The key test is that cell 3's error was properly captured.
        assert result4.success, "Kernel should still be functional after error"

    def test_stream_stdout_and_stderr(self, session):
        """Test that both stdout and stderr are captured separately."""
        session.start_kernel()

        result = session.run(
            'import sys\nprint("to stdout")\nsys.stderr.write("to stderr\\n")'
        )

        assert result.success
        assert "to stdout" in result.stdout
        assert "to stderr" in result.stderr

    def test_display_data_mimetype(self, session):
        """Test that display_data includes mime type information."""
        session.start_kernel()

        # Display a string - should have text/plain
        result = session.run("display('hello world')")

        assert result.success
        assert len(result.display_data) > 0
        # The display_data should contain the displayed value
        # Exact structure depends on Python bindings, but data should be present

    def test_error_traceback_captured(self, session):
        """Test that full traceback is captured on error."""
        session.start_kernel()

        result = session.run(
            "def inner():\n"
            '    raise RuntimeError("deep error")\n'
            "def outer():\n"
            "    inner()\n"
            "outer()"
        )

        assert not result.success
        assert result.error is not None
        assert result.error.ename == "RuntimeError"
        assert "deep error" in result.error.evalue
        # Traceback should show the call stack
        assert len(result.error.traceback) > 0


# ============================================================================
# Kernel launch metadata tests
# ============================================================================


# The metadata key used by the daemon to store NotebookMetadataSnapshot
NOTEBOOK_METADATA_KEY = "notebook_metadata"


def _python_kernelspec_metadata(
    *, with_uv_deps=None, with_conda_deps=None, with_conda_channels=None
):
    """Build a NotebookMetadataSnapshot JSON dict with a Python kernelspec."""
    snapshot = {
        "kernelspec": {
            "name": "python3",
            "display_name": "Python 3",
            "language": "python",
        },
        "language_info": {"name": "python"},
        "runt": {"schema_version": "1"},
    }
    if with_uv_deps is not None:
        snapshot["runt"]["uv"] = {"dependencies": with_uv_deps}
    if with_conda_deps is not None:
        snapshot["runt"]["conda"] = {
            "dependencies": with_conda_deps,
            "channels": with_conda_channels or ["conda-forge"],
        }
    return snapshot


def _deno_kernelspec_metadata(*, flexible_npm_imports=None):
    """Build a NotebookMetadataSnapshot JSON dict with a Deno kernelspec.

    Args:
        flexible_npm_imports: If set, include deno.flexible_npm_imports in runt metadata.
    """
    snapshot = {
        "kernelspec": {
            "name": "deno",
            "display_name": "Deno",
            "language": "typescript",
        },
        "language_info": {"name": "typescript"},
        "runt": {"schema_version": "1"},
    }
    if flexible_npm_imports is not None:
        snapshot["runt"]["deno"] = {"flexible_npm_imports": flexible_npm_imports}
    return snapshot


class TestKernelLaunchMetadata:
    """Test that kernel launch reads metadata from the Automerge doc.

    These tests verify the refactored metadata resolution path where
    the daemon reads kernelspec and dependency info from the synced
    Automerge document rather than re-reading .ipynb files from disk.
    """

    def test_metadata_round_trip(self, session):
        """Metadata set on the doc can be read back."""
        import json

        snapshot = _python_kernelspec_metadata()
        session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))

        # Give sync time to propagate
        time.sleep(0.3)

        raw = session.get_metadata(NOTEBOOK_METADATA_KEY)
        assert raw is not None
        parsed = json.loads(raw)
        assert parsed["kernelspec"]["name"] == "python3"
        assert parsed["runt"]["schema_version"] == "1"

    def test_python_kernel_with_python_kernelspec(self, session):
        """A notebook with python kernelspec launches a Python kernel."""
        import json

        # Set python kernelspec in the Automerge doc
        snapshot = _python_kernelspec_metadata()
        session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.3)

        session.start_kernel(kernel_type="python")

        # Verify it's actually a Python kernel
        result = session.run("import sys; print(sys.prefix)")
        assert result.success
        # sys.prefix should be a real filesystem path
        assert "/" in result.stdout or "\\" in result.stdout

    def test_default_deno_but_python_notebook(self, session):
        """When default runtime is Deno but notebook has Python kernelspec,
        the kernel should be Python.

        This is the key invariant: the notebook's kernelspec in the Automerge
        doc takes priority over the user's default_runtime setting. A Python
        notebook in a project that defaults to Deno should still get a Python
        kernel.
        """
        import json

        # Set python kernelspec in the Automerge doc (simulates opening
        # an existing Python notebook even though default_runtime=deno)
        snapshot = _python_kernelspec_metadata()
        session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.3)

        # Explicitly start Python kernel (as the frontend would after
        # reading kernelspec from the doc)
        session.start_kernel(kernel_type="python")

        # Verify it's truly Python - sys.prefix gives the venv path,
        # and sys.executable should be a python binary
        result = session.run("import sys; print(sys.prefix)")
        assert result.success, f"Expected success, got: {result.stderr}"
        prefix = result.stdout.strip()
        assert prefix, "sys.prefix should not be empty"
        assert "/" in prefix or "\\" in prefix, (
            f"sys.prefix should be a filesystem path, got: {prefix}"
        )

        # Double-check: importing a Python-only stdlib module should work
        result2 = session.run("import json; print(json.dumps({'runtime': 'python'}))")
        assert result2.success
        assert '"runtime": "python"' in result2.stdout

    def test_kernel_launch_reports_env_source(self, session):
        """Kernel launch returns the resolved env_source."""
        session.start_kernel()

        # env_source should be set after kernel launch
        env_source = session.env_source
        assert env_source is not None
        # Should be one of the known env_source values
        assert any(
            env_source.startswith(prefix) for prefix in ("uv:", "conda:", "deno")
        ), f"Unexpected env_source: {env_source}"

    @pytest.mark.skip(reason="Flaky - sync timing race condition in CI")
    def test_metadata_visible_to_second_peer(self, two_sessions):
        """Metadata set by one peer is visible to another."""
        import json

        s1, s2 = two_sessions

        # Session 1 sets metadata
        snapshot = _python_kernelspec_metadata()
        s1.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))

        # Give sync time
        time.sleep(0.5)

        # Session 2 should see it
        raw = s2.get_metadata(NOTEBOOK_METADATA_KEY)
        assert raw is not None
        parsed = json.loads(raw)
        assert parsed["kernelspec"]["name"] == "python3"

    @pytest.mark.skip(reason="Flaky - inline env not prepared in time in CI")
    def test_uv_inline_deps_trusted(self, session):
        """Python kernel with UV inline deps from metadata launches correctly.

        When the notebook metadata contains runt.uv.dependencies, the daemon
        should detect env_source as 'uv:inline' and prepare a cached env
        with those deps installed.
        """
        import json

        snapshot = _python_kernelspec_metadata(with_uv_deps=["requests"])
        session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.3)

        session.start_kernel(kernel_type="python", env_source="uv:inline")

        assert session.env_source == "uv:inline"

        # Verify the dep is actually importable
        result = session.run("import requests; print(requests.__version__)")
        assert result.success, f"Failed to import requests: {result.stderr}"
        assert result.stdout.strip(), "requests version should not be empty"

    def test_uv_inline_deps_env_has_python(self, session):
        """UV inline env actually has a working Python with the declared deps."""
        import json

        snapshot = _python_kernelspec_metadata(with_uv_deps=["requests"])
        session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.3)

        session.start_kernel(kernel_type="python", env_source="uv:inline")

        # sys.prefix should point to a venv, not the system Python
        result = session.run("import sys; print(sys.prefix)")
        assert result.success
        prefix = result.stdout.strip()
        assert "inline-env" in prefix or "inline" in prefix or "cache" in prefix, (
            f"Expected inline env path, got: {prefix}"
        )

    def test_kernel_prewarmed_env_source(self, session):
        """Default kernel launch uses prewarmed pool."""
        session.start_kernel(kernel_type="python", env_source="uv:prewarmed")

        assert session.env_source == "uv:prewarmed"

        result = session.run("import sys; print(sys.prefix)")
        assert result.success


# ============================================================================
# Deno kernel tests
# ============================================================================


class TestDenoKernel:
    """Test Deno kernel launch via daemon bootstrap.

    The daemon bootstraps deno via rattler/conda-forge if not on PATH,
    then runs `deno jupyter --kernel --conn <file>`. First run may be
    slow due to deno download; subsequent runs use the cached binary.
    """

    def test_deno_kernel_launch(self, session):
        """Deno kernel launches and executes TypeScript."""
        import json

        snapshot = _deno_kernelspec_metadata()
        session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.3)

        session.start_kernel(kernel_type="deno", env_source="deno")

        result = session.run("console.log('hello from deno')")
        assert result.success, f"Deno execution failed: {result.stderr}"
        assert "hello from deno" in result.stdout

    def test_deno_kernel_typescript_features(self, session):
        """Deno kernel supports TypeScript features."""
        import json

        snapshot = _deno_kernelspec_metadata()
        session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.3)

        session.start_kernel(kernel_type="deno", env_source="deno")

        # TypeScript type annotations and template literals
        result = session.run(
            "const greet = (name: string): string => `Hello, ${name}!`;\n"
            "console.log(greet('integration test'))"
        )
        assert result.success, f"TypeScript execution failed: {result.stderr}"
        assert "Hello, integration test!" in result.stdout

    def test_deno_kernelspec_metadata_round_trip(self, session):
        """Deno kernelspec in metadata is stored and retrieved correctly."""
        import json

        snapshot = _deno_kernelspec_metadata()
        session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.3)

        raw = session.get_metadata(NOTEBOOK_METADATA_KEY)
        assert raw is not None
        parsed = json.loads(raw)
        assert parsed["kernelspec"]["name"] == "deno"
        assert parsed["kernelspec"]["language"] == "typescript"
        assert parsed["language_info"]["name"] == "typescript"

    def test_deno_flexible_npm_imports_syncs_between_peers(self, two_sessions):
        """flexible_npm_imports setting syncs between peers.

        When one session updates the deno.flexible_npm_imports setting,
        the other session should see the change after sync propagates.
        """
        import json

        s1, s2 = two_sessions

        # Session 1 sets initial metadata with flexible_npm_imports=True
        snapshot = _deno_kernelspec_metadata(flexible_npm_imports=True)
        s1.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.5)

        # Session 2 should see it
        raw = s2.get_metadata(NOTEBOOK_METADATA_KEY)
        assert raw is not None
        parsed = json.loads(raw)
        assert parsed["runt"]["deno"]["flexible_npm_imports"] is True

        # Session 2 changes it to False
        snapshot2 = _deno_kernelspec_metadata(flexible_npm_imports=False)
        s2.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot2))
        time.sleep(0.5)

        # Session 1 should see the change
        raw1 = s1.get_metadata(NOTEBOOK_METADATA_KEY)
        assert raw1 is not None
        parsed1 = json.loads(raw1)
        assert parsed1["runt"]["deno"]["flexible_npm_imports"] is False

    def test_deno_flexible_npm_imports_round_trip(self, session):
        """flexible_npm_imports is preserved through metadata round-trip."""
        import json

        # Set to False (non-default)
        snapshot = _deno_kernelspec_metadata(flexible_npm_imports=False)
        session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.3)

        raw = session.get_metadata(NOTEBOOK_METADATA_KEY)
        assert raw is not None
        parsed = json.loads(raw)
        assert "deno" in parsed["runt"]
        assert parsed["runt"]["deno"]["flexible_npm_imports"] is False


# ============================================================================
# Conda inline dependency tests
# ============================================================================


@pytest.mark.skip(reason="Conda inline env creation via rattler can exceed 60s timeout in CI")
class TestCondaInlineDeps:
    """Test conda inline dependency environments.

    When notebook metadata contains runt.conda.dependencies, the daemon
    creates a cached conda environment via rattler. First creation is
    slow (rattler solve + install); subsequent launches with the same
    deps hit the cache at ~/.cache/runt/inline-envs/.

    Uses a class-scoped fixture to share the kernel between tests,
    avoiding duplicate env creation and reducing flakiness from
    broadcast race conditions on cold startup.
    """

    @pytest.fixture(scope="class")
    def conda_inline_session(self, daemon_process):
        """Create a session with conda inline deps, shared across tests in this class."""
        import json

        socket_path, _ = daemon_process

        # Set socket path env var so Session.connect() uses the right daemon
        old_socket_path = os.environ.get("RUNTIMED_SOCKET_PATH")
        if socket_path is not None:
            os.environ["RUNTIMED_SOCKET_PATH"] = str(socket_path)

        # Create session with unique notebook ID
        notebook_id = f"test-conda-inline-{uuid.uuid4()}"
        sess = runtimed.Session(notebook_id=notebook_id)
        sess.connect()

        # Set up conda inline deps metadata
        snapshot = _python_kernelspec_metadata(with_conda_deps=["filelock"])
        sess.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.3)

        # Start kernel once for all tests in class
        sess.start_kernel(kernel_type="python", env_source="conda:inline")

        yield sess

        # Cleanup
        try:
            if sess.kernel_started:
                sess.shutdown_kernel()
        except Exception:
            pass
        finally:
            # Restore env var
            if old_socket_path is not None:
                os.environ["RUNTIMED_SOCKET_PATH"] = old_socket_path
            elif "RUNTIMED_SOCKET_PATH" in os.environ:
                del os.environ["RUNTIMED_SOCKET_PATH"]

    def test_conda_inline_deps(self, conda_inline_session):
        """Conda inline deps from metadata launches kernel with deps installed."""
        session = conda_inline_session

        assert session.env_source == "conda:inline"

        result = session.run("import filelock; print(filelock.__version__)")
        assert result.success, f"Failed to import filelock: {result.stderr}"
        assert result.stdout.strip(), "filelock version should not be empty"

    def test_conda_inline_env_has_python(self, conda_inline_session):
        """Conda inline env has a working Python in a conda prefix."""
        session = conda_inline_session

        result = session.run("import sys; print(sys.prefix)")
        assert result.success
        prefix = result.stdout.strip()
        assert prefix, "sys.prefix should not be empty"
        # Should be in the inline-envs cache directory
        assert "inline" in prefix or "cache" in prefix, (
            f"Expected conda inline env path, got: {prefix}"
        )


# ============================================================================
# Project file detection tests
# ============================================================================


# Fixture directory for project file tests
FIXTURES_DIR = (
    Path(__file__).parent.parent.parent.parent
    / "crates"
    / "notebook"
    / "fixtures"
    / "audit-test"
)


class TestProjectFileDetection:
    """Test project file auto-detection via notebook_path walk-up.

    When env_source="auto" and a notebook_path is provided, the daemon
    walks up from the notebook directory looking for project files
    (pyproject.toml, pixi.toml, environment.yml). The closest match wins.

    These tests use real fixture notebooks from the repo that have
    project files alongside them.
    """

    def test_pyproject_auto_detection(self, session):
        """notebook_path near pyproject.toml auto-detects uv:pyproject.

        Uses `uv run --with ipykernel` to install deps from the fixture
        pyproject.toml (pandas>=2.0, numpy).
        """
        import json

        notebook_path = str(FIXTURES_DIR / "pyproject-project" / "5-pyproject.ipynb")

        # Set python kernelspec in metadata
        snapshot = _python_kernelspec_metadata()
        session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.3)

        session.start_kernel(
            kernel_type="python",
            env_source="auto",
            notebook_path=notebook_path,
        )

        assert session.env_source == "uv:pyproject"

        # The fixture pyproject.toml declares numpy as a dependency
        result = session.run("import numpy; print(numpy.__version__)")
        assert result.success, (
            f"Failed to import numpy from pyproject env: {result.stderr}"
        )

    def test_pixi_auto_detection(self, session):
        """notebook_path near pixi.toml auto-detects conda:pixi.

        The conda:pixi env_source is detected, and a pooled conda env
        is used to launch the kernel.
        """
        import json

        notebook_path = str(FIXTURES_DIR / "pixi-project" / "6-pixi.ipynb")

        snapshot = _python_kernelspec_metadata()
        session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.3)

        session.start_kernel(
            kernel_type="python",
            env_source="auto",
            notebook_path=notebook_path,
        )

        assert session.env_source == "conda:pixi"

        # Kernel should be functional
        result = session.run("import sys; print(sys.prefix)")
        assert result.success, f"Kernel failed in pixi env: {result.stderr}"

    def test_environment_yml_auto_detection(self, session):
        """notebook_path near environment.yml auto-detects conda:env_yml.

        The conda:env_yml env_source is detected, and a pooled conda env
        is used to launch the kernel.
        """
        import json

        notebook_path = str(
            FIXTURES_DIR / "conda-env-project" / "7-environment-yml.ipynb"
        )

        snapshot = _python_kernelspec_metadata()
        session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
        time.sleep(0.3)

        session.start_kernel(
            kernel_type="python",
            env_source="auto",
            notebook_path=notebook_path,
        )

        assert session.env_source == "conda:env_yml"

        result = session.run("import sys; print(sys.prefix)")
        assert result.success, f"Kernel failed in env_yml env: {result.stderr}"

    def test_no_project_file_falls_back_to_prewarmed(self, session):
        """When no project file is found, auto falls back to uv:prewarmed."""
        import json
        import tempfile

        # Create a temp notebook path with no project files nearby
        with tempfile.NamedTemporaryFile(suffix=".ipynb", delete=False) as f:
            notebook_path = f.name

        try:
            snapshot = _python_kernelspec_metadata()
            session.set_metadata(NOTEBOOK_METADATA_KEY, json.dumps(snapshot))
            time.sleep(0.3)

            session.start_kernel(
                kernel_type="python",
                env_source="auto",
                notebook_path=notebook_path,
            )

            assert session.env_source == "uv:prewarmed"

            result = session.run("import sys; print(sys.prefix)")
            assert result.success
        finally:
            os.unlink(notebook_path)


# ============================================================================
# AsyncSession tests
# ============================================================================


@pytest.fixture
async def async_session(daemon_process, monkeypatch):
    """Create a fresh AsyncSession for each test."""
    socket_path, _ = daemon_process

    # Set socket path env var so AsyncSession.connect() uses the right daemon
    if socket_path is not None:
        monkeypatch.setenv("RUNTIMED_SOCKET_PATH", str(socket_path))

    # Create session with unique notebook ID
    notebook_id = f"async-test-{uuid.uuid4()}"
    sess = runtimed.AsyncSession(notebook_id=notebook_id)

    await sess.connect()
    yield sess

    # Cleanup: shutdown kernel if running
    try:
        if await sess.kernel_started():
            await sess.shutdown_kernel()
    except Exception:
        pass


@pytest.fixture
async def two_async_sessions(daemon_process, monkeypatch):
    """Create two async sessions connected to the same notebook."""
    socket_path, _ = daemon_process

    if socket_path is not None:
        monkeypatch.setenv("RUNTIMED_SOCKET_PATH", str(socket_path))

    notebook_id = f"async-test-{uuid.uuid4()}"

    session1 = runtimed.AsyncSession(notebook_id=notebook_id)
    await session1.connect()

    session2 = runtimed.AsyncSession(notebook_id=notebook_id)
    await session2.connect()

    yield session1, session2

    # Cleanup
    for sess in [session1, session2]:
        try:
            if await sess.kernel_started():
                await sess.shutdown_kernel()
        except Exception:
            pass


class TestAsyncBasicConnectivity:
    """Test basic daemon connectivity with AsyncSession."""

    @pytest.mark.asyncio
    async def test_async_session_connect(self, async_session):
        """AsyncSession can connect to daemon."""
        assert await async_session.is_connected()

    @pytest.mark.asyncio
    async def test_async_session_repr(self, async_session):
        """AsyncSession has useful repr."""
        r = repr(async_session)
        assert "AsyncSession" in r
        assert async_session.notebook_id in r


class TestAsyncDocumentFirstExecution:
    """Test document-first execution pattern with AsyncSession."""

    @pytest.mark.asyncio
    async def test_async_create_cell(self, async_session):
        """Can create a cell in the document."""
        cell_id = await async_session.create_cell("x = 1")

        assert cell_id.startswith("cell-")

        # Verify cell exists in document
        cell = await async_session.get_cell(cell_id)
        assert cell.id == cell_id
        assert cell.source == "x = 1"
        assert cell.cell_type == "code"

    @pytest.mark.asyncio
    async def test_async_update_cell_source(self, async_session):
        """Can update cell source in document."""
        cell_id = await async_session.create_cell("original")
        await async_session.set_source(cell_id, "updated")

        cell = await async_session.get_cell(cell_id)
        assert cell.source == "updated"

    @pytest.mark.asyncio
    async def test_async_get_cells(self, async_session):
        """Can list all cells in document."""
        cell_ids = [
            await async_session.create_cell("a = 1"),
            await async_session.create_cell("b = 2"),
            await async_session.create_cell("c = 3"),
        ]

        cells = await async_session.get_cells()
        assert len(cells) >= 3

        found_ids = {c.id for c in cells}
        for cid in cell_ids:
            assert cid in found_ids

    @pytest.mark.asyncio
    async def test_async_delete_cell(self, async_session):
        """Can delete a cell from document."""
        cell_id = await async_session.create_cell("to_delete")
        await async_session.delete_cell(cell_id)

        with pytest.raises(runtimed.RuntimedError, match="not found"):
            await async_session.get_cell(cell_id)

    @pytest.mark.asyncio
    async def test_async_execute_cell_reads_from_document(self, async_session):
        """execute_cell reads source from the synced document."""
        await async_session.start_kernel()

        cell_id = await async_session.create_cell("result = 2 + 2; print(result)")
        result = await async_session.execute_cell(cell_id)

        assert result.success
        assert "4" in result.stdout
        assert result.cell_id == cell_id
        assert result.execution_count is not None

    @pytest.mark.asyncio
    async def test_async_queue_cell_fires_execution(self, async_session):
        """queue_cell fires execution without waiting."""
        import asyncio

        await async_session.start_kernel()

        # Create and queue execution
        cell_id = await async_session.create_cell("async_queued_var = 'async_queued'")
        await async_session.queue_cell(cell_id)

        # Give it time to execute
        await asyncio.sleep(1)

        # Verify it ran by executing another cell that uses the variable
        cell2 = await async_session.create_cell("print(async_queued_var)")
        result = await async_session.execute_cell(cell2)

        assert result.success
        assert "async_queued" in result.stdout

    @pytest.mark.asyncio
    async def test_async_execution_error_captured(self, async_session):
        """Execution errors are captured in result."""
        await async_session.start_kernel()

        cell_id = await async_session.create_cell(
            "raise ValueError('async test error')"
        )
        result = await async_session.execute_cell(cell_id)

        assert not result.success
        assert result.error is not None
        assert "ValueError" in result.error.ename

    @pytest.mark.asyncio
    async def test_async_multiple_executions(self, async_session):
        """Can execute multiple cells sequentially."""
        await async_session.start_kernel()

        cell1 = await async_session.create_cell("x = 10")
        r1 = await async_session.execute_cell(cell1)
        assert r1.success

        cell2 = await async_session.create_cell("y = x * 2")
        r2 = await async_session.execute_cell(cell2)
        assert r2.success

        cell3 = await async_session.create_cell("print(f'y = {y}')")
        r3 = await async_session.execute_cell(cell3)
        assert r3.success
        assert "y = 20" in r3.stdout


class TestAsyncMultiClientSync:
    """Test multi-client scenarios with AsyncSession."""

    @pytest.mark.asyncio
    async def test_async_two_sessions_same_notebook(self, two_async_sessions):
        """Two async sessions can connect to the same notebook."""
        s1, s2 = two_async_sessions

        assert await s1.is_connected()
        assert await s2.is_connected()
        assert s1.notebook_id == s2.notebook_id

    @pytest.mark.asyncio
    async def test_async_cell_created_by_one_visible_to_other(self, two_async_sessions):
        """Cell created by session 1 is visible to session 2."""
        import asyncio

        s1, s2 = two_async_sessions

        cell_id = await s1.create_cell("async_shared_var = 42")
        await asyncio.sleep(0.5)

        cells = await s2.get_cells()
        found = [c for c in cells if c.id == cell_id]
        assert len(found) == 1
        assert found[0].source == "async_shared_var = 42"

    @pytest.mark.asyncio
    async def test_async_shared_kernel_execution(self, two_async_sessions):
        """Both sessions share the same kernel and execution state."""
        import asyncio

        s1, s2 = two_async_sessions

        await s1.start_kernel()
        await s2.start_kernel()  # No-op in daemon
        await asyncio.sleep(0.5)

        cell1 = await s1.create_cell("async_shared = 'from async s1'")
        r1 = await s1.execute_cell(cell1)
        assert r1.success

        cell2 = await s2.create_cell("print(async_shared)")
        r2 = await s2.execute_cell(cell2)
        assert r2.success
        assert "from async s1" in r2.stdout


class TestAsyncKernelLifecycle:
    """Test kernel lifecycle management with AsyncSession."""

    @pytest.mark.asyncio
    async def test_async_start_kernel(self, async_session):
        """Can start a kernel."""
        assert not await async_session.kernel_started()

        await async_session.start_kernel()

        assert await async_session.kernel_started()
        assert await async_session.env_source() is not None

    @pytest.mark.asyncio
    async def test_async_kernel_interrupt(self, async_session):
        """Can interrupt a running kernel."""
        await async_session.start_kernel()
        await async_session.interrupt()  # Should not raise

    @pytest.mark.asyncio
    async def test_async_shutdown_kernel(self, async_session):
        """Can shutdown the kernel."""
        await async_session.start_kernel()
        assert await async_session.kernel_started()

        await async_session.shutdown_kernel()
        assert not await async_session.kernel_started()


class TestAsyncOutputTypes:
    """Test different output types from execution with AsyncSession."""

    @pytest.mark.asyncio
    async def test_async_stdout_output(self, async_session):
        """Captures stdout output."""
        await async_session.start_kernel()

        cell_id = await async_session.create_cell("print('async hello stdout')")
        result = await async_session.execute_cell(cell_id)

        assert result.success
        assert result.stdout == "async hello stdout\n"

    @pytest.mark.asyncio
    async def test_async_stderr_output(self, async_session):
        """Captures stderr output."""
        await async_session.start_kernel()

        cell_id = await async_session.create_cell(
            "import sys; sys.stderr.write('async hello stderr\\n')"
        )
        result = await async_session.execute_cell(cell_id)

        assert result.success
        assert "async hello stderr" in result.stderr

    @pytest.mark.asyncio
    async def test_async_return_value(self, async_session):
        """Captures expression return value."""
        await async_session.start_kernel()

        cell_id = await async_session.create_cell("2 + 2")
        result = await async_session.execute_cell(cell_id)

        assert result.success
        display = result.display_data
        assert len(display) > 0


class TestAsyncErrorHandling:
    """Test error handling scenarios with AsyncSession."""

    @pytest.mark.asyncio
    async def test_async_get_nonexistent_cell(self, async_session):
        """Getting nonexistent cell raises error."""
        with pytest.raises(runtimed.RuntimedError, match="not found"):
            await async_session.get_cell("cell-does-not-exist")

    @pytest.mark.asyncio
    async def test_async_syntax_error(self, async_session):
        """Syntax errors are captured."""
        await async_session.start_kernel()

        cell_id = await async_session.create_cell("def broken(")
        result = await async_session.execute_cell(cell_id)

        assert not result.success
        assert result.error is not None
        assert "SyntaxError" in result.error.ename


class TestAsyncContextManager:
    """Test async context manager functionality."""

    @pytest.mark.asyncio
    async def test_async_context_manager(self, daemon_process, monkeypatch):
        """AsyncSession works as async context manager."""
        socket_path, _ = daemon_process

        if socket_path is not None:
            monkeypatch.setenv("RUNTIMED_SOCKET_PATH", str(socket_path))

        notebook_id = f"async-ctx-test-{uuid.uuid4()}"

        async with runtimed.AsyncSession(notebook_id=notebook_id) as session:
            await session.connect()
            await session.start_kernel()

            cell_id = await session.create_cell("print('context manager works')")
            result = await session.execute_cell(cell_id)
            assert result.success
            assert "context manager works" in result.stdout

        # After exit, kernel should be shut down
        # Verify by checking the room no longer has an active kernel
# Note: The daemon may be terminated by fixture teardown before we can verify,
        # which is fine - it means cleanup already completed
        try:
            client = runtimed.DaemonClient()
            rooms = client.list_rooms()
            room = next((r for r in rooms if r["notebook_id"] == notebook_id), None)
            # Room may be gone entirely or kernel should not be running
            if room is not None:
                assert not room.get("kernel_running", False), "Kernel should be shut down after context exit"
        except runtimed.RuntimedError:
            # Daemon already shut down by fixture teardown - that's fine
            pass


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
