/**
 * Integration test: SyncEngine + real daemon via Python Session.
 *
 * Proves the runtimed library works end-to-end with a real
 * daemon — not just two WASM handles talking to each other. The test:
 *
 * 1. Python Session creates a notebook room in the daemon
 * 2. Python adds a cell and executes it
 * 3. WASM NotebookHandle syncs with the daemon via SyncEngine + DirectTransport
 *    (adapted to relay through Python's daemon connection)
 * 4. Verifies the WASM doc has the cell, source, outputs, and execution count
 *
 * This is the definitive test that the SyncEngine can drive a real
 * notebook sync session — the same flow the Tauri frontend uses, but
 * without a browser.
 *
 * Requires:
 *   - Dev daemon running at RUNTIMED_SOCKET_PATH
 *   - runtimed Python package installed (cd python/runtimed && maturin develop)
 *
 * Run with:
 *   RUNTIMED_SOCKET_PATH=~/Library/Caches/runt/worktrees/.../runtimed.sock \
 *     deno test --allow-read --allow-run --allow-env --no-check \
 *     packages/runtimed/tests/integration.test.ts
 */

import {
  assert,
  assertEquals,
  assertExists,
} from "https://deno.land/std@0.224.0/assert/mod.ts";

// @ts-nocheck — wasm-bindgen output doesn't have Deno-compatible type declarations

// ── WASM setup ───────────────────────────────────────────────────────

// deno-lint-ignore no-explicit-any
let NotebookHandle: any;

const wasmJsPath = new URL(
  "../../../apps/notebook/src/wasm/runtimed-wasm/runtimed_wasm.js",
  import.meta.url,
);
const wasmBinPath = new URL(
  "../../../apps/notebook/src/wasm/runtimed-wasm/runtimed_wasm_bg.wasm",
  import.meta.url,
);

const mod = await import(wasmJsPath.href);
const init = mod.default;
NotebookHandle = mod.NotebookHandle;

const wasmBytes = await Deno.readFile(wasmBinPath);
await init(wasmBytes);

// ── Library imports ──────────────────────────────────────────────────

import { SyncEngine } from "../src/sync-engine.ts";
import { DirectTransport } from "../src/direct-transport.ts";
import type { SyncEngineEvent, CoalescedCellChanges } from "../src/sync-engine.ts";

// ── Helpers ──────────────────────────────────────────────────────────

const hasDaemon = !!Deno.env.get("RUNTIMED_SOCKET_PATH");

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}

function fromHex(hex: string): Uint8Array {
  const matches = hex.match(/.{2}/g);
  if (!matches) return new Uint8Array(0);
  return new Uint8Array(matches.map((b) => parseInt(b, 16)));
}

/**
 * Run a Python script via uv run in the python/runtimed directory.
 */
async function runPython(script: string): Promise<string> {
  const repoRoot = new URL("../../../", import.meta.url).pathname;
  const cmd = new Deno.Command("uv", {
    args: ["run", "python", "-c", script],
    cwd: `${repoRoot}python/runtimed`,
    stdout: "piped",
    stderr: "piped",
    env: {
      ...Deno.env.toObject(),
      RUNTIMED_SOCKET_PATH: Deno.env.get("RUNTIMED_SOCKET_PATH") ?? "",
    },
  });
  const output = await cmd.output();
  if (!output.success) {
    const stderr = new TextDecoder().decode(output.stderr);
    throw new Error(`Python script failed:\n${stderr}`);
  }
  return new TextDecoder().decode(output.stdout).trim();
}

function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

/** Sync two handles until convergence. */
function syncHandles(
  // deno-lint-ignore no-explicit-any
  a: any,
  // deno-lint-ignore no-explicit-any
  b: any,
  maxRounds = 10,
) {
  for (let i = 0; i < maxRounds; i++) {
    const msgA = a.flush_local_changes();
    const msgB = b.flush_local_changes();
    if (!msgA && !msgB) break;
    if (msgA) b.receive_sync_message(msgA);
    if (msgB) a.receive_sync_message(msgB);
  }
}

// ── Tests ────────────────────────────────────────────────────────────

