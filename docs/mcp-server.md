# MCP Server for Jupyter Notebooks

The `runtimed` package includes an MCP (Model Context Protocol) server that enables AI agents to interact with Jupyter notebooks programmatically.

## Installation

Install with the MCP extra:

```bash
pip install runtimed[mcp]
```

Or with uv:

```bash
uv pip install "runtimed[mcp]"
```

## Running the Server

The MCP server runs over stdio for integration with AI tools like Claude Code:

```bash
# Using the entry point
runtimed-mcp

# Or as a module
python -m runtimed._mcp_server
```

## Claude Code Configuration

Add to your Claude Code MCP configuration:

```json
{
  "mcpServers": {
    "runtimed-mcp": {
      "command": "runtimed-mcp"
    }
  }
}
```

Or if using uv in a development environment:

```json
{
  "mcpServers": {
    "runtimed-mcp": {
      "command": "uv",
      "args": ["run", "--directory", "/path/to/python/runtimed", "python", "-m", "runtimed._mcp_server"]
    }
  }
}
```

## Available Tools

### Session Management

| Tool | Description |
|------|-------------|
| `connect_notebook` | Connect to a notebook session (by ID or path) |
| `disconnect_notebook` | Disconnect from the current session |
| `list_notebooks` | List all active notebook rooms |

### Kernel Management

| Tool | Description |
|------|-------------|
| `start_kernel` | Start a Python or Deno kernel |
| `shutdown_kernel` | Stop the kernel |
| `interrupt_kernel` | Interrupt execution |
| `get_kernel_status` | Get kernel state |

### Cell Operations

| Tool | Description |
|------|-------------|
| `create_cell` | Create a new cell with source |
| `set_cell_source` | Update an existing cell's source |
| `get_cell` | Read a single cell by ID |
| `get_all_cells` | Read all cells in notebook |
| `delete_cell` | Remove a cell |

### Execution

| Tool | Description |
|------|-------------|
| `execute_cell` | Execute a cell, return outputs |
| `run_code` | Create + execute in one call (convenience) |

## Available Resources

| URI | Description |
|-----|-------------|
| `notebook://cells` | All cells in current notebook |
| `notebook://status` | Session and kernel status |
| `notebook://rooms` | Active notebook rooms |

## Example Usage

Here's how an AI agent might use these tools:

1. **Connect to a notebook:**
   ```
   connect_notebook(notebook_id="my-analysis")
   ```

2. **Start a kernel:**
   ```
   start_kernel(kernel_type="python", env_source="uv:prewarmed")
   ```

3. **Run code:**
   ```
   run_code("import pandas as pd\ndf = pd.DataFrame({'a': [1,2,3]})\ndf")
   ```

4. **Create and execute cells:**
   ```
   cell_id = create_cell("# Analysis\nresults = df.describe()")
   result = execute_cell(cell_id)
   ```

## Execution Results

The `execute_cell` and `run_code` tools return a result dictionary:

```python
{
    "cell_id": "cell-abc123",
    "success": True,
    "execution_count": 5,
    "stdout": "Hello, world!\n",
    "stderr": "",
    "outputs": [
        {
            "output_type": "stream",
            "name": "stdout",
            "text": "Hello, world!\n"
        },
        {
            "output_type": "execute_result",
            "data": {"text/plain": "42"},
            "execution_count": 5
        }
    ],
    "error": None  # or error details if execution failed
}
```

## Realtime Collaboration with nteract

The MCP server connects to the same daemon as the nteract desktop app. This enables realtime collaboration:

- Open the same notebook in nteract to see changes as the agent makes them
- The kernel is shared between the MCP server and nteract
- Multiple MCP clients can connect with the same `notebook_id`
- Changes sync instantly via Automerge CRDT

## Environment Sources

When starting a kernel, you can specify the environment source:

| Source | Description |
|--------|-------------|
| `auto` | Auto-detect from notebook metadata or project files (default) |
| `uv:prewarmed` | Fast startup from UV pool |
| `conda:prewarmed` | Conda environment from pool |
| `uv:inline` | Use notebook's inline UV dependencies |
| `conda:inline` | Use notebook's inline conda dependencies |
| `uv:pyproject` | Use pyproject.toml in notebook's directory |
| `conda:env_yml` | Use environment.yml in notebook's directory |

**Note:** For Deno kernels (`kernel_type="deno"`), the `env_source` is ignored and always uses `"deno"`.

## Development

For development, start the dev daemon first:

```bash
# Terminal 1: Start dev daemon
cargo xtask dev-daemon

# Terminal 2: Run MCP server
cd python/runtimed
uv run python -m runtimed._mcp_server
```

Run tests:

```bash
cd python/runtimed
uv run pytest tests/test_mcp.py -v

# Integration tests (requires running daemon)
uv run pytest tests/test_mcp.py -v -m integration
```
