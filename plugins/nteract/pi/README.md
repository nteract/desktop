# @nteract/pi

**Python that remembers.** Run Python code in Pi with persistent state, hot dependency installation, and zero cold starts.

Perfect for data analysis, plotting, and multi-step workflows where you want variables and imports to stick around between agent turns.

## What you get

- **`python`** — Execute Python code. State (variables, imports) persists across calls in your Pi session. The last expression is returned as the result; `print()` for side effects. Images (matplotlib, PIL) come back inline so you can see them.
  
- **`python_add_dependencies`** — Install packages into the running environment without restarting. Pass `dependencies` on the first `python` call to pre-install before the kernel starts.

- **`python_save_notebook`** — Save your session as an `.ipynb` file.

- **`/python-reset`** — Start fresh (new kernel, clean slate).

## Install

```bash
pi install npm:@nteract/pi
```

## How it works

Under the hood, this uses the local **nteract** daemon (the same Python runtime that powers the nteract desktop app). If you have nteract installed, you already have everything you need. The daemon manages isolated environments per working directory, handles dependency installation via `uv`, and keeps your Python state hot between agent calls.

**Power users:** The daemon is controlled by the `runt` CLI (installed with nteract). You can inspect active sessions, manage kernels, and open notebooks in the desktop app:

```bash
# List active Python sessions
runt list

# Open the current session in nteract Desktop
runt show <notebook-id>

# Check daemon status
runt daemon status
```

## Local development

From this repo:

```bash
pi --extension ./plugins/nteract/pi/extensions/repl.ts
```
