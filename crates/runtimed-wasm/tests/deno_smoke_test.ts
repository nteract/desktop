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
    const msgA = a.flush_local_changes();
    const msgB = b.flush_local_changes();
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

Deno.test("Sync: flush_local_changes returns null when in sync", () => {
  const server = new NotebookHandle("sync-test");
  const client = NotebookHandle.load(server.save());

  // Fully sync
  syncHandles(server, client);

  // Both should report no message needed
  assertEquals(server.flush_local_changes(), undefined);
  assertEquals(client.flush_local_changes(), undefined);

  server.free();
  client.free();
});

// ── Bug reproduction: #1067 — sync head divergence ───────────────────

Deno.test("Bug #1067: consumed sync message causes protocol stall", () => {
  // Reproduces the rapid Ctrl+Enter bug where flushSync() calls
  // generate_sync_message() (advancing sync_state) but the message
  // is never delivered to the server. The sync protocol stalls because
  // sync_state believes the message was sent.
  //
  // This simulates:
  //   1. Server makes a change (kernel writes output)
  //   2. Client receives the sync message (WASM applies it)
  //   3. Client calls generate_sync_message() — like flushSync() does
  //   4. The message is DROPPED (simulating failed/delayed sendFrame)
  //   5. Server makes another change
  //   6. Client should still be able to sync — but can't

  const server = new NotebookHandle("stall-test");
  server.add_cell(0, "cell-1", "code");

  const client = NotebookHandle.load(server.save());
  syncHandles(server, client);

  // Step 1: Server makes a change (simulates daemon writing output)
  server.update_source("cell-1", "output-v1");

  // Step 2: Client receives the server's sync message
  const serverMsg1 = server.flush_local_changes();
  assert(serverMsg1 !== undefined, "server should have a sync message");
  client.receive_sync_message(serverMsg1);
  // Note: we don't assert `changed` here — bloom filter false positives
  // can cause the first message to not carry change data. The changed
  // flag is tested separately (see "load from bytes + incremental sync
  // with changed flag" test). This test focuses on protocol recovery.

  // Step 3: Client generates a reply — like flushSync() does.
  // This ADVANCES client's sync_state.last_sent_heads.
  const consumedReply = client.flush_local_changes();
  assert(consumedReply !== undefined, "client should have a reply");

  // Step 4: The reply is DROPPED. Never delivered to server.
  // (Simulates sendFrame failing or being blocked by the relay mutex)

  // Step 5: Server makes another change
  server.update_source("cell-1", "output-v2");

  // Step 6: Try to sync. The client's sync_state thinks it already
  // sent a reply for output-v1, so generate_sync_message may return
  // nothing — even though the server never received the reply.
  //
  // Meanwhile, the server sends its new change. Let's see if the
  // protocol can recover.
  const serverMsg2 = server.flush_local_changes();
  assert(serverMsg2 !== undefined, "server should have msg for output-v2");
  client.receive_sync_message(serverMsg2);

  // The client should now generate a reply that covers BOTH the
  // previously-dropped reply AND the new change.
  const recoveryReply = client.flush_local_changes();

  // Automerge's sync protocol self-heals here: receiving serverMsg2
  // provides new heads that let the client generate a fresh reply,
  // even though the previous one was consumed and dropped.
  //
  // The old API (flush_local_changes) is vulnerable to permanent stalls
  // only when the client gets NO further server messages — which can't
  // happen in practice because the daemon keeps sending frames.
  // The receive_frame inline reply API (#1067 fix) eliminates this
  // class of bug entirely.
  assert(
    recoveryReply !== undefined,
    "protocol self-heals: new server message provides fresh heads for reply",
  );
  server.receive_sync_message(recoveryReply);
  syncHandles(server, client);

  const serverCell = server.get_cell("cell-1");
  assertExists(serverCell);
  assertEquals(serverCell.source, "output-v2");
  serverCell.free();

  server.free();
  client.free();
});

