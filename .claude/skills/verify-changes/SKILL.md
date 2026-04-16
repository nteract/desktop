---
name: verify-changes
description: Verify code changes work end-to-end after modifying daemon, kernel, sync, CRDT, Python bindings, or MCP server code. Use after making changes to confirm they work before committing.
---

# Verify Changes

After making code changes, run the narrowest credible verification first, then broader checks if MCP tools are available.

## Step 1: Identify What Changed

Run `git diff --name-only` to see changed files and match them to the table below.

## Step 2: Run Narrow Tests

| Files changed | Test command |
|---|---|
| `crates/runtimed/src/**` | `cargo test -p runtimed` |
| `crates/notebook-doc/src/**` | `cargo test -p notebook-doc` |
| `crates/notebook-protocol/src/**` | `cargo test -p notebook-protocol` |
| `crates/notebook-sync/src/**` | `cargo test -p notebook-sync` |
| `crates/kernel-env/src/**` | `cargo test -p kernel-env` |
| `crates/kernel-launch/src/**` | `cargo test -p kernel-launch` |
| `crates/runt/src/**` | `cargo test -p runt` |
| `crates/runt-workspace/src/**` | `cargo test -p runt-workspace` |
| `crates/runtimed-py/src/**` | `up rebuild=true` (rebuilds Python bindings) |
| `python/nteract/src/**` | `nteract-dev` auto-restarts; no explicit test needed |
| `python/runtimed/src/**` | `nteract-dev` auto-restarts; run `pytest python/runtimed/tests/test_session_unit.py -v` |
| `apps/notebook/src/**` | `pnpm test:run` |
| `crates/runtimed-wasm/**` | `cargo xtask wasm` then `deno test --allow-read --allow-env --no-check` |

If multiple crates changed, run tests for each: `cargo test -p runtimed -p notebook-doc`.

## Step 3: MCP Live Verification (when nteract-dev tools are available)

If narrow tests pass and you have `nteract-dev` (`up`/`down`/`status`) and `open_notebook`/`execute_cell` tools, do a live check:

### For daemon/kernel changes (`crates/runtimed/`, `crates/runtimed-py/`):

1. `up rebuild=true` (if Rust changed)
2. `create_notebook` with runtime "python"
3. `create_cell` with source `1 + 1`
4. `execute_cell` on that cell
5. `get_cell` — verify output contains `2` and success is true
6. `create_cell` with source `print("verify-stream-ok")`
7. `execute_cell` and verify stream output contains `verify-stream-ok`

### For CRDT/doc changes (`crates/notebook-doc/`, `crates/notebook-sync/`):

1. `create_notebook`
2. `create_cell` with source `# round-trip test`
3. `get_cell` — verify source matches exactly
4. `set_cell` to change source
5. `get_cell` — verify source updated

### For MCP server changes (`python/nteract/`):

1. Call the changed tool directly with known inputs
2. Verify the output matches expectations

### For kernel environment changes (`crates/kernel-env/`, `crates/kernel-launch/`):

1. `up rebuild=true` (if Rust changed)
2. `create_notebook`
3. `create_cell` with source `import sys; print(sys.executable)`
4. `execute_cell` — verify output shows a Python path

## Step 4: Metrics Dashboard (optional, for performance-sensitive changes)

For changes to daemon, kernel, sync, or execution paths, open the harness dashboard notebook to measure impact:

```
open_notebook("scripts/metrics/harness-dashboard.ipynb")
```

Run the setup cell first, then run any metric cell. Each metric cell is self-contained — it creates a fresh notebook, measures, disconnects, and plots results against the committed baseline.

**How the notebook works:**
- Uses the **project venv** (no inline dependencies) — `runtimed` and `matplotlib` are both project dev deps
- Imports `runtimed.Client` directly for inline async measurements (no subprocess needed)
- Loads `baseline.json` from the notebook's own directory (`scripts/metrics/`)
- The daemon socket is discovered automatically via `RUNTIMED_SOCKET_PATH` env var (set by the daemon or `nteract-dev`)

**Three measurement cells:**
- **Execution latency** — cold start + warm p50/p95 distribution, compared to baseline
- **Kernel reliability** — success rate over a diverse 18-type cell battery (expressions, imports, intentional errors)
- **Sync correctness** — CRDT convergence across N concurrent peers doing random mutations

**Tuning:** Each cell has parameters you can tweak inline (e.g. `measure_latency(n=100)`, `measure_reliability(rounds=72)`, `measure_sync(n_peers=5, rounds=50)`).

**CLI alternative** (no notebook needed):
```
uv run python scripts/metrics/compare.py                 # all metrics vs baseline
uv run python scripts/metrics/compare.py --only latency  # just latency
```

**Regenerate baseline on main:**
```
uv run python scripts/metrics/generate-baseline.py
```

## Step 5: Report

State the verification result clearly:

- **HIGH confidence**: Narrow tests passed AND MCP live verification passed
- **MEDIUM confidence**: Narrow tests passed, MCP verification skipped (no tools available)
- **LOW confidence**: Only compilation checked, no tests run

Always run `cargo xtask lint` before committing.
