# nteract

An MCP (Model Context Protocol) server that connects AI assistants to Jupyter notebooks via the [nteract desktop app](https://nteract.io).

**[Download the nteract desktop app](https://nteract.io)** — you'll need it to see notebooks, collaborate with agents, and manage environments.

> Looking for the old Electron-based nteract desktop app? The source is archived at [nteract/archived-desktop-app](https://github.com/nteract/archived-desktop-app). The new native app is actively developed at [nteract/desktop](https://github.com/nteract/desktop).

## Bringing Agents in the Loop

We're in the preliminary stages of hooking up the realtime system from nteract/desktop to any agent of your choice. Collaborate with agents in notebooks, render interactive elements, and explore data together.

### Quick Start

#### Claude Code

```bash
# Stable
claude mcp add nteract -- uvx nteract

# Nightly
claude mcp add nteract-nightly -- uvx --prerelease allow nteract --nightly
```

#### Manual JSON config

```json
{
  "mcpServers": {
    "nteract": {
      "command": "uvx",
      "args": ["nteract"]
    }
  }
}
```

That's it. Now Claude can execute Python code, create visualizations, and work with your data.

## What is this?

nteract is an MCP (Model Context Protocol) server that connects AI assistants like Claude to Jupyter notebooks. It enables:

- **Code execution**: Run Python in a persistent kernel
- **Real-time collaboration**: Watch the AI work in the nteract desktop app
- **Shared state**: Multiple agents can work on the same notebook
- **Environment management**: Automatic Python environment setup

## Example

Ask Claude:

> "Help me visualize my log data"

Claude will:
1. Connect to a notebook session
2. Write and execute code
3. Generate visualizations
4. Show you the results

You can open the same notebook in the [nteract desktop app](https://nteract.io) to see changes in real-time and collaborate with the AI.

## Available Tools

| Tool | Description |
|------|-------------|
| `list_active_notebooks` | List all open notebook sessions |
| `join_notebook` | Join an existing notebook session by ID |
| `open_notebook` | Open an existing .ipynb file |
| `create_notebook` | Create a new notebook |
| `save_notebook` | Save notebook to disk as .ipynb file |
| `create_cell` | Add a cell to the notebook (use `and_run=True` to execute) |
| `execute_cell` | Run a specific cell (returns partial results after timeout) |
| `run_all_cells` | Queue all code cells for execution |
| `set_cell` | Update a cell's source and/or type |
| `get_cell` | Get a cell by ID with outputs |
| `get_all_cells` | View all cells in the notebook |
| `replace_match` | Targeted literal text find-and-replace in a cell |
| `replace_regex` | Regex-based find-and-replace in a cell |
| `move_cell` | Reorder a cell within the notebook |
| `clear_outputs` | Clear a cell's outputs |
| `delete_cell` | Remove a cell from the notebook |
| `interrupt_kernel` | Interrupt the currently executing cell |
| `restart_kernel` | Restart kernel with updated dependencies |
| `show_notebook` | Open the notebook in the nteract desktop app (disabled with `--no-show`) |
| `add_dependency` | Add a Python package dependency |
| `remove_dependency` | Remove a dependency |
| `get_dependencies` | List current dependencies |
| `sync_environment` | Hot-install new deps without restart |

### CLI Flags

| Flag | Description |
|------|-------------|
| `--version` | Print version and exit |
| `--nightly` | Connect to the nightly daemon and open nightly app |
| `--stable` | Connect to the stable daemon and open stable app |
| `--no-show` | Do not register the `show_notebook` tool (for headless environments) |

By default, `nteract` lets `runtimed.default_socket_path()` choose the socket. That means `RUNTIMED_SOCKET_PATH` wins when it is set; otherwise the package's build channel decides.

`--stable` and `--nightly` are mutually exclusive convenience overrides. When `RUNTIMED_SOCKET_PATH` is unset, they set it to `runtimed.socket_path_for_channel(...)`. When `RUNTIMED_SOCKET_PATH` is already set, the explicit env var wins. In either case, the selected flag still controls which desktop app `show_notebook` tries to launch.

## Architecture

```
┌─────────────┐     ┌─────────────┐     ┌─────────────┐
│   Claude    │────▶│   nteract   │────▶│   runtimed  │
│  (or other  │     │ MCP Server  │     │   daemon    │
│     AI)     │     │             │     │             │
└─────────────┘     └─────────────┘     └──────┬──────┘
                                               │
                    ┌─────────────┐             │
                    │   nteract   │◀────────────┘
                    │ Desktop App │  (real-time sync)
                    └─────────────┘
```

- **nteract** (this package): MCP server for AI assistants
- **runtimed**: Low-level daemon and Python bindings ([docs](https://github.com/nteract/desktop))
- **nteract desktop**: Native app for humans to collaborate with AI

## Real-time Collaboration

The magic of nteract is that AI and humans share the same notebook:

1. AI connects via MCP and runs code
2. Human opens the same notebook in nteract desktop
3. Changes sync instantly via CRDT
4. Both see the same kernel state

This enables workflows like:
- AI does initial analysis, human refines
- Human writes code, AI debugs errors
- Multiple AI agents collaborate on complex tasks

## Development

```bash
# Clone
git clone https://github.com/nteract/desktop
cd desktop/python/nteract

# Install dependencies
uv sync

# Run tests
uv run pytest
```

## Related Projects

- [nteract/desktop](https://github.com/nteract/desktop) - Native desktop app
- [runtimed on PyPI](https://pypi.org/project/runtimed/) - Low-level Python bindings

## License

BSD-3-Clause