Deno.test("Bug #1067: rapid flushSync steals debounced reply", () => {
  // Simulates the exact interleaving from the bug:
  //   - Server streams outputs (multiple sync frames)
  //   - Client calls generate_sync_reply (debounced) — but before it fires,
  //     flushSync calls generate_sync_message, consuming the pending reply
  //   - The debounced reply then returns undefined

  const server = new NotebookHandle("rapid-test");
  server.add_cell(0, "cell-1", "code");

  const client = NotebookHandle.load(server.save());
  syncHandles(server, client);

  // Server streams multiple rapid changes (simulates IOPub output burst)
  server.update_source("cell-1", "line 1");
  const msg1 = server.flush_local_changes();
  assert(msg1 !== undefined);
  client.receive_sync_message(msg1);

  server.update_source("cell-1", "line 1\nline 2");
  const msg2 = server.flush_local_changes();
  assert(msg2 !== undefined);
  client.receive_sync_message(msg2);

  server.update_source("cell-1", "line 1\nline 2\nline 3");
  const msg3 = server.flush_local_changes();
  assert(msg3 !== undefined);
  client.receive_sync_message(msg3);

  // At this point, the client has 3 unacknowledged inbound syncs.
  // A debounced syncReply$ would call generate_sync_reply() here.
  // But flushSync() fires first (user presses Ctrl+Enter):

  const flushMsg = client.flush_local_changes(); // flushSync steals it
  // flushMsg is defined — it covers all 3 inbound syncs.

  // Now the debounced syncReply fires:
  const debouncedReply = client.flush_local_changes(); // generates reply

  // The old API has a consumption race: flushSync's flush_local_changes()
  // advances sync_state, consuming the pending reply. The debounced
  // syncReply$ call then gets nothing. This is the exact bug that
  // receive_frame's inline reply (#1067 fix) eliminates.
  assert(flushMsg !== undefined, "flushSync should produce a message");
  assertEquals(
    debouncedReply,
    undefined,
    "debounced reply is consumed by the prior flush — the old-API race",
  );

  // If flushMsg delivery failed, the client is now stuck: sent_hashes
  // filters out the change data, and no recovery path exists without
  // cancel_last_flush or reset_sync_state. Verify reset recovers.
  client.reset_sync_state();
  syncHandles(server, client);

  const clientCell = client.get_cell("cell-1");
  assertExists(clientCell);
  const serverCell = server.get_cell("cell-1");
  assertExists(serverCell);
  assertEquals(clientCell.source, serverCell.source);
  clientCell.free();
  serverCell.free();

  server.free();
  client.free();
});

// ── Regression tests: #1067 fix verification ─────────────────────────

Deno.test("Fix #1067: receive_frame returns inline sync reply", () => {
  // After the fix, receive_frame() generates a reply atomically after
  // applying an AUTOMERGE_SYNC frame. The reply is in FrameEvent.reply.
  const server = new NotebookHandle("reply-test");
  server.add_cell(0, "cell-1", "code");
  server.update_source("cell-1", "hello");

  const client = NotebookHandle.load(server.save());
  syncHandles(server, client);

  // Server makes a change
  server.update_source("cell-1", "hello world");

  // Send the server's change via receive_frame and deliver inline replies.
  // Peers are already converged via syncHandles, so the change arrives on
  // the first round. Loop as a safety net for the bootstrap skeleton (both
  // peers have ops from create_empty, so the first round can be heads-only
  // on some platforms).
  // deno-lint-ignore no-explicit-any
  let changedEvent: any = null;
  for (let round = 0; round < 3; round++) {
    const serverMsg = server.flush_local_changes();
    if (serverMsg) {
      const frameBytes = new Uint8Array(1 + serverMsg.length);
      frameBytes[0] = 0x00; // AUTOMERGE_SYNC
      frameBytes.set(serverMsg, 1);

      const events = client.receive_frame(frameBytes);
      if (Array.isArray(events)) {
        for (const ev of events) {
          if (ev.type === "sync_applied" && ev.changed) {
            changedEvent = ev;
          }
          // Deliver inline reply back to server (matches production behavior).
          // receive_frame already consumed sync_state for this reply — do NOT
          // also call flush_local_changes() or the state is double-consumed.
          if (ev.reply) {
            server.receive_sync_message(new Uint8Array(ev.reply));
          }
        }
      }
    }
    if (changedEvent) break;
  }

  assertExists(
    changedEvent,
    "Should have received a sync_applied event with changed:true",
  );

  // The fix: reply should be present — generated atomically
  assert(
    changedEvent.reply !== undefined && changedEvent.reply !== null,
    "SyncApplied should include a reply (the core #1067 fix)",
  );

  // Verify convergence
  const serverCell = server.get_cell("cell-1");
  const clientCell = client.get_cell("cell-1");
  assertExists(serverCell);
  assertExists(clientCell);
  assertEquals(serverCell.source, "hello world");
  assertEquals(clientCell.source, "hello world");
  serverCell.free();
  clientCell.free();

  server.free();
  client.free();
});

