# Settings

nteract Desktop settings control default behavior for new notebooks, appearance, and runtime configuration.

## Quick Reference

| Setting | Options | Default | Stored In |
|---------|---------|---------|-----------|
| Theme | light, dark, system | system | Synced (Automerge) + settings file |
| Default runtime | python, deno | python | Synced (Automerge) + settings file |
| Default Python env | uv, conda | uv | Synced (Automerge) + settings file |
| Default uv packages | list of strings | (empty) | Synced (Automerge) + settings file |
| Default conda packages | list of strings | (empty) | Synced (Automerge) + settings file |

## How Settings Sync Works

Settings are synced across all notebook windows via the runtimed daemon using Automerge. The daemon holds the canonical document; each notebook window maintains a local replica.

- **Source of truth:** The Automerge document in the daemon
- **Persistence:** Settings are also written to `settings.json` in the same nested format
- **External edits:** The daemon watches `settings.json` for external changes (manual edits, CLI tools) and propagates them to all connected windows automatically
- **Fallback:** When the daemon is unavailable, settings are read directly from `settings.json`
- **Theme special case:** Theme also uses browser localStorage to prevent a flash of unstyled content on startup

When you change a setting in any window, it propagates to all other open windows in real time.

### Automerge Document Structure

The synced settings use nested maps for environment-specific configuration:

```
ROOT/
  theme: "system"
  default_runtime: "python"
  default_python_env: "uv"
  uv/                                         ← nested Map
    default_packages: List["numpy", "pandas"] ← List of Str
  conda/                                      ← nested Map
    default_packages: List["scipy"]           ← List of Str
```

Environment-specific settings (packages, future: channels) live under `uv/` and `conda/` sub-maps, making the schema extensible without adding more root-level keys.

## Settings File

Settings are persisted to a JSON file shared across all notebook windows. Both the daemon and the notebook app write the same nested JSON format.

| Platform | Path |
|----------|------|
| macOS | `~/Library/Application Support/nteract/settings.json` |
| Linux | `~/.config/nteract/settings.json` |
| Windows | `C:\Users\<User>\AppData\Roaming\nteract\settings.json` |

The file is created automatically when you first change a setting. You can also edit it by hand — changes are detected and applied automatically when the daemon is running.

Example:

```json
{
  "theme": "system",
  "default_runtime": "python",
  "default_python_env": "uv",
  "uv": {
    "default_packages": ["numpy", "pandas", "matplotlib"]
  },
  "conda": {
    "default_packages": ["numpy", "pandas", "scikit-learn"]
  }
}
```

### JSON Schema

The settings structs derive `schemars::JsonSchema`. Both `SyncedSettings` (in runtimed) and `AppSettings` (in the notebook crate) serialize to the same JSON schema.

## Theme

Controls light/dark appearance for the notebook editor and output viewer.

- **Light** — forces light mode
- **Dark** — forces dark mode
- **System** — follows your OS preference and updates automatically when it changes

Change the theme by clicking the gear icon in the notebook toolbar, then selecting Light, Dark, or System.

## Default Runtime

Determines which runtime is used when creating a new notebook with **Cmd+N** (or **Ctrl+N** on Windows/Linux).

```json
{
  "default_runtime": "python"
}
```

Valid values: `"python"`, `"deno"`

You can always create a notebook with a specific runtime using the **File > New Notebook As...** submenu.
You can open an existing notebook from the file picker using **File > Open > Open...**.
Bundled example notebooks are available under **File > Open > Sample Notebooks**.

## Default Python Environment

Controls which package manager is used for Python notebooks when no project-level configuration is detected.

```json
{
  "default_python_env": "uv"
}
```

Valid values: `"uv"`, `"conda"`

- **uv** — uses uv for package management (fast, pip-compatible)
- **conda** — uses conda/rattler for package management (supports conda packages)

If the notebook directory contains a `pyproject.toml` or `environment.yml`, the environment type is determined by that file instead of this setting.

## Default Packages

Controls which packages are pre-installed in prewarmed environments. These packages are available immediately when you open a new notebook, without needing to add them as inline dependencies.

Since uv and conda have different package ecosystems, packages are configured separately:

```json
{
  "uv": {
    "default_packages": ["numpy", "pandas", "matplotlib"]
  },
  "conda": {
    "default_packages": ["numpy", "pandas", "scikit-learn"]
  }
}
```

Changes take effect on the next pool replenishment cycle — existing prewarmed environments keep their original packages until replaced. Restarting the app clears the pool and rebuilds with the updated packages.

The packages are installed alongside `ipykernel` and `ipywidgets` (which are always included).
