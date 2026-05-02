#!/usr/bin/env node
/**
 * Smoke-test the high-level @runtimed/node API surface against a running daemon.
 *
 * This intentionally avoids kernel execution so it stays quick and does not
 * depend on environment solving. Build the N-API binding first, then run:
 *
 *   RUNTIMED_SOCKET_PATH=/path/to/runtimed.sock \
 *   RUNTIMED_NODE_SMOKE_MODULE=/tmp/runtimed-node-napi-check/index.cjs \
 *   node packages/runtimed-node/scripts/smoke-api.cjs
 */
"use strict";

const assert = require("node:assert/strict");
const path = require("node:path");

const modulePath = process.env.RUNTIMED_NODE_SMOKE_MODULE
  ? path.resolve(process.env.RUNTIMED_NODE_SMOKE_MODULE)
  : path.resolve(__dirname, "../src/index.cjs");
const rt = require(modulePath);

async function main() {
  const before = await rt.listActiveNotebooks();
  assert(Array.isArray(before), "listActiveNotebooks() returns an array");

  const session = await rt.createNotebook({
    description: "@runtimed/node smoke-api",
    dependencies: ["numpy"],
    packageManager: rt.PackageManager.Uv,
  });

  try {
    assert.match(session.notebookId, /^[0-9a-f-]{36}$/i, "session has UUID notebookId");

    await session.addDependencies(["pandas", "matplotlib"], {
      packageManager: rt.PackageManager.Uv,
    });
    assert.equal(
      await session.removeDependencies(["matplotlib"], {
        packageManager: rt.PackageManager.Uv,
      }),
      1,
      "removeDependencies() returns removal count",
    );

    const deps = await session.getDependencyStatus();
    assert(deps.uv, "uv dependency status is present");
    assert(deps.uv.dependencies.includes("numpy"), "createNotebook() records dependencies");
    assert(deps.uv.dependencies.includes("pandas"), "addDependencies() records dependencies");
    assert(
      !deps.uv.dependencies.includes("matplotlib"),
      "removeDependencies() removes dependencies",
    );
    assert(deps.trust, "trust status is grouped");
    assert.equal(typeof deps.fingerprint, "string", "dependency fingerprint is present");

    const runtime = await session.getRuntimeStatus();
    assert.equal(typeof runtime.status, "string", "runtime status is present");
    assert.equal(typeof runtime.lifecycle, "string", "runtime lifecycle is present");

    const rooms = await rt.listActiveNotebooks();
    assert(
      rooms.some((room) => room.notebookId === session.notebookId),
      "created notebook is listed as active",
    );

    const cellId = await session.createCell("x = 1");
    assert.match(cellId, /^cell-/, "createCell() returns a generated cell ID");

    const cells = await session.listCells();
    assert(
      cells.some((cell) => cell.id === cellId),
      "created cell is listed",
    );

    assert.equal(await session.setCell(cellId, { source: "x = 2" }), true);
    const cell = await session.getCell(cellId);
    assert.equal(cell.source, "x = 2", "setCell() updates source");

    const newPosition = await session.moveCell(cellId, null);
    assert.equal(typeof newPosition, "string", "moveCell() returns a position");

    const show = await session.showNotebook();
    assert.equal(typeof show.opened, "boolean", "showNotebook() returns structured status");

    assert.equal(await session.deleteCell(cellId), true, "deleteCell() deletes existing cell");
    assert.equal(await session.getCell(cellId), null, "deleted cell is absent");

    assert.equal(await session.shutdownNotebook(), true, "shutdownNotebook() evicts room");
    const after = await rt.listActiveNotebooks();
    assert(
      !after.some((room) => room.notebookId === session.notebookId),
      "shutdown notebook is no longer active",
    );

    console.log(
      JSON.stringify({
        ok: true,
        module: modulePath,
        before: before.length,
        after: after.length,
      }),
    );
  } finally {
    try {
      await session.close();
    } catch {
      // The smoke path normally closes by shutting down the notebook room.
    }
  }
}

main().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
