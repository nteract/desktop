"""Sidecar launcher for Jupyter kernels."""

from __future__ import annotations

import subprocess
from pathlib import Path

from runtimed._binary import find_binary


class Sidecar:
    """Handle to a running sidecar viewer process."""

    def __init__(self, process: subprocess.Popen, connection_file: Path) -> None:
        self._process = process
        self._connection_file = connection_file
        self._stderr_cache: str | None = None

    @property
    def process(self) -> subprocess.Popen:
        """The underlying subprocess."""
        return self._process

    @property
    def connection_file(self) -> Path:
        """Path to the kernel connection file."""
        return self._connection_file

    @property
    def running(self) -> bool:
        """Whether the sidecar process is still running."""
        return self._process.poll() is None

    @property
    def error(self) -> str | None:
        """Error message if the process has exited with an error, None otherwise.

        This reads stderr from the process, so it should only be called after
        the process has exited (when running is False).
        """
        if self.running:
            return None
        if self._process.returncode == 0:
            return None
        if self._stderr_cache is None:
            _, stderr = self._process.communicate()
            self._stderr_cache = stderr.decode().strip() if stderr else None
        return self._stderr_cache

    def close(self) -> None:
        """Terminate the sidecar process."""
        self._process.terminate()

    def __repr__(self) -> str:
        if self.running:
            status = "running"
        elif self._process.returncode == 0:
            status = "exited (0)"
        else:
            # Include error message in repr for failed processes
            err = self.error
            if err:
                # Take first line of error for brevity
                first_line = err.split("\n")[0]
                if len(first_line) > 60:
                    first_line = first_line[:57] + "..."
                status = f"failed: {first_line}"
            else:
                status = f"exited ({self._process.returncode})"
        kernel = self._connection_file.stem
        return f"Sidecar({kernel}, {status})"


class BridgedSidecar(Sidecar):
    """Sidecar with an IOPub bridge for plain IPython.

    In addition to the sidecar process, holds a reference to the
    IPythonBridge that publishes outputs on a synthetic IOPub channel.
    Closing this also shuts down the bridge and its ZMQ sockets.
    """

    def __init__(self, process: subprocess.Popen, connection_file: Path, bridge: object) -> None:
        super().__init__(process, connection_file)
        self._bridge = bridge

    def close(self) -> None:
        """Terminate the sidecar process and shut down the IOPub bridge."""
        super().close()
        self._bridge.close()  # type: ignore[attr-defined]

    def __repr__(self) -> str:
        status = "running" if self.running else f"exited ({self._process.returncode})"
        return f"BridgedSidecar(ipython-bridge, {status})"


def sidecar(
    connection_file: str | Path | None = None,
    *,
    quiet: bool = True,
    dump: str | Path | None = None,
) -> Sidecar:
    """Launch the sidecar viewer for a running Jupyter kernel.

    When called with no arguments from within a running IPython kernel,
    automatically detects the kernel's connection file. When called from
    plain IPython (not a Jupyter kernel), automatically creates an IOPub
    bridge that forwards outputs to the sidecar.

    Args:
        connection_file: Path to a kernel connection JSON file.
            If None, auto-detects from the running kernel, or creates
            an IOPub bridge if in plain IPython.
        quiet: Suppress sidecar log output. Defaults to True.
        dump: Optional path to dump all Jupyter messages as JSON.

    Returns:
        A Sidecar handle (or BridgedSidecar in plain IPython).

    Raises:
        RuntimeError: If connection_file is None and auto-detection fails.
        FileNotFoundError: If the runt binary cannot be found.
        FileNotFoundError: If the connection file does not exist.

    Example:
        In a Jupyter console, notebook, or plain IPython::

            import runtimed
            s = runtimed.sidecar()

        With an explicit connection file::

            import runtimed
            s = runtimed.sidecar("/path/to/kernel-12345.json")
    """
    if connection_file is None:
        env = _detect_environment()
        if env == "terminal":
            return _launch_bridged_sidecar(quiet=quiet, dump=dump)
        else:
            connection_file = _get_kernel_connection_file()

    connection_path = Path(connection_file)
    if not connection_path.exists():
        raise FileNotFoundError(f"Kernel connection file not found: {connection_path}")

    runt_bin = find_binary("runt")

    cmd: list[str] = [runt_bin, "sidecar"]
    if quiet:
        cmd.append("--quiet")
    if dump is not None:
        cmd.extend(["--dump", str(dump)])
    cmd.append(str(connection_path))

    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    return Sidecar(proc, connection_path)


def _detect_environment() -> str:
    """Detect whether we're in a Jupyter kernel, terminal IPython, or plain Python."""
    try:
        ip = get_ipython()  # type: ignore[name-defined]  # noqa: F821
    except NameError:
        return "plain_python"

    shell_class = type(ip).__name__
    if shell_class == "ZMQInteractiveShell":
        return "kernel"
    elif shell_class == "TerminalInteractiveShell":
        return "terminal"
    return "unknown"


def _launch_bridged_sidecar(*, quiet: bool, dump: str | Path | None) -> BridgedSidecar:
    """Create an IOPub bridge and launch the sidecar against it."""
    try:
        from runtimed._ipython_bridge import install_bridge
    except ImportError:
        raise RuntimeError(
            "IOPub bridge requires pyzmq. "
            "Install with: pip install runtimed[bridge]\n"
            "Or use 'jupyter console' instead of 'ipython'."
        ) from None

    import sys

    ip = get_ipython()  # type: ignore[name-defined]  # noqa: F821
    bridge = install_bridge(ip)

    runt_bin = find_binary("runt")
    cmd: list[str] = [runt_bin, "sidecar"]
    if quiet:
        cmd.append("--quiet")
    if dump is not None:
        cmd.extend(["--dump", str(dump)])
    cmd.append(str(bridge.connection_file))

    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )

    print(
        "Sidecar running in bridged mode (plain IPython).\n"
        "Rich output (HTML, images, LaTeX) is supported.\n"
        "Interactive widgets are not supported in this mode.\n"
        "For full widget support, use: jupyter console",
        file=sys.stderr,
    )

    return BridgedSidecar(proc, bridge.connection_file, bridge)


def _get_kernel_connection_file() -> str:
    """Auto-detect the connection file for the currently running kernel."""
    try:
        from ipykernel.connect import get_connection_file
    except ImportError:
        raise RuntimeError(
            "Cannot auto-detect kernel connection file: "
            "'ipykernel' is not installed.\n"
            "Install it with: pip install ipykernel\n"
            "Or provide connection_file explicitly: "
            "runtimed.sidecar('/path/to/kernel.json')"
        ) from None

    try:
        return get_connection_file()
    except Exception as e:
        raise RuntimeError(
            "Cannot auto-detect kernel connection file. "
            "Are you running inside a Jupyter kernel?\n"
            f"Original error: {type(e).__name__}: {e}\n"
            "You can provide connection_file explicitly: "
            "runtimed.sidecar('/path/to/kernel.json')"
        ) from e