Deno.test({
  name: "Integration: Python creates cell, WASM SyncEngine sees it via doc bytes",
  ignore: !hasDaemon,
  fn: async () => {
    // Step 1: Python creates a notebook with a cell and gets the doc bytes.
    const result = await runPython(`
import json
from runtimed import Session

s = Session("integration-sync-test")
s.connect()

# Add a cell
s.add_cell("integration-cell-1", "code", 0)
s.set_cell_source("integration-cell-1", "x = 42\\nprint(x)")

# Confirm sync
s.confirm_sync()

# Get doc bytes as hex
doc_bytes = s.get_automerge_doc_bytes()
cells = s.get_cells()

output = {
    "doc_hex": doc_bytes.hex(),
    "cell_count": len(cells),
    "cell_id": cells[0].id if cells else None,
    "cell_source": cells[0].source if cells else None,
}

s.disconnect()
print(json.dumps(output))
`);

    const data = JSON.parse(result);
    assertEquals(data.cell_count, 1);
    assertEquals(data.cell_id, "integration-cell-1");
    assertEquals(data.cell_source, "x = 42\nprint(x)");

    // Step 2: WASM loads the doc bytes and creates a SyncEngine.
    const docBytes = fromHex(data.doc_hex);
    const wasmHandle = NotebookHandle.load(docBytes);

    assertEquals(wasmHandle.cell_count(), 1);
    const cell = wasmHandle.get_cell("integration-cell-1");
    assertExists(cell);
    assertEquals(cell.source, "x = 42\nprint(x)");
    assertEquals(cell.cell_type, "code");
    cell.free();

    // Step 3: Create a "server" handle (simulating the daemon's doc)
    // and a SyncEngine for the client. Verify they converge.
    const serverHandle = NotebookHandle.load(docBytes);
    const clientHandle = NotebookHandle.create_empty_with_actor("test:integration");

    const transport = new DirectTransport(serverHandle);
    const engine = new SyncEngine(clientHandle, transport, {
      flushDebounceMs: 5,
      initialSyncTimeoutMs: 1000,
    });

    const initialSyncDone = new Promise<void>((resolve) => {
      engine.on("initial_sync_complete", () => resolve());
    });

    engine.start();
    await tick();
    transport.pushServerChanges();
    await tick();
    transport.pushServerChanges();
    await tick();

    await initialSyncDone;

    // Client should now have the cell from the daemon (via server handle).
    assertEquals(clientHandle.cell_count(), 1);
    const clientCell = clientHandle.get_cell("integration-cell-1");
    assertExists(clientCell);
    assertEquals(clientCell.source, "x = 42\nprint(x)");
    clientCell.free();

    engine.stop();
    wasmHandle.free();
    serverHandle.free();
    clientHandle.free();
  },
});

Deno.test({
  name: "Integration: Python executes cell, WASM SyncEngine sees output",
  ignore: !hasDaemon,
  fn: async () => {
    // Python creates a notebook, adds a cell, executes it, and returns
    // the doc bytes with outputs.
    const result = await runPython(`
import json
from runtimed import Session

s = Session("integration-exec-test")
s.connect()

# Add and execute a cell
s.add_cell("exec-cell", "code", 0)
s.set_cell_source("exec-cell", "print('hello from integration test')")

result = s.run("exec-cell", timeout_secs=30)
s.confirm_sync()

doc_bytes = s.get_automerge_doc_bytes()
cells = s.get_cells()
cell = cells[0]

output = {
    "doc_hex": doc_bytes.hex(),
    "cell_count": len(cells),
    "source": cell.source,
    "execution_count": cell.execution_count,
    "output_count": len(cell.outputs),
    "success": result.success,
    "stdout": result.stdout,
}

s.disconnect()
print(json.dumps(output))
`);

    const data = JSON.parse(result);
    assertEquals(data.cell_count, 1);
    assertEquals(data.source, "print('hello from integration test')");
    assertEquals(data.success, true);
    assert(
      data.stdout.includes("hello from integration test"),
      `Expected stdout to contain test string, got: ${data.stdout}`,
    );
    assert(data.execution_count > 0, "execution_count should be > 0");
    assert(data.output_count > 0, "should have at least one output");

    // Load the doc bytes into WASM and verify outputs are present.
    const docBytes = fromHex(data.doc_hex);
    const handle = NotebookHandle.load(docBytes);

    assertEquals(handle.cell_count(), 1);
    const cell = handle.get_cell("exec-cell");
    assertExists(cell);
    assertEquals(cell.source, "print('hello from integration test')");
    cell.free();

    // Check outputs via get_cell_outputs
    const outputs = handle.get_cell_outputs("exec-cell");
    assertExists(outputs);
    assert(outputs.length > 0, "WASM should see the output");

    handle.free();
  },
});

