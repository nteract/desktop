# Environments

nteract Desktop automatically manages Python and Deno environments for your notebooks. You don't need to manually create virtual environments or install packages — the app handles it based on what's in your notebook and what project files are nearby.

## How It Works

When you open a notebook, nteract Desktop uses a two-stage detection:

### Stage 1: Which runtime? (Python or Deno)

nteract Desktop checks the notebook's stored kernel type:
- If the notebook says it's a **Deno notebook** → Deno kernel
- If the notebook says it's a **Python notebook** → Python kernel (then proceeds to Stage 2)
- New notebooks use your **default runtime** preference

This means Python and Deno notebooks can coexist in the same project directory — each uses its correct kernel regardless of what project files are nearby.

### Stage 2: Which Python environment?

For Python notebooks, nteract Desktop looks for dependencies in this order:

1. **Inline dependencies** stored in the notebook itself
2. **PEP 723 script metadata** embedded in Python cell source (`# /// script` blocks) — nteract Desktop creates a cached UV environment from those dependencies
3. **Closest project file** — nteract Desktop walks up from the notebook's directory looking for `pyproject.toml`, `pixi.toml`, or `environment.yml`/`environment.yaml`. The closest match wins, regardless of file type. If the same directory has multiple project files, the tiebreaker is: pyproject.toml > pixi.toml > environment.yml > environment.yaml
4. If none found, a **prewarmed environment** with just the basics

The search stops at git repository boundaries and your home directory, so project files from unrelated repos won't interfere.

This means your notebook starts with the right packages automatically.

## Inline Dependencies

The simplest way to manage packages. Dependencies are stored directly in the notebook file, making it fully portable — anyone who opens the notebook gets the same packages.

**Adding packages**: Use the dependency panel in the sidebar to add, remove, or sync packages. UV dependencies use pip-style package names (`pandas`, `numpy>=2.0`). Conda dependencies support conda channels.

**How it's stored**: Dependencies live in the notebook's JSON metadata under `metadata.runt.uv.dependencies` (for UV/pip packages) or `metadata.runt.conda.dependencies` (for conda packages). Legacy notebooks may use `metadata.uv` / `metadata.conda` directly — these are still read as fallbacks.

Python notebooks can also declare dependencies directly in cell source via a PEP 723 `# /// script` block. When present, nteract Desktop treats those as UV dependencies and prepares a cached environment with `env_source = "uv:pep723"`.

## Working with pyproject.toml

If your notebook is in a directory with a `pyproject.toml`, nteract Desktop auto-detects it and uses `uv run` to start the kernel in the project's virtual environment.

- The project's `.venv/` is used directly — no separate cached environment
- Dependencies stay in sync with the project
- The dependency panel shows the project's deps in read-only mode

The dependency panel offers two actions:
- **Use project environment** — run the kernel in the project's `.venv` (keeps deps in sync with the project)
- **Copy to notebook** — snapshot the project's dependencies into the notebook metadata (makes the notebook portable but deps may drift from the project)

## Working with environment.yml / environment.yaml

Conda `environment.yml` (and `environment.yaml`) files are auto-detected. nteract Desktop parses the channels, conda dependencies, and pip dependencies from the file and creates a conda environment using rattler.

The dependency panel shows the environment.yml dependencies and offers an "Import to notebook" action to copy them into the notebook's conda metadata for portability.

## Working with pixi.toml

Pixi project files are auto-detected. nteract Desktop converts pixi dependencies to conda format and creates the environment using rattler. Both `[dependencies]` (conda packages) and `[pypi-dependencies]` (pip packages) are supported.

The dependency panel shows pixi dependencies and offers an "Import to notebook" action.

## Deno Notebooks

Deno notebooks use the Deno runtime for TypeScript/JavaScript. Unlike Python, Deno manages its own dependencies through import maps and URL imports, so there's no separate environment to create.

**How Deno is obtained:**
- nteract Desktop first checks if a system `deno` 2.x+ is on your PATH
- If not found, it downloads Deno from GitHub releases (primary method)
- As a last resort, it falls back to installing Deno from conda-forge via rattler (stored in `~/.cache/runt/tools/`)

This means Deno notebooks work out of the box — you don't need to install Deno manually.

**Project configuration:** If your notebook is near a `deno.json` or `deno.jsonc` file, Deno will use that configuration for import maps and permissions.

## User Preferences

You can configure two default preferences in settings:

### Default Runtime
Choose what type of kernel new notebooks use:
- **Python** (default) — standard Python notebooks with ipykernel
- **Deno** — TypeScript/JavaScript notebooks using the Deno kernel

### Default Python Environment
Choose which package manager to use for Python notebooks:
- **UV** (default) — fast, pip-compatible package management
- **Conda** — supports conda packages (useful for non-Python dependencies like CUDA libraries)

This preference is used when no project files are detected and the notebook has no inline dependencies. When a project file is present, nteract Desktop picks the appropriate backend automatically (UV for pyproject.toml, Conda for environment.yml/environment.yaml and pixi.toml).

See [Settings](settings.md) for how to change these defaults.

## Trust Dialog

When you open a notebook with dependencies for the first time, nteract Desktop may show a trust dialog asking you to approve the dependency installation. This happens because:

- Dependencies are signed with a per-machine HMAC-SHA256 key
- Notebooks from other machines (shared by a colleague, cloned from a repo) have a different signature
- nteract Desktop asks you to verify the dependencies before installing anything

After you approve, the notebook is re-signed with your machine's key and won't prompt again — unless the dependency list changes, which invalidates the signature and re-prompts.

The key lives at `~/.config/runt/trust-key` (Linux), `~/Library/Application Support/runt/trust-key` (macOS), or the platform equivalent — file mode `0600`. It's created on first run and never sent off the machine.

## Cache and Cleanup

nteract Desktop caches environments so notebooks with the same dependencies share a single environment, making subsequent opens instant.

| What | macOS | Linux |
|------|-------|-------|
| UV environments | `~/Library/Caches/runt/envs/` | `~/.cache/runt/envs/` |
| Conda environments | `~/Library/Caches/runt/conda-envs/` | `~/.cache/runt/conda-envs/` |
| Inline dependency envs | `~/Library/Caches/runt/inline-envs/` | `~/.cache/runt/inline-envs/` |
| Tools (uv, deno) | `~/Library/Caches/runt/tools/` | `~/.cache/runt/tools/` |
| Trust key | `~/Library/Application Support/runt/trust-key` | `~/.config/runt/trust-key` |

To reclaim disk space, delete the environment cache directories. nteract Desktop will recreate environments as needed.

## Troubleshooting

**Packages aren't available after adding them**: Click "Sync Now" in the dependency panel to install pending changes, then restart the kernel.

**Wrong environment**: If the kernel started with a prewarmed environment instead of your project's dependencies, check that your project file (pyproject.toml, environment.yml/environment.yaml, pixi.toml) is in the notebook's directory or a parent directory within the same git repository.

**Slow first start**: The first time a notebook opens with dependencies, nteract Desktop needs to download and install packages. Subsequent opens with the same dependencies are instant due to caching.

**Trust dialog keeps appearing**: This happens when the notebook's dependency signature doesn't match your machine's key. Approve the dependencies once and nteract Desktop will re-sign the notebook.
