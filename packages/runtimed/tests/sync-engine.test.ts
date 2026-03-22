/**
 * Deno tests for SyncEngine + DirectTransport.
 *
 * Proves the @nteract/runtimed library works end-to-end without a browser,
 * daemon, or Tauri. Two WASM NotebookHandles sync through the SyncEngine
 * and DirectTransport.
 *
 * Run with:
 *   deno test --no-check --allow-read packages/runtimed/tests/sync-engine.test.ts
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

// ── Import library under test ────────────────────────────────────────

import { SyncEngine } from "../src/sync-engine.ts";
import { DirectTransport } from "../src/direct-transport.ts";
import { FrameType } from "../src/transport.ts";
import type { SyncEngineEvent } from "../src/sync-engine.ts";

// ── Helpers ──────────────────────────────────────────────────────────

/** Collect events from a SyncEngine into an array. */
function collectEvents(
  engine: SyncEngine,
  ...types: string[]
): SyncEngineEvent[] {
  const events: SyncEngineEvent[] = [];
  for (const type of types) {
    // deno-lint-ignore no-explicit-any
    engine.on(type as any, (e: SyncEngineEvent) => events.push(e));
  }
  return events;
}

/** Wait for a specific event type, with timeout. */
function waitForEvent(
  engine: SyncEngine,
  type: string,
  timeoutMs = 2000,
): Promise<SyncEngineEvent> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(
      () => reject(new Error(`Timed out waiting for '${type}' event`)),
      timeoutMs,
    );
    // deno-lint-ignore no-explicit-any
    engine.on(type as any, (e: SyncEngineEvent) => {
      clearTimeout(timer);
      resolve(e);
    });
  });
}

