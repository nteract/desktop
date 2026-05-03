# @nteract/pi

**Persistent notebook-backed Python REPL for Pi coding agents.** Stateful execution, hot dependency sync, zero cold starts.

Run Python in a real IPython runtime. State (variables, imports, matplotlib figures) persists between agent turns. Perfect for data analysis, plotting, and multi-step workflows.

## What you get

- **`python_repl`** — Execute Python in your persistent REPL. Backed by a real IPython runtime. Variables, imports, and state stick around between calls. The last expression is the result; use `print()` or `display()` for intermediate output. Images (matplotlib, PIL, widgets) are returned inline.
  
- **`python_add_dependencies`** — Install packages mid-session without restarting the kernel. Or pass `dependencies` on the first `python_repl` call to pre-install before kernel start.

- **`python_save_notebook`** — Save your session as an `.ipynb` file you can open in Jupyter or nteract.

- **`/python-reset`** — Start fresh: new kernel, clean slate.

## Install

```bash
pi install npm:@nteract/pi
```

## How it works

This extension uses **`@runtimed/node`**, the Node.js bindings for the nteract daemon (the same runtime that powers the nteract desktop app). If you have nteract installed, you already have everything you need. The daemon manages isolated Jupyter kernels and environments per working directory, handles dependency installation via `uv`, and keeps your Python state hot between agent calls.

## Building your own

This extension is a starting point. Want more control? Build your own Pi extensions using `@runtimed/node`.

See [`packages/runtimed-node/README.md`](../../packages/runtimed-node/README.md) for the full API. The source for this extension ([`extensions/repl.ts`](./extensions/repl.ts)) shows how to wire it up to Pi's tool registration.

### Inspecting and managing sessions

The daemon is controlled by the `runt` CLI (installed with nteract). Use it to inspect active sessions, open notebooks in the desktop app, or troubleshoot:

```bash
# List active Python sessions
runt list

# Open a session in nteract Desktop
runt show <notebook-id>

# Check daemon status
runt daemon status
```

## Local development

From this repo:

```bash
pi --extension ./plugins/nteract/pi/extensions/repl.ts
```
