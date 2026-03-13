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
  assert,
  assertEquals,
  assertExists,
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

// ── Sync protocol integration tests (WASM-specific) ─────────────────

Deno.test("Sync: bootstrap from saved bytes preserves all content", () => {
  // Daemon has existing content with cells, outputs pattern (like bootstrap)
  const daemon = new NotebookHandle("bootstrap-test");
  daemon.add_cell(0, "cell-1", "code");
  daemon.update_source("cell-1", "import numpy as np");
  daemon.add_cell(1, "cell-2", "markdown");
  daemon.update_source("cell-2", "# Analysis");
  daemon.set_metadata("custom_key", "custom_value");

  // WASM loads from daemon's bytes (the GetDocBytes bootstrap path)
  const wasm = NotebookHandle.load(daemon.save());

  // WASM should have all content immediately
  assertEquals(wasm.cell_count(), 2);
  const cells = wasm.get_cells();
  assertEquals(cells[0].id, "cell-1");
  assertEquals(cells[0].source, "import numpy as np");
  assertEquals(cells[1].id, "cell-2");
  assertEquals(cells[1].source, "# Analysis");
  assertEquals(wasm.get_metadata("custom_key"), "custom_value");

  // Sync should converge immediately (no changes needed)
  syncHandles(daemon, wasm);
  assertEquals(daemon.generate_sync_message(), undefined);
  assertEquals(wasm.generate_sync_message(), undefined);

  for (const c of cells) c.free();
  daemon.free();
  wasm.free();
});

Deno.test("Sync: load from bytes + incremental sync with changed flag", () => {
  const daemon = new NotebookHandle("incremental-test");
  daemon.add_cell(0, "existing", "code");
  daemon.update_source("existing", "x = 42");

  // WASM loads existing content via GetDocBytes equivalent
  const wasm = NotebookHandle.load(daemon.save());
  assertEquals(wasm.cell_count(), 1);

  // Initial sync — should already be converged (no changes expected)
  syncHandles(daemon, wasm);

  // Verify sync state is converged
  assertEquals(daemon.generate_sync_message(), undefined);
  assertEquals(wasm.generate_sync_message(), undefined);

  // Daemon adds new content
  daemon.add_cell(1, "new-cell", "markdown");
  daemon.update_source("new-cell", "# New section");

  // Generate sync message from daemon
  const msg = daemon.generate_sync_message();
  assertExists(msg, "Daemon should have sync message after mutation");

  // WASM receives and should report changed=true
  const changed = wasm.receive_sync_message(msg);
  assertEquals(
    changed,
    true,
    "receive_sync_message should return true when doc changes",
  );

  // WASM should now have the new cell
  assertEquals(wasm.cell_count(), 2);
  const newCell = wasm.get_cell("new-cell");
  assertExists(newCell);
  assertEquals(newCell.source, "# New section");
  newCell.free();

  daemon.free();
  wasm.free();
});

Deno.test("Sync: converged peers have no sync messages", () => {
  const daemon = new NotebookHandle("converged-test");
  daemon.add_cell(0, "cell-1", "code");
  daemon.update_source("cell-1", "x = 42");

  const wasm = NotebookHandle.load(daemon.save());

  // Sync to convergence
  syncHandles(daemon, wasm);

  // After convergence, neither should have messages
  assertEquals(
    daemon.generate_sync_message(),
    undefined,
    "Daemon has no message when converged",
  );
  assertEquals(
    wasm.generate_sync_message(),
    undefined,
    "WASM has no message when converged",
  );

  // Verify both have identical content
  assertEquals(daemon.cell_count(), wasm.cell_count());
  assertEquals(
    daemon.get_cell("cell-1")?.source,
    wasm.get_cell("cell-1")?.source,
  );

  daemon.free();
  wasm.free();
});

