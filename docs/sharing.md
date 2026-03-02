# Sharing Notebooks

Notebooks created with nteract Desktop are standard `.ipynb` files that work with Jupyter and other notebook tools. This guide covers how to share notebooks so recipients get the right environment.

## Two Sharing Models

### Inline Dependencies (Portable)

Dependencies stored directly in the notebook metadata. The notebook is self-contained — anyone who opens it gets the same packages without needing any project files.

**Best for**: Sending a notebook to a colleague, posting to GitHub, or any situation where the recipient won't have your project setup.

**How to create**: Add dependencies through the dependency panel, or use "Copy to notebook" from a project file banner to snapshot project deps into the notebook.

### Project-Level Reference (Stays in Sync)

The notebook lives alongside a project file (`pyproject.toml`, `environment.yml`, or `pixi.toml`). nteract Desktop auto-detects the project file and creates the environment from it.

**Best for**: Notebooks in a git repository where everyone clones the same project structure.

**How to create**: Put your notebook in a directory with a project file. When the recipient clones the repo and opens the notebook, nteract Desktop detects the project file automatically.

## What Happens on the Recipient's End

### Notebook with inline dependencies

1. Recipient opens the notebook
2. Trust dialog appears (dependencies are unsigned on a new machine)
3. Recipient approves the dependencies
4. nteract Desktop installs the packages and starts the kernel
5. Notebook is ready to use

### Notebook in a project (pyproject.toml, environment.yml, pixi.toml)

1. Recipient clones the repository
2. Opens the notebook in nteract Desktop
3. nteract Desktop detects the project file and creates the environment automatically
4. No trust dialog (project file deps don't require trust approval)
5. Notebook is ready to use

### Notebook with no dependencies

1. Recipient opens the notebook
2. nteract Desktop starts a prewarmed environment with just the basics (ipykernel, ipywidgets)
3. Recipient needs to add packages manually

## Making Sharing Seamless

- **Include dependencies**: Always add your packages to the notebook or use a project file. A notebook with no dependency information forces the recipient to guess what's needed.
- **Use inline deps for standalone notebooks**: If the notebook is meant to be shared without a project, use "Copy to notebook" to embed dependencies directly.
- **Use project files for repositories**: If the notebook lives in a git repo, keep dependencies in `pyproject.toml` or `environment.yml` and let nteract Desktop auto-detect them.
- **Avoid mixing UV and conda deps**: A notebook with both UV and conda dependencies can cause confusion. Pick one backend for each notebook.

## The Trust Dialog

When you open a notebook with inline dependencies from another machine, nteract Desktop shows a trust dialog. This is a security measure — it prevents notebooks from silently installing arbitrary packages.

The dialog shows what dependencies the notebook wants to install. After you approve, nteract Desktop signs the notebook with your machine's key and won't ask again for that notebook (unless the dependencies change).

Project-file-based environments (pyproject.toml, environment.yml, pixi.toml) don't trigger the trust dialog because the dependencies come from files you can inspect in the repository, not from embedded notebook metadata.