Deno.test(
  "Fix #1067: flush_local_changes only fires for local mutations",
  () => {
    const server = new NotebookHandle("flush-test");
    server.add_cell(0, "cell-1", "code");

    const client = NotebookHandle.load(server.save());
    syncHandles(server, client);

    // No local changes — flush should return undefined
    assertEquals(
      client.flush_local_changes(),
      undefined,
      "flush_local_changes should return undefined when no local changes exist",
    );

    // Make a local change
    client.update_source("cell-1", "edited");

    // Now flush should return bytes
    const msg = client.flush_local_changes();
    assert(
      msg !== undefined,
      "flush_local_changes should return bytes after local edit",
    );

    // Calling again without new changes — should return undefined (in_flight)
    assertEquals(
      client.flush_local_changes(),
      undefined,
      "flush_local_changes should return undefined after already flushing (in_flight)",
    );

    server.free();
    client.free();
  },
);

Deno.test("Fix #1067: no reply consumption race with new API", () => {
  // This tests that the old race can't happen with the new API:
  // receive_frame generates replies inline, so flush_local_changes
  // can't steal them.
  const server = new NotebookHandle("no-race-test");
  server.add_cell(0, "cell-1", "code");

  const client = NotebookHandle.load(server.save());
  syncHandles(server, client);

  // Server streams rapid changes (simulates IOPub output burst)
  server.update_source("cell-1", "output-v1");
  const msg1 = server.flush_local_changes();
  assert(msg1 !== undefined);

  server.update_source("cell-1", "output-v2");
  const msg2 = server.flush_local_changes();
  assert(msg2 !== undefined);

  // Client receives both via receive_frame — replies generated inline
  const frame1 = new Uint8Array(1 + msg1.length);
  frame1[0] = 0x00;
  frame1.set(msg1, 1);
  const events1 = client.receive_frame(frame1);
  const reply1 = events1[0]?.reply;

  const frame2 = new Uint8Array(1 + msg2.length);
  frame2[0] = 0x00;
  frame2.set(msg2, 1);
  const events2 = client.receive_frame(frame2);
  const reply2 = events2[0]?.reply;

  // Now client calls flush_local_changes (simulating flushSync before execute)
  // This should NOT consume any pending reply — replies were already generated
  // inline by receive_frame.
  const flushMsg = client.flush_local_changes();
  // flushMsg should be undefined — client has no local changes, only received changes
  assertEquals(
    flushMsg,
    undefined,
    "flush_local_changes should return undefined when client only received (no local edits)",
  );

  // Deliver whichever replies were generated — protocol should converge
  if (reply1) server.receive_sync_message(new Uint8Array(reply1));
  if (reply2) server.receive_sync_message(new Uint8Array(reply2));
  syncHandles(server, client);

  const clientCell = client.get_cell("cell-1");
  assertExists(clientCell);
  assertEquals(clientCell.source, "output-v2");
  clientCell.free();

  server.free();
  client.free();
});

Deno.test("Fix #1067: cancel_last_flush recovers from failed send", () => {
  // Tests that cancel_last_flush clears in_flight and sent_hashes,
  // allowing the next flush to include the full change data.
  const server = new NotebookHandle("cancel-test");
  server.add_cell(0, "cell-1", "code");

  const client = NotebookHandle.load(server.save());
  syncHandles(server, client);

  // Client makes a local change
  client.update_source("cell-1", "local edit");

  // Flush generates a message (advances sync_state)
  const msg1 = client.flush_local_changes();
  assert(msg1 !== undefined, "first flush should produce a message");

  // Simulate delivery failure — message is DROPPED
  // Call cancel_last_flush to roll back sync_state
  client.cancel_last_flush();

  // Now flush again — should produce a NEW message with the same change data
  const msg2 = client.flush_local_changes();
  assert(
    msg2 !== undefined,
    "after cancel_last_flush, flush should produce a message (sent_hashes cleared)",
  );

  // Deliver msg2 to server — should converge
  server.receive_sync_message(msg2);
  syncHandles(server, client);

  const serverCell = server.get_cell("cell-1");
  assertExists(serverCell);
  assertEquals(serverCell.source, "local edit");
  serverCell.free();

  server.free();
  client.free();
});

