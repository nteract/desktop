"""``python -m nteract_kernel_launcher`` — drops into the kernel main loop.

The daemon spawns kernels as::

    python -m nteract_kernel_launcher -f <connection.json>

which resolves to this module, which calls :func:`main`, which delegates to
``NteractKernelApp.launch_instance()``. The traitlets ``Type`` cascade on the
app class causes ipykernel to construct ``NteractKernel`` → ``NteractShell``
→ ``NteractShellDisplayHook`` automatically during ``initialize``. The
bootstrap extension (``nteract_kernel_launcher._bootstrap``) auto-loads via
``default_extensions`` before any user code runs.
"""

from nteract_kernel_launcher import main

if __name__ == "__main__":
    main()
