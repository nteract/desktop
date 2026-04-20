"""nteract-kernel-launcher — wrapper around ipykernel_launcher with kernel bootstrap.

Two supported invocations:

    python -m nteract_kernel_launcher -f <connection_file>   # vendored into venv
    python /path/to/nteract_kernel_launcher.py -f <file>     # run as a script

Bootstrap runs inside the kernel, *after* IPython is initialized but *before*
any user code executes. We achieve that ordering by appending the bootstrap
snippet to ``IPKernelApp.exec_lines`` on the process's argv before handing
off to ``ipykernel.kernelapp.launch_new_instance()``.
"""

from __future__ import annotations

import os
import sys

# Code run inside the kernel once IPython is initialized.
# Must be a single CLI-safe string (no newlines — use `;`).
_DX_EXEC_LINE = "import dx as _nteract_dx; _nteract_dx.install()"


def enabled_exec_lines() -> list[str]:
    """Return the exec_lines snippets that should run inside the kernel."""
    lines: list[str] = []
    if os.environ.get("RUNT_BOOTSTRAP_DX"):
        lines.append(_DX_EXEC_LINE)
    return lines


def _inject_exec_lines(argv: list[str], lines: list[str]) -> None:
    """Append ``--IPKernelApp.exec_lines=...`` args to argv in place."""
    for line in lines:
        argv.append(f"--IPKernelApp.exec_lines={line}")


def main() -> None:
    """Configure ipykernel's exec_lines, then hand off to ipykernel_launcher."""
    _inject_exec_lines(sys.argv, enabled_exec_lines())
    from ipykernel import kernelapp

    kernelapp.launch_new_instance()


if __name__ == "__main__":
    main()
