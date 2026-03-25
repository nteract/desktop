"""Sync-property guard types.

Thin subclasses of builtins that raise helpful errors when accidentally
awaited or called.  Used by sync properties to prevent common mistakes
like ``await cell.source`` instead of ``cell.source``.
"""

from __future__ import annotations


class _SyncGuardMixin:
    """Mixin that adds __await__ and __call__ guards."""

    __slots__ = ()
    _attr: str = ""

    def __await__(self):
        raise TypeError(
            f"'{self._attr}' is a sync property — use it directly, no await needed: .{self._attr}"
        )

    def __call__(self, *a, **kw):
        raise TypeError(
            f"'{self._attr}' is a property, not a method — drop the parentheses: .{self._attr}"
        )


_guard_cache: dict[tuple[type, str], type] = {}


def sync_guard(attr: str, value):
    """Wrap *value* in a guard subclass that raises on ``await`` / ``()``.

    The returned object is a subclass of the original type, so
    ``isinstance(sync_guard("x", "hi"), str)`` is ``True`` and all
    normal operations work unchanged.

    ``None`` is returned as-is (``NoneType`` cannot be subclassed).
    """
    if value is None:
        return None
    base = type(value)
    # bool cannot be subclassed; use int instead (bool is a subclass of int)
    inherit = int if base is bool else base
    key = (base, attr)
    cls = _guard_cache.get(key)
    if cls is None:
        cls = type(
            f"_Sync{base.__name__}",
            (_SyncGuardMixin, inherit),
            {"_attr": attr},
        )
        _guard_cache[key] = cls
    return cls(value)