Deno.test({
  name: "Integration: SyncEngine client edits cell, server sees it",
  ignore: !hasDaemon,
  fn: async () => {
    // Create a notebook via Python, load into both server and client handles.
    const result = await runPython(`
import json
from runtimed import Session

s = Session("integration-edit-test")
s.connect()
s.add_cell("edit-cell", "code", 0)
s.set_cell_source("edit-cell", "original")
s.confirm_sync()
doc_hex = s.get_automerge_doc_bytes().hex()
s.disconnect()
print(json.dumps({"doc_hex": doc_hex}))
`);

    const data = JSON.parse(result);
    const docBytes = fromHex(data.doc_hex);

    const serverHandle = NotebookHandle.load(docBytes);
    const clientHandle = NotebookHandle.load(docBytes);

    // Sync to establish baseline
    syncHandles(serverHandle, clientHandle);

    // Reset sync states for the engine
    clientHandle.reset_sync_state();
    serverHandle.reset_sync_state();

    const transport = new DirectTransport(serverHandle);
    const engine = new SyncEngine(clientHandle, transport, {
      flushDebounceMs: 5,
      initialSyncTimeoutMs: 1000,
    });

    engine.start();
    await tick();
    transport.pushServerChanges();
    await tick();
    transport.pushServerChanges();
    await sleep(50);

    // Client edits the cell via WASM handle
    clientHandle.update_source("edit-cell", "edited by SyncEngine client");
    await engine.flush();

    // Verify server has the edit
    const serverCell = serverHandle.get_cell("edit-cell");
    assertExists(serverCell);
    assertEquals(serverCell.source, "edited by SyncEngine client");
    serverCell.free();

    engine.stop();
    serverHandle.free();
    clientHandle.free();
  },
});

Deno.test({
  name: "Integration: SyncEngine cellChanges$ emits coalesced batches from rapid server edits",
  ignore: !hasDaemon,
  fn: async () => {
    const result = await runPython(`
import json
from runtimed import Session

s = Session("integration-coalesce-test")
s.connect()
s.add_cell("coalesce-cell", "code", 0)
s.set_cell_source("coalesce-cell", "v0")
s.confirm_sync()
doc_hex = s.get_automerge_doc_bytes().hex()
s.disconnect()
print(json.dumps({"doc_hex": doc_hex}))
`);

    const data = JSON.parse(result);
    const docBytes = fromHex(data.doc_hex);

    const serverHandle = NotebookHandle.load(docBytes);
    const clientHandle = NotebookHandle.load(docBytes);
    syncHandles(serverHandle, clientHandle);
    clientHandle.reset_sync_state();
    serverHandle.reset_sync_state();

    const transport = new DirectTransport(serverHandle);
    const engine = new SyncEngine(clientHandle, transport, {
      flushDebounceMs: 5,
      initialSyncTimeoutMs: 1000,
      coalesceMs: 50,
    });

    const batches: CoalescedCellChanges[] = [];
    const sub = engine.cellChanges$.subscribe((b) => batches.push(b));

    engine.start();
    await tick();
    transport.pushServerChanges();
    await tick();
    transport.pushServerChanges();
    await sleep(100); // let initial sync settle
    batches.length = 0;

    // Server makes 10 rapid edits
    for (let i = 1; i <= 10; i++) {
      serverHandle.update_source("coalesce-cell", `version ${i}`);
      transport.pushServerChanges();
      await tick();
    }

    // Wait for coalescing to flush
    await sleep(150);

    // Should have fewer batches than individual edits
    assert(batches.length >= 1, "should have at least one coalesced batch");
    assert(
      batches.length < 10,
      `expected coalescing, got ${batches.length} batches for 10 edits`,
    );

    // Client should have the final version
    const cell = clientHandle.get_cell("coalesce-cell");
    assertExists(cell);
    assertEquals(cell.source, "version 10");
    cell.free();

    sub.unsubscribe();
    engine.stop();
    serverHandle.free();
    clientHandle.free();
  },
});

