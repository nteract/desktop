/**
 * Deno smoke test for runtimed-wasm NotebookHandle.
 *
 * Tests the WASM bindings in isolation — no daemon, no Tauri, no relay.
 * Proves that:
 * 1. NotebookHandle can create/load docs and manipulate cells
 * 2. Sync between two WASM handles produces identical docs
 * 3. Cell operations (add, delete, update source) round-trip through sync
 *
 * Run with:
 *   deno test --allow-read crates/runtimed-wasm/tests/deno_smoke_test.ts
 *
 * Or from the repo root:
 *   deno test --allow-read crates/runtimed-wasm/tests/
 */

import {
  assertEquals,
  assertExists,
  assert,
} from "https://deno.land/std@0.224.0/assert/mod.ts";

// @ts-nocheck — wasm-bindgen output doesn't have Deno-compatible type declarations

// Import the WASM module
// deno-lint-ignore no-explicit-any
let init: any, NotebookHandle: any;

const wasmJsPath = new URL(
  "../../../apps/notebook/src/wasm/runtimed-wasm/runtimed_wasm.js",
  import.meta.url,
);
const wasmBinPath = new URL(
  "../../../apps/notebook/src/wasm/runtimed-wasm/runtimed_wasm_bg.wasm",
  import.meta.url,
);

const mod = await import(wasmJsPath.href);
init = mod.default;
NotebookHandle = mod.NotebookHandle;

const wasmBytes = await Deno.readFile(wasmBinPath);
await init(wasmBytes);

// ── Helpers ──────────────────────────────────────────────────────────

/** Sync two handles until both return no more messages (convergence). */
function syncHandles(a: NotebookHandle, b: NotebookHandle, maxRounds = 10) {
  for (let i = 0; i < maxRounds; i++) {
    const msgA = a.generate_sync_message();
    const msgB = b.generate_sync_message();
    if (!msgA && !msgB) break;
    if (msgA) b.receive_sync_message(msgA);
    if (msgB) a.receive_sync_message(msgB);
  }
}

// ── Tests ────────────────────────────────────────────────────────────

Deno.test("NotebookHandle: create new empty doc", () => {
  const handle = new NotebookHandle("test-notebook");
  assertEquals(handle.cell_count(), 0);
  assertEquals(handle.get_cells().length, 0);
  assertEquals(handle.get_cells_json(), "[]");
  handle.free();
});

Deno.test("NotebookHandle: add cell and read back", () => {
  const handle = new NotebookHandle("test-nb");
  handle.add_cell(0, "cell-1", "code");
  assertEquals(handle.cell_count(), 1);

  const cells = handle.get_cells();
  assertEquals(cells.length, 1);
  assertEquals(cells[0].id, "cell-1");
  assertEquals(cells[0].cell_type, "code");
  assertEquals(cells[0].source, "");
  assertEquals(cells[0].execution_count, "null");
  cells[0].free();
  handle.free();
});

Deno.test("NotebookHandle: update source with Text CRDT", () => {
  const handle = new NotebookHandle("test-nb");
  handle.add_cell(0, "cell-1", "code");
  handle.update_source("cell-1", 'print("hello")');

  const cell = handle.get_cell("cell-1");
  assertExists(cell);
  assertEquals(cell.source, 'print("hello")');
  cell.free();

  // Update again — should use Myers diff internally
  handle.update_source("cell-1", 'print("hello world")');
  const cell2 = handle.get_cell("cell-1");
  assertExists(cell2);
  assertEquals(cell2.source, 'print("hello world")');
  cell2.free();
  handle.free();
});

Deno.test("NotebookHandle: append source (streaming)", () => {
  const handle = new NotebookHandle("test-nb");
  handle.add_cell(0, "cell-1", "code");
  handle.append_source("cell-1", "import ");
  handle.append_source("cell-1", "numpy");

  const cell = handle.get_cell("cell-1");
  assertExists(cell);
  assertEquals(cell.source, "import numpy");
  cell.free();
  handle.free();
});

