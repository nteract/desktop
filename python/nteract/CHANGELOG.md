# Changelog

## 2.0.0 (unreleased)

First release from `nteract/desktop`. The `nteract` Python package is now an MCP server for AI agents to interact with Jupyter notebooks through runt/runtimed.

### Highlights

- **MCP server** — `nteract` runs as a stdio MCP server, compatible with Claude, ChatGPT, Gemini, OpenCode, and any MCP-capable agent
- **Notebook lifecycle** — `list_active_notebooks`, `connect_notebook`, `create_notebook`, `save_notebook`, `show_notebook`
- **Cell operations** — `create_cell`, `get_cell`, `get_all_cells`, `set_cell`, `delete_cell`, `move_cell`, `clear_outputs`
- **Targeted editing** — `replace_match` (literal find-and-replace) and `replace_regex` (regex-based) for surgical cell edits without rewriting entire sources
- **Execution** — `execute_cell`, `run_all_cells`, `interrupt_kernel`, `restart_kernel`
- **Dependencies** — `get_dependencies`, `add_dependency`, `remove_dependency`, `sync_environment` with UV and Conda support
- **Resources** — `notebook://cells`, `notebook://cell/{cell_id}`, `notebook://cells/by-index/{index}`, `notebook://cell/{cell_id}/outputs`, `notebook://status`, `notebook://rooms` for read-only state access

### Breaking changes from 1.x

The package was completely rewritten. The 1.x series (published from `nteract/nteract`) was a different project. There is no migration path — install and configure as new.