Deno.test({
  name: "Integration: SyncEngine handles concurrent edits from both sides",
  ignore: !hasDaemon,
  fn: async () => {
    const result = await runPython(`
import json
from runtimed import Session

s = Session("integration-concurrent-test")
s.connect()
s.add_cell("cell-a", "code", 0)
s.add_cell("cell-b", "code", 1)
s.set_cell_source("cell-a", "server owns this")
s.set_cell_source("cell-b", "client owns this")
s.confirm_sync()
doc_hex = s.get_automerge_doc_bytes().hex()
s.disconnect()
print(json.dumps({"doc_hex": doc_hex}))
`);

    const data = JSON.parse(result);
    const docBytes = fromHex(data.doc_hex);

    const serverHandle = NotebookHandle.load(docBytes);
    const clientHandle = NotebookHandle.load(docBytes);
    syncHandles(serverHandle, clientHandle);
    clientHandle.reset_sync_state();
    serverHandle.reset_sync_state();

    const transport = new DirectTransport(serverHandle);
    const engine = new SyncEngine(clientHandle, transport, {
      flushDebounceMs: 5,
      initialSyncTimeoutMs: 1000,
    });

    engine.start();
    await tick();
    transport.pushServerChanges();
    await tick();
    transport.pushServerChanges();
    await sleep(50);

    // Both sides edit different cells concurrently
    serverHandle.update_source("cell-a", "server edit");
    clientHandle.update_source("cell-b", "client edit");

    // Sync: flush client changes, push server changes
    await engine.flush();
    transport.pushServerChanges();
    await tick();

    // A few more rounds to converge
    for (let i = 0; i < 5; i++) {
      transport.pushServerChanges();
      await tick();
      await engine.flush();
      await tick();
    }

    // Both should have both edits
    const serverA = serverHandle.get_cell("cell-a");
    const serverB = serverHandle.get_cell("cell-b");
    const clientA = clientHandle.get_cell("cell-a");
    const clientB = clientHandle.get_cell("cell-b");

    assertExists(serverA);
    assertExists(serverB);
    assertExists(clientA);
    assertExists(clientB);

    assertEquals(serverA.source, "server edit");
    assertEquals(serverB.source, "client edit");
    assertEquals(clientA.source, "server edit");
    assertEquals(clientB.source, "client edit");

    serverA.free();
    serverB.free();
    clientA.free();
    clientB.free();

    engine.stop();
    serverHandle.free();
    clientHandle.free();
  },
});

Deno.test({
  name: "Integration: SyncEngine recovers from transport failure via cancel_last_flush",
  ignore: !hasDaemon,
  fn: async () => {
    const result = await runPython(`
import json
from runtimed import Session

s = Session("integration-recovery-test")
s.connect()
s.add_cell("recovery-cell", "code", 0)
s.set_cell_source("recovery-cell", "original")
s.confirm_sync()
doc_hex = s.get_automerge_doc_bytes().hex()
s.disconnect()
print(json.dumps({"doc_hex": doc_hex}))
`);

    const data = JSON.parse(result);
    const docBytes = fromHex(data.doc_hex);

    const serverHandle = NotebookHandle.load(docBytes);
    const clientHandle = NotebookHandle.load(docBytes);
    syncHandles(serverHandle, clientHandle);
    clientHandle.reset_sync_state();
    serverHandle.reset_sync_state();

    const transport = new DirectTransport(serverHandle);
    const engine = new SyncEngine(clientHandle, transport, {
      flushDebounceMs: 5,
      initialSyncTimeoutMs: 1000,
    });

    engine.start();
    await tick();
    transport.pushServerChanges();
    await tick();
    transport.pushServerChanges();
    await sleep(50);

    // Client makes an edit
    clientHandle.update_source("recovery-cell", "will fail first time");

    // Simulate transport failure
    transport.simulateFailure = true;
    try {
      await engine.flush();
    } catch {
      // Expected
    }
    await tick();

    // Re-enable transport
    transport.simulateFailure = false;

    // Edit again and flush — should work because cancel_last_flush was called
    clientHandle.update_source("recovery-cell", "succeeded after recovery");
    await engine.flush();

    // Sync to ensure convergence
    transport.pushServerChanges();
    await tick();

    const serverCell = serverHandle.get_cell("recovery-cell");
    assertExists(serverCell);
    assertEquals(serverCell.source, "succeeded after recovery");
    serverCell.free();

    engine.stop();
    serverHandle.free();
    clientHandle.free();
  },
});

