# @runtimed/node

Node.js bindings for the nteract `runtimed` daemon. This package lets Node,
Bun, and other CommonJS-compatible runtimes create notebooks, run Python cells,
queue executions, read outputs, save notebooks, and manage notebook dependencies
through the same local daemon used by nteract desktop.

## Install

```bash
npm install @runtimed/node
```

`@runtimed/node` ships a small JavaScript wrapper plus TypeScript declarations.
The native binding is installed through an optional platform package such as
`@runtimed/node-darwin-arm64` or `@runtimed/node-linux-x64-gnu`.

## Basic Usage

```js
const { createNotebook, defaultSocketPath } = require("@runtimed/node");

async function main() {
  const session = await createNotebook({
    runtime: "python",
    workingDir: process.cwd(),
    // Record these before the first cell runs.
    dependencies: ["numpy", "matplotlib"],
    packageManager: "uv",
    description: "plotting smoke test",
  });

  try {
    console.log("daemon socket:", defaultSocketPath());

    await session.syncEnvironment();

    const result = await session.runCell(`
import numpy as np
import matplotlib.pyplot as plt

x = np.linspace(0, 6.28, 200)
plt.plot(x, np.sin(x))
plt.show()
`);
    console.log(result.status);
    console.log(result.outputs);

    await session.saveNotebook();
  } finally {
    await session.close();
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
```

## Notebook Dependencies

`createNotebook()` accepts `dependencies` so agent code can declare packages
up-front instead of failing the first import and retrying after `addUvDependency()`.
The `packageManager` option is typed from the native binding's `PackageManager`
string enum (`"uv"`, `"conda"`, or `"pixi"`) and is converted to the shared
notebook protocol enum before the daemon handshake. `description` can be used
as a human-readable peer label for agent-created sessions.

## API Surface

- `defaultSocketPath()` returns the socket path for the current nteract channel
  or the `RUNTIMED_SOCKET_PATH` override.
- `socketPathForChannel("stable" | "nightly")` returns a channel-specific
  daemon socket path.
- `listActiveNotebooks(options)` lists active daemon notebook rooms.
- `createNotebook(options)` creates a notebook and records optional first-call dependencies.
- `openNotebook(notebookId, options)` connects to an existing daemon notebook.
- `openNotebookPath(path, options)` opens a notebook file through the daemon.
- `showNotebook(options)` opens an active notebook or path in nteract Desktop,
  returning a structured `opened: false` response in headless environments.
- `shutdownNotebook(notebookId, options)` shuts down a notebook room by ID.
- `getExecutionResult(executionId, options)` reads a result by execution ID.
- `Session.listCells()` and `Session.getCell(cellId)` inspect notebook cells.
- `Session.createCell(source, options)`, `Session.setCell(cellId, options)`,
  `Session.deleteCell(cellId)`, and `Session.moveCell(cellId, options)` provide
  direct notebook editing without MCP JSON round-trips.
- `Session.executeCell(cellId, options)` runs an existing code cell.
- `Session.showNotebook()` opens the session in nteract Desktop when a display
  is available.
- `Session.interruptKernel()`, `Session.shutdownKernel()`, and
  `Session.restartKernel()` manage the running kernel.
- `Session.shutdownNotebook()` shuts down this notebook room and closes the session.
- `Session.runCell(source, options)` creates, runs, and waits for a cell.
- `Session.queueCell(source, options)` queues a cell and returns IDs.
- `Session.waitForExecution(executionId, options)` waits for queued work.
- `Session.addDependency(spec, { packageManager })` /
  `Session.addDependencies(specs, { packageManager })` and
  `Session.removeDependency(spec, { packageManager })` /
  `Session.removeDependencies(specs, { packageManager })` edit notebook
  dependency metadata for UV, Conda, or Pixi. Batch variants use one CRDT
  metadata transaction.
- `Session.getDependencyStatus()` returns dependency metadata, fingerprint, and
  trust state in one call.
- `Session.getRuntimeStatus()` returns kernel lifecycle, activity, env source,
  and startup error details.
- `Session.addUvDependency(spec)` records a UV dependency for the notebook.
- `Session.syncEnvironment()` installs recorded notebook dependencies.
- `Session.saveNotebook(path?)` saves the notebook.
- `Session.close()` releases the daemon connection.

## Daemon Requirements

The package talks to a local `runtimed` daemon over its Unix socket. In a
development checkout, run the per-worktree daemon before using the bindings:

```bash
cargo xtask dev-daemon
```

Published nteract desktop builds manage their own daemon. Set
`RUNTIMED_SOCKET_PATH` when you need to connect to a specific daemon instance.

## Development Smoke Test

After building the native binding, run the daemon-backed API smoke test with:

```bash
RUNTIMED_SOCKET_PATH=/path/to/runtimed.sock pnpm --dir packages/runtimed-node smoke:api
```

When testing an out-of-tree N-API build, point the smoke script at it:

```bash
RUNTIMED_NODE_SMOKE_MODULE=/tmp/runtimed-node-napi-check/index.cjs \
RUNTIMED_SOCKET_PATH=/path/to/runtimed.sock \
pnpm --dir packages/runtimed-node smoke:api
```

## Platform Packages

The platform packages are implementation details and should normally be
installed through `@runtimed/node`:

- `@runtimed/node-darwin-arm64`
- `@runtimed/node-linux-x64-gnu`

They contain only the compiled native `.node` binary for their target platform.
