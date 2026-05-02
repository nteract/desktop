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

## API Surface

- `defaultSocketPath()` returns the socket path for the current nteract channel
  or the `RUNTIMED_SOCKET_PATH` override.
- `socketPathForChannel("stable" | "nightly")` returns a channel-specific
  daemon socket path.
- `createNotebook(options)` creates a notebook and records optional first-call dependencies.
- `openNotebook(notebookId, options)` connects to an existing notebook.
- `getExecutionResult(executionId, options)` reads a result by execution ID.
- `Session.listCells()` and `Session.getCell(cellId)` inspect notebook cells.
- `Session.createCell(source, options)`, `Session.setCell(cellId, options)`,
  `Session.deleteCell(cellId)`, and `Session.moveCell(cellId, options)` provide
  direct notebook editing without MCP JSON round-trips.
- `Session.executeCell(cellId, options)` runs an existing code cell.
- `Session.runCell(source, options)` creates, runs, and waits for a cell.
- `Session.queueCell(source, options)` queues a cell and returns IDs.
- `Session.waitForExecution(executionId, options)` waits for queued work.
- `Session.addUvDependency(spec)` records a UV dependency for the notebook.
- `Session.syncEnvironment()` installs recorded notebook dependencies.
- `Session.saveNotebook(path?)` saves the notebook.
- `Session.close()` releases the daemon connection.

## Daemon Requirements

`createNotebook()` accepts `dependencies` so agent code can declare packages
up-front instead of failing the first import and retrying after `addUvDependency()`.
The `packageManager` option is typed from the native binding's `PackageManager`
string enum (`"uv"`, `"conda"`, or `"pixi"`) and is converted to the shared
notebook protocol enum before the daemon handshake. `description` can be used
as a human-readable peer label for agent-created sessions.

The package talks to a local `runtimed` daemon over its Unix socket. In a
development checkout, run the per-worktree daemon before using the bindings:

```bash
cargo xtask dev-daemon
```

Published nteract desktop builds manage their own daemon. Set
`RUNTIMED_SOCKET_PATH` when you need to connect to a specific daemon instance.

## Platform Packages

The platform packages are implementation details and should normally be
installed through `@runtimed/node`:

- `@runtimed/node-darwin-arm64`
- `@runtimed/node-linux-x64-gnu`

They contain only the compiled native `.node` binary for their target platform.
