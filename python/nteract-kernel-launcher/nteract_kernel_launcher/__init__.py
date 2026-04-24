"""nteract-kernel-launcher — superpowered IPython kernel for the nteract desktop app.

Subclasses ``IPKernelApp``/``IPythonKernel``/``ZMQInteractiveShell``/
``ZMQShellDisplayHook`` to add:

- A hook chain on the shell's displayhook (lets ``execute_result`` carry
  buffers, matching ``display_data``'s existing hook chain).
- An auto-loaded bootstrap extension that registers DataFrame formatters
  and publisher hooks for the nteract data-experience integration.

The daemon invokes this module in place of ``ipykernel_launcher`` when the
``bootstrap_dx`` feature flag (user-facing: "Enhanced Data Experience") is
on. See ``docs/superpowers/specs/2026-04-24-launcher-hosted-display-bootstrap.md``.
"""

from nteract_kernel_launcher.app import (
    NteractKernel,
    NteractKernelApp,
    NteractShell,
    NteractShellDisplayHook,
)


def main() -> None:
    """CLI entry point for ``python -m nteract_kernel_launcher``."""
    NteractKernelApp.launch_instance()


__all__ = [
    "NteractKernel",
    "NteractKernelApp",
    "NteractShell",
    "NteractShellDisplayHook",
    "main",
]