Deno.test("Sync: reset_sync_state allows re-sync from scratch", () => {
  const daemon = new NotebookHandle("reset-test");
  daemon.add_cell(0, "cell-1", "code");
  daemon.update_source("cell-1", "original");

  const wasm = NotebookHandle.load(daemon.save());
  syncHandles(daemon, wasm);

  // Both converged
  assertEquals(daemon.generate_sync_message(), undefined);
  assertEquals(wasm.generate_sync_message(), undefined);

  // Daemon updates the cell
  daemon.update_source("cell-1", "updated");

  // WASM resets sync state (simulating HMR reload or reconnect)
  wasm.reset_sync_state();

  // After reset, WASM should need to sync again
  const wasmMsg = wasm.generate_sync_message();
  assertExists(
    wasmMsg,
    "After reset_sync_state, WASM should generate sync message",
  );

  // Sync should converge with daemon's update
  syncHandles(daemon, wasm);

  const cell = wasm.get_cell("cell-1");
  assertExists(cell);
  assertEquals(cell.source, "updated");
  cell.free();

  daemon.free();
  wasm.free();
});

Deno.test("Sync: bidirectional mutations converge", () => {
  const daemon = new NotebookHandle("bidirectional-test");
  const wasm = NotebookHandle.load(daemon.save());
  syncHandles(daemon, wasm);

  // WASM adds a cell
  wasm.add_cell(0, "wasm-cell", "code");
  wasm.update_source("wasm-cell", "# From WASM");

  // Sync to daemon
  syncHandles(wasm, daemon);
  assertEquals(daemon.cell_count(), 1);
  assertEquals(daemon.get_cell("wasm-cell")?.source, "# From WASM");

  // Daemon adds another cell (simulating output or execution)
  daemon.add_cell(1, "daemon-cell", "code");
  daemon.update_source("daemon-cell", "# From daemon");

  // Sync back to WASM
  syncHandles(daemon, wasm);

  // Both should have both cells
  assertEquals(wasm.cell_count(), 2);
  assertEquals(daemon.cell_count(), 2);

  const wasmCells = wasm.get_cells();
  const daemonCells = daemon.get_cells();

  // deno-lint-ignore no-explicit-any
  const wasmIds = wasmCells.map((c: any) => {
    const id = c.id;
    c.free();
    return id;
  });
  // deno-lint-ignore no-explicit-any
  const daemonIds = daemonCells.map((c: any) => {
    const id = c.id;
    c.free();
    return id;
  });

  // Same cells in same order
  assertEquals(wasmIds.sort(), daemonIds.sort());
  assert(wasmIds.includes("wasm-cell"));
  assert(wasmIds.includes("daemon-cell"));

  daemon.free();
  wasm.free();
});

// ── create_empty() sync-only bootstrap tests (PR #622) ──────────────

Deno.test("create_empty: creates doc with zero cells", () => {
  const handle = NotebookHandle.create_empty();
  assertEquals(handle.cell_count(), 0);
  assertEquals(handle.get_cells().length, 0);
  assertEquals(handle.get_cells_json(), "[]");
  // Empty doc has no metadata
  assertEquals(handle.get_metadata("runtime"), undefined);
  handle.free();
});

Deno.test("create_empty: sync-only bootstrap receives all content from daemon", () => {
  // Daemon has existing content (simulates loaded notebook)
  const daemon = new NotebookHandle("sync-bootstrap-test");
  daemon.add_cell(0, "cell-1", "code");
  daemon.update_source("cell-1", "import numpy as np");
  daemon.add_cell(1, "cell-2", "markdown");
  daemon.update_source("cell-2", "# Analysis");
  daemon.set_metadata("custom_key", "custom_value");

  // WASM starts completely empty (zero operations) — the #622 path
  const wasm = NotebookHandle.create_empty();
  assertEquals(wasm.cell_count(), 0);

  // Sync should transfer all content
  syncHandles(daemon, wasm);

  // WASM should have all content from daemon
  assertEquals(wasm.cell_count(), 2);
  const cells = wasm.get_cells();
  assertEquals(cells[0].id, "cell-1");
  assertEquals(cells[0].source, "import numpy as np");
  assertEquals(cells[1].id, "cell-2");
  assertEquals(cells[1].source, "# Analysis");
  assertEquals(wasm.get_metadata("custom_key"), "custom_value");

  for (const c of cells) c.free();
  daemon.free();
  wasm.free();
});

