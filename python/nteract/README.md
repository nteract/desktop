# nteract

AI-collaborative Jupyter notebooks. The MCP server ships with the [nteract desktop app](https://nteract.io) — this PyPI package is a thin wrapper that finds and launches `runt mcp` from your app install.

## Install the plugin (recommended)

In Claude Code:

```
/plugin marketplace add nteract/claude-plugin
/plugin install nteract@nteract
```

The plugin ships the right binary for your platform — no app install required for the plugin itself. Pair it with the [nteract desktop app](https://nteract.io) to see the notebook update live while an agent works.

## Install via uvx (alternative)

If you'd rather go through `uvx`, use the pre-release channel:

```bash
claude mcp add nteract -- uvx --prerelease allow nteract
```

That will run this PyPI package, which locates `runt mcp` from your nteract desktop install and exec's it.

Only pre-release wheels are being published while the library surface settles — the stable channel is frozen. See [#2217](https://github.com/nteract/desktop/issues/2217).

## What you get

With nteract connected to Claude, you can:

- Create, open, and save notebooks by path or session ID
- Read, write, and execute cells with live output streaming
- Manage Python dependencies (UV and Conda)
- Watch the notebook update in real-time in the desktop app while an agent works
- Hand a notebook back and forth with a human collaborator

## Related

- [nteract/desktop](https://github.com/nteract/desktop) — desktop app, daemon, and plugin source
- [runtimed on PyPI](https://pypi.org/project/runtimed/) — low-level Python bindings for the daemon

## License

BSD-3-Clause