/** Flush microtask queue (let async callbacks fire). */
function tick(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

/** Wait a specific number of milliseconds. */
function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

// ── Tests ────────────────────────────────────────────────────────────

Deno.test("SyncEngine: start delivers initial sync from server", async () => {
  // Server has a notebook with a cell.
  const server = new NotebookHandle("init-test");
  server.add_cell(0, "cell-1", "code");
  server.update_source("cell-1", "print('hello')");

  // Client starts empty.
  const client = NotebookHandle.create_empty_with_actor("test:client");

  const transport = new DirectTransport(server);
  const engine = new SyncEngine(client, transport, {
    flushDebounceMs: 5,
    initialSyncTimeoutMs: 500,
  });

  const initialSyncPromise = waitForEvent(engine, "initial_sync_complete");

  engine.start();

  // The engine's start() calls flushNow(), which generates a sync message
  // from the empty client → delivered to server via transport → server replies.
  // We need to push the server's response back.
  await tick();
  transport.pushServerChanges();
  await tick();
  transport.pushServerChanges(); // may need a second round
  await tick();

  await initialSyncPromise;

  assert(engine.synced, "engine should be synced after initial sync");
  assertEquals(client.cell_count(), 1);

  const cell = client.get_cell("cell-1");
  assertExists(cell);
  assertEquals(cell.source, "print('hello')");
  cell.free();

  engine.stop();
  server.free();
  client.free();
});

Deno.test("SyncEngine: emits cells_changed on server mutations", async () => {
  const server = new NotebookHandle("change-test");
  server.add_cell(0, "cell-1", "code");
  server.update_source("cell-1", "original");

  const client = NotebookHandle.create_empty_with_actor("test:client");
  const transport = new DirectTransport(server);
  const engine = new SyncEngine(client, transport, {
    flushDebounceMs: 5,
    initialSyncTimeoutMs: 500,
  });

  const events = collectEvents(engine, "cells_changed");

  engine.start();
  await tick();
  transport.pushServerChanges();
  await tick();
  transport.pushServerChanges();
  await tick();

  // Wait for initial sync to complete
  await sleep(50);

  // Clear events from initial sync
  events.length = 0;

  // Server makes a change
  server.update_source("cell-1", "updated");
  transport.pushServerChanges();
  await tick();

  assert(events.length > 0, "should have received cells_changed event");
  assertEquals(events[0].type, "cells_changed");

  const cell = client.get_cell("cell-1");
  assertExists(cell);
  assertEquals(cell.source, "updated");
  cell.free();

  engine.stop();
  server.free();
  client.free();
});

Deno.test(
  "SyncEngine: scheduleFlush sends local changes to server",
  async () => {
    const server = new NotebookHandle("flush-test");
    server.add_cell(0, "cell-1", "code");

    const client = NotebookHandle.load(server.save());

    // Manually sync to establish baseline
    for (let i = 0; i < 10; i++) {
      const a = server.flush_local_changes();
      const b = client.flush_local_changes();
      if (!a && !b) break;
      if (a) client.receive_sync_message(a);
      if (b) server.receive_sync_message(b);
    }

    // Reset sync states since SyncEngine will manage them
    client.reset_sync_state();
    server.reset_sync_state();

    const transport = new DirectTransport(server);
    const engine = new SyncEngine(client, transport, {
      flushDebounceMs: 10,
      initialSyncTimeoutMs: 500,
    });

    engine.start();
    await tick();
    // Push initial sync handshake
    transport.pushServerChanges();
    await tick();
    transport.pushServerChanges();
    await sleep(50);

    // Client makes a local edit
    client.update_source("cell-1", "client edit");
    engine.scheduleFlush();

    // Wait for debounce to fire (10ms + buffer)
    await sleep(50);

    // The transport should have received an AUTOMERGE_SYNC frame
    const syncFrames = transport.sentFrames.filter(
      (f) => f.frameType === FrameType.AUTOMERGE_SYNC,
    );
    assert(syncFrames.length > 0, "should have sent sync frame(s)");

    // Server should now have the client's edit
    const serverCell = server.get_cell("cell-1");
    assertExists(serverCell);
    assertEquals(serverCell.source, "client edit");
    serverCell.free();

    engine.stop();
    server.free();
    client.free();
  },
);

Deno.test("SyncEngine: flush() sends immediately (no debounce)", async () => {
  const server = new NotebookHandle("immediate-flush-test");
  server.add_cell(0, "cell-1", "code");

  const client = NotebookHandle.load(server.save());

  // Bootstrap sync
  for (let i = 0; i < 10; i++) {
    const a = server.flush_local_changes();
    const b = client.flush_local_changes();
    if (!a && !b) break;
    if (a) client.receive_sync_message(a);
    if (b) server.receive_sync_message(b);
  }
  client.reset_sync_state();
  server.reset_sync_state();

  const transport = new DirectTransport(server);
  const engine = new SyncEngine(client, transport, {
    flushDebounceMs: 5000, // Long debounce — flush() should bypass it
    initialSyncTimeoutMs: 500,
  });

  engine.start();
  await tick();
  transport.pushServerChanges();
  await tick();
  transport.pushServerChanges();
  await sleep(50);

  // Client makes a local edit
  client.update_source("cell-1", "immediate edit");

  // Flush immediately (awaits the send)
  await engine.flush();

  // Server should have the edit — no waiting for debounce
  const serverCell = server.get_cell("cell-1");
  assertExists(serverCell);
  assertEquals(serverCell.source, "immediate edit");
  serverCell.free();

  engine.stop();
  server.free();
  client.free();
});

Deno.test(
  "SyncEngine: rolls back sync state on transport failure",
  async () => {
    const server = new NotebookHandle("rollback-test");
    server.add_cell(0, "cell-1", "code");

    const client = NotebookHandle.load(server.save());

    // Bootstrap sync
    for (let i = 0; i < 10; i++) {
      const a = server.flush_local_changes();
      const b = client.flush_local_changes();
      if (!a && !b) break;
      if (a) client.receive_sync_message(a);
      if (b) server.receive_sync_message(b);
    }
    client.reset_sync_state();
    server.reset_sync_state();

    const transport = new DirectTransport(server);
    const engine = new SyncEngine(client, transport, {
      flushDebounceMs: 5,
      initialSyncTimeoutMs: 500,
    });

    const errors = collectEvents(engine, "error");

    engine.start();
    await tick();
    transport.pushServerChanges();
    await tick();
    transport.pushServerChanges();
    await sleep(50);

    // Client makes a local edit
    client.update_source("cell-1", "will fail to send");

    // Enable failure simulation BEFORE flush
    transport.simulateFailure = true;

    // Flush — will fail, should emit error and rollback
    try {
      await engine.flush();
    } catch {
      // Expected — flush propagates the error
    }

    await tick();

    // Disable failure
    transport.simulateFailure = false;

    // The rollback should allow the next flush to include the change data.
    // Make another edit to ensure heads change.
    client.update_source("cell-1", "will succeed this time");
    await engine.flush();

    // Server should now have the edit
    const serverCell = server.get_cell("cell-1");
    assertExists(serverCell);
    assertEquals(serverCell.source, "will succeed this time");
    serverCell.free();

    engine.stop();
    server.free();
    client.free();
  },
);

Deno.test(
  "SyncEngine: inline reply rollback on send failure (#1068 review)",
  async () => {
    const server = new NotebookHandle("reply-rollback-test");
    server.add_cell(0, "cell-1", "code");

    const client = NotebookHandle.load(server.save());

    // Bootstrap sync
    for (let i = 0; i < 10; i++) {
      const a = server.flush_local_changes();
      const b = client.flush_local_changes();
      if (!a && !b) break;
      if (a) client.receive_sync_message(a);
      if (b) server.receive_sync_message(b);
    }
    client.reset_sync_state();
    server.reset_sync_state();

    const transport = new DirectTransport(server);
    const engine = new SyncEngine(client, transport, {
      flushDebounceMs: 5,
      initialSyncTimeoutMs: 500,
    });

    engine.start();
    await tick();
    transport.pushServerChanges();
    await tick();
    transport.pushServerChanges();
    await sleep(50);

    // Client makes a local edit (not yet flushed)
    client.update_source("cell-1", "local edit");

    // Simulate failure on the next sendFrame (the inline reply)
    transport.simulateFailure = true;

    // Server pushes a change — the inline reply will fail
    server.update_source("cell-1", "server edit");
    transport.pushServerChanges();
    await tick();

    // Re-enable sends
    transport.simulateFailure = false;

    // Server pushes another change — this time the reply should include
    // the client's local edit (because cancel_last_flush was called on
    // the failed reply)
    server.update_source("cell-1", "server edit v2");
    transport.pushServerChanges();
    await tick();
    transport.pushServerChanges();
    await tick();

    // Let everything settle
    await sleep(50);

    // Also flush the client's local changes explicitly
    await engine.flush();
    transport.pushServerChanges();
    await tick();

    // Both should converge — the client's "local edit" should be merged
    const serverCell = server.get_cell("cell-1");
    const clientCell = client.get_cell("cell-1");
    assertExists(serverCell);
    assertExists(clientCell);
    assertEquals(
      clientCell.source,
      serverCell.source,
      "client and server should converge after reply rollback recovery",
    );
    serverCell.free();
    clientCell.free();

    engine.stop();
    server.free();
    client.free();
  },
);

Deno.test("SyncEngine: emits broadcast events", async () => {
  const server = new NotebookHandle("broadcast-test");
  const client = NotebookHandle.create_empty_with_actor("test:client");

  const transport = new DirectTransport(server);
  const engine = new SyncEngine(client, transport, {
    flushDebounceMs: 5,
    initialSyncTimeoutMs: 500,
  });

  const broadcasts = collectEvents(engine, "broadcast");

  engine.start();
  await tick();

  // Push a broadcast event
  transport.pushBroadcast({
    event: "execution_started",
    cell_id: "cell-1",
    execution_count: 1,
  });
  await tick();

  assert(broadcasts.length > 0, "should have received broadcast event");
  assertEquals(broadcasts[0].type, "broadcast");
  // deno-lint-ignore no-explicit-any
  const payload = (broadcasts[0] as any).payload;
  assertEquals(payload.event, "execution_started");
  assertEquals(payload.cell_id, "cell-1");

  engine.stop();
  server.free();
  client.free();
});

Deno.test("SyncEngine: sendRequest routes through transport", async () => {
  const server = new NotebookHandle("request-test");
  const client = NotebookHandle.create_empty_with_actor("test:client");

  const transport = new DirectTransport(server);

  // Mock request handler
  transport.onRequest = (req) => {
    // deno-lint-ignore no-explicit-any
    const r = req as any;
    if (r.action === "execute_cell") {
      return {
        result: "CellQueued",
        cell_id: r.cell_id,
        execution_id: "exec-123",
      };
    }
    return { result: "error", error: "unknown" };
  };

  const engine = new SyncEngine(client, transport);
  engine.start();
  await tick();

  // Send a request through the transport
  const response = await transport.sendRequest({
    action: "execute_cell",
    cell_id: "cell-1",
  });
  // deno-lint-ignore no-explicit-any
  const resp = response as any;
  assertEquals(resp.result, "CellQueued");
  assertEquals(resp.execution_id, "exec-123");

  engine.stop();
  server.free();
  client.free();
});

Deno.test("SyncEngine: rapid server changes all arrive", async () => {
  const server = new NotebookHandle("rapid-test");
  server.add_cell(0, "cell-1", "code");

  const client = NotebookHandle.create_empty_with_actor("test:client");
  const transport = new DirectTransport(server);
  const engine = new SyncEngine(client, transport, {
    flushDebounceMs: 5,
    initialSyncTimeoutMs: 500,
  });

  engine.start();
  await tick();
  transport.pushServerChanges();
  await tick();
  transport.pushServerChanges();
  await sleep(50);

  // Rapid server changes
  for (let i = 1; i <= 10; i++) {
    server.update_source("cell-1", `version ${i}`);
    transport.pushServerChanges();
    await tick();
  }

  // Additional sync rounds to converge
  for (let i = 0; i < 5; i++) {
    transport.pushServerChanges();
    await tick();
  }

  const cell = client.get_cell("cell-1");
  assertExists(cell);
  assertEquals(cell.source, "version 10");
  cell.free();

  engine.stop();
  server.free();
  client.free();
});

Deno.test("SyncEngine: stop flushes pending changes", async () => {
  const server = new NotebookHandle("stop-flush-test");
  server.add_cell(0, "cell-1", "code");

  const client = NotebookHandle.load(server.save());

  // Bootstrap
  for (let i = 0; i < 10; i++) {
    const a = server.flush_local_changes();
    const b = client.flush_local_changes();
    if (!a && !b) break;
    if (a) client.receive_sync_message(a);
    if (b) server.receive_sync_message(b);
  }
  client.reset_sync_state();
  server.reset_sync_state();

  const transport = new DirectTransport(server);
  const engine = new SyncEngine(client, transport, {
    flushDebounceMs: 60000, // Very long debounce
    initialSyncTimeoutMs: 500,
  });

  engine.start();
  await tick();
  transport.pushServerChanges();
  await tick();
  transport.pushServerChanges();
  await sleep(50);

  // Client makes a local edit
  client.update_source("cell-1", "final edit");

  // DON'T call scheduleFlush — just stop.
  // stop() should flush pending changes.
  engine.stop();

  // Server should have the edit from the teardown flush.
  const serverCell = server.get_cell("cell-1");
  assertExists(serverCell);
  assertEquals(serverCell.source, "final edit");
  serverCell.free();

  server.free();
  client.free();
});

Deno.test("SyncEngine: concurrent edits from both sides converge", async () => {
  const server = new NotebookHandle("concurrent-test");
  server.add_cell(0, "cell-1", "code");
  server.add_cell(1, "cell-2", "code");

  const client = NotebookHandle.load(server.save());

  // Bootstrap
  for (let i = 0; i < 10; i++) {
    const a = server.flush_local_changes();
    const b = client.flush_local_changes();
    if (!a && !b) break;
    if (a) client.receive_sync_message(a);
    if (b) server.receive_sync_message(b);
  }
  client.reset_sync_state();
  server.reset_sync_state();

  const transport = new DirectTransport(server);
  const engine = new SyncEngine(client, transport, {
    flushDebounceMs: 5,
    initialSyncTimeoutMs: 500,
  });

  engine.start();
  await tick();
  transport.pushServerChanges();
  await tick();
  transport.pushServerChanges();
  await sleep(50);

  // Both sides make concurrent edits to different cells
  client.update_source("cell-1", "from client");
  server.update_source("cell-2", "from server");

  // Flush client changes
  await engine.flush();

  // Push server changes
  transport.pushServerChanges();
  await tick();

  // More sync rounds
  for (let i = 0; i < 5; i++) {
    transport.pushServerChanges();
    await tick();
    await engine.flush();
    await tick();
  }

  // Both should have both edits
  const serverCell1 = server.get_cell("cell-1");
  const serverCell2 = server.get_cell("cell-2");
  const clientCell1 = client.get_cell("cell-1");
  const clientCell2 = client.get_cell("cell-2");

  assertExists(serverCell1);
  assertExists(serverCell2);
  assertExists(clientCell1);
  assertExists(clientCell2);

  assertEquals(serverCell1.source, "from client");
  assertEquals(serverCell2.source, "from server");
  assertEquals(clientCell1.source, "from client");
  assertEquals(clientCell2.source, "from server");

  serverCell1.free();
  serverCell2.free();
  clientCell1.free();
  clientCell2.free();

  engine.stop();
  server.free();
  client.free();
});

Deno.test("SyncEngine: multiple scheduleFlush calls coalesce", async () => {
  const server = new NotebookHandle("coalesce-test");
  server.add_cell(0, "cell-1", "code");

  const client = NotebookHandle.load(server.save());

  // Bootstrap
  for (let i = 0; i < 10; i++) {
    const a = server.flush_local_changes();
    const b = client.flush_local_changes();
    if (!a && !b) break;
    if (a) client.receive_sync_message(a);
    if (b) server.receive_sync_message(b);
  }
  client.reset_sync_state();
  server.reset_sync_state();

  const transport = new DirectTransport(server);
  const engine = new SyncEngine(client, transport, {
    flushDebounceMs: 30,
    initialSyncTimeoutMs: 500,
  });

  engine.start();
  await tick();
  transport.pushServerChanges();
  await tick();
  transport.pushServerChanges();
  await sleep(50);

  transport.clearSentFrames();

  // Rapid local edits — simulating fast typing
  for (let i = 0; i < 20; i++) {
    client.update_source("cell-1", "x".repeat(i + 1));
    engine.scheduleFlush();
  }

  // Wait for the debounce to fire (30ms + buffer)
  await sleep(80);

  // Should have sent far fewer sync messages than 20 edits
  const syncFrames = transport.sentFrames.filter(
    (f) => f.frameType === FrameType.AUTOMERGE_SYNC,
  );
  assert(
    syncFrames.length < 10,
    `Expected coalescing to reduce sends, got ${syncFrames.length}`,
  );
  assert(syncFrames.length >= 1, "Should have sent at least one sync message");

  // Server should have the final edit
  const serverCell = server.get_cell("cell-1");
  assertExists(serverCell);
  assertEquals(serverCell.source, "x".repeat(20));
  serverCell.free();

  engine.stop();
  server.free();
  client.free();
});

Deno.test("SyncEngine: retry timer fires on stalled initial sync", async () => {
  // Server has content but we won't push it — simulating a lost message.
  const server = new NotebookHandle("retry-test");
  server.add_cell(0, "cell-1", "code");
  server.update_source("cell-1", "delayed content");

  const client = NotebookHandle.create_empty_with_actor("test:client");
  const transport = new DirectTransport(server);

  const engine = new SyncEngine(client, transport, {
    flushDebounceMs: 5,
    initialSyncTimeoutMs: 200, // Short timeout for test
  });

  const retryEvents = collectEvents(engine, "sync_retry");

  engine.start();
  await tick();

  // Don't push server changes — simulate lost initial sync.
  // Wait for the retry timer to fire.
  await sleep(350);

  assert(retryEvents.length >= 1, "should have retried at least once");

  // Now push server changes — should complete initial sync.
  transport.pushServerChanges();
  await tick();
  transport.pushServerChanges();
  await tick();
  await sleep(50);

  assert(engine.synced, "should be synced after retry succeeded");
  assertEquals(client.cell_count(), 1);

  engine.stop();
  server.free();
  client.free();
});
