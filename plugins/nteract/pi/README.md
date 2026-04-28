# @nteract/pi

Pi extensions for nteract.

## Extensions

- `extensions/repl.ts` registers a persistent `python` tool backed by the local
  nteract `runtimed` daemon through `@runtimed/node`.
- It also registers `python_add_dependencies`, `python_save_notebook`, and
  `/python-reset`.

## Install

Once published:

```bash
pi install npm:@nteract/pi@next
```

## Local Use

From this checkout:

```bash
pi --extension ./plugins/nteract/pi/extensions/repl.ts
```