Deno.test("create_empty: can mutate after sync bootstrap", () => {
  const daemon = new NotebookHandle("mutate-after-bootstrap");
  daemon.add_cell(0, "existing", "code");
  daemon.update_source("existing", "x = 1");

  const wasm = NotebookHandle.create_empty();
  syncHandles(daemon, wasm);
  assertEquals(wasm.cell_count(), 1);

  // WASM adds a new cell after bootstrap
  wasm.add_cell(1, "new-cell", "markdown");
  wasm.update_source("new-cell", "# Added by WASM");

  // Sync back to daemon
  syncHandles(wasm, daemon);

  // Both should have both cells
  assertEquals(daemon.cell_count(), 2);
  assertEquals(wasm.cell_count(), 2);
  assertEquals(daemon.get_cell("new-cell")?.source, "# Added by WASM");

  daemon.free();
  wasm.free();
});

Deno.test("create_empty: incremental sync after bootstrap works", () => {
  const daemon = new NotebookHandle("incremental-bootstrap");
  daemon.add_cell(0, "cell-1", "code");

  const wasm = NotebookHandle.create_empty();
  syncHandles(daemon, wasm);
  assertEquals(wasm.cell_count(), 1);

  // Daemon adds more content after initial sync
  daemon.add_cell(1, "cell-2", "code");
  daemon.update_source("cell-2", "y = 2");

  // Generate sync message and verify change detection
  const msg = daemon.generate_sync_message();
  assertExists(msg, "Daemon should have sync message after adding cell");

  const changed = wasm.receive_sync_message(msg);
  assertEquals(changed, true, "WASM should detect document changed");

  // Complete sync
  syncHandles(daemon, wasm);

  assertEquals(wasm.cell_count(), 2);
  assertEquals(wasm.get_cell("cell-2")?.source, "y = 2");

  daemon.free();
  wasm.free();
});

// ── Cell metadata tests ─────────────────────────────────────────────

Deno.test("Cell metadata: set_cell_source_hidden", () => {
  const handle = new NotebookHandle("metadata-test");
  handle.add_cell(0, "cell-1", "code");
  handle.update_source("cell-1", "print('hello')");

  // Initially not hidden
  const cells1 = JSON.parse(handle.get_cells_json());
  assertEquals(cells1[0].metadata?.jupyter?.source_hidden, undefined);

  // Hide source
  const updated = handle.set_cell_source_hidden("cell-1", true);
  assertEquals(updated, true);

  const cells2 = JSON.parse(handle.get_cells_json());
  assertEquals(cells2[0].metadata?.jupyter?.source_hidden, true);

  // Unhide source
  handle.set_cell_source_hidden("cell-1", false);
  const cells3 = JSON.parse(handle.get_cells_json());
  assertEquals(cells3[0].metadata?.jupyter?.source_hidden, false);

  handle.free();
});

Deno.test("Cell metadata: set_cell_outputs_hidden", () => {
  const handle = new NotebookHandle("metadata-test");
  handle.add_cell(0, "cell-1", "code");

  // Hide outputs
  const updated = handle.set_cell_outputs_hidden("cell-1", true);
  assertEquals(updated, true);

  const cells = JSON.parse(handle.get_cells_json());
  assertEquals(cells[0].metadata?.jupyter?.outputs_hidden, true);

  // Unhide outputs
  handle.set_cell_outputs_hidden("cell-1", false);
  const cells2 = JSON.parse(handle.get_cells_json());
  assertEquals(cells2[0].metadata?.jupyter?.outputs_hidden, false);

  handle.free();
});

