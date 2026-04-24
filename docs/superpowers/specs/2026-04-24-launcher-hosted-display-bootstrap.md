# Launcher-hosted display bootstrap

**Status:** Draft (rewrite 2026-04-24, derived from live-verified notebook prototype)
**Related:**
- [`2026-04-13-nteract-dx-design.md`](2026-04-13-nteract-dx-design.md) — the dx wire contract we preserve
- `crates/kernel-env/src/launcher.rs` — how the launcher gets vendored into envs
- `crates/runtimed/src/jupyter_kernel.rs` — where `bootstrap_dx` selects the launcher module
- `crates/runtimed/src/daemon.rs::uv_prewarmed_packages` — where `dx` is installed into pool envs (removed by this work)
- `python/nteract-kernel-launcher/nteract_kernel_launcher.py` — the current thin launcher

## Motivation

Today's display bootstrap is three moving parts that drift:

1. **`dx` PyPI package.** Installed into each env. Owns formatter/hook registration logic (`dx._format_install.install_formatters`).
2. **`nteract_kernel_launcher.py` vendored into each env.** Appends `--IPKernelApp.exec_lines="import dx as _n; _n.install()"` to argv when `RUNT_BOOTSTRAP_DX=1`.
3. **Pool warmer `bootstrap_dx` flag.** When on, `uv_prewarmed_packages` adds `"dx"` to the pool env install list.

Concrete bug: uv's default prerelease strategy is `if-necessary-or-explicit`, so the pool installer resolves bare `"dx"` to the latest *stable* version. On nightly builds the daemon speaks a display contract that only the prerelease `dx` emits. Users on nightly got stable `dx==2.0.0`, which has no `application/vnd.nteract.blob-ref+json` formatter. Result: DataFrames render as `text/llm+plain` summaries instead of the rich Sift table, and it's invisible — the kernel works, dx imports, just not the right dx.

That's the symptom. The deeper issue is **display-layer behavior is PyPI-version-coupled instead of daemon-version-coupled**. Every daemon release ships a display-layer contract; its actual implementation arrives through a separate dependency-resolution path that only coincidentally agrees.

## Goals

1. **Daemon-version-locked display behavior.** Formatter registration lives inside the daemon binary (vendored via `include_str!`), shipped with the launcher. Whatever display contract the daemon speaks this commit, the kernel speaks the same.
2. **All-or-nothing feature gate.** Either the kernel is vanilla (`ipykernel_launcher`) and nothing of ours runs, or it's enhanced (`nteract_kernel_launcher`) and everything is on. The current per-launcher flag already gates this — we keep the gate and remove the internal "am I enabled" checks.
3. **First-class `execute_result`.** Bare-`df`-on-last-line emits an `execute_result` with buffers — not a suppressed `execute_result` masqueraded by a `display_data`. Matches what the nbformat ecosystem expects.
4. **No PyPI bootstrap dependency.** Users can install `dx` or not, pin any version — kernel boot behavior does not depend on it.
5. **No protocol drift.** No patches to `jupyter-protocol` or `nbformat`. Schemas upstream are effectively frozen and we don't push against them.

## Non-Goals