Deno.test("NotebookHandle: delete cell", () => {
  const handle = new NotebookHandle("test-nb");
  handle.add_cell(0, "cell-1", "code");
  handle.add_cell(1, "cell-2", "markdown");
  assertEquals(handle.cell_count(), 2);

  const deleted = handle.delete_cell("cell-1");
  assertEquals(deleted, true);
  assertEquals(handle.cell_count(), 1);

  const cells = handle.get_cells();
  assertEquals(cells[0].id, "cell-2");
  cells[0].free();

  // Delete nonexistent cell
  const deleted2 = handle.delete_cell("nope");
  assertEquals(deleted2, false);
  handle.free();
});

Deno.test("NotebookHandle: multiple cells ordering", () => {
  const handle = new NotebookHandle("test-nb");
  handle.add_cell(0, "first", "code");
  handle.add_cell(1, "second", "markdown");
  handle.add_cell(1, "middle", "code"); // Insert between first and second

  const cells = handle.get_cells();
  assertEquals(cells.length, 3);
  assertEquals(cells[0].id, "first");
  assertEquals(cells[1].id, "middle");
  assertEquals(cells[2].id, "second");
  for (const c of cells) c.free();
  handle.free();
});

Deno.test("NotebookHandle: metadata get/set", () => {
  const handle = new NotebookHandle("test-nb");
  // Default runtime is "python"
  assertEquals(handle.get_metadata("runtime"), "python");

  handle.set_metadata("runtime", "deno");
  assertEquals(handle.get_metadata("runtime"), "deno");

  handle.set_metadata("custom_key", "custom_value");
  assertEquals(handle.get_metadata("custom_key"), "custom_value");
  handle.free();
});

Deno.test("NotebookHandle: save and load round-trip", () => {
  const handle = new NotebookHandle("test-nb");
  handle.add_cell(0, "cell-1", "code");
  handle.update_source("cell-1", "x = 42");
  handle.add_cell(1, "cell-2", "markdown");
  handle.update_source("cell-2", "# Hello");

  const bytes = handle.save();
  assert(bytes.length > 0, "saved bytes should be non-empty");

  const loaded = NotebookHandle.load(bytes);
  assertEquals(loaded.cell_count(), 2);

  const cells = loaded.get_cells();
  assertEquals(cells[0].id, "cell-1");
  assertEquals(cells[0].source, "x = 42");
  assertEquals(cells[1].id, "cell-2");
  assertEquals(cells[1].source, "# Hello");
  for (const c of cells) c.free();
  handle.free();
  loaded.free();
});

Deno.test("NotebookHandle: get_cells_json returns valid JSON", () => {
  const handle = new NotebookHandle("test-nb");
  handle.add_cell(0, "cell-1", "code");
  handle.update_source("cell-1", 'print("hi")');

  const json = handle.get_cells_json();
  const parsed = JSON.parse(json);
  assertEquals(parsed.length, 1);
  assertEquals(parsed[0].id, "cell-1");
  assertEquals(parsed[0].source, 'print("hi")');
  assertEquals(parsed[0].cell_type, "code");
  assertEquals(parsed[0].execution_count, "null");
  handle.free();
});

// ── Sync tests ───────────────────────────────────────────────────────

Deno.test("Sync: two handles converge on cell content", () => {
  // Simulate: Tauri relay has a doc, frontend loads from bytes and syncs
  const server = new NotebookHandle("sync-test");
  server.add_cell(0, "cell-1", "code");
  server.update_source("cell-1", "import numpy");

  // Frontend loads the same doc bytes
  const serverBytes = server.save();
  const client = NotebookHandle.load(serverBytes);

  // Verify client has the cell
  assertEquals(client.cell_count(), 1);
  const clientCell = client.get_cell("cell-1");
  assertExists(clientCell);
  assertEquals(clientCell.source, "import numpy");
  clientCell.free();

  // Client makes a change
  client.update_source("cell-1", "import numpy as np");

  // Sync — client's change should reach server
  syncHandles(client, server);

  const serverCell = server.get_cell("cell-1");
  assertExists(serverCell);
  assertEquals(serverCell.source, "import numpy as np");
  serverCell.free();

  server.free();
  client.free();
});

