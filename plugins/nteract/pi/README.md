# @nteract/pi

Pi extensions for running Python through the local nteract `runtimed` daemon.
The package provides a persistent notebook-backed REPL for coding agents and
terminal workflows that need stateful Python execution.

## Extensions

- `extensions/repl.ts` registers a persistent `python` tool backed by
  `@runtimed/node`.
- `python_add_dependencies` records notebook UV dependencies.
- `python_save_notebook` saves the backing notebook.
- `/python-reset` starts a fresh notebook session.

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
