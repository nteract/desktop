---
paths:
  - crates/kernel-launch/**
  - crates/runtimed/src/inline_env*
  - crates/notebook/src/*env*
  - crates/notebook/src/pyproject*
  - crates/notebook/src/pixi*
  - crates/notebook/src/environment_yml*
  - crates/notebook/src/trust*
  - crates/runt-trust/**
---

# Environment Management

## Two-Stage Detection

When a notebook opens, Runt determines the kernel via two stages:

1. **Runtime Detection** -- Is this Python or Deno?
2. **Environment Resolution** -- For Python, what environment to use?

This allows Python and Deno notebooks to coexist in the same project directory.

### Stage 1: Runtime Detection

The daemon reads the notebook's kernelspec:

| Priority | Source | Check | Result |
|----------|--------|-------|--------|
| 1 | Notebook metadata | `kernelspec.name == "deno"` | Launch Deno kernel |
| 2 | Notebook metadata | `kernelspec.name` contains "python" | Resolve Python environment |
| 3 | Notebook metadata | `kernelspec.language == "typescript"` | Launch Deno kernel |
| 4 | Notebook metadata | `language_info.name == "typescript"` | Launch Deno kernel |
| 5 | User setting | `default_runtime` preference | Python or Deno |

**Key invariant:** The notebook's encoded kernelspec takes priority over project files. A Deno notebook in a directory with `pyproject.toml` will launch a Deno kernel, not Python.

### Stage 2: Python Environment Resolution

| Priority | Source | Backend | Environment Type |
|----------|--------|---------|-----------------|
| 1 | Inline notebook metadata | UV or Conda deps from `metadata.runt.uv` / `metadata.runt.conda` | Cached by dep hash |
| 2 | Closest project file | Single walk-up via `project_file::find_nearest_project_file` | Depends on file type |
| 3 | User preference | Prewarmed UV or Conda env from pool | Shared pool env |

For step 2, walk-up checks for `pyproject.toml`, `pixi.toml`, `environment.yml`/`environment.yaml` at each directory level. Closest match wins. Same-directory tiebreaker: pyproject.toml > pixi.toml > environment.yml.

Walk-up stops at `.git` boundaries and user's home directory.

| Project file | Backend | Environment Type |
|-------------|---------|-----------------|
| `pyproject.toml` | `uv run --with ipykernel` in project dir | Project `.venv/` |
| `pixi.toml` | `pixi run python -m ipykernel_launcher` in project dir | Pixi-managed env |
| `environment.yml` | Parse deps, use rattler | Cached by dep hash |

### Deno Kernel Launching

Deno kernels do not use environment pools:
1. Get deno via `kernel_launch::tools::get_deno_path()` (PATH first, then bootstrap from conda-forge)
2. Launch: `deno jupyter --kernel --conn <connection_file>`

## Environment Source Labels

The daemon returns an `env_source` string with `KernelLaunched`:
- `"uv:inline"` / `"uv:pyproject"` / `"uv:prewarmed"`
- `"conda:inline"` / `"conda:env_yml"` / `"conda:prewarmed"`
- `"pixi:toml"`

## Kernel Starting Phases

When a kernel is starting, the RuntimeStateDoc tracks granular phases via `kernel.starting_phase`:

| Phase | Description |
|-------|-------------|
| `"resolving"` | Dependency resolution (reading project files, computing env hash) |
| `"preparing_env"` | Environment creation or cache lookup (UV install, Conda solve) |
| `"launching"` | Spawning the kernel process |
| `"connecting"` | Establishing ZMQ connection to kernel |

Phases are written by the daemon to RuntimeStateDoc. Frontend displays them via `useRuntimeState()`. Cleared when kernel reaches `idle` or `error`.

## Content-Addressed Caching

Environments are cached by dependency hash so notebooks with identical deps share a single environment.

**UV** (`uv_env.rs`): Hash = SHA256(sorted deps + requires_python + prerelease + env_id), first 16 hex chars. Location: `~/.cache/runt/envs/{hash}/`. When deps are non-empty, env_id is excluded (cross-notebook sharing). When empty, env_id is included (per-notebook isolation).

**Conda** (`conda_env.rs`): Hash = SHA256(sorted deps + sorted channels + python version + env_id), first 16 hex chars. Location: `~/.cache/runt/conda-envs/{hash}/`.

Cache hit check: verify `{hash}/bin/python` (Unix) or `{hash}/Scripts/python.exe` (Windows) exists.

## Inline Dependency Environments

For notebooks with inline UV deps (`metadata.runt.uv.dependencies`), the daemon creates cached environments in `~/.cache/runt/inline-envs/`. Keyed by hash of sorted dependencies for fast reuse. Cache hit = instant startup.

### PEP 723 Support

`notebook-doc` includes a PEP 723 parser (`crates/notebook-doc/src/pep723.rs`) that extracts inline script metadata from Python cells. This enables reading `# /// script` blocks for dependency declarations within individual cells.

## Prewarming and Daemon Pool

The daemon maintains a pool of pre-created environments with `ipykernel` and `ipywidgets` installed:
- Default pool size: 3 per type (UV and Conda)
- Max age: 2 days (172800 seconds)
- Warming loops replenish as environments are consumed
- Prewarmed environments have no `env_id` so they can be reused by any notebook

## Project File Discovery

Unified detection in `project_file.rs`, used by daemon's `auto_launch_kernel()`:

| Module | Purpose |
|--------|---------|
| `project_file.rs` | `find_nearest_project_file()` -- single walk-up, closest wins |
| `pyproject.rs` | `find_pyproject()`, parsing, Tauri commands |
| `pixi.rs` | `find_pixi_toml()`, parsing, Tauri commands |
| `environment_yml.rs` | `find_environment_yml()`, parsing, Tauri commands |
| `deno_env.rs` | `find_deno_config()` |

All walk-up functions stop at `.git` boundaries and user's home directory.

## Notebook Metadata Schema

```json
{
  "metadata": {
    "kernelspec": { "name": "python3", "display_name": "Python 3", "language": "python" },
    "runt": {
      "schema_version": "1",
      "env_id": "uuid",
      "uv": { "dependencies": ["pandas", "numpy"], "requires-python": ">=3.10" },
      "conda": { "dependencies": ["numpy", "scipy"], "channels": ["conda-forge"], "python": "3.12" },
      "deno": { "permissions": ["--allow-net"], "config": "deno.json" }
    }
  }
}
```

Runtime type is determined by `kernelspec.name`, not by a field in `runt`.

## Trust System

Dependencies are signed with HMAC-SHA256 to prevent untrusted code execution on notebook open.

- **Key:** 32 random bytes at `~/Library/Application Support/runt/trust-key` (macOS) or `~/.config/runt/trust-key` (Linux), generated on first use
- **Signed content:** Canonical JSON of `metadata.runt.uv` + `metadata.runt.conda` (with fallback to legacy `metadata.uv` + `metadata.conda`; not cell contents or outputs)
- **Format:** `"hmac-sha256:{hex_digest}"` in notebook metadata
- **Machine-specific:** Every shared notebook is untrusted on the recipient's machine
- **Verification:** `verify_signature()` returns `bool`. Higher-level `verify_notebook_trust()` returns `TrustInfo` with `TrustStatus`: Trusted, Untrusted, SignatureInvalid, or NoDependencies

Changes to dependency metadata structure require updating `crates/runt-trust/src/lib.rs` (re-exported by `crates/notebook/src/trust.rs`).

## Adding a New Project File Format

1. Create `crates/notebook/src/{format}.rs` with `find_{format}()` (directory walk) and `parse_{format}()` functions
2. Add Tauri commands in `lib.rs`: `detect_{format}`, `get_{format}_dependencies`, `import_{format}_dependencies`
3. Wire detection into daemon's `auto_launch_kernel()` in `notebook_sync_server.rs` at the correct priority position
4. Add frontend detection in `useDaemonKernel.ts` and the appropriate dependencies hook
5. Add test fixture in `crates/notebook/fixtures/audit-test/`

## Frontend Architecture

| Component | Hook | Manages |
|-----------|------|---------|
| `DependencyHeader.tsx` | `useDependencies.ts` | UV deps, pyproject.toml detection |
| `CondaDependencyHeader.tsx` | `useCondaDependencies.ts` | Conda deps, environment.yml detection |
| `PixiDependencyHeader.tsx` | `usePixiDependencies.ts` | Pixi project detection, read-only dep display |
| `DenoDependencyHeader.tsx` | `useDenoDependencies.ts` | Deno configuration and deno.json detection |

## Key Files

### Shared Kernel Launch Crate

| File | Role |
|------|------|
| `crates/kernel-launch/src/lib.rs` | Public API for kernel launching |
| `crates/kernel-launch/src/tools.rs` | Tool bootstrapping (deno, uv, ruff) via rattler |

### Daemon

| File | Role |
|------|------|
| `crates/runtimed/src/daemon.rs` | Pool management |
| `crates/runtimed/src/notebook_sync_server.rs` | `auto_launch_kernel()` -- detection and resolution |
| `crates/runtimed/src/runtime_agent.rs` | Spawned as a subprocess by `RuntimeAgentHandle::spawn()`. `run_runtime_agent()` is the per-notebook event loop owning sockets, `QueueCommand` channels, and RuntimeStateDoc writes; `handle_runtime_agent_request()` dispatches each `LaunchKernel`/`RestartKernel`/etc. RPC |
| `crates/runtimed/src/jupyter_kernel.rs` | `JupyterKernel::launch()` -- spawns the kernel process and wires ZMQ sockets |
| `crates/runtimed/src/output_prep.rs` | Output-prep helpers — `QueueCommand`, `KernelStatus`, `QueuedCell`, iopub → nbformat conversion + display-update helpers, widget-buffer offload to the blob store. Imported by `runtime_agent.rs`, `jupyter_kernel.rs`, and `kernel_state.rs` |
| `crates/runtimed/src/project_file.rs` | Unified closest-wins project file detection |
| `crates/runtimed/src/inline_env.rs` | Cached inline dep environments (UV and Conda) |

### Notebook Crate (Tauri Commands)

| File | Role |
|------|------|
| `crates/notebook/src/lib.rs` | Tauri commands, `launch_kernel_via_daemon` |
| `crates/notebook/src/uv_env.rs` | UV dependency metadata |
| `crates/notebook/src/conda_env.rs` | Conda dependency metadata |
| `crates/notebook/src/pyproject.rs` | pyproject.toml discovery and parsing |
| `crates/notebook/src/pixi.rs` | pixi.toml discovery and parsing |
| `crates/notebook/src/environment_yml.rs` | environment.yml discovery and parsing |
| `crates/notebook/src/trust.rs` | HMAC trust verification (re-exports from `runt-trust`) |

### Frontend

| File | Role |
|------|------|
| `apps/notebook/src/hooks/useDaemonKernel.ts` | Kernel execution, env sync |
| `apps/notebook/src/hooks/useDependencies.ts` | UV dep management |
| `apps/notebook/src/hooks/useCondaDependencies.ts` | Conda dep management |