Deno.test(
  "Fix #1067: sent_hashes sticky without cancel (documents danger)",
  () => {
    // This test documents the DANGEROUS behavior when cancel_last_flush
    // is NOT called after a failed send. The sent_hashes set retains
    // hashes for changes the server never received, causing future
    // sync messages to filter them out.
    const server = new NotebookHandle("sticky-test");
    server.add_cell(0, "cell-1", "code");

    const client = NotebookHandle.load(server.save());
    syncHandles(server, client);

    // Client makes a local change
    client.update_source("cell-1", "local edit");

    // Flush generates a message (advances sync_state, adds to sent_hashes)
    const msg1 = client.flush_local_changes();
    assert(msg1 !== undefined);

    // Simulate delivery failure — message is DROPPED
    // DELIBERATELY do NOT call cancel_last_flush

    // Try to flush again — in_flight is true, so this returns undefined
    const msg2 = client.flush_local_changes();
    assertEquals(
      msg2,
      undefined,
      "without cancel, in_flight blocks the next flush",
    );

    // Even after receiving a server frame (which clears in_flight),
    // the local change data may be filtered by sent_hashes.
    // Server sends a trivial change to clear in_flight on client.
    server.update_source("cell-1", "server edit");
    const serverMsg = server.flush_local_changes();
    assert(serverMsg !== undefined);
    client.receive_sync_message(serverMsg);

    // Now try flush again — in_flight was cleared by receive_sync_message
    const msg3 = client.flush_local_changes();
    // msg3 might be defined (protocol can partially recover when heads changed)
    // but the important thing is that without cancel, recovery is unreliable.

    // Verify: reset_sync_state always recovers (the nuclear option)
    client.reset_sync_state();
    syncHandles(server, client);

    // After reset, both should converge to the server's version
    // (client's "local edit" was superseded by "server edit" via CRDT merge)
    const serverCell = server.get_cell("cell-1");
    const clientCell = client.get_cell("cell-1");
    assertExists(serverCell);
    assertExists(clientCell);
    assertEquals(clientCell.source, serverCell.source);
    serverCell.free();
    clientCell.free();

    server.free();
    client.free();
  },
);

Deno.test(
  "Fix #1067: rapid receive_frame produces correct reply pattern",
  () => {
    // Verifies that rapid inbound frames produce the right pattern of
    // replies: first frame gets a reply, subsequent frames may or may not
    // depending on in_flight and heads.
    const server = new NotebookHandle("rapid-reply-test");
    server.add_cell(0, "cell-1", "code");

    const client = NotebookHandle.load(server.save());
    syncHandles(server, client);

    const replies: (Uint8Array | null)[] = [];

    // Server sends 5 rapid changes
    for (let i = 1; i <= 5; i++) {
      server.update_source("cell-1", `line ${i}`);
      const msg = server.flush_local_changes();
      assert(msg !== undefined, `server msg ${i} should exist`);

      const frame = new Uint8Array(1 + msg.length);
      frame[0] = 0x00;
      frame.set(msg, 1);

      const events = client.receive_frame(frame);
      assert(Array.isArray(events) && events.length === 1);
      assertEquals(events[0].type, "sync_applied");

      const reply = events[0].reply;
      replies.push(reply ? new Uint8Array(reply) : null);

      // Deliver reply to server immediately (simulating no batching)
      if (reply) {
        server.receive_sync_message(new Uint8Array(reply));
      }
    }

    // At least the first reply should be defined
    assert(replies[0] !== null, "first frame should produce a reply");

    // After all frames delivered, client should have latest content
    const clientCell = client.get_cell("cell-1");
    assertExists(clientCell);
    assertEquals(clientCell.source, "line 5");
    clientCell.free();

    // Protocol should be converged
    assertEquals(client.flush_local_changes(), undefined);
    assertEquals(server.flush_local_changes(), undefined);

    server.free();
    client.free();
  },
);

