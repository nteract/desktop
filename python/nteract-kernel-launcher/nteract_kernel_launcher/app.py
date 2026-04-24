"""IPKernelApp → IPythonKernel → ZMQInteractiveShell → ZMQShellDisplayHook
subclass cascade.

Three ``Type`` traits compose through subclassing to land
``NteractShellDisplayHook`` as the active displayhook when a kernel boots
via ``NteractKernelApp.launch_instance()``. The only behavioral change vs.
the upstream ``ipykernel`` cascade is a hook chain added to
``finish_displayhook`` — mirroring the one ``ZMQDisplayPublisher`` already
has for ``display_data``.

With the hook chain in place, ``execute_result`` messages (emitted for
bare-``df``-on-last-line) can carry ZeroMQ buffers via the same
transform pipeline. The ``ipython_display_formatter`` short-circuit that
dx's ``install()`` uses today to route bare last-expressions through
``display_data`` becomes unnecessary.
"""

from __future__ import annotations

import sys
import threading

from ipykernel.displayhook import ZMQShellDisplayHook
from ipykernel.ipkernel import IPythonKernel
from ipykernel.kernelapp import IPKernelApp
from ipykernel.zmqshell import ZMQInteractiveShell
from traitlets import List, Type, Unicode


class NteractShellDisplayHook(ZMQShellDisplayHook):
    """``ZMQShellDisplayHook`` + a thread-local hook chain on ``finish_displayhook``.

    Mirrors ``ZMQDisplayPublisher._hooks`` / ``.register_hook`` exactly so the
    same hook function can be registered on both seats and be agnostic about
    which message type is being built.

    Hook contract (identical to ``ZMQDisplayPublisher``):

    - ``hook(msg) -> msg``  — pass through; default ``session.send`` runs.
    - ``hook(msg) -> None`` — hook handled send itself; default is suppressed.
    """

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._tls = threading.local()

    @property
    def _hooks(self):
        if not hasattr(self._tls, "hooks"):
            self._tls.hooks = []
        return self._tls.hooks

    def register_hook(self, hook):
        """Append *hook* to the thread-local hook chain."""
        self._hooks.append(hook)

    def unregister_hook(self, hook):
        """Remove *hook* from the hook chain. Returns ``True`` on success."""
        try:
            self._hooks.remove(hook)
            return True
        except ValueError:
            return False

    def finish_displayhook(self):
        """Override: run the hook chain before ``session.send``.

        Preserves the parent's guards — only sends if ``self.msg`` exists,
        ``content.data`` is non-empty, and ``self.session`` is configured.
        That keeps any ``ipython_display_formatter`` returning ``True``
        from producing a bufferless follow-up send.
        """
        sys.stdout.flush()
        sys.stderr.flush()
        if self.msg and self.msg["content"]["data"] and self.session:
            msg = self.msg
            for hook in self._hooks:
                msg = hook(msg)
                if msg is None:
                    self.msg = None
                    return
            self.session.send(self.pub_socket, msg, ident=self.topic)
        self.msg = None


class NteractShell(ZMQInteractiveShell):
    """Shell subclass that wires in ``NteractShellDisplayHook``."""

    displayhook_class = Type(NteractShellDisplayHook)


class NteractKernel(IPythonKernel):
    """Kernel subclass that wires in ``NteractShell``."""

    shell_class = Type(NteractShell)


class NteractKernelApp(IPKernelApp):
    """Kernel-app subclass. Activates the full ``Nteract*`` cascade and
    auto-loads the bootstrap extension before any user code runs.

    ``default_extensions`` is an ``InteractiveShellApp`` trait consulted by
    ``init_extensions`` during kernel startup. Extensions listed here load
    via ``ExtensionManager.load_extension``, which log-warns on failure
    rather than raising — bootstrap problems never present as a traceback
    to the user.
    """

    kernel_class = Type(NteractKernel)

    default_extensions = List(
        Unicode(),
        [
            "storemagic",  # IPython's own default
            "nteract_kernel_launcher._bootstrap",
        ],
    )