- Writing a custom Jupyter kernel. Still `IPKernelApp`-based.
- Dropping the `dx` PyPI wheel. Stays as a standalone library for users under vanilla Jupyter / Colab / etc. See [dx package](#the-dx-pypi-package).
- Persisting rich traceback data to disk. See [tracebacks](#tracebacks-deferred) — deferred to a separate doc.

## The seats we bind to

All live-verified in `prototype.ipynb` (notebook-driven design artifact). Every reference below is documented public API; no internals are monkey-patched.

### `IPKernelApp` subclass trait cascade

Three class-level `Type` traits compose through subclasses:

```
IPKernelApp
├── kernel_class = Type(IPythonKernel)         ← we override here
│   IPythonKernel
│   ├── shell_class = Type(ZMQInteractiveShell)   ← we override here
│   │   ZMQInteractiveShell
│   │   ├── displayhook_class = Type(ZMQShellDisplayHook)   ← we override here
│   │   │   ZMQShellDisplayHook
│   │   │   └── .finish_displayhook()  ← we add a hook chain
│   │   └── display_pub_class = Type(ZMQDisplayPublisher)   ← unchanged; already has hooks
│   └── compiler_class = Type(XCachingCompiler)             ← unchanged
└── crash_handler_class = Type(CrashHandler)                ← unchanged
```

Four subclasses, each thin. Launch entry:

```python
from nteract_kernel_launcher.app import NteractKernelApp
NteractKernelApp.launch_instance()
```

### `default_extensions` trait auto-loads our bootstrap

`InteractiveShellApp.default_extensions = List(Unicode(), ['storemagic'])`. Extended on our `NteractKernelApp` subclass to also include `nteract_kernel_launcher._bootstrap`. The extension auto-loads during `init_extensions()` — before `init_code()` and before any user code runs. No `--InteractiveShellApp.extensions=...` argv injection. No `exec_lines` string. Extension failures log-warn, they don't traceback to the user.

### Three formatter registrations inside `_bootstrap.load_ipython_extension`

```python
# 1. For display(df) — returns the bundle.
mimebundle_formatter.for_type_by_name("pandas.core.frame", "DataFrame", _pd_bundle)
mimebundle_formatter.for_type_by_name("polars.dataframe.frame", "DataFrame", _pl_bundle)
mimebundle_formatter.for_type_by_name("narwhals.dataframe", "DataFrame", _nw_bundle)
mimebundle_formatter.for_type_by_name("datasets.arrow_dataset", "Dataset", _ds_bundle)

# 2. Publisher hooks — attach parquet bytes to outgoing messages.
ip.display_pub.register_hook(buffer_hook)    # for display_data
ip.displayhook.register_hook(buffer_hook)    # for execute_result — NEW, enabled by subclass
```

**`for_type_by_name` is the keystone for "no pyarrow in the launcher startup path."** Registration by `(module_str, class_name)` puts the entry in `deferred_printers`; the type isn't imported. pandas/polars/pyarrow only import when a real DataFrame instance flows through the formatter. The launcher's import-time cost is stdlib-only; heavy deps are lazy.

A consequence of lazy binding: user code importing a less-common DataFrame type we haven't registered by name (e.g. `pandas.someexotic.DataFrame`) won't be formatted. Whack-a-mole as those show up; acceptable.

### `ZMQShellDisplayHook` subclass adds a hook chain

Today, `ZMQDisplayPublisher.publish()` runs a hook chain before `session.send`. `ZMQShellDisplayHook.finish_displayhook` does not — it's a plain `session.send`. Hence dx's current workaround: register an `ipython_display_formatter` that returns `True` for DataFrames, which makes `DisplayFormatter.format()` return `({}, {})`, which makes `write_format_data` leave `msg.content.data` empty, which makes `finish_displayhook` skip the send entirely. The handler then publishes a `display_data` of its own, which goes through `ZMQDisplayPublisher.publish` and picks up buffers via the hook chain.

The dance works but has a visible cost: bare `df` on a cell's last line produces a `display_data` output in the .ipynb instead of an `execute_result`. Subtle but breaks tools that special-case execute_result (history view, output-type filters, nbformat strict validators expecting the "normal" last-expression shape).

`NteractShellDisplayHook` subclasses `ZMQShellDisplayHook` and adds a thread-local hook chain to `finish_displayhook` — same shape as `ZMQDisplayPublisher._hooks`, same contract. The override is ~30 lines of Python.

With this in place, the `ipython_display_formatter` trick is unnecessary for our use case. The mimebundle formatter returns a bundle, `write_format_data` populates the execute_result message, `finish_displayhook` runs our hook chain, `buffer_hook` attaches parquet buffers, `session.send` goes out once. Bare `df` produces an `execute_result` with buffers, same as `display(df)` produces a `display_data` with buffers.

## Architecture

### `python/nteract-kernel-launcher/` layout

```
python/nteract-kernel-launcher/
├── __init__.py                # re-exports main for `-m nteract_kernel_launcher`
├── __main__.py                # CLI entry: NteractKernelApp.launch_instance()
├── app.py                     # subclass cascade
├── _bootstrap.py              # load_ipython_extension — the IPython extension
├── _buffer_hook.py            # buffer attachment hook, shared by display_pub + displayhook
├── _format.py                 # dataframe → parquet bundle; lazy imports pandas/polars/pyarrow
├── _refs.py                   # BlobRef, BLOB_REF_MIME — wire types
└── _summary.py                # text/llm+plain synthesis — column stats, head preview
```

Ships vendored into each venv's site-packages via `kernel_env::launcher::vendor_into_venv` (existing, single-file write today → multi-file directory write after this change).

### `__main__.py`

```python
def main() -> None:
    from nteract_kernel_launcher.app import NteractKernelApp
    NteractKernelApp.launch_instance()


if __name__ == "__main__":
    main()
```

Gone: `RUNT_BOOTSTRAP_DX` env check, argv manipulation. No logic.

### `app.py` — the subclass cascade

```python
import threading
import sys
from traitlets import Type, List, Unicode
from ipykernel.kernelapp import IPKernelApp
from ipykernel.ipkernel import IPythonKernel
from ipykernel.zmqshell import ZMQInteractiveShell
from ipykernel.displayhook import ZMQShellDisplayHook


class NteractShellDisplayHook(ZMQShellDisplayHook):
    """ZMQShellDisplayHook + a thread-local hook chain on finish_displayhook.

    Mirrors ZMQDisplayPublisher._hooks / register_hook exactly so the same
    hook function can be registered on both seats and be agnostic about
    which message type is being built.

    Hook contract (matches ZMQDisplayPublisher):
      - hook(msg) → msg (pass through to default send)
      - hook(msg) → None (hook handled send itself; suppress default)
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
        self._hooks.append(hook)

    def unregister_hook(self, hook):
        try:
            self._hooks.remove(hook)
            return True
        except ValueError:
            return False

    def finish_displayhook(self):
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
    displayhook_class = Type(NteractShellDisplayHook)


class NteractKernel(IPythonKernel):
    shell_class = Type(NteractShell)


class NteractKernelApp(IPKernelApp):
    kernel_class = Type(NteractKernel)
    default_extensions = List(Unicode(), [
        "storemagic",
        "nteract_kernel_launcher._bootstrap",
    ])
```

### `_bootstrap.py` — the extension

```python
def load_ipython_extension(ip):
    _install_dataframe_formatters(ip)
    _install_buffer_hooks(ip)
    _enable_third_party_renderers()


def unload_ipython_extension(ip):
    # Best-effort symmetry; kernels don't typically unload the bootstrap.
    ...
```

- `_install_dataframe_formatters` — `for_type_by_name` calls for pandas / polars / narwhals / datasets. Ported from `dx._format_install`.
- `_install_buffer_hooks` — registers `buffer_hook` on both `display_pub` and `displayhook`. Tags the hook with `_nteract_installed = True` for idempotency (double-load is a no-op).
- `_enable_third_party_renderers` — altair `enable("nteract")`, plotly `default = "nteract"`, each gated by `try: import`. No-op if the library isn't present.

### `_buffer_hook.py`

One function, same shape as `dx._format_install._dx_display_pub_hook` today, with two differences:

1. Handles `execute_result` in addition to `display_data` / `update_display_data`. The new message type is reachable because `NteractShellDisplayHook` has hooks now.
2. Dispatches `session.send` through either `display_pub` or `displayhook` based on `msg_type` (routing fix — `execute_result` has a different `pub_socket` / `topic`).

### `_format.py` / `_refs.py` / `_summary.py`

Ports from `dx._format`, `dx._refs`, `dx._summary`. Heavy deps (pyarrow, pandas) are imported **inside** formatter functions, not at module top. Module import cost is near-zero.

These three files are roughly 400 lines total, largely unchanged from their dx counterparts. See [Source of truth](#source-of-truth) for the duplication discipline.

## What this changes in `runtimed`

### `jupyter_kernel.rs`

Already does the right thing:

```rust
let launcher_module = if bootstrap_dx {
    "nteract_kernel_launcher"
} else {
    "ipykernel_launcher"
};
```

in every spawn path (inline, pyproject, pixi, conda, prewarmed, etc.). **No change needed.** The flag gates which module is invoked. Turning EDX off = byte-for-byte `ipykernel_launcher`. Turning EDX on = our subclass cascade boots instead.

### `daemon.rs::uv_prewarmed_packages`

Removes the bootstrap_dx branch that appended `"dx"`:

```rust
// BEFORE
if feature_flags.bootstrap_dx {
    packages.push("dx".to_string());
}

// AFTER
// (removed — the launcher no longer depends on dx-the-PyPI-package)
```

Same deletion in `UV_BASE_PACKAGES`-adjacent code and in the conda warmer if it has a parallel branch.

### `kernel-env::launcher::vendor_into_venv`

Today writes a single `nteract_kernel_launcher.py` to purelib. After: writes a **directory** `nteract_kernel_launcher/` with the seven files listed above. Implementation:

- Embed each Python file via `include_str!` and a const table `(filename, content)`.
- Replace the single `vendor_into_venv(python)` call with one that creates `purelib/nteract_kernel_launcher/` and writes each file atomically (write-and-rename per file; same race-avoidance logic as today, per-file).
- The `_test_write_launcher` helper grows parallel.

Old per-file launcher path (`purelib/nteract_kernel_launcher.py`) has to be removed if present — the directory variant shadows it, but a stale single-file would shadow the directory on some Python import resolution orderings. Safe cleanup step in `vendor_into_venv`: `os.remove(purelib / "nteract_kernel_launcher.py")` if it exists before writing the directory.

### `FeatureFlags::bootstrap_dx`

Name stays. User-facing display ("Enhanced Data Experience") is a label on the settings UI, not a code rename. The flag continues to mean: kernel spawn uses `nteract_kernel_launcher`.

## The `dx` PyPI package

Keeps its API surface:
- `dx.BlobRef`, `dx.BLOB_REF_MIME` — wire types
- `dx.DxError`
- `dx.display()` — forwards to `IPython.display.display`
- `dx.install()` — no-op with a `DeprecationWarning` when called under a launcher-booted kernel (detectable via the `_nteract_installed` tag on the display_pub hook). Still functional when imported in a vanilla kernel — people using `dx` from plain Jupyter/Colab can still `import dx; dx.install()` and get the same behavior they always did.

The package remains on PyPI as a **reference implementation** of the blob-ref + text/llm+plain pattern. Long-term intent: nudge Jupyter Server / ecosystem toward adopting a similar approach. Keeping it importable and standalone-functional keeps that conversation alive.

## Source of truth

Two copies of the formatter logic will exist: one in `python/nteract-kernel-launcher/` (the in-daemon vendored version), one in the `dx` PyPI package (the standalone version). They must agree.

Options:

| Approach | Pro | Con |
|---|---|---|
| **Hard duplicate + CI diff** | Simple, each copy lives where it runs | Copies drift; CI has to enforce |
| **Launcher as canonical; dx vendors from launcher at build** | One source | Couples dx's release process to this repo |
| **dx as canonical; launcher `include_str!`s a frozen snapshot** | dx is the public face; frozen snapshots are predictable | Daemon ships whatever version we snapshotted |

**Recommended: hard duplicate + CI diff on the shared functions.** `diff` check in CI fails the build if `_bootstrap.py`, `_buffer_hook.py`, `_format.py`, `_refs.py`, `_summary.py` contents drift from their `dx._format_install.py`, `dx._format.py`, `dx._refs.py`, `dx._summary.py` counterparts. Operator updates both in lockstep, CI enforces agreement. The PyPI-released `dx` wheel has its own release cadence; at build time its content is bit-identical to what shipped in the daemon for that daemon version.

## Tracebacks (deferred)

Structured-traceback persistence hit a schema-imposed wall:

- `nbformat.v4.5` error outputs have `additionalProperties: false` and no `metadata` field (verified against `https://github.com/jupyter/nbformat/blob/main/nbformat/v4/nbformat.v4.5.schema.json`).
- No output type has an `id` field, so even storing rich data at `cell.metadata` level leaves **no stable key to correlate cell-metadata payloads with a specific error output** on load.

Landing zone: **no persistence. Rich traceback data is a render-time reconstruction**, every time. Kernel emits standard `error` messages. Daemon stores standard error in the output manifest. On render (in-session or on reload), a structured parser walks the `traceback` strings and rebuilds frames, ename/evalue, chained causes. Lossy for anything not derivable from printed text (locals at raise-time, any AI-generated suggestions) — that's an acceptable loss for an error display.

This work is **entirely daemon-side + UI-side**. Zero kernel-protocol surface. Zero wire-format surface. Zero on-disk surface. It does not depend on this spec. Deferred to a separate design doc once the display bootstrap ships.

## Rollout

Because `bootstrap_dx` already gates the launcher module selection in `jupyter_kernel.rs`, this is a seamless swap at deploy time:

- **Stable channel**: `bootstrap_dx = false` by default. Existing behavior: `ipykernel_launcher` boots, nothing of ours runs. No risk.
- **Nightly channel**: `bootstrap_dx = true` by default. Users get the new `NteractKernelApp` cascade with the bootstrap extension. Rich display, typed MIME, buffers flowing. Failure mode: if the extension or the subclass cascade errors at load, the kernel still starts because `init_extensions` log-warns rather than raising — the only degradation is to vanilla-looking display.
- **Per-user toggle**: Settings UI "Enhanced Data Experience" (the user-facing label for `bootstrap_dx`) remains functional. Users can opt into the behavior on stable or out of it on nightly.

A users' env on nightly that currently has stable `dx==2.0.0` installed via bootstrap_dx keeps working after upgrade because:
1. Pool envs built before this change still have `dx` installed — no harm, the package is still on PyPI.
2. Kernel boot uses `nteract_kernel_launcher` module → hits our subclass cascade first → `_bootstrap` extension loads → our formatters register → dx's `install()` no-ops when it eventually runs (via user-level code, if at all).
3. Pool envs built after this change don't have `dx` installed — but kernel boot is unaffected, because the launcher doesn't depend on dx.

## Phase 1 task breakdown

Rough order, each step independently testable:

1. **Write the seven `python/nteract-kernel-launcher/` files.** Port the three dx internals (`_format.py`, `_refs.py`, `_summary.py`) verbatim. Ship `app.py`, `_bootstrap.py`, `_buffer_hook.py`, `__main__.py` new.
2. **Update `kernel-env::launcher::vendor_into_venv`** to write a directory instead of a file; include all seven via `include_str!`; delete the old single-file path during write.
3. **Remove `bootstrap_dx` branch from `uv_prewarmed_packages`** (and conda warmer if parallel). Remove `"dx"` from `UV_BASE_PACKAGES`. Simplify the `inline_deps_with_bootstrap` helper in `inline_env.rs` (it no longer needs to inject `dx`).
4. **Test launch locally** on nightly build. Pool env with `EDX=on`, run `df.head()`, confirm Sift renders (i.e., `application/vnd.nteract.blob-ref+json` flowed and buffers attached).
5. **Deprecate `dx.install()` inside the wheel** — emit `DeprecationWarning` if the launcher's `_nteract_installed` tag is detected on the display_pub hook. Non-breaking for vanilla-Jupyter users.

Not in Phase 1: pyarrow lazy-loading polish (first install gets a slight stall when a DataFrame first displays — acceptable), the dx-wheel source-of-truth duplication discipline (set up once the layout stabilizes).

## Acceptance criteria

Phase 1 is done when:

- [ ] `cargo xtask dev` on nightly runs the new launcher for python kernels. Hash of `nteract_kernel_launcher/_bootstrap.py` in the vendored directory matches the bytes `include_str!`'d from the Rust source.
- [ ] `NteractKernelApp` + subclass cascade visible via kernel introspection (`IPKernelApp.instance().__class__.__name__ == "NteractKernelApp"`).
- [ ] `df = pd.DataFrame(...); df` on last line emits **one** `execute_result` message with `application/vnd.nteract.blob-ref+json` + `text/plain` + `text/llm+plain` in `data`, one `buffers` entry containing parquet bytes, no accompanying `display_data`.
- [ ] `pool_env.prewarmed_packages` no longer contains `"dx"`. New envs don't have dx installed unless a user pins it.
- [ ] Turning `bootstrap_dx = false` in settings causes next kernel spawn to use `ipykernel_launcher` — verified via daemon log `Starting Python kernel with ipykernel_launcher ...`. Nothing of ours runs. Vanilla Jupyter behavior.
- [ ] Existing dx-pinning notebooks (`runt.uv.dependencies = ["dx"]`) still launch cleanly; user code `import dx` succeeds; `dx.install()` no-ops with a deprecation warning.
- [ ] `cargo test -p kernel-env` passes — `vendor_into_venv` writes the directory atomically under concurrent calls (the existing race test extends to the multi-file case).

## Open questions

1. **Altair/plotly renderer flips at bootstrap vs. on-import-detect?** dx's current code flips them at `install()` time, which means it imports altair/plotly if they're installed. Possibly too eager. Alternative: hook a post-import callback via `sys.meta_path` that flips them only when the user imports them. Lower bootstrap cost. Parked for now — port the current behavior, optimize later.
2. **`dx` wheel's `install()` deprecation timing.** One release of warn-don't-fail, then remove? Two releases? Not blocking; pick a timeline at the time of Phase 1 release.
3. **Handling `XCachingCompiler` for cell attribution.** Out of scope for Phase 1. Mentioned here only because it's the next lever if we want cell-id-stamped filenames in tracebacks, which interacts with the deferred traceback work. No action until the traceback doc exists.

## Alternatives considered

- **Extension-only, no subclassing.** The previous draft of this spec. Works but leaves the `ipython_display_formatter` trick in place, forces bare-`df`-last-line to emit as `display_data`, and asymmetrically routes the hook chain through only one of the two output-emission paths. Rejected in favor of the subclass cascade once the prototype showed subclassing is cheap (~30 lines of `NteractShellDisplayHook`) and solves the execute_result asymmetry cleanly.
- **Patch `jupyter-protocol` / `nbformat` for error metadata.** The previous draft also suggested this for the traceback work. Ruled out: upstream schemas are effectively frozen, and patching our Rust mirrors would emit .ipynb files that fail validation against the canonical JSON schema. Tracebacks are reconstructed from plain strings at render time instead.
- **Replace `ipykernel` / write a custom kernel.** Big project, few gains over subclassing for the Phase 1 scope. Revisit if we hit a wall that subclassing can't clear.
- **Move `dx` bootstrap into a `sitecustomize.py` or `.pth` file.** Too broad — runs on any `python -c` in the env, not just kernels. IPython extensions are the scoped equivalent.