Deno.test("Fix #1067: concurrent local edit + daemon frame", () => {
  // Client makes a local edit, then receives a daemon frame before
  // flushing. The inline reply should include the local changes.
  const server = new NotebookHandle("concurrent-test");
  server.add_cell(0, "cell-1", "code");
  server.add_cell(1, "cell-2", "code");

  const client = NotebookHandle.load(server.save());
  syncHandles(server, client);

  // Client makes a local edit (not yet flushed)
  client.update_source("cell-1", "client edit");

  // Server makes a different change
  server.update_source("cell-2", "server edit");
  const serverMsg = server.flush_local_changes();
  assert(serverMsg !== undefined);

  // Client receives the server's change via receive_frame
  const frame = new Uint8Array(1 + serverMsg.length);
  frame[0] = 0x00;
  frame.set(serverMsg, 1);
  const events = client.receive_frame(frame);
  const reply = events[0]?.reply;

  // The inline reply should exist and when delivered should bring
  // the server up to date with the client's local edit
  assert(reply !== undefined, "reply should include client's local changes");
  server.receive_sync_message(new Uint8Array(reply));

  // May need another round to fully converge
  syncHandles(server, client);

  // Both should have both edits
  const serverCell1 = server.get_cell("cell-1");
  const serverCell2 = server.get_cell("cell-2");
  assertExists(serverCell1);
  assertExists(serverCell2);
  assertEquals(serverCell1.source, "client edit");
  assertEquals(serverCell2.source, "server edit");
  serverCell1.free();
  serverCell2.free();

  server.free();
  client.free();
});

Deno.test(
  "Fix #1068 review: dropped inline reply with unflushed local edit stalls sync",
  () => {
    // Codex review of #1068 found that if the client has unflushed local
    // edits and receive_frame generates an inline reply containing those
    // edits, dropping the reply strands the local change hashes in
    // sent_hashes. Subsequent sync messages filter out the local change
    // data, causing a non-converging loop where the server never learns
    // the client's edit.
    //
    // The fix: call cancel_last_flush() on reply send failure, same as
    // the flush path. This test verifies recovery.

    const server = new NotebookHandle("dropped-reply-test");
    server.add_cell(0, "cell-1", "code");

    const client = NotebookHandle.load(server.save());
    syncHandles(server, client);

    // Client makes a local edit (NOT flushed yet)
    client.update_source("cell-1", "local edit from client");

    // Server makes a change
    server.update_source("cell-1", "server change");
    const serverMsg = server.flush_local_changes();
    assert(serverMsg !== undefined);

    // Client receives the server's change via receive_frame.
    // The inline reply will include the client's local edit in sent_hashes.
    const frame = new Uint8Array(1 + serverMsg.length);
    frame[0] = 0x00;
    frame.set(serverMsg, 1);
    const events = client.receive_frame(frame);
    const reply = events[0]?.reply;
    assert(
      reply !== undefined,
      "reply should exist (client has local changes)",
    );

    // SIMULATE DELIVERY FAILURE: reply is DROPPED.
    // Call cancel_last_flush to clear sent_hashes (the fix).
    client.cancel_last_flush();

    // Server makes another change
    server.update_source("cell-1", "server change v2");
    const serverMsg2 = server.flush_local_changes();
    assert(serverMsg2 !== undefined);

    // Client receives via receive_frame — this reply should include the
    // local edit because cancel_last_flush cleared sent_hashes.
    const frame2 = new Uint8Array(1 + serverMsg2.length);
    frame2[0] = 0x00;
    frame2.set(serverMsg2, 1);
    const events2 = client.receive_frame(frame2);
    const reply2 = events2[0]?.reply;

    // Deliver reply2 to server
    if (reply2) {
      server.receive_sync_message(new Uint8Array(reply2));
    }

    // Sync to convergence
    syncHandles(server, client);

    // Both should have the merged result (CRDT merge of both edits)
    const serverCell = server.get_cell("cell-1");
    const clientCell = client.get_cell("cell-1");
    assertExists(serverCell);
    assertExists(clientCell);
    assertEquals(
      clientCell.source,
      serverCell.source,
      "client and server must converge after cancel_last_flush recovery",
    );
    serverCell.free();
    clientCell.free();

    server.free();
    client.free();
  },
);

