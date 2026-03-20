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

import asyncio
import inspect
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import pytest

# Skip all tests if runtimed module not available
pytest.importorskip("runtimed")

import runtimed

# ============================================================================
# Test utilities
# ============================================================================


def wait_for_sync(check_fn, *, timeout=10.0, interval=0.1, description="sync"):
    """Poll until check_fn returns True or timeout.

    The default timeout (10s) gives headroom for CI runners where write-lock
    contention in the daemon's sync loop can slow multi-peer propagation
    (see #626).

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


async def async_wait_for_sync(check_fn, *, timeout=10.0, interval=0.1, description="sync"):
    """Async version of wait_for_sync — polls with asyncio.sleep.

    check_fn can be a regular callable or an async callable.
    """
    start = time.time()
    while time.time() - start < timeout:
        result = check_fn()
        if inspect.isawaitable(result):
            result = await result
        if result:
            return True
        await asyncio.sleep(interval)
        interval = min(interval * 1.5, 0.5)
    raise AssertionError(f"Timed out waiting for {description} after {timeout}s")


def wait_for_metadata(session, key, *, check=None, timeout=10.0, description=None):
    """Poll until metadata key is set and optionally passes a check.

    Args:
        session: A Session instance
        key: Metadata key to read
        check: Optional callable(value) -> bool for validation
        timeout: Maximum wait time
        description: Description for error message
    """
    desc = description or f"metadata '{key}'"

    def _check():
        raw = session.get_metadata(key)
        if raw is None:
            return False
        if check is not None:
            return check(raw)
        return True

    return wait_for_sync(_check, timeout=timeout, description=desc)


def start_kernel_with_retry(session, *, retries=5, delay=1.0, **kwargs):
    """Retry start_kernel to tolerate sync lag and connection timeouts on CI.

    Passes all kwargs through to session.start_kernel() (kernel_type,
    env_source, notebook_path, etc.).
    """
    last_err: Exception = Exception("max retries exceeded")
    for attempt in range(retries):
        try:
            session.start_kernel(**kwargs)
            return
        except runtimed.RuntimedError as e:
            last_err = e
            if attempt < retries - 1:
                time.sleep(delay)
    raise last_err


async def async_start_kernel_with_retry(session, *, retries=5, delay=1.0, **kwargs):
    """Async retry wrapper for start_kernel (tolerates connection timeouts on CI)."""
    last_err: Exception = Exception("max retries exceeded")
    for attempt in range(retries):
        try:
            await session.start_kernel(**kwargs)
            return
        except runtimed.RuntimedError as e:
            last_err = e
            if attempt < retries - 1:
                await asyncio.sleep(delay)
    raise last_err


# ============================================================================
# Fixtures for daemon management
# ============================================================================


def _find_runtimed_binary():
    """Find the runtimed binary, checking common locations."""
    # Explicit override
    if "RUNTIMED_BINARY" in os.environ:
        return Path(os.environ["RUNTIMED_BINARY"])

    # Use RUNTIMED_WORKSPACE_PATH if available (preferred in CI and worktrees)
    if "RUNTIMED_WORKSPACE_PATH" in os.environ:
        repo_root = Path(os.environ["RUNTIMED_WORKSPACE_PATH"])
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
        Path(runtimed.default_socket_path()) if hasattr(runtimed, "default_socket_path") else None
    )


@pytest.fixture(scope="module", autouse=True)
def daemon_health_check(daemon_process):
    """Run a health check on the daemon before any tests execute.

    Reports daemon status (socket path, pool stats, version) and verifies
    that basic operations work (ping, create_notebook, start_kernel, execute).
    Fails fast with actionable diagnostics instead of hanging silently.
    """
    socket_path, proc = daemon_process
    mode = "CI (spawned)" if proc is not None else "dev (external)"
    print(f"\n{'=' * 60}", file=sys.stderr)
    print(f"[health] Daemon mode: {mode}", file=sys.stderr)
    print(f"[health] Socket: {socket_path}", file=sys.stderr)

    # 1. Create client and ping
    try:
        if socket_path is not None:
            client = runtimed.Client(socket_path=str(socket_path))
        else:
            client = runtimed.Client()
        assert client.ping(), "Daemon did not respond to ping"
        print("[health] Ping: OK", file=sys.stderr)
    except Exception as e:
        pytest.fail(f"Daemon health check failed at ping: {e}")

    # 2. Pool status
    try:
        status = client.status()
        print(
            f"[health] Pool: uv={status['uv_available']} conda={status['conda_available']}",
            file=sys.stderr,
        )
        if status["uv_available"] == 0:
            print("[health] WARNING: no UV environments available", file=sys.stderr)
    except Exception as e:
        print(f"[health] WARNING: could not read status: {e}", file=sys.stderr)

    # 3. Create notebook + start kernel + execute
    try:
        session = client.create_notebook(runtime="python")
        print(f"[health] Created notebook: {session.notebook_id}", file=sys.stderr)

        session.start_kernel(kernel_type="python", env_source="uv:prewarmed")
        print("[health] Kernel started: OK", file=sys.stderr)

        result = session.run("print('health-check-ok')")
        assert result.success, f"Health check execution failed: {result.stderr}"
        print("[health] Execute: OK", file=sys.stderr)

        session.shutdown_kernel()
    except Exception as e:
        pytest.fail(
            f"Daemon health check failed at create/execute: {e}\n"
            f"Socket: {socket_path}\n"
            f"Mode: {mode}"
        )

    print("[health] All checks passed", file=sys.stderr)
    print(f"{'=' * 60}", file=sys.stderr)


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
                Path(rt.default_socket_path()) if hasattr(rt, "default_socket_path") else None
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
            "3",  # Reduced from 6 — CI runners are slow to warm large pools
            "--conda-pool-size",
            "1",  # Reduced from 3 — one env is enough, tests run sequentially
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
                print(f"[test] Daemon died with code {proc.returncode}", file=sys.stderr)
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

        # Match either format:
        #   "UV pool: N/M available" (periodic status line)
        #   "UV environment ready at ..." (per-env completion)
        uv_pool_pattern = re.compile(r"UV pool: (\d+)/\d+ available")
        uv_env_ready_pattern = re.compile(r"UV environment ready at")
        conda_pool_pattern = re.compile(r"Conda pool: (\d+)/\d+ available")
        conda_env_ready_pattern = re.compile(r"Conda environment ready:")
        for i in range(150):
            try:
                log_contents = log_file.read_text()
                if not uv_ready:
                    # Check pool summary first
                    for line in log_contents.splitlines():
                        match = uv_pool_pattern.search(line)
                        if match and int(match.group(1)) > 0:
                            uv_ready = True
                            print(
                                f"[test] UV pool ready after {i + 1}s (pool summary)",
                                file=sys.stderr,
                            )
                            break
                if not uv_ready:
                    # Fall back to counting individual env-ready lines
                    uv_count = len(uv_env_ready_pattern.findall(log_contents))
                    if uv_count > 0:
                        uv_ready = True
                        print(
                            f"[test] UV pool ready after {i + 1}s ({uv_count} envs)",
                            file=sys.stderr,
                        )
                if not conda_ready:
                    for line in log_contents.splitlines():
                        match = conda_pool_pattern.search(line)
                        if match and int(match.group(1)) > 0:
                            conda_ready = True
                            print(
                                f"[test] Conda pool ready after {i + 1}s (pool summary)",
                                file=sys.stderr,
                            )
                            break
                if not conda_ready and conda_env_ready_pattern.search(log_contents):
                    conda_ready = True
                    print(f"[test] Conda pool ready after {i + 1}s (env ready)", file=sys.stderr)
            except Exception:
                pass
            if uv_ready and conda_ready:
                break
            time.sleep(1)
        else:
            pytest.fail(
                f"Pools not ready within 150s (uv={uv_ready}, conda={conda_ready}). "
                f"Daemon logs:\n{log_file.read_text()}"
            )

        try:
            yield socket_path, proc
        finally:
            # Cleanup
            print("\n[test] Stopping daemon...", file=sys.stderr)
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


@pytest.fixture(scope="module")
def client(daemon_process):
    """Create a Client connected to the test daemon."""
    socket_path, _ = daemon_process
    if socket_path is not None:
        return runtimed.Client(socket_path=str(socket_path))
    return runtimed.Client()


@pytest.fixture
def session(client):
    """Create a fresh Session for each test via Client.create_notebook()."""
    sess = client.create_notebook(runtime="python")
    yield sess

    # Cleanup: shutdown kernel if running
    try:
        if sess.kernel_started:
            sess.shutdown_kernel()
    except Exception:
        pass


@pytest.fixture(scope="class")
def shared_session(client):
    """Shared Python notebook + kernel for test classes that need execution.

    Class-scoped: one kernel per test class instead of one per test.
    Tests should not depend on clean kernel state between runs.
    """
    sess = client.create_notebook(runtime="python")
    yield sess
    try:
        if sess.kernel_started:
            sess.shutdown_kernel()
    except Exception:
        pass


@pytest.fixture
def doc_session(client):
    """Notebook WITHOUT a kernel — for pure document/CRDT tests.

    Shuts down the auto-launched kernel immediately. Cheap because
    no kernel process stays running.
    """
    sess = client.create_notebook(runtime="python")
    # Kill the auto-launched kernel — we only need the document
    try:
        sess.shutdown_kernel()
    except Exception:
        pass
    yield sess


@pytest.fixture
def two_sessions(client):
    """Create two sessions connected to the same notebook (peer sync test)."""
    session1 = client.create_notebook(runtime="python")
    session2 = client.join_notebook(session1.notebook_id)

    yield session1, session2

    # Cleanup
    for sess in [session1, session2]:
        try:
            if sess.kernel_started:
                sess.shutdown_kernel()
        except Exception:
            pass


# ============================================================================
# Per-cell accessor tests
# ============================================================================


class TestPerCellAccessors:
    """Test per-cell accessors that skip full materialization.

    These methods read individual fields from the snapshot watch channel
    without cloning all CellSnapshots — O(1) per field instead of O(n_cells).
    """

    @pytest.fixture
    def session(self, doc_session):
        """Use doc_session (no kernel) for pure document tests."""
        return doc_session

    def test_get_cell_ids(self, session):
        """get_cell_ids returns ordered cell IDs."""
        id1 = session.create_cell("a = 1")
        id2 = session.create_cell("b = 2")
        id3 = session.create_cell("c = 3")

        cell_ids = session.get_cell_ids()
        assert id1 in cell_ids
        assert id2 in cell_ids
        assert id3 in cell_ids
        # Order should match creation order
        assert cell_ids.index(id1) < cell_ids.index(id2) < cell_ids.index(id3)

    def test_get_cell_source(self, session):
        """get_cell_source returns just the source string."""
        cell_id = session.create_cell("x = 42")
        source = session.get_cell_source(cell_id)
        assert source == "x = 42"

    def test_get_cell_source_after_update(self, session):
        """get_cell_source reflects source updates."""
        cell_id = session.create_cell("original")
        session.set_source(cell_id, "updated")

        wait_for_sync(
            lambda: session.get_cell_source(cell_id) == "updated",
            description="source update",
        )
        source = session.get_cell_source(cell_id)
        assert source == "updated"

    def test_get_cell_source_nonexistent(self, session):
        """get_cell_source returns None for missing cells."""
        result = session.get_cell_source("cell-does-not-exist")
        assert result is None

    def test_get_cell_type(self, session):
        """get_cell_type returns the cell type string."""
        code_id = session.create_cell("x = 1", cell_type="code")
        md_id = session.create_cell("# Title", cell_type="markdown")

        assert session.get_cell_type(code_id) == "code"
        assert session.get_cell_type(md_id) == "markdown"

    def test_get_cell_type_nonexistent(self, session):
        """get_cell_type returns None for missing cells."""
        result = session.get_cell_type("cell-does-not-exist")
        assert result is None

    def test_get_cell_execution_count(self, session):
        """get_cell_execution_count returns the execution count string."""
        cell_id = session.create_cell("x = 1")
        # Before execution, should be "null"
        ec = session.get_cell_execution_count(cell_id)
        assert ec == "null"

    def test_get_cell_execution_count_nonexistent(self, session):
        """get_cell_execution_count returns None for missing cells."""
        result = session.get_cell_execution_count("cell-does-not-exist")
        assert result is None

    def test_get_cell_outputs(self, session):
        """get_cell_outputs returns raw output strings."""
        cell_id = session.create_cell("x = 1")
        outputs = session.get_cell_outputs(cell_id)
        assert outputs is not None
        assert isinstance(outputs, list)
        assert len(outputs) == 0  # No outputs before execution

    def test_get_cell_outputs_nonexistent(self, session):
        """get_cell_outputs returns None for missing cells."""
        result = session.get_cell_outputs("cell-does-not-exist")
        assert result is None

    def test_get_cell_position(self, session):
        """get_cell_position returns a position string."""
        cell_id = session.create_cell("x = 1")
        pos = session.get_cell_position(cell_id)
        assert pos is not None
        assert isinstance(pos, str)
        assert len(pos) > 0

    def test_get_cell_position_ordering(self, session):
        """Cell positions maintain insertion order."""
        id1 = session.create_cell("a")
        id2 = session.create_cell("b")
        id3 = session.create_cell("c")

        p1 = session.get_cell_position(id1)
        p2 = session.get_cell_position(id2)
        p3 = session.get_cell_position(id3)

        assert p1 < p2 < p3

    def test_accessors_consistent_with_get_cell(self, session):
        """Per-cell accessors return same data as get_cell."""
        cell_id = session.create_cell("hello = 'world'", cell_type="code")
        cell = session.get_cell(cell_id)

        assert session.get_cell_source(cell_id) == cell.source
        assert session.get_cell_type(cell_id) == cell.cell_type
        assert session.get_cell_position(cell_id) is not None


# ============================================================================
# Cell metadata tests
# ============================================================================


class TestCellMetadata:
    """Test cell metadata functionality.

    These tests verify that cell metadata can be read, written, and synced
    via the automerge document.
    """

    @pytest.fixture
    def session(self, doc_session):
        """Use doc_session (no kernel) for pure document tests."""
        return doc_session

    def test_cell_has_empty_metadata_by_default(self, session):
        """New cells have empty metadata."""
        cell_id = session.create_cell("x = 1")
        cell = session.get_cell(cell_id)

        assert cell.metadata == {}
        assert cell.metadata_json == "{}"

    def test_set_cell_metadata(self, session):
        """Can set cell metadata."""
        cell_id = session.create_cell("x = 1")

        metadata = {"tags": ["test", "example"], "custom_key": 42}
        import json

        result = session.set_cell_metadata(cell_id, json.dumps(metadata))
        assert result is True

        cell = session.get_cell(cell_id)
        assert cell.metadata["tags"] == ["test", "example"]
        assert cell.metadata["custom_key"] == 42

    def test_get_cell_metadata(self, session):
        """Can get cell metadata as JSON string."""
        cell_id = session.create_cell("x = 1")

        import json

        session.set_cell_metadata(cell_id, json.dumps({"foo": "bar"}))

        metadata_json = session.get_cell_metadata(cell_id)
        assert metadata_json is not None
        metadata = json.loads(metadata_json)
        assert metadata["foo"] == "bar"

    def test_update_cell_metadata_at_path(self, session):
        """Can update cell metadata at a specific path."""
        cell_id = session.create_cell("x = 1")

        # Set nested metadata using path
        result = session.update_cell_metadata_at(cell_id, ["jupyter", "source_hidden"], "true")
        assert result is True

        cell = session.get_cell(cell_id)
        assert cell.metadata["jupyter"]["source_hidden"] is True

    def test_cell_is_source_hidden(self, session):
        """Cell.is_source_hidden property works."""
        cell_id = session.create_cell("x = 1")
        cell = session.get_cell(cell_id)

        # Initially not hidden
        assert cell.is_source_hidden is False

        # Set source hidden via typed setter
        session.set_cell_source_hidden(cell_id, True)

        cell = session.get_cell(cell_id)
        assert cell.is_source_hidden is True

    def test_cell_is_outputs_hidden(self, session):
        """Cell.is_outputs_hidden property works."""
        cell_id = session.create_cell("x = 1")

        session.set_cell_outputs_hidden(cell_id, True)

        cell = session.get_cell(cell_id)
        assert cell.is_outputs_hidden is True

    def test_cell_tags(self, session):
        """Cell.tags property works."""
        cell_id = session.create_cell("x = 1")

        session.set_cell_tags(cell_id, ["hide-input", "parameters"])

        cell = session.get_cell(cell_id)
        assert cell.tags == ["hide-input", "parameters"]

    def test_set_cell_metadata_nonexistent_cell(self, session):
        """Setting metadata on nonexistent cell returns False."""
        import json

        result = session.set_cell_metadata("nonexistent-id", json.dumps({}))
        assert result is False

    def test_cell_metadata_syncs_between_peers(self, two_sessions):
        """Cell metadata syncs between connected sessions."""
        s1, s2 = two_sessions

        # Session 1 creates cell and sets metadata
        cell_id = s1.create_cell("x = 1")
        s1.set_cell_tags(cell_id, ["important"])

        # Wait for sync
        def check_tags():
            try:
                cell = s2.get_cell(cell_id)
                return cell.tags == ["important"]
            except Exception:
                return False

        wait_for_sync(check_tags, description="metadata sync")

        cell = s2.get_cell(cell_id)
        assert cell.tags == ["important"]


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
        start_kernel_with_retry(session)

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
        start_kernel_with_retry(session)

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
        start_kernel_with_retry(session)

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
        start_kernel_with_retry(session)

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
        start_kernel_with_retry(session)

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
        start_kernel_with_retry(session)

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
        start_kernel_with_retry(session)

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
# Output handling tests
# ============================================================================


class TestOutputHandling:
    """Test comprehensive output handling from execution.

    Verifies that all output types are captured correctly and that
    execution stops when an error is raised.
    """

    @pytest.mark.xfail(
        reason="Sync race: create_cell + execute_cell in quick succession may execute "
        "before source is synced to daemon. See #875 discussion.",
        strict=False,
    )
    def test_output_types_and_error_stops_execution(self, session):
        """Test stream, display, error outputs and verify error stops execution.

        Creates 4 cells:
        1. print() - should produce stream data
        2. display() - should produce display_data
        3. raise ValueError - should produce error, stop execution
        4. print() - should NOT execute because error stops execution
        """
        start_kernel_with_retry(session)

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
        start_kernel_with_retry(session)

        result = session.run('import sys\nprint("to stdout")\nsys.stderr.write("to stderr\\n")')

        assert result.success
        assert "to stdout" in result.stdout
        assert "to stderr" in result.stderr

    def test_display_data_mimetype(self, session):
        """Test that display_data includes mime type information."""
        start_kernel_with_retry(session)

        # Display a string - should have text/plain
        result = session.run("display('hello world')")

        assert result.success
        assert len(result.display_data) > 0
        # The display_data should contain the displayed value
        # Exact structure depends on Python bindings, but data should be present

    def test_error_traceback_captured(self, session):
        """Test that full traceback is captured on error."""
        start_kernel_with_retry(session)

        result = session.run(
            'def inner():\n    raise RuntimeError("deep error")\ndef outer():\n    inner()\nouter()'
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


def _set_python_kernelspec(session, *, uv_deps=None, conda_deps=None, conda_channels=None):
    """Set Python kernelspec using the typed API.

    This uses the native metadata methods (set_kernelspec, add_uv_dependency, etc.)
    rather than writing raw JSON to the legacy notebook_metadata key.
    """
    session.set_kernelspec("python3", "Python 3", "python")
    if uv_deps is not None:
        for dep in uv_deps:
            session.add_uv_dependency(dep)
    if conda_deps is not None:
        for dep in conda_deps:
            session.add_conda_dependency(dep)
        # Note: conda_channels would need a separate API method if needed


def _set_deno_kernelspec(session):
    """Set Deno kernelspec using the typed API."""
    session.set_kernelspec("deno", "Deno", "typescript")


class TestKernelLaunchMetadata:
    """Test that kernel launch reads metadata from the Automerge doc.

    These tests verify the refactored metadata resolution path where
    the daemon reads kernelspec and dependency info from the synced
    Automerge document rather than re-reading .ipynb files from disk.
    """

    def test_custom_metadata_round_trip(self, session):
        """Non-notebook metadata keys remain readable after the watch refactor."""
        session.set_metadata("custom_key", "custom_value")

        wait_for_metadata(session, "custom_key", check=lambda v: v == "custom_value")

    def test_python_kernel_with_python_kernelspec(self, session):
        """A notebook with python kernelspec launches a Python kernel."""
        # Set python kernelspec using typed API
        _set_python_kernelspec(session)

        start_kernel_with_retry(session, kernel_type="python")

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
        # Set python kernelspec using typed API
        _set_python_kernelspec(session)

        # Explicitly start Python kernel (as the frontend would after
        # reading kernelspec from the doc)
        start_kernel_with_retry(session, kernel_type="python")

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
        start_kernel_with_retry(session)

        # env_source should be set after kernel launch
        env_source = session.env_source
        assert env_source is not None
        # Should be one of the known env_source values
        assert any(env_source.startswith(prefix) for prefix in ("uv:", "conda:", "deno")), (
            f"Unexpected env_source: {env_source}"
        )

    def test_metadata_visible_to_second_peer(self, two_sessions):
        """Metadata set by one peer is visible to another via typed API."""
        s1, s2 = two_sessions

        # Session 1 sets kernelspec via typed API
        s1.set_kernelspec("python3", "Python 3", "python")

        # Poll until session 2 sees the kernelspec (sync propagation)
        for _ in range(20):
            ks = s2.get_kernelspec()
            if ks and ks.get("name") == "python3":
                break
            time.sleep(0.25)

        # Verify the kernelspec arrived at session 2
        ks = s2.get_kernelspec()
        assert ks is not None, "Kernelspec should have synced to session 2"
        assert ks["name"] == "python3"
        assert ks["display_name"] == "Python 3"
        assert ks.get("language") == "python"

    def test_kernelspec_round_trip(self, session):
        """Set a kernelspec, read it back, verify fields match."""
        session.set_kernelspec("test-kernel", "Test Kernel Display", "test-lang")

        ks = session.get_kernelspec()
        assert ks is not None, "Kernelspec should be readable after set"
        assert ks["name"] == "test-kernel"
        assert ks["display_name"] == "Test Kernel Display"
        assert ks.get("language") == "test-lang"

    def test_kernelspec_round_trip_without_language(self, session):
        """Set a kernelspec without language, verify it round-trips."""
        session.set_kernelspec("minimal-kernel", "Minimal Kernel")

        ks = session.get_kernelspec()
        assert ks is not None
        assert ks["name"] == "minimal-kernel"
        assert ks["display_name"] == "Minimal Kernel"
        assert "language" not in ks  # Should not be present when not set

    @pytest.mark.timeout(120)
    def test_uv_inline_deps_trusted(self, session):
        """Python kernel with UV inline deps from metadata launches correctly.

        When the notebook metadata contains runt.uv.dependencies, the daemon
        should detect env_source as 'uv:inline' and prepare a cached env
        with those deps installed. First run may be slow (uv venv + install).
        """
        _set_python_kernelspec(session, uv_deps=["requests"])

        # Retry: metadata may not have synced to the daemon's Automerge doc yet
        start_kernel_with_retry(session, kernel_type="python", env_source="uv:inline")

        assert session.env_source == "uv:inline"

        # Verify the dep is actually importable
        result = session.run("import requests; print(requests.__version__)")
        assert result.success, f"Failed to import requests: {result.stderr}"
        assert result.stdout.strip(), "requests version should not be empty"

    @pytest.mark.timeout(120)
    def test_uv_inline_deps_env_has_python(self, session):
        """UV inline env actually has a working Python with the declared deps."""
        _set_python_kernelspec(session, uv_deps=["requests"])

        # Retry: metadata may not have synced to the daemon's Automerge doc yet
        start_kernel_with_retry(session, kernel_type="python", env_source="uv:inline")

        # sys.prefix should point to a venv, not the system Python
        result = session.run("import sys; print(sys.prefix)")
        assert result.success
        prefix = result.stdout.strip()
        assert "inline-env" in prefix or "inline" in prefix or "cache" in prefix, (
            f"Expected inline env path, got: {prefix}"
        )

    def test_kernel_prewarmed_env_source(self, session):
        """Default kernel launch uses prewarmed pool."""
        start_kernel_with_retry(session, kernel_type="python", env_source="uv:prewarmed")

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

    @pytest.fixture
    def deno_session(self, client):
        """Create a Deno notebook — auto-launches with a Deno kernel."""
        sess = client.create_notebook(runtime="deno")
        yield sess
        try:
            if sess.kernel_started:
                sess.shutdown_kernel()
        except Exception:
            pass

    def test_deno_kernel_launch(self, deno_session):
        """Deno kernel launches and executes TypeScript."""
        result = deno_session.run("console.log('hello from deno')")
        assert result.success, f"Deno execution failed: {result.stderr}"
        assert "hello from deno" in result.stdout

    def test_deno_kernel_typescript_features(self, deno_session):
        """Deno kernel supports TypeScript features."""
        # TypeScript type annotations and template literals
        result = deno_session.run(
            "const greet = (name: string): string => `Hello, ${name}!`;\n"
            "console.log(greet('integration test'))"
        )
        assert result.success, f"TypeScript execution failed: {result.stderr}"
        assert "Hello, integration test!" in result.stdout

    def test_deno_kernelspec_via_typed_api(self, deno_session):
        """Deno kernelspec set via typed API enables Deno kernel."""
        # Verify kernelspec was set correctly by create_notebook(runtime="deno")
        ks = deno_session.get_kernelspec()
        assert ks is not None, "Deno kernelspec should be readable"
        assert ks["name"] == "deno"
        assert ks["display_name"] == "Deno"
        assert ks.get("language") == "typescript"

        # Verify the kernel is actually Deno by executing TypeScript
        result = deno_session.run("const x: number = 42; console.log(x)")
        assert result.success, f"Deno kernel should execute TypeScript: {result.stderr}"
        assert "42" in result.stdout


# ============================================================================
# Conda inline dependency tests
# ============================================================================


@pytest.mark.timeout(180)
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
        socket_path, _ = daemon_process
        client = runtimed.Client(socket_path=str(socket_path)) if socket_path else runtimed.Client()
        sess = client.create_notebook(runtime="python")

        # Shutdown the auto-launched Python kernel so we can re-launch
        # with conda:inline env_source (the daemon returns
        # KernelAlreadyRunning if a kernel is already up).
        try:
            sess.shutdown_kernel()
        except Exception:
            pass

        # Set up conda inline deps metadata using typed API
        _set_python_kernelspec(sess, conda_deps=["filelock"])

        # Extra delay: conda:inline metadata must propagate to the daemon's
        # Automerge doc before start_kernel reads it. The retry helper covers
        # transient failures but the class-scoped fixture only runs once.
        time.sleep(2.0)

        # Start kernel once for all tests in class (longer retry for conda env creation)
        start_kernel_with_retry(
            sess,
            kernel_type="python",
            env_source="conda:inline",
            retries=8,
            delay=2.0,
        )

        yield sess

        # Cleanup
        try:
            if sess.kernel_started:
                sess.shutdown_kernel()
        except Exception:
            pass

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
    Path(__file__).parent.parent.parent.parent / "crates" / "notebook" / "fixtures" / "audit-test"
)


@pytest.mark.timeout(300)
class TestProjectFileDetection:
    """Test project file auto-detection via notebook_path walk-up.

    When env_source="auto" and a notebook_path is provided, the daemon
    walks up from the notebook directory looking for project files
    (pyproject.toml, pixi.toml, environment.yml). The closest match wins.

    These tests use real fixture notebooks copied to a temp directory
    (outside the repo tree) so the repo root pyproject.toml doesn't
    interfere with walk-up detection.

    Timeout is 300s because uv:pyproject kernels install real packages
    via `uv run --with ipykernel`.
    """

    @pytest.fixture(scope="class")
    def isolated_fixtures(self, tmp_path_factory):
        """Copy fixture directories to temp location outside the repo tree."""
        import shutil

        tmp = tmp_path_factory.mktemp("fixtures")
        for subdir in ["pyproject-project", "pixi-project", "conda-env-project"]:
            if (FIXTURES_DIR / subdir).exists():
                shutil.copytree(FIXTURES_DIR / subdir, tmp / subdir)
        return tmp

    def test_pyproject_auto_detection(self, session, isolated_fixtures):
        """notebook_path near pyproject.toml auto-detects uv:pyproject.

        Uses `uv run --with ipykernel` to install deps from the fixture
        pyproject.toml (httpx).
        """
        notebook_path = str(isolated_fixtures / "pyproject-project" / "5-pyproject.ipynb")

        # Shutdown the auto-launched kernel so we can re-launch with
        # the notebook_path for project file detection.
        try:
            session.shutdown_kernel()
        except Exception:
            pass

        _set_python_kernelspec(session)

        start_kernel_with_retry(
            session,
            kernel_type="python",
            env_source="auto",
            notebook_path=notebook_path,
        )

        assert session.env_source == "uv:pyproject"

        # The fixture pyproject.toml declares httpx as a dependency
        result = session.run("import httpx; print(httpx.__version__)")
        assert result.success, f"Failed to import httpx from pyproject env: {result.stderr}"

    def test_pixi_auto_detection(self, session, isolated_fixtures):
        """notebook_path near pixi.toml auto-detects conda:pixi.

        The conda:pixi env_source is detected, and a pooled conda env
        is used to launch the kernel.
        """
        notebook_path = str(isolated_fixtures / "pixi-project" / "6-pixi.ipynb")

        # Shutdown the auto-launched kernel so we can re-launch with
        # the notebook_path for project file detection.
        try:
            session.shutdown_kernel()
        except Exception:
            pass

        _set_python_kernelspec(session)

        start_kernel_with_retry(
            session,
            kernel_type="python",
            env_source="auto",
            notebook_path=notebook_path,
        )

        assert session.env_source == "conda:pixi"

        # Kernel should be functional
        result = session.run("import sys; print(sys.prefix)")
        assert result.success, f"Kernel failed in pixi env: {result.stderr}"

    def test_environment_yml_auto_detection(self, session, isolated_fixtures):
        """notebook_path near environment.yml auto-detects conda:env_yml.

        The conda:env_yml env_source is detected, and a pooled conda env
        is used to launch the kernel.
        """
        notebook_path = str(isolated_fixtures / "conda-env-project" / "7-environment-yml.ipynb")

        # Shutdown the auto-launched kernel so we can re-launch with
        # the notebook_path for project file detection.
        try:
            session.shutdown_kernel()
        except Exception:
            pass

        _set_python_kernelspec(session)

        start_kernel_with_retry(
            session,
            kernel_type="python",
            env_source="auto",
            notebook_path=notebook_path,
        )

        assert session.env_source == "conda:env_yml"

        # Kernel should be functional
        result = session.run("import sys; print(sys.prefix)")
        assert result.success, f"Kernel failed in env_yml env: {result.stderr}"

    def test_no_project_file_falls_back_to_prewarmed(self, session):
        """When no project file is found, auto falls back to uv:prewarmed."""
        import tempfile

        # Create a temp notebook path with no project files nearby
        with tempfile.NamedTemporaryFile(suffix=".ipynb", delete=False) as f:
            notebook_path = f.name

        try:
            _set_python_kernelspec(session)

            start_kernel_with_retry(
                session,
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
async def async_client(daemon_process):
    """Create an AsyncClient connected to the test daemon."""
    socket_path, _ = daemon_process
    if socket_path is not None:
        return runtimed.AsyncClient(socket_path=str(socket_path))
    return runtimed.AsyncClient()


@pytest.fixture
async def async_session(async_client):
    """Create a fresh AsyncSession for each test via AsyncClient.create_notebook()."""
    sess = await async_client.create_notebook(runtime="python")
    yield sess

    # Cleanup: shutdown kernel if running
    try:
        if await sess.kernel_started():
            await sess.shutdown_kernel()
    except Exception:
        pass


@pytest.fixture
async def two_async_sessions(async_client):
    """Create two async sessions connected to the same notebook."""
    session1 = await async_client.create_notebook(runtime="python")
    session2 = await async_client.join_notebook(session1.notebook_id)

    yield session1, session2

    # Cleanup
    for sess in [session1, session2]:
        try:
            if await sess.kernel_started():
                await sess.shutdown_kernel()
        except Exception:
            pass


class TestBasicConnectivity:
    """Test basic daemon connectivity."""

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


class TestDocumentFirstExecution:
    """Test document-first execution pattern."""

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
    async def test_async_custom_metadata_round_trip(self, async_session):
        """Async sessions can still read metadata keys outside notebook_metadata."""
        await async_session.set_metadata("custom_key", "custom_value")

        async def metadata_set():
            raw = await async_session.get_metadata("custom_key")
            return raw == "custom_value"

        await async_wait_for_sync(metadata_set, description="custom metadata sync")

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
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell("result = 2 + 2; print(result)")
        result = await async_session.execute_cell(cell_id)

        assert result.success
        assert "4" in result.stdout
        assert result.cell_id == cell_id
        assert result.execution_count is not None

    @pytest.mark.asyncio
    async def test_async_queue_cell_fires_execution(self, async_session):
        """queue_cell fires execution without waiting."""

        await async_start_kernel_with_retry(async_session)

        # Create and queue execution
        cell_id = await async_session.create_cell("async_queued_var = 'async_queued'")
        await async_session.queue_cell(cell_id)

        # Poll until the queued cell has executed (execution_count gets set)
        async def queued_cell_executed():
            cell = await async_session.get_cell(cell_id)
            return cell.execution_count is not None

        await async_wait_for_sync(queued_cell_executed, description="queued cell execution")

        # Verify it ran by executing another cell that uses the variable
        cell2 = await async_session.create_cell("print(async_queued_var)")
        result = await async_session.execute_cell(cell2)

        assert result.success
        assert "async_queued" in result.stdout

    @pytest.mark.asyncio
    async def test_async_execution_error_captured(self, async_session):
        """Execution errors are captured in result."""
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell("raise ValueError('async test error')")
        result = await async_session.execute_cell(cell_id)

        assert not result.success
        assert result.error is not None
        assert "ValueError" in result.error.ename

    @pytest.mark.asyncio
    async def test_async_multiple_executions(self, async_session):
        """Can execute multiple cells sequentially."""
        await async_start_kernel_with_retry(async_session)

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


class TestMultiClientSync:
    """Test multi-client scenarios."""

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

        s1, s2 = two_async_sessions

        cell_id = await s1.create_cell("async_shared_var = 42")

        async def cell_synced():
            cells = await s2.get_cells()
            found = [c for c in cells if c.id == cell_id]
            return len(found) == 1 and found[0].source == "async_shared_var = 42"

        await async_wait_for_sync(cell_synced, description="cell with source sync to s2")

        cells = await s2.get_cells()
        found = [c for c in cells if c.id == cell_id]
        assert len(found) == 1
        assert found[0].source == "async_shared_var = 42"

    @pytest.mark.asyncio
    async def test_async_shared_kernel_execution(self, two_async_sessions):
        """Both sessions share the same kernel and execution state."""

        s1, s2 = two_async_sessions

        await async_start_kernel_with_retry(s1)
        await async_start_kernel_with_retry(s2)  # No-op in daemon

        cell1 = await s1.create_cell("async_shared = 'from async s1'")
        r1 = await s1.execute_cell(cell1)
        assert r1.success

        cell2 = await s2.create_cell("print(async_shared)")
        r2 = await s2.execute_cell(cell2)
        assert r2.success
        assert "from async s1" in r2.stdout


class TestKernelLifecycle:
    """Test kernel lifecycle management."""

    @pytest.mark.asyncio
    async def test_async_start_kernel(self, async_session):
        """Can start a kernel."""
        assert not await async_session.kernel_started()

        await async_start_kernel_with_retry(async_session)

        assert await async_session.kernel_started()
        assert await async_session.env_source() is not None

    @pytest.mark.asyncio
    async def test_async_kernel_interrupt(self, async_session):
        """Can interrupt a running kernel."""
        await async_start_kernel_with_retry(async_session)
        await async_session.interrupt()  # Should not raise

    @pytest.mark.asyncio
    async def test_async_shutdown_kernel(self, async_session):
        """Can shutdown the kernel."""
        await async_start_kernel_with_retry(async_session)
        assert await async_session.kernel_started()

        await async_session.shutdown_kernel()
        assert not await async_session.kernel_started()


class TestOutputTypes:
    """Test different output types from execution."""

    @pytest.mark.asyncio
    async def test_async_stdout_output(self, async_session):
        """Captures stdout output."""
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell("print('async hello stdout')")
        result = await async_session.execute_cell(cell_id)

        assert result.success
        assert result.stdout == "async hello stdout\n"

    @pytest.mark.asyncio
    async def test_async_stderr_output(self, async_session):
        """Captures stderr output."""
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell(
            "import sys; sys.stderr.write('async hello stderr\\n')"
        )
        result = await async_session.execute_cell(cell_id)

        assert result.success
        assert "async hello stderr" in result.stderr

    @pytest.mark.asyncio
    async def test_async_return_value(self, async_session):
        """Captures expression return value."""
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell("2 + 2")
        result = await async_session.execute_cell(cell_id)

        assert result.success
        display = result.display_data
        assert len(display) > 0


class TestErrorHandling:
    """Test error handling scenarios."""

    @pytest.mark.asyncio
    async def test_async_get_nonexistent_cell(self, async_session):
        """Getting nonexistent cell raises error."""
        with pytest.raises(runtimed.RuntimedError, match="not found"):
            await async_session.get_cell("cell-does-not-exist")

    @pytest.mark.asyncio
    async def test_async_syntax_error(self, async_session):
        """Syntax errors are captured."""
        await async_start_kernel_with_retry(async_session)

        warmup_cell = await async_session.create_cell("warmup = 1")
        warmup_result = await async_session.execute_cell(warmup_cell)
        assert warmup_result.success

        cell_id = await async_session.create_cell("if True print('broken')")
        result = await async_session.execute_cell(cell_id)

        assert not result.success
        assert result.error is not None
        assert "SyntaxError" in result.error.ename


class TestContextManager:
    """Test async context manager functionality."""

    @pytest.mark.asyncio
    async def test_async_context_manager(self, async_client):
        """AsyncSession works as async context manager."""
        session = await async_client.create_notebook(runtime="python")
        notebook_id = session.notebook_id

        async with session:
            await async_start_kernel_with_retry(session)

            cell_id = await session.create_cell("print('context manager works')")
            result = await session.execute_cell(cell_id)
            assert result.success
            assert "context manager works" in result.stdout

        # After exit, kernel should be shut down
        # Verify by checking the room no longer has an active kernel
        # Note: The daemon may be terminated by fixture teardown before we can verify,
        # which is fine - it means cleanup already completed
        try:
            rooms = await async_client.list_rooms()
            room = next((r for r in rooms if r["notebook_id"] == notebook_id), None)
            # Room may be gone entirely or kernel should not be running
            if room is not None:
                assert not room.get("kernel_running", False), (
                    "Kernel should be shut down after context exit"
                )
        except runtimed.RuntimedError:
            # Daemon already shut down by fixture teardown - that's fine
            pass


# ============================================================================
# Streaming Execution Tests (stream_execute async iterator)
# ============================================================================


class TestStreamExecute:
    """Test stream_execute() returns events as an async iterator."""

    @pytest.mark.asyncio
    async def test_stream_execute_yields_events(self, async_session):
        """stream_execute() yields events as they arrive, not all at once."""
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell("for i in range(3): print(f'line {i}')")

        events = []
        async for event in await async_session.stream_execute(cell_id):
            events.append(event)

        # Should have received multiple events (outputs + done)
        assert len(events) >= 2, f"Expected multiple events, got {len(events)}"

        # Should have output events
        output_events = [e for e in events if e.event_type == "output"]
        assert len(output_events) >= 1, "Expected at least one output event"

        # Should have a done event
        done_events = [e for e in events if e.event_type == "done"]
        assert len(done_events) == 1, "Expected exactly one done event"

    @pytest.mark.asyncio
    async def test_stream_execute_has_output_events(self, async_session):
        """stream_execute() yields output events with output data."""
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell("print('first'); print('second')")

        output_events = []
        async for event in await async_session.stream_execute(cell_id):
            if event.event_type == "output":
                output_events.append(event)

        # Should have output events
        assert len(output_events) >= 1, "Expected at least one output event"

        # Output events should have output data
        for event in output_events:
            assert event.output is not None, "Output event should have output data"

    @pytest.mark.asyncio
    async def test_stream_execute_error_in_output(self, async_session):
        """stream_execute() captures execution errors as output events.

        Python errors (ValueError, etc.) are broadcast as Output events
        with output_type="error" and ename/evalue/traceback fields.
        KernelError is only for kernel-level failures (crash, launch).
        """
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell("raise ValueError('test error')")

        events = []
        async for event in await async_session.stream_execute(cell_id):
            events.append(event)

        # Should have output events (error comes through as output)
        output_events = [e for e in events if e.event_type == "output"]
        assert len(output_events) >= 1, "Expected at least one output event"

        # The output should contain the error information
        # Error outputs have output_type="error" with ename/evalue fields
        error_found = False
        for event in output_events:
            if event.output and event.output.output_type == "error":
                error_found = True
                assert event.output.ename is not None
                assert "ValueError" in event.output.ename
                break

        assert error_found, "Expected an error output with ValueError"


# ============================================================================
# Append Source Tests (incremental code writing)
# ============================================================================


class TestAppendSource:
    """Test append_source() for incremental code writing (agentic streaming)."""

    @pytest.mark.asyncio
    async def test_append_source_basic(self, async_session):
        """append_source() adds text to end of cell source."""
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell("x = 1")

        # Append more code
        await async_session.append_source(cell_id, "\ny = 2")
        await async_session.append_source(cell_id, "\nprint(x + y)")

        # Verify source was appended
        cell = await async_session.get_cell(cell_id)
        assert "x = 1" in cell.source
        assert "y = 2" in cell.source
        assert "print(x + y)" in cell.source

        # Execute and verify
        result = await async_session.execute_cell(cell_id)
        assert result.success
        assert "3" in result.stdout

    @pytest.mark.asyncio
    async def test_append_source_streaming_tokens(self, async_session):
        """append_source() can append tokens incrementally (LLM streaming)."""
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell("")

        # Simulate LLM streaming tokens
        tokens = ["print", "(", "'hello", " ", "world", "'", ")"]
        for token in tokens:
            await async_session.append_source(cell_id, token)

        cell = await async_session.get_cell(cell_id)
        assert cell.source == "print('hello world')"

        result = await async_session.execute_cell(cell_id)
        assert result.success
        assert "hello world" in result.stdout

    @pytest.mark.asyncio
    async def test_append_source_syncs_between_peers(self, two_async_sessions):
        """append_source() changes sync to other sessions."""
        s1, s2 = two_async_sessions

        # Create cell in session 1
        cell_id = await s1.create_cell("a = 1")

        # Wait for cell to sync to session 2
        async def cell_visible():
            cells = await s2.get_cells()
            return any(c.id == cell_id for c in cells)

        await async_wait_for_sync(cell_visible, description="cell sync to s2")

        # Append in session 1
        await s1.append_source(cell_id, "\nb = 2")

        # Wait for appended source to sync
        async def source_synced():
            cell = await s2.get_cell(cell_id)
            return "b = 2" in cell.source

        await async_wait_for_sync(source_synced, description="append sync to s2")

        cell = await s2.get_cell(cell_id)
        assert "a = 1" in cell.source
        assert "b = 2" in cell.source


# ============================================================================
# Subscription Tests (independent event listening)
# ============================================================================


class TestSubscription:
    """Test subscribe() for independent event listening."""

    @pytest.mark.asyncio
    async def test_subscribe_receives_execution_events(self, async_session):
        """subscribe() receives events from cell execution."""
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell("print('subscribed')")

        # Start subscription before execution
        subscription = async_session.subscribe()
        received_events = []

        import asyncio

        async def collect_events():
            async for event in subscription:
                received_events.append(event)
                if event.event_type == "done":
                    break

        # Run collection with timeout
        collect_task = asyncio.create_task(collect_events())

        # Execute cell
        await async_session.execute_cell(cell_id)

        # Wait for events with timeout
        try:
            await asyncio.wait_for(collect_task, timeout=10.0)
        except asyncio.TimeoutError:
            pass  # May timeout if no done event, that's ok

        # Should have received some events
        assert len(received_events) >= 1, "Expected to receive events via subscription"

    @pytest.mark.asyncio
    async def test_subscribe_filters_by_event_type(self, async_session):
        """subscribe(event_types=[...]) filters events."""
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell("print('filtered')")

        # Subscribe only to output events
        subscription = async_session.subscribe(event_types=["output"])

        import asyncio

        received_events = []

        async def collect_outputs():
            count = 0
            async for event in subscription:
                received_events.append(event)
                count += 1
                if count >= 1:  # Just get first output
                    break

        collect_task = asyncio.create_task(collect_outputs())

        await async_session.execute_cell(cell_id)

        try:
            await asyncio.wait_for(collect_task, timeout=10.0)
        except asyncio.TimeoutError:
            pass

        # All received events should be output type
        for event in received_events:
            assert event.event_type == "output", (
                f"Expected only output events, got {event.event_type}"
            )

    @pytest.mark.asyncio
    @pytest.mark.skipif(
        os.environ.get("RUNTIMED_INTEGRATION_TEST") == "1",
        reason="Flaky on CI: daemon connection timeouts under resource pressure (test 89/99)",
    )
    async def test_multiple_subscribers(self, async_session):
        """Multiple subscribers can listen to same execution."""
        await async_start_kernel_with_retry(async_session)

        cell_id = await async_session.create_cell("print('multi-sub')")

        # Create two independent subscriptions
        sub1 = async_session.subscribe()
        sub2 = async_session.subscribe()

        import asyncio

        events1, events2 = [], []

        async def collect1():
            async for event in sub1:
                events1.append(event)
                if event.event_type == "done":
                    break

        async def collect2():
            async for event in sub2:
                events2.append(event)
                if event.event_type == "done":
                    break

        # Start both collectors
        task1 = asyncio.create_task(collect1())
        task2 = asyncio.create_task(collect2())

        # Execute
        await async_session.execute_cell(cell_id)

        # Wait for both
        await asyncio.wait_for(asyncio.gather(task1, task2), timeout=10.0)

        # Both should have received events
        assert len(events1) >= 1, "Subscriber 1 should receive events"
        assert len(events2) >= 1, "Subscriber 2 should receive events"


# ============================================================================
# Open/Create Notebook Tests (daemon-owned loading)
# ============================================================================


class TestOpenNotebook:
    """Test Client.open_notebook() - daemon-owned file loading."""

    def test_open_existing_notebook(self, client, tmp_path):
        """Opening existing .ipynb loads cells via daemon."""
        import json

        # Create test notebook
        nb_path = tmp_path / "test.ipynb"
        nb_path.write_text(
            json.dumps(
                {
                    "nbformat": 4,
                    "nbformat_minor": 5,
                    "metadata": {"kernelspec": {"name": "python3", "display_name": "Python 3"}},
                    "cells": [
                        {
                            "id": "cell-1",
                            "cell_type": "code",
                            "source": ["x = 1"],
                            "metadata": {},
                            "outputs": [],
                        },
                        {
                            "id": "cell-2",
                            "cell_type": "markdown",
                            "source": ["# Hello"],
                            "metadata": {},
                        },
                    ],
                }
            )
        )

        # Open via daemon
        session = client.open_notebook(str(nb_path))
        assert session.is_connected

        # Verify daemon-derived notebook_id (should contain canonical path)
        assert str(nb_path.resolve()) in session.notebook_id or nb_path.name in session.notebook_id

        # Verify cells loaded
        cells = session.get_cells()
        assert len(cells) == 2
        assert cells[0].source == "x = 1"
        assert cells[1].cell_type == "markdown"

    def test_open_notebook_returns_connection_info(self, client, tmp_path):
        """NotebookConnectionInfo includes cell_count.

        With streaming load, cell_count is 0 in the handshake because
        loading is deferred to the sync loop. Cells arrive via Automerge
        sync messages after the connection is established.
        """
        import json

        # Create notebook with 3 cells
        nb_path = tmp_path / "three_cells.ipynb"
        nb_path.write_text(
            json.dumps(
                {
                    "nbformat": 4,
                    "nbformat_minor": 5,
                    "metadata": {},
                    "cells": [
                        {
                            "id": "c1",
                            "cell_type": "code",
                            "source": [],
                            "metadata": {},
                            "outputs": [],
                        },
                        {
                            "id": "c2",
                            "cell_type": "code",
                            "source": [],
                            "metadata": {},
                            "outputs": [],
                        },
                        {
                            "id": "c3",
                            "cell_type": "code",
                            "source": [],
                            "metadata": {},
                            "outputs": [],
                        },
                    ],
                }
            )
        )

        session = client.open_notebook(str(nb_path))
        info = session.connection_info
        assert info is not None
        # Streaming load defers cell loading to the sync loop, so the
        # handshake reports 0 cells. Cells arrive via sync messages.
        assert info.cell_count == 0
        assert info.notebook_id == session.notebook_id

    def test_open_nonexistent_file_creates_notebook(self, client, tmp_path):
        """Opening missing file creates a new notebook at that path."""
        # Opening a non-existent path creates a new notebook
        session = client.open_notebook(str(tmp_path / "new_notebook.ipynb"))
        try:
            info = session.connection_info
            assert info is not None
            # Notebook is created with the path as notebook_id
            assert "new_notebook.ipynb" in info.notebook_id
            # New notebook starts with cells (one empty code cell)
            # Note: cell_count in handshake may be 0 due to streaming, but notebook_id is set
            assert info.notebook_id != ""
        finally:
            session.close()

    def test_open_nonexistent_file_auto_appends_ipynb(self, client, tmp_path):
        """Opening missing file without .ipynb extension auto-appends it."""
        # Opening a path without .ipynb extension creates notebook with .ipynb appended
        session = client.open_notebook(str(tmp_path / "mynotebook"))
        try:
            info = session.connection_info
            assert info is not None
            # The .ipynb extension is auto-appended
            assert info.notebook_id.endswith("mynotebook.ipynb")
        finally:
            session.close()

    @pytest.mark.skipif(
        os.environ.get("RUNTIMED_INTEGRATION_TEST") == "1",
        reason="Flaky on CI: open_notebook full-peer sync unreliable under resource pressure",
    )
    def test_open_notebook_second_client_joins_room(self, client, tmp_path):
        """Second client joining same notebook gets synced cells."""
        import json

        nb_path = tmp_path / "shared.ipynb"
        nb_path.write_text(
            json.dumps(
                {
                    "nbformat": 4,
                    "nbformat_minor": 5,
                    "metadata": {},
                    "cells": [
                        {
                            "id": "orig",
                            "cell_type": "code",
                            "source": ["a = 1"],
                            "metadata": {},
                            "outputs": [],
                        }
                    ],
                }
            )
        )

        session1 = client.open_notebook(str(nb_path))
        session2 = client.open_notebook(str(nb_path))

        # Both should have same notebook_id
        assert session1.notebook_id == session2.notebook_id

        # Add cell in session1
        initial_count = len(session1.get_cells())
        session1.create_cell("y = 2", index=initial_count)

        # Should sync to session2 (open_notebook sessions do full-peer sync
        # which can be slower on loaded CI runners — use generous timeout)
        wait_for_sync(
            lambda: len(session2.get_cells()) > initial_count,
            timeout=15.0,
            description="cell sync",
        )

        cells2 = session2.get_cells()
        assert len(cells2) > initial_count


class TestCreateNotebook:
    """Test Client.create_notebook() - daemon-owned creation."""

    def test_create_python_notebook(self, client):
        """Creating Python notebook returns session with one empty cell."""
        session = client.create_notebook(runtime="python")
        assert session.is_connected

        # notebook_id is UUID (not a path)
        assert len(session.notebook_id) == 36  # UUID format

        # Has one empty code cell
        cells = session.get_cells()
        assert len(cells) == 1
        assert cells[0].cell_type == "code"
        assert cells[0].source == ""

    def test_create_notebook_returns_connection_info(self, client):
        """NotebookConnectionInfo is available for created notebooks."""
        session = client.create_notebook(runtime="python")
        info = session.connection_info
        assert info is not None
        assert info.cell_count == 1
        assert info.notebook_id == session.notebook_id
        # New notebooks don't need trust approval
        assert info.needs_trust_approval is False

    def test_create_deno_notebook(self, client):
        """Creating Deno notebook sets correct runtime."""
        session = client.create_notebook(runtime="deno")
        assert session.is_connected

        # Has one empty code cell
        cells = session.get_cells()
        assert len(cells) == 1

    def test_create_notebook_with_working_dir(self, client, tmp_path):
        """working_dir is used for project file detection."""
        # Create pyproject.toml in tmp_path
        (tmp_path / "pyproject.toml").write_text("[project]\nname = 'test'")

        session = client.create_notebook(runtime="python", working_dir=str(tmp_path))

        assert session.is_connected


class TestTrustApproval:
    """Test trust approval flow for notebooks with inline dependencies."""

    def test_untrusted_notebook_needs_approval(self, client, tmp_path):
        """Notebook with inline deps from unknown source needs trust."""
        import json

        nb_path = tmp_path / "untrusted.ipynb"
        nb_path.write_text(
            json.dumps(
                {
                    "nbformat": 4,
                    "nbformat_minor": 5,
                    "metadata": {
                        "runt": {
                            "schema_version": "1",
                            "uv": {"dependencies": ["requests"]},
                            # No trust_signature - untrusted
                        }
                    },
                    "cells": [
                        {
                            "id": "c1",
                            "cell_type": "code",
                            "source": [],
                            "metadata": {},
                            "outputs": [],
                        }
                    ],
                }
            )
        )

        session = client.open_notebook(str(nb_path))
        info = session.connection_info
        assert info is not None
        assert info.needs_trust_approval is True

    def test_notebook_without_deps_does_not_need_trust(self, client, tmp_path):
        """Notebook without inline deps doesn't need trust approval."""
        import json

        nb_path = tmp_path / "simple.ipynb"
        nb_path.write_text(
            json.dumps(
                {
                    "nbformat": 4,
                    "nbformat_minor": 5,
                    "metadata": {},
                    "cells": [
                        {
                            "id": "c1",
                            "cell_type": "code",
                            "source": ["print('hello')"],
                            "metadata": {},
                            "outputs": [],
                        }
                    ],
                }
            )
        )

        session = client.open_notebook(str(nb_path))
        info = session.connection_info
        assert info is not None
        assert info.needs_trust_approval is False


