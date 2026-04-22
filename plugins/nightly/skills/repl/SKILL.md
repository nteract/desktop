---
name: repl
description: Use nteract notebooks as a persistent Python REPL. Trigger this skill whenever you're about to run python3 -c, write a throwaway .py script, or chain multiple shell commands for data exploration, analysis, plotting, or iterative computation. Notebooks preserve state between cells, show rich output, and can be used in realtime with users.
---

# Use a Notebook Instead of python3 -c

When you have nteract MCP tools available and you're about to do multi-step Python work — chaining `python3 -c` commands, writing a throwaway `.py` script, or running exploratory code — use a notebook instead. You get persistent state between cells, rich output (tables, plots, errors with tracebacks), and users and agents can view the notebook in realtime.

## If the notebook tools aren't appearing

If you loaded this skill but tools like `create_notebook`, `create_cell`, `execute_cell` are not available, don't fall back to `python3 -c`. Ask the user to run `/reload-plugins` and try again. The most common cause is that the plugin was installed this session — Claude Code queues newly-registered MCP servers for the next reload.

If tools still don't appear after `/reload-plugins`:

- Confirm the nteract desktop app is running (the MCP server talks to `runtimed` via a Unix socket).
- Run `runt doctor` to check on the installation. (`runt-nightly` if this is the nightly release)
- Share any error messages from the session

## Quick Start

```
create_notebook(path="~/analysis.ipynb")
create_cell(source="import pandas as pd\ndf = pd.read_csv('data.csv')\ndf.head()", cell_type="code", and_run=true)
```

## Core Workflow

1. **Start a notebook:**
   `create_notebook(path="~/analysis.ipynb")` — creates and opens it.

2. **Add and run code cells:**
   `create_cell(source="your code here", cell_type="code", and_run=true)` — creates the cell AND executes it in one call. State persists: variables from earlier cells are available in later ones.

3. **Iterate on a cell:**
   `set_cell(cell_id="...", source="updated code")` then `execute_cell(cell_id="...")` — edit and re-run without creating a new cell.

4. **Check your work:**
   `get_all_cells(format="summary", include_outputs=true)` — see all cells with output previews at a glance.

5a. **Save when done:**
   `save_notebook()` — writes the `.ipynb` to disk.

5b. **Open the App for the User:**
   `launch_app()` — opens the notebook app for the user. Use if they ask to show it to them. Can be disruptive if the user doesn't expect it.


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
