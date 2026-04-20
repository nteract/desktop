# nteract-kernel-launcher

Thin wrapper around `ipykernel_launcher` that performs nteract-specific kernel
bootstrap (feature-flagged) before handing control to ipykernel. Designed to be
a drop-in replacement for `python -m ipykernel_launcher`:

```text
python -m nteract_kernel_launcher -f <connection_file>
```

## Bootstrap flags

All bootstrap is gated on environment variables so the behavior can be
controlled per-kernel by the daemon:

| Env var | Effect |
|---------|--------|
| `RUNT_BOOTSTRAP_DX=1` | Import `dx` and call `dx.install()` before the kernel starts, wiring up DataFrame formatters and visualization renderers. Silently skipped if `dx` is not installed. |

If bootstrap raises, the error is logged to stderr but the launcher still
starts the kernel — a broken bootstrap should not prevent the user from
running code.
