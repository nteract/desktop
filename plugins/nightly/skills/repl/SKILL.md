---
name: repl
description: Use nteract notebooks as a persistent Python REPL. Trigger this skill whenever you're about to run python3 -c, write a throwaway .py script, or chain multiple shell commands for data exploration, analysis, plotting, or iterative computation. Notebooks preserve state between cells, show rich output, and can be used in realtime with users.
---

# Use a Notebook Instead of python3 -c

When nteract notebook tools are available and you're about to do multi-step Python work — chaining `python3 -c` commands, writing a throwaway `.py` script, or running exploratory code — use a notebook-backed REPL instead. You get persistent state between cells, rich output (tables, plots, errors with tracebacks), and users and agents can view the notebook in realtime.

Prefer the direct pi tools when present:

- `python` — execute code in the persistent notebook session.
- `python_add_dependencies` — batch-add packages and hot-sync the environment.
- `python_save_notebook` — save the backing notebook.

If only MCP tools are available, use `create_notebook`, `create_cell`, `execute_cell`, `set_cell`, and `get_all_cells`.

## If the notebook tools aren't appearing

If you loaded this skill but neither the direct pi tools (`python`, `python_add_dependencies`) nor the MCP notebook tools are available, don't fall back to `python3 -c`. Ask the user to run `/reload-plugins` and try again. The most common cause is that the plugin was installed this session and the agent needs a reload.

If tools still don't appear after reload:

- Confirm the nteract desktop app/daemon is running.
- Run `runt doctor` to check the installation. (`runt-nightly` if this is the nightly release)
- Share any error messages from the session.

## Quick Start (direct pi tools)

```json
python({
  "code": "import numpy as np\nnp.arange(3)",
  "dependencies": ["numpy"]
})
```

Pass `dependencies` on the first `python` call when imports may be missing. The pi REPL records them before kernel startup when possible; later dependency additions use hot-sync.

## Quick Start (MCP tools)

```
create_notebook(dependencies=["numpy"])
create_cell(source="import numpy as np\nnp.arange(3)", cell_type="code", and_run=true)
```

## Core Workflow

1. **Start or reuse a notebook-backed REPL:**
   - Direct: call `python(...)`; the session is created lazily and state persists.
   - MCP: call `create_notebook(...)` or `connect_notebook(...)`.

2. **Declare dependencies before import-heavy code:**
   - Direct: pass `dependencies` to `python` or call `python_add_dependencies`.
   - MCP: pass `dependencies` to `create_notebook` or use `manage_dependencies`.

3. **Run and iterate:**
   - Direct: call `python` repeatedly; variables/imports persist.
   - MCP: edit with `set_cell(...)` and rerun with `execute_cell(...)`.

4. **Check your work:**
   - Direct: inspect returned text/images/tables.
   - MCP: `get_all_cells(format="summary", include_outputs=true)`.

5. **Save when done:**
   `python_save_notebook(...)` or `save_notebook(...)`.

6. **Open the app for the user:**
   `show_notebook()` when they ask to see it. This can be disruptive if unexpected.

## When to Use This

- Exploring a dataset (load, filter, plot, iterate)
- Running multi-step computations where later steps depend on earlier results
- Generating visualizations (matplotlib, plotly, altair)
- Prototyping code that you'll refine over several iterations
- Any task where you'd otherwise chain 3+ `python3 -c` commands

## When NOT to Use This

- One-shot commands (`python3 -c "print(2+2)"` is fine as-is)
- Running existing scripts (`python3 script.py`)
- Non-Python tasks
