# @nteract/pi

Pi extensions for running Python through the local nteract `runtimed` daemon.
The package provides a persistent notebook-backed REPL for coding agents and
terminal workflows that need stateful Python execution.

## Extensions

- `extensions/repl.ts` registers a persistent `python` tool backed by
  `@runtimed/node`.
- `python` accepts an optional `dependencies` array. On first use, those
  packages are recorded before the kernel starts; on later calls they are
  hot-synced before executing the cell.
- `python_add_dependencies` batch-records notebook UV dependencies and hot-syncs
  them using the direct Node binding.
- `python_save_notebook` saves the backing notebook.
- `/python-reset` shuts down the backing notebook room and starts fresh on the
  next `python` call.

## Install

```bash
pi install npm:@nteract/pi@next
```

Use the `next` tag for prerelease builds until the package is promoted to the
default npm tag.

## Local Use

From this checkout:

```bash
pi --extension ./plugins/nteract/pi/extensions/repl.ts
```