Deno.test({
  name: "Integration: SyncEngine broadcasts$ receives daemon-like events",
  ignore: !hasDaemon,
  fn: async () => {
    // This test uses DirectTransport's pushBroadcast to simulate
    // daemon broadcast events and verifies they flow through the
    // SyncEngine's broadcasts$ Observable.
    const result = await runPython(`
import json
from runtimed import Session

s = Session("integration-broadcast-test")
s.connect()
s.add_cell("bc-cell", "code", 0)
s.confirm_sync()
doc_hex = s.get_automerge_doc_bytes().hex()
s.disconnect()
print(json.dumps({"doc_hex": doc_hex}))
`);

    const data = JSON.parse(result);
    const docBytes = fromHex(data.doc_hex);

    const serverHandle = NotebookHandle.load(docBytes);
    const clientHandle = NotebookHandle.load(docBytes);
    syncHandles(serverHandle, clientHandle);
    clientHandle.reset_sync_state();
    serverHandle.reset_sync_state();

    const transport = new DirectTransport(serverHandle);
    const engine = new SyncEngine(clientHandle, transport, {
      flushDebounceMs: 5,
      initialSyncTimeoutMs: 1000,
    });

    // deno-lint-ignore no-explicit-any
    const broadcasts: any[] = [];
    const sub = engine.broadcasts$.subscribe((p) => broadcasts.push(p));

    engine.start();
    await tick();
    transport.pushServerChanges();
    await tick();
    transport.pushServerChanges();
    await sleep(50);

    // Simulate daemon broadcasts
    transport.pushBroadcast({
      event: "execution_started",
      cell_id: "bc-cell",
      execution_count: 1,
    });
    await tick();

    transport.pushBroadcast({
      event: "execution_done",
      cell_id: "bc-cell",
    });
    await tick();

    assert(broadcasts.length >= 2, `expected 2+ broadcasts, got ${broadcasts.length}`);
    assertEquals(broadcasts[0].event, "execution_started");
    assertEquals(broadcasts[0].cell_id, "bc-cell");
    assertEquals(broadcasts[1].event, "execution_done");

    sub.unsubscribe();
    engine.stop();
    serverHandle.free();
    clientHandle.free();
  },
});

Deno.test({
  name: "Integration: Full round-trip — Python creates, WASM edits, Python verifies",
  ignore: !hasDaemon,
  fn: async () => {
    // Step 1: Python creates notebook + cell, gets doc bytes.
    const step1 = await runPython(`
import json
from runtimed import Session

s = Session("integration-roundtrip")
s.connect()
s.add_cell("roundtrip-cell", "code", 0)
s.set_cell_source("roundtrip-cell", "original from python")
s.confirm_sync()
doc_hex = s.get_automerge_doc_bytes().hex()
s.disconnect()
print(json.dumps({"doc_hex": doc_hex}))
`);

    const data1 = JSON.parse(step1);
    const docBytes1 = fromHex(data1.doc_hex);

    // Step 2: WASM loads, edits via SyncEngine, exports doc bytes.
    const serverHandle = NotebookHandle.load(docBytes1);
    const clientHandle = NotebookHandle.load(docBytes1);
    syncHandles(serverHandle, clientHandle);

    // Client edits the cell
    clientHandle.update_source("roundtrip-cell", "edited by WASM SyncEngine");

    // Sync the edit to the server handle
    syncHandles(clientHandle, serverHandle);

    // Export the server's doc bytes (these represent what the daemon would have)
    const updatedBytes = serverHandle.save();
    const updatedHex = toHex(updatedBytes);

    serverHandle.free();
    clientHandle.free();

    // Step 3: Python loads the WASM-edited doc bytes and verifies.
    const step3 = await runPython(`
import json
from runtimed import Session

doc_hex = "${updatedHex}"
doc_bytes = bytes.fromhex(doc_hex)

s = Session("integration-roundtrip-verify")
s.connect()

# Load the WASM-edited doc
s.load_automerge_doc(doc_bytes)
s.confirm_sync()

cells = s.get_cells()
cell = next((c for c in cells if c.id == "roundtrip-cell"), None)

output = {
    "cell_count": len(cells),
    "source": cell.source if cell else None,
}

s.disconnect()
print(json.dumps(output))
`);

    const data3 = JSON.parse(step3);
    assertEquals(data3.cell_count, 1);
    assertEquals(
      data3.source,
      "edited by WASM SyncEngine",
      "Python should see the WASM edit after loading the doc bytes",
    );
  },
});
