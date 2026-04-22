# Changelog

## 2.0.0 (unreleased)

First release from `nteract/desktop`. The `nteract` package is now a thin wrapper that locates and launches the `runt mcp` server that ships with the [nteract desktop app](https://nteract.io).

### Highlights

- AI-collaborative Jupyter notebooks over MCP. Works with Claude, ChatGPT, Gemini, and any MCP-capable agent.
- Agents can create, open, and save notebooks; read, write, and execute cells with live output; and manage Python dependencies with UV or Conda.
- Real-time collaboration with a human in the nteract desktop app — both sides see the same notebook update as it changes.
- Recommended install is the Claude Code plugin at [`nteract/claude-plugin`](https://github.com/nteract/claude-plugin). This PyPI package exists as an alternative for `uvx`-based workflows.

### Breaking changes from 1.x

The package was completely rewritten. The 1.x series (published from `nteract/nteract`) was a different project. There is no migration path — install and configure as new.