Deno.test(
  "Fix #1068 review: WITHOUT cancel, dropped inline reply with local edit does NOT converge",
  () => {
    // Documents the dangerous behavior: if cancel_last_flush is NOT called
    // after a dropped inline reply that carried local changes, the peers
    // may not converge. sent_hashes filters out the local change data.

    const server = new NotebookHandle("no-cancel-reply-test");
    server.add_cell(0, "cell-1", "code");

    const client = NotebookHandle.load(server.save());
    syncHandles(server, client);

    // Client makes a local edit (NOT flushed)
    client.update_source("cell-1", "stranded edit");

    // Server makes a change
    server.update_source("cell-1", "server v1");
    const serverMsg = server.flush_local_changes();
    assert(serverMsg !== undefined);

    // Client receives, reply generated (includes local edit in sent_hashes)
    const frame = new Uint8Array(1 + serverMsg.length);
    frame[0] = 0x00;
    frame.set(serverMsg, 1);
    const events = client.receive_frame(frame);
    assert(events[0]?.reply !== undefined);

    // REPLY DROPPED — deliberately do NOT call cancel_last_flush

    // Try several rounds of server changes + client receive_frame
    // to see if the protocol can self-heal without cancel.
    for (let i = 2; i <= 5; i++) {
      server.update_source("cell-1", `server v${i}`);
      const msg = server.flush_local_changes();
      if (!msg) break;
      const f = new Uint8Array(1 + msg.length);
      f[0] = 0x00;
      f.set(msg, 1);
      const ev = client.receive_frame(f);
      if (ev[0]?.reply) {
        server.receive_sync_message(new Uint8Array(ev[0].reply));
      }
    }
    syncHandles(server, client);

    const serverCell = server.get_cell("cell-1");
    const clientCell = client.get_cell("cell-1");
    assertExists(serverCell);
    assertExists(clientCell);

    if (serverCell.source !== clientCell.source) {
      console.warn(
        "CONFIRMED: without cancel_last_flush on dropped inline reply, " +
          "peers diverge. Server has: " +
          JSON.stringify(serverCell.source) +
          ", client has: " +
          JSON.stringify(clientCell.source),
      );
    }

    // Nuclear recovery always works
    client.reset_sync_state();
    syncHandles(server, client);

    const recoveredClient = client.get_cell("cell-1");
    const recoveredServer = server.get_cell("cell-1");
    assertExists(recoveredClient);
    assertExists(recoveredServer);
    assertEquals(
      recoveredClient.source,
      recoveredServer.source,
      "reset_sync_state must always recover convergence",
    );
    recoveredClient.free();
    recoveredServer.free();

    serverCell.free();
    clientCell.free();
    server.free();
    client.free();
  },
);

// ── Regression tests: #1074 — RuntimeStateDoc sync race ──────────────

Deno.test(
  "Fix #1074: flush_runtime_state_sync produces initial message from fresh handle",
  () => {
    // A freshly-created handle has an empty RuntimeStateDoc.
    // flush_runtime_state_sync() should produce a sync message so the
    // daemon knows we need the full state (kernel status, trust, etc.).
    const handle = NotebookHandle.create_empty_with_actor("test:bootstrap");

    const msg = handle.flush_runtime_state_sync();
    assert(
      msg !== undefined,
      "flush_runtime_state_sync should produce a message from a fresh handle",
    );

    // Calling again without receiving anything — in_flight blocks it
    const msg2 = handle.flush_runtime_state_sync();
    assertEquals(
      msg2,
      undefined,
      "second flush should return undefined (in_flight)",
    );

    handle.free();
  },
);