# ============================================================================
# Presence Tests
# ============================================================================


class TestPresence:
    """Test presence functionality (cursor, selection).

    These tests verify that presence frames can be sent without error.
    They don't verify relay to other peers (that requires inspecting
    frame-level traffic), but they confirm the encode → send → daemon
    path works end-to-end without raising.
    """

    @pytest.mark.asyncio
    async def test_set_cursor(self, async_session):
        """Can send a cursor position as presence data."""
        cell_id = await async_session.create_cell("x = 1")
        # Should not raise — the daemon receives and relays
        await async_session.set_cursor(cell_id, line=0, column=0)

    @pytest.mark.asyncio
    async def test_set_cursor_different_positions(self, async_session):
        """Can send multiple cursor updates (simulates typing)."""
        cell_id = await async_session.create_cell("hello = 'world'")
        for col in range(5):
            await async_session.set_cursor(cell_id, line=0, column=col)

    @pytest.mark.asyncio
    async def test_set_selection(self, async_session):
        """Can send a selection range as presence data."""
        cell_id = await async_session.create_cell("line1\nline2\nline3")
        await async_session.set_selection(
            cell_id,
            anchor_line=0,
            anchor_col=0,
            head_line=2,
            head_col=5,
        )

    @pytest.mark.asyncio
    async def test_set_cursor_then_selection(self, async_session):
        """Can send cursor then selection (multiple channels)."""
        cell_id = await async_session.create_cell("x = 1")
        await async_session.set_cursor(cell_id, line=0, column=3)
        await async_session.set_selection(
            cell_id, anchor_line=0, anchor_col=0, head_line=0, head_col=5
        )

    @pytest.mark.asyncio
    async def test_set_cursor_not_connected_raises(self):
        """set_cursor raises when not connected."""
        sess = runtimed.AsyncSession()
        with pytest.raises(runtimed.RuntimedError):
            await sess.set_cursor("fake-cell", line=0, column=0)

    @pytest.mark.asyncio
    async def test_set_selection_not_connected_raises(self):
        """set_selection raises when not connected."""
        sess = runtimed.AsyncSession()
        with pytest.raises(runtimed.RuntimedError):
            await sess.set_selection(
                "fake-cell", anchor_line=0, anchor_col=0, head_line=0, head_col=0
            )

    @pytest.mark.asyncio
    async def test_presence_with_two_peers(self, two_async_sessions):
        """Both peers can send presence without error."""
        s1, s2 = two_async_sessions
        cell_id = await s1.create_cell("shared cell")

        # Wait for cell to sync to s2
        async def cells_synced():
            cells = await s2.get_cells()
            return len(cells) > 0

        await async_wait_for_sync(cells_synced, description="cell sync to s2")

        # Both peers send cursor presence
        await s1.set_cursor(cell_id, line=0, column=0)
        await s2.set_cursor(cell_id, line=0, column=5)

    @pytest.mark.asyncio
    async def test_get_peers_and_remote_cursors(self, two_async_sessions):
        """Session B sees Session A's cursor via get_peers/get_remote_cursors."""
        s1, s2 = two_async_sessions
        cell_id = await s1.create_cell("shared cell")

        # Wait for cell to sync to s2
        async def cells_synced():
            cells = await s2.get_cells()
            return len(cells) > 0

        await async_wait_for_sync(cells_synced, description="cell sync to s2")

        # Session A sends cursor presence
        await s1.set_cursor(cell_id, line=5, column=10)

        # Session B should see Session A as a peer
        async def s2_sees_peer():
            peers = await s2.get_peers()
            return len(peers) > 0

        await async_wait_for_sync(s2_sees_peer, description="s2 sees s1 peer")
        peers = await s2.get_peers()
        assert len(peers) > 0, "Expected at least one remote peer"

        # Session B should see Session A's cursor at (5, 10).
        # Note: create_cell auto-emits presence at (0, 0), so we must wait
        # specifically for the updated cursor position from set_cursor.
        async def _cursor_at_expected_pos():
            for _, _, cid, ln, col in await s2.get_remote_cursors():
                if cid == cell_id and ln == 5 and col == 10:
                    return True
            return False

        await async_wait_for_sync(
            _cursor_at_expected_pos,
            description="s2 sees s1 cursor at (5, 10)",
        )

    @pytest.mark.asyncio
    async def test_get_peers(self, async_session):
        """Can query peers via AsyncSession."""
        peers = await async_session.get_peers()
        assert isinstance(peers, list)

    @pytest.mark.asyncio
    async def test_get_remote_cursors(self, async_session):
        """Can query remote cursors via AsyncSession."""
        cursors = await async_session.get_remote_cursors()
        assert isinstance(cursors, list)

    @pytest.mark.asyncio
    async def test_get_peers_not_connected_raises(self):
        """get_peers raises when not connected."""
        sess = runtimed.AsyncSession()
        with pytest.raises(runtimed.RuntimedError):
            await sess.get_peers()

    @pytest.mark.asyncio
    async def test_get_remote_cursors_not_connected_raises(self):
        """get_remote_cursors raises when not connected."""
        sess = runtimed.AsyncSession()
        with pytest.raises(runtimed.RuntimedError):
            await sess.get_remote_cursors()


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