Deno.test("Cell metadata: set_cell_tags", () => {
  const handle = new NotebookHandle("metadata-test");
  handle.add_cell(0, "cell-1", "code");

  // Set tags
  const updated = handle.set_cell_tags(
    "cell-1",
    '["hide-input", "parameters"]',
  );
  assertEquals(updated, true);

  const cells = JSON.parse(handle.get_cells_json());
  assertEquals(cells[0].metadata?.tags, ["hide-input", "parameters"]);

  // Clear tags
  handle.set_cell_tags("cell-1", "[]");
  const cells2 = JSON.parse(handle.get_cells_json());
  assertEquals(cells2[0].metadata?.tags, []);

  handle.free();
});

Deno.test("Cell metadata: update_cell_metadata_at", () => {
  const handle = new NotebookHandle("metadata-test");
  handle.add_cell(0, "cell-1", "code");

  // Set nested value
  const updated = handle.update_cell_metadata_at(
    "cell-1",
    '["custom", "nested", "key"]',
    '"test-value"',
  );
  assertEquals(updated, true);

  const cells = JSON.parse(handle.get_cells_json());
  assertEquals(cells[0].metadata?.custom?.nested?.key, "test-value");

  handle.free();
});

Deno.test("Cell metadata: set_cell_metadata (full replacement)", () => {
  const handle = new NotebookHandle("metadata-test");
  handle.add_cell(0, "cell-1", "code");

  // Set full metadata
  const updated = handle.set_cell_metadata(
    "cell-1",
    '{"jupyter": {"source_hidden": true}, "custom": "value"}',
  );
  assertEquals(updated, true);

  const cells = JSON.parse(handle.get_cells_json());
  assertEquals(cells[0].metadata?.jupyter?.source_hidden, true);
  assertEquals(cells[0].metadata?.custom, "value");

  handle.free();
});

Deno.test("Cell metadata: set_cell_metadata rejects non-object", () => {
  const handle = new NotebookHandle("metadata-test");
  handle.add_cell(0, "cell-1", "code");

  // Try to set metadata to an array (not an object)
  let threw = false;
  try {
    handle.set_cell_metadata("cell-1", '["not", "an", "object"]');
  } catch (e) {
    threw = true;
    assert(
      String(e).includes("must be a JSON object"),
      `Expected error about JSON object, got: ${e}`,
    );
  }
  assertEquals(threw, true, "should throw for non-object metadata");

  // Try to set metadata to a string
  threw = false;
  try {
    handle.set_cell_metadata("cell-1", '"just a string"');
  } catch (e) {
    threw = true;
  }
  assertEquals(threw, true, "should throw for string metadata");

  handle.free();
});

Deno.test("Cell metadata: returns false for non-existent cell", () => {
  const handle = new NotebookHandle("metadata-test");
  handle.add_cell(0, "cell-1", "code");

  // Try to update non-existent cell
  const updated = handle.set_cell_source_hidden("non-existent", true);
  assertEquals(updated, false);

  handle.free();
});

Deno.test("Cell metadata: syncs between handles", () => {
  const daemon = new NotebookHandle("metadata-sync-test");
  daemon.add_cell(0, "cell-1", "code");
  daemon.update_source("cell-1", "x = 1");

  const wasm = NotebookHandle.load(daemon.save());
  syncHandles(daemon, wasm);

  // WASM sets metadata
  wasm.set_cell_source_hidden("cell-1", true);
  wasm.set_cell_tags("cell-1", '["hide-input"]');

  // Sync to daemon
  syncHandles(wasm, daemon);

  // Daemon should have the metadata
  const daemonCells = JSON.parse(daemon.get_cells_json());
  assertEquals(daemonCells[0].metadata?.jupyter?.source_hidden, true);
  assertEquals(daemonCells[0].metadata?.tags, ["hide-input"]);

  daemon.free();
  wasm.free();
});