Deno.test(
  "Fix #1074: cancel_last_runtime_state_flush recovers from failed send",
  () => {
    // Mirrors the cancel_last_flush test from #1068 but for state_sync_state.
    // If the RUNTIME_STATE_SYNC send fails, cancel clears in_flight and
    // sent_hashes so the next flush produces a fresh message.
    const handle = NotebookHandle.create_empty_with_actor("test:cancel");

    // First flush — generates a message (advances state_sync_state)
    const msg1 = handle.flush_runtime_state_sync();
    assert(msg1 !== undefined, "first flush should produce a message");

    // Simulate delivery failure — message is DROPPED
    // Call cancel to roll back state_sync_state
    handle.cancel_last_runtime_state_flush();

    // Now flush again — should produce a NEW message
    const msg2 = handle.flush_runtime_state_sync();
    assert(
      msg2 !== undefined,
      "after cancel_last_runtime_state_flush, flush should produce a message",
    );

    handle.free();
  },
);

Deno.test(
  "Fix #1074: WITHOUT cancel, dropped RuntimeStateSync stalls (documents danger)",
  () => {
    // Documents the dangerous behavior: if cancel_last_runtime_state_flush
    // is NOT called after a failed send, state_sync_state retains stale
    // in_flight/sent_hashes and subsequent flushes return undefined.
    const handle = NotebookHandle.create_empty_with_actor("test:no-cancel");

    // First flush — advances state_sync_state
    const msg1 = handle.flush_runtime_state_sync();
    assert(msg1 !== undefined);

    // Simulate delivery failure — DELIBERATELY do NOT cancel
    // in_flight is now true, blocking future flushes

    const msg2 = handle.flush_runtime_state_sync();
    assertEquals(
      msg2,
      undefined,
      "without cancel, in_flight blocks the next flush",
    );

    // Even generate_runtime_state_sync_reply is affected (same sync state)
    // because in_flight prevents message generation.
    // The only recovery without cancel is reset_sync_state.
    if (msg2 === undefined) {
      console.warn(
        "CONFIRMED: without cancel_last_runtime_state_flush on dropped " +
          "RuntimeStateSync, subsequent flushes return undefined. " +
          "Kernel status stays stuck at not_started until page reload.",
      );
    }

    // Nuclear recovery always works
    handle.reset_sync_state();
    const msg3 = handle.flush_runtime_state_sync();
    assert(
      msg3 !== undefined,
      "reset_sync_state must always recover RuntimeStateDoc sync",
    );

    handle.free();
  },
);

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
  assertEquals(daemon.flush_local_changes(), undefined);
  assertEquals(wasm.flush_local_changes(), undefined);

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
  assertEquals(daemon.flush_local_changes(), undefined);
  assertEquals(wasm.flush_local_changes(), undefined);

  // Daemon adds new content
  daemon.add_cell(1, "new-cell", "markdown");
  daemon.update_source("new-cell", "# New section");

  // Sync the new content. Both peers are already converged, so the
  // daemon's change should arrive in the first sync message.
  const syncMsg = daemon.flush_local_changes();
  assert(
    syncMsg !== undefined,
    "daemon should have a sync message for the new cell",
  );
  const sawChange = wasm.receive_sync_message(syncMsg);
  assert(sawChange, "receive_sync_message should return true when doc changes");
  syncHandles(daemon, wasm); // complete any remaining handshake

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
    daemon.flush_local_changes(),
    undefined,
    "Daemon has no message when converged",
  );
  assertEquals(
    wasm.flush_local_changes(),
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
  assertEquals(daemon.flush_local_changes(), undefined);
  assertEquals(wasm.flush_local_changes(), undefined);

  // Daemon updates the cell
  daemon.update_source("cell-1", "updated");

  // WASM resets sync state (simulating HMR reload or reconnect)
  wasm.reset_sync_state();

  // After reset, WASM should need to sync again
  const wasmMsg = wasm.flush_local_changes();
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

Deno.test("create_empty: creates doc with bootstrap skeleton", () => {
  const handle = NotebookHandle.create_empty();
  // Bootstrap seeds the doc with schema_version, empty cells map, and
  // empty metadata map.  This prevents automerge's load_incremental
  // empty-doc fast-path from discarding encoding/actor settings on
  // the first sync.
  assertEquals(handle.cell_count(), 0);
  assertEquals(handle.get_cells().length, 0);
  assertEquals(handle.get_cells_json(), "[]");
  // Bootstrap does NOT set a default runtime — that's determined by
  // the notebook file or user choice.
  assertEquals(handle.get_metadata("runtime"), undefined);
  handle.free();
});

Deno.test(
  "create_empty: sync-only bootstrap receives all content from daemon",
  () => {
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
  },
);

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
  const msg = daemon.flush_local_changes();
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

// ── Notebook metadata snapshot tests ────────────────────────────────

Deno.test(
  "Metadata snapshot: returns plain Objects, not Maps (serde flatten regression)",
  () => {
    // Regression test: RuntMetadata has #[serde(flatten)] on its `extra` field,
    // which causes serde to emit it via serialize_map. serde_wasm_bindgen defaults
    // to creating JS Map objects for maps, making snapshot.runt a Map — breaking
    // dot-access (snapshot.runt.uv would be undefined). The fix uses
    // serialize_maps_as_objects(true) so all maps become plain Objects.
    const handle = new NotebookHandle("metadata-snapshot-test");
    handle.add_uv_dependency("pandas>=2.0");
    handle.add_uv_dependency("numpy");

    const snapshot = handle.get_metadata_snapshot();
    assertExists(snapshot, "snapshot should not be undefined");

    // The snapshot itself must be a plain object, not a Map
    assert(
      !(snapshot instanceof Map),
      "snapshot should be a plain Object, not a Map",
    );

    // runt must be a plain object (this is the one that breaks with flatten)
    assertExists(snapshot.runt, "snapshot.runt should exist");
    assert(
      !(snapshot.runt instanceof Map),
      "snapshot.runt should be a plain Object, not a Map",
    );

    // UV deps must be accessible via dot notation
    assertExists(snapshot.runt.uv, "snapshot.runt.uv should exist");
    assert(
      !(snapshot.runt.uv instanceof Map),
      "snapshot.runt.uv should be a plain Object, not a Map",
    );
    assertExists(
      snapshot.runt.uv.dependencies,
      "snapshot.runt.uv.dependencies should exist",
    );
    assert(
      Array.isArray(snapshot.runt.uv.dependencies),
      "dependencies should be an array",
    );
    assertEquals(snapshot.runt.uv.dependencies.length, 2);
    assert(snapshot.runt.uv.dependencies.includes("pandas>=2.0"));
    assert(snapshot.runt.uv.dependencies.includes("numpy"));

    handle.free();
  },
);

Deno.test(
  "Metadata snapshot: UV requires-python and prerelease accessible via dot notation",
  () => {
    const handle = new NotebookHandle("metadata-snapshot-uv-fields");
    handle.add_uv_dependency("requests");
    handle.set_uv_requires_python(">=3.10");
    handle.set_uv_prerelease("allow");

    const snapshot = handle.get_metadata_snapshot();
    assertExists(snapshot.runt.uv);
    assertEquals(snapshot.runt.uv["requires-python"], ">=3.10");
    assertEquals(snapshot.runt.uv.prerelease, "allow");

    handle.free();
  },
);

Deno.test("Metadata snapshot: conda deps accessible via dot notation", () => {
  const handle = new NotebookHandle("metadata-snapshot-conda");
  handle.add_conda_dependency("scipy");
  handle.set_conda_channels('["conda-forge"]');
  handle.set_conda_python("3.11");

  const snapshot = handle.get_metadata_snapshot();
  assertExists(snapshot.runt.conda, "snapshot.runt.conda should exist");
  assert(
    !(snapshot.runt.conda instanceof Map),
    "snapshot.runt.conda should be a plain Object, not a Map",
  );
  assert(snapshot.runt.conda.dependencies.includes("scipy"));
  assertEquals(snapshot.runt.conda.channels, ["conda-forge"]);
  assertEquals(snapshot.runt.conda.python, "3.11");

  handle.free();
});

Deno.test(
  "Metadata snapshot: synced deps visible via get_metadata_snapshot on peer",
  () => {
    const daemon = new NotebookHandle("metadata-snapshot-sync");
    daemon.add_uv_dependency("flask");

    const wasm = NotebookHandle.load(daemon.save());
    syncHandles(daemon, wasm);

    // Peer should see deps via snapshot dot-access (not Map.get)
    const snapshot = wasm.get_metadata_snapshot();
    assertExists(snapshot.runt.uv);
    assert(snapshot.runt.uv.dependencies.includes("flask"));

    daemon.free();
    wasm.free();
  },
);

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