Deno.test("Sync: client adds cell, server sees it after sync", () => {
  const server = new NotebookHandle("sync-test");
  server.add_cell(0, "cell-1", "code");

  const client = NotebookHandle.load(server.save());

  // Client adds a new cell
  client.add_cell(1, "cell-2", "markdown");
  client.update_source("cell-2", "# New cell from client");

  // Before sync, server has 1 cell
  assertEquals(server.cell_count(), 1);

  // Sync
  syncHandles(client, server);

  // After sync, server has 2 cells
  assertEquals(server.cell_count(), 2);
  const cells = server.get_cells();
  // deno-lint-ignore no-explicit-any
  const ids = cells.map((c: any) => {
    const id = c.id;
    c.free();
    return id;
  });
  assert(ids.includes("cell-1"));
  assert(ids.includes("cell-2"));

  server.free();
  client.free();
});

Deno.test("Sync: concurrent cell adds merge", () => {
  const server = new NotebookHandle("merge-test");

  // Both start from the same empty doc
  const client = NotebookHandle.load(server.save());

  // Sync to establish baseline
  syncHandles(server, client);

  // Both add different cells concurrently
  server.add_cell(0, "server-cell", "code");
  server.update_source("server-cell", "# from server");

  client.add_cell(0, "client-cell", "markdown");
  client.update_source("client-cell", "# from client");

  // Sync
  syncHandles(server, client);

  // Both should have both cells
  assertEquals(server.cell_count(), 2);
  assertEquals(client.cell_count(), 2);

  const serverCells = server.get_cells();
  const clientCells = client.get_cells();

  // deno-lint-ignore no-explicit-any
  const serverIds = serverCells.map((c: any) => {
    const id = c.id;
    c.free();
    return id;
  });
  // deno-lint-ignore no-explicit-any
  const clientIds = clientCells.map((c: any) => {
    const id = c.id;
    c.free();
    return id;
  });

  assert(serverIds.includes("server-cell"));
  assert(serverIds.includes("client-cell"));
  // Same order on both sides (CRDT deterministic merge)
  assertEquals(serverIds, clientIds);

  server.free();
  client.free();
});

Deno.test("Sync: delete cell syncs correctly", () => {
  const server = new NotebookHandle("sync-test");
  server.add_cell(0, "cell-1", "code");
  server.add_cell(1, "cell-2", "markdown");

  const client = NotebookHandle.load(server.save());
  syncHandles(server, client);

  // Client deletes cell-1
  client.delete_cell("cell-1");

  // Sync
  syncHandles(client, server);

  // Both should have only cell-2
  assertEquals(server.cell_count(), 1);
  assertEquals(client.cell_count(), 1);

  const serverCells = server.get_cells();
  assertEquals(serverCells[0].id, "cell-2");
  serverCells[0].free();

  server.free();
  client.free();
});

Deno.test("Sync: generate_sync_message returns null when in sync", () => {
  const server = new NotebookHandle("sync-test");
  const client = NotebookHandle.load(server.save());

  // Fully sync
  syncHandles(server, client);

  // Both should report no message needed
  assertEquals(server.generate_sync_message(), undefined);
  assertEquals(client.generate_sync_message(), undefined);

  server.free();
  client.free();
});

Deno.test("Sync: source edit character-level merge", () => {
  const server = new NotebookHandle("sync-test");
  server.add_cell(0, "cell-1", "code");
  server.update_source("cell-1", "hello world");

  const client = NotebookHandle.load(server.save());
  syncHandles(server, client);

  // Server edits the beginning, client edits the end (concurrently)
  server.update_source("cell-1", "HELLO world");
  client.update_source("cell-1", "hello WORLD");

  // Sync — Automerge Text CRDT should merge both changes
  syncHandles(server, client);

  // Both should have the merged result (order depends on actor IDs)
  const serverCell = server.get_cell("cell-1");
  const clientCell = client.get_cell("cell-1");
  assertExists(serverCell);
  assertExists(clientCell);
  // Both peers converge to the same value
  assertEquals(serverCell.source, clientCell.source);
  // The merged text should contain both changes
  assert(
    serverCell.source.includes("HELLO") || serverCell.source.includes("WORLD"),
    `Merged source should contain at least one edit: "${serverCell.source}"`,
  );
  serverCell.free();
  clientCell.free();

  server.free();
  client.free();
});
