"""Typed string constants mirroring the Rust daemon enums.

These are the same literals the Rust ``runtime_doc`` crate writes to the
CRDT and the TypeScript ``@runtimed`` package exposes to the frontend.
Pulling them in as Python constants lets readers match on kernel state
without typing bare strings. A typo in ``"missing_ipykernel"`` is
caught at import time rather than producing a silent always-false
comparison.

The Rust enums (``KernelErrorReason``, ``RuntimeLifecycle``) are the
authoritative API; this module only surfaces the wire strings they
serialise to. If you're adding a new variant, add it here AND on the
Rust/TS sides to keep the three languages in lockstep.
"""

from __future__ import annotations

from typing import Final, Literal

# ── Kernel error reasons ────────────────────────────────────────────

#: Pixi-managed environment is missing the ``ipykernel`` package.
#: Matches ``KernelErrorReason::MissingIpykernel.as_str()`` on the Rust
#: side and ``KERNEL_ERROR_REASON.MISSING_IPYKERNEL`` in TypeScript.
KernelErrorReasonKey = Literal["missing_ipykernel"]


class KERNEL_ERROR_REASON:
    """Typed error-reason strings written to ``kernel.error_reason`` in the
    RuntimeStateDoc when the lifecycle transitions to ``Error``.

    Mirrors ``runtime_doc::KernelErrorReason`` (Rust) and
    ``KERNEL_ERROR_REASON`` (TypeScript ``@runtimed`` package). Use the
    class attributes instead of bare string literals so typos fail loud.
    """

    MISSING_IPYKERNEL: Final[KernelErrorReasonKey] = "missing_ipykernel"


# ── Kernel status strings (legacy `kernel.status` vocabulary) ───────

#: Flat kernel-status vocabulary written to ``kernel.status`` in the
#: RuntimeStateDoc. This is the compressed legacy shape that predates
#: ``RuntimeLifecycle``; it collapses the four starting sub-phases into
#: ``"starting"`` and flattens ``Running(activity)`` down to ``"idle"``
#: or ``"busy"``. Matches ``KERNEL_STATUS`` in the TypeScript
#: ``@runtimed`` package.
KernelStatusKey = Literal[
    "not_started",
    "awaiting_trust",
    "starting",
    "idle",
    "busy",
    "error",
    "shutdown",
]


class KERNEL_STATUS:
    """Typed kernel-status strings for the legacy ``kernel.status`` field
    on the RuntimeStateDoc.

    Mirror of the TypeScript ``KERNEL_STATUS`` constant object. Prefer
    these over bare strings when polling ``notebook.runtime.kernel.status``
    so typos and stale status names fail at import time instead of
    silently producing always-false comparisons.

    When you can, match on ``RuntimeLifecycle`` (the typed successor) via
    the Rust side instead. This vocabulary exists for callers that still
    consume the flat string field during the Phase 2-5 transition.
    """

    NOT_STARTED: Final[KernelStatusKey] = "not_started"
    AWAITING_TRUST: Final[KernelStatusKey] = "awaiting_trust"
    STARTING: Final[KernelStatusKey] = "starting"
    IDLE: Final[KernelStatusKey] = "idle"
    BUSY: Final[KernelStatusKey] = "busy"
    ERROR: Final[KernelStatusKey] = "error"
    SHUTDOWN: Final[KernelStatusKey] = "shutdown"


__all__ = [
    "KERNEL_ERROR_REASON",
    "KERNEL_STATUS",
    "KernelErrorReasonKey",
    "KernelStatusKey",
]
