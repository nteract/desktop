/**
 * Comprehensive tests for splice_source — the character-level Automerge Text
 * splice primitive that powers the CodeMirror ↔ CRDT bridge.
 *
 * These tests exercise the real WASM binary in Deno with no daemon, no Tauri,
 * no React. They cover:
 *
 *   1. Basic splice operations (insert, delete, replace, no-op)
 *   2. Boundary conditions (start, end, empty doc, empty cell)
 *   3. Error handling (bad cell ID, out-of-range index)
 *   4. Sequential rapid splices (simulating fast typing)
 *   5. Sync between two handles after splice_source
 *   6. Concurrent edits: splice_source on one handle, update_source on another
 *   7. Concurrent character-level edits at different positions
 *   8. Concurrent edits at the same position (conflict resolution)
 *   9. Interleaved splice + sync (simulating the real bridge pattern)
 *  10. Text attributions produced by splice_source after sync
 *  11. Mixed operations: splice + append + update on same cell
 *  12. Large document stress test
 *  13. Unicode / multi-byte character handling
 *  14. Newline handling (multi-line source)
 *  15. Delete-all then retype (re-execution pattern)
 *
 * Run with:
 *   deno test --allow-read crates/runtimed-wasm/tests/splice_source_test.ts
 */

import {
  assert,
  assertEquals,
  assertExists,
} from "https://deno.land/std@0.224.0/assert/mod.ts";

// @ts-nocheck — wasm-bindgen output doesn't have Deno-compatible type declarations

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

// deno-lint-ignore no-explicit-any
type Handle = any;

/** Sync two handles until convergence (no more messages in either direction). */
function syncHandles(a: Handle, b: Handle, maxRounds = 20) {
  for (let i = 0; i < maxRounds; i++) {
    const msgA = a.flush_local_changes();
    const msgB = b.flush_local_changes();
    if (!msgA && !msgB) break;
    if (msgA) b.receive_sync_message(msgA);
    if (msgB) a.receive_sync_message(msgB);
  }
}

/** Create a handle with one code cell, optionally pre-populated with source. */
function makeHandle(nbId: string, cellId: string, source = ""): Handle {
  const h = new NotebookHandle(nbId);
  h.add_cell(0, cellId, "code");
  if (source) {
    h.update_source(cellId, source);
  }
  return h;
}

/** Read a cell's source from a handle. */
function getSource(h: Handle, cellId: string): string {
  return h.get_cell_source(cellId) ?? "";
}

/**
 * Sync handle `from` → `to` via receive_frame and return the FrameEvents.
 * This simulates the relay path: one handle generates a sync message,
 * the other receives it as a frame and returns events with changesets
 * and attributions.
 */
function syncViaFrame(
  from: Handle,
  to: Handle,
  // deno-lint-ignore no-explicit-any
): any[] | null {
  const msg = from.flush_local_changes();
  if (!msg) return null;

  // Build a frame: type byte 0x00 (AUTOMERGE_SYNC) + payload
  const frame = new Uint8Array(1 + msg.length);
  frame[0] = 0x00; // AUTOMERGE_SYNC
  frame.set(msg, 1);

  const events = to.receive_frame(frame);
  return events ?? null;
}

// ── 1. Basic splice operations ───────────────────────────────────────

Deno.test("splice_source: insert at start of empty source", () => {
  const h = makeHandle("basic-1", "c1");
  const ok = h.splice_source("c1", 0, 0, "hello");
  assertEquals(ok, true);
  assertEquals(getSource(h, "c1"), "hello");
  h.free();
});

Deno.test("splice_source: insert at end", () => {
  const h = makeHandle("basic-2", "c1", "hello");
  h.splice_source("c1", 5, 0, " world");
  assertEquals(getSource(h, "c1"), "hello world");
  h.free();
});

Deno.test("splice_source: insert in middle", () => {
  const h = makeHandle("basic-3", "c1", "helo");
  h.splice_source("c1", 2, 0, "l");
  assertEquals(getSource(h, "c1"), "hello");
  h.free();
});

Deno.test("splice_source: delete from start", () => {
  const h = makeHandle("basic-4", "c1", "hello world");
  h.splice_source("c1", 0, 6, "");
  assertEquals(getSource(h, "c1"), "world");
  h.free();
});

Deno.test("splice_source: delete from end", () => {
  const h = makeHandle("basic-5", "c1", "hello world");
  h.splice_source("c1", 5, 6, "");
  assertEquals(getSource(h, "c1"), "hello");
  h.free();
});

Deno.test("splice_source: delete from middle", () => {
  const h = makeHandle("basic-6", "c1", "hello world");
  h.splice_source("c1", 5, 1, "");
  assertEquals(getSource(h, "c1"), "helloworld");
  h.free();
});

Deno.test("splice_source: replace (delete + insert)", () => {
  const h = makeHandle("basic-7", "c1", "hello world");
  h.splice_source("c1", 6, 5, "there");
  assertEquals(getSource(h, "c1"), "hello there");
  h.free();
});

Deno.test("splice_source: replace with longer text", () => {
  const h = makeHandle("basic-8", "c1", "abc");
  h.splice_source("c1", 1, 1, "XYZ");
  assertEquals(getSource(h, "c1"), "aXYZc");
  h.free();
});

Deno.test("splice_source: replace with shorter text", () => {
  const h = makeHandle("basic-9", "c1", "abcde");
  h.splice_source("c1", 1, 3, "X");
  assertEquals(getSource(h, "c1"), "aXe");
  h.free();
});

Deno.test("splice_source: no-op (insert empty string, delete 0)", () => {
  const h = makeHandle("basic-10", "c1", "hello");
  h.splice_source("c1", 2, 0, "");
  assertEquals(getSource(h, "c1"), "hello");
  h.free();
});

Deno.test("splice_source: delete entire content", () => {
  const h = makeHandle("basic-11", "c1", "hello");
  h.splice_source("c1", 0, 5, "");
  assertEquals(getSource(h, "c1"), "");
  h.free();
});

Deno.test("splice_source: replace entire content", () => {
  const h = makeHandle("basic-12", "c1", "hello");
  h.splice_source("c1", 0, 5, "goodbye");
  assertEquals(getSource(h, "c1"), "goodbye");
  h.free();
});

// ── 2. Boundary conditions ───────────────────────────────────────────

Deno.test(
  "splice_source: insert into truly empty cell (never had source)",
  () => {
    const h = new NotebookHandle("boundary-1");
    h.add_cell(0, "c1", "code");
    // Cell source is "" by default
    assertEquals(getSource(h, "c1"), "");
    h.splice_source("c1", 0, 0, "x");
    assertEquals(getSource(h, "c1"), "x");
    h.free();
  },
);

Deno.test("splice_source: single character insert then delete", () => {
  const h = makeHandle("boundary-2", "c1");
  h.splice_source("c1", 0, 0, "a");
  assertEquals(getSource(h, "c1"), "a");
  h.splice_source("c1", 0, 1, "");
  assertEquals(getSource(h, "c1"), "");
  h.free();
});

Deno.test("splice_source: insert at exactly doc.length", () => {
  const h = makeHandle("boundary-3", "c1", "abc");
  h.splice_source("c1", 3, 0, "d");
  assertEquals(getSource(h, "c1"), "abcd");
  h.free();
});

// ── 3. Error handling ────────────────────────────────────────────────

Deno.test("splice_source: non-existent cell returns false", () => {
  const h = makeHandle("err-1", "c1", "hello");
  const ok = h.splice_source("nonexistent", 0, 0, "x");
  assertEquals(ok, false);
  // Original cell unchanged
  assertEquals(getSource(h, "c1"), "hello");
  h.free();
});

Deno.test("splice_source: empty cell ID returns false", () => {
  const h = makeHandle("err-2", "c1", "hello");
  const ok = h.splice_source("", 0, 0, "x");
  assertEquals(ok, false);
  h.free();
});

// ── 4. Sequential rapid splices (simulating typing) ──────────────────

Deno.test(
  "splice_source: simulate typing 'import time' character by character",
  () => {
    const h = makeHandle("typing-1", "c1");
    const text = "import time";
    for (let i = 0; i < text.length; i++) {
      h.splice_source("c1", i, 0, text[i]);
    }
    assertEquals(getSource(h, "c1"), "import time");
    h.free();
  },
);

Deno.test("splice_source: simulate typing with backspace corrections", () => {
  const h = makeHandle("typing-2", "c1");
  // Type "improt" (typo — 'r' and 'o' transposed)
  for (const ch of "improt") {
    const len = getSource(h, "c1").length;
    h.splice_source("c1", len, 0, ch);
  }
  assertEquals(getSource(h, "c1"), "improt");
  // Backspace three times from end to get back to "imp"
  h.splice_source("c1", 5, 1, ""); // delete 't' at index 5 → "impro"
  h.splice_source("c1", 4, 1, ""); // delete 'o' at index 4 → "impr"
  h.splice_source("c1", 3, 1, ""); // delete 'r' at index 3 → "imp"
  assertEquals(getSource(h, "c1"), "imp");
  // Retype correctly
  for (const ch of "ort") {
    const len = getSource(h, "c1").length;
    h.splice_source("c1", len, 0, ch);
  }
  assertEquals(getSource(h, "c1"), "import");
  h.free();
});

Deno.test("splice_source: simulate typing multiline code", () => {
  const h = makeHandle("typing-3", "c1");
  const lines = ["import time", "\n", "time.sleep(1)", "\n", "print('done')"];
  let pos = 0;
  for (const segment of lines) {
    for (const ch of segment) {
      h.splice_source("c1", pos, 0, ch);
      pos++;
    }
  }
  assertEquals(getSource(h, "c1"), "import time\ntime.sleep(1)\nprint('done')");
  h.free();
});

Deno.test("splice_source: 1000 sequential character inserts", () => {
  const h = makeHandle("typing-4", "c1");
  const chars = "abcdefghij".repeat(100); // 1000 chars
  for (let i = 0; i < chars.length; i++) {
    h.splice_source("c1", i, 0, chars[i]);
  }
  assertEquals(getSource(h, "c1"), chars);
  assertEquals(getSource(h, "c1").length, 1000);
  h.free();
});

Deno.test("splice_source: paste (single large insert)", () => {
  const h = makeHandle("typing-5", "c1", "before\nafter");
  const pasted = "line1\nline2\nline3\n";
  // Paste between "before\n" and "after"
  h.splice_source("c1", 7, 0, pasted);
  assertEquals(getSource(h, "c1"), "before\nline1\nline2\nline3\nafter");
  h.free();
});

Deno.test("splice_source: select-all and replace (Cmd+A, type)", () => {
  const h = makeHandle("typing-6", "c1", "old content that should go away");
  const oldLen = getSource(h, "c1").length;
  h.splice_source("c1", 0, oldLen, "new");
  assertEquals(getSource(h, "c1"), "new");
  h.free();
});

// ── 5. Sync between two handles after splice_source ──────────────────

Deno.test("splice_source: sync single insert to second handle", () => {
  const a = makeHandle("sync-1", "c1", "hello");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  a.splice_source("c1", 5, 0, " world");
  syncHandles(a, b);

  assertEquals(getSource(a, "c1"), "hello world");
  assertEquals(getSource(b, "c1"), "hello world");
  a.free();
  b.free();
});

Deno.test("splice_source: sync deletion to second handle", () => {
  const a = makeHandle("sync-2", "c1", "hello world");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  a.splice_source("c1", 5, 6, "");
  syncHandles(a, b);

  assertEquals(getSource(a, "c1"), "hello");
  assertEquals(getSource(b, "c1"), "hello");
  a.free();
  b.free();
});

Deno.test("splice_source: sync replacement to second handle", () => {
  const a = makeHandle("sync-3", "c1", "hello world");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  a.splice_source("c1", 6, 5, "there");
  syncHandles(a, b);

  assertEquals(getSource(a, "c1"), "hello there");
  assertEquals(getSource(b, "c1"), "hello there");
  a.free();
  b.free();
});

Deno.test("splice_source: sync many rapid splices", () => {
  const a = makeHandle("sync-4", "c1");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  // Type a whole line without syncing in between
  const line = "print('hello world')";
  for (let i = 0; i < line.length; i++) {
    a.splice_source("c1", i, 0, line[i]);
  }

  // Now sync — all changes arrive at once
  syncHandles(a, b);

  assertEquals(getSource(a, "c1"), line);
  assertEquals(getSource(b, "c1"), line);
  a.free();
  b.free();
});

// ── 6. Concurrent edits: splice_source vs update_source ──────────────

Deno.test(
  "concurrent: splice_source on A, update_source on B, non-overlapping",
  () => {
    const a = makeHandle("conc-1", "c1", "hello world");
    const b = NotebookHandle.load(a.save());
    syncHandles(a, b);

    // A modifies the beginning via splice
    a.splice_source("c1", 0, 5, "HELLO");
    // B modifies the end via update_source (Myers diff)
    b.update_source("c1", "hello WORLD");

    syncHandles(a, b);

    // Both changes should merge — CRDT merges non-overlapping edits
    assertEquals(getSource(a, "c1"), "HELLO WORLD");
    assertEquals(getSource(b, "c1"), "HELLO WORLD");
    a.free();
    b.free();
  },
);

Deno.test(
  "concurrent: splice_source on A, update_source on B, overlapping region",
  () => {
    const a = makeHandle("conc-2", "c1", "abcdef");
    const b = NotebookHandle.load(a.save());
    syncHandles(a, b);

    // A replaces "cd" with "XX" via splice
    a.splice_source("c1", 2, 2, "XX");
    // B replaces "cd" with "YY" via update_source
    b.update_source("c1", "abYYef");

    syncHandles(a, b);

    // Both should converge — Automerge will interleave or pick one
    const sourceA = getSource(a, "c1");
    const sourceB = getSource(b, "c1");
    assertEquals(sourceA, sourceB); // Must converge
    // The exact result depends on Automerge's conflict resolution,
    // but both peers must agree.
    assert(
      sourceA.startsWith("ab") && sourceA.endsWith("ef"),
      `Expected "ab...ef" but got "${sourceA}"`,
    );
    a.free();
    b.free();
  },
);

// ── 7. Concurrent character-level edits at different positions ────────

Deno.test("concurrent: both use splice_source at different positions", () => {
  const a = makeHandle("conc-3", "c1", "hello world");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  // A inserts at position 0
  a.splice_source("c1", 0, 0, ">>> ");
  // B inserts at position 11 (end)
  b.splice_source("c1", 11, 0, "!!!");

  syncHandles(a, b);

  const sourceA = getSource(a, "c1");
  const sourceB = getSource(b, "c1");
  assertEquals(sourceA, sourceB);
  assertEquals(sourceA, ">>> hello world!!!");
  a.free();
  b.free();
});

Deno.test("concurrent: both delete at different positions", () => {
  const a = makeHandle("conc-4", "c1", "abcdefghij");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  // A deletes "abc" (first 3)
  a.splice_source("c1", 0, 3, "");
  // B deletes "hij" (last 3)
  b.splice_source("c1", 7, 3, "");

  syncHandles(a, b);

  const sourceA = getSource(a, "c1");
  const sourceB = getSource(b, "c1");
  assertEquals(sourceA, sourceB);
  assertEquals(sourceA, "defg");
  a.free();
  b.free();
});

Deno.test("concurrent: interleaved typing from two peers", () => {
  const a = makeHandle("conc-5", "c1", "");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  // A types "aaa" at position 0
  a.splice_source("c1", 0, 0, "a");
  a.splice_source("c1", 1, 0, "a");
  a.splice_source("c1", 2, 0, "a");

  // B types "bbb" at position 0 (concurrent — hasn't seen A's changes)
  b.splice_source("c1", 0, 0, "b");
  b.splice_source("c1", 1, 0, "b");
  b.splice_source("c1", 2, 0, "b");

  syncHandles(a, b);

  const sourceA = getSource(a, "c1");
  const sourceB = getSource(b, "c1");
  assertEquals(sourceA, sourceB);
  // Should contain all 6 characters
  assertEquals(sourceA.length, 6);
  // All a's and b's present
  assertEquals([...sourceA].filter((c) => c === "a").length, 3);
  assertEquals([...sourceA].filter((c) => c === "b").length, 3);
  a.free();
  b.free();
});

// ── 8. Concurrent edits at the same position ─────────────────────────

Deno.test("concurrent: both insert at same position", () => {
  const a = makeHandle("same-pos-1", "c1", "XY");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  // Both insert between X and Y
  a.splice_source("c1", 1, 0, "A");
  b.splice_source("c1", 1, 0, "B");

  syncHandles(a, b);

  const sourceA = getSource(a, "c1");
  const sourceB = getSource(b, "c1");
  assertEquals(sourceA, sourceB);
  // Both "A" and "B" present, order determined by actor IDs
  assertEquals(sourceA.length, 4);
  assert(sourceA.startsWith("X"));
  assert(sourceA.endsWith("Y"));
  assert(sourceA.includes("A"));
  assert(sourceA.includes("B"));
  a.free();
  b.free();
});

Deno.test("concurrent: both delete same range", () => {
  const a = makeHandle("same-pos-2", "c1", "abcdef");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  // Both delete "cd"
  a.splice_source("c1", 2, 2, "");
  b.splice_source("c1", 2, 2, "");

  syncHandles(a, b);

  const sourceA = getSource(a, "c1");
  const sourceB = getSource(b, "c1");
  assertEquals(sourceA, sourceB);
  // Double-delete of the same range should still result in "abef"
  assertEquals(sourceA, "abef");
  a.free();
  b.free();
});

Deno.test("concurrent: one inserts, other deletes at same position", () => {
  const a = makeHandle("same-pos-3", "c1", "abcdef");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  // A inserts "X" at position 3
  a.splice_source("c1", 3, 0, "X");
  // B deletes "d" at position 3
  b.splice_source("c1", 3, 1, "");

  syncHandles(a, b);

  const sourceA = getSource(a, "c1");
  const sourceB = getSource(b, "c1");
  assertEquals(sourceA, sourceB);
  // "d" should be deleted, "X" should be inserted
  // Result: "abcXef" (X inserted, d deleted)
  assert(sourceA.includes("X"), `Expected "X" in "${sourceA}"`);
  assert(!sourceA.includes("d"), `Did not expect "d" in "${sourceA}"`);
  assertEquals(sourceA.length, 6); // was 6, -1 delete +1 insert = 6
  a.free();
  b.free();
});

// ── 9. Interleaved splice + sync ─────────────────────────────────────

Deno.test("interleaved: type a few chars, sync, type more, sync", () => {
  const frontend = makeHandle("interleave-1", "c1");
  const daemon = NotebookHandle.load(frontend.save());
  syncHandles(frontend, daemon);

  // Round 1: type "hel"
  frontend.splice_source("c1", 0, 0, "h");
  frontend.splice_source("c1", 1, 0, "e");
  frontend.splice_source("c1", 2, 0, "l");
  syncHandles(frontend, daemon);
  assertEquals(getSource(daemon, "c1"), "hel");

  // Round 2: type "lo"
  frontend.splice_source("c1", 3, 0, "l");
  frontend.splice_source("c1", 4, 0, "o");
  syncHandles(frontend, daemon);
  assertEquals(getSource(daemon, "c1"), "hello");

  // Round 3: daemon writes execution count (simulated by writing metadata)
  // Meanwhile frontend keeps typing
  frontend.splice_source("c1", 5, 0, " ");
  frontend.splice_source("c1", 6, 0, "w");
  // Sync mid-typing
  syncHandles(frontend, daemon);
  assertEquals(getSource(daemon, "c1"), "hello w");

  // Continue typing
  frontend.splice_source("c1", 7, 0, "o");
  frontend.splice_source("c1", 8, 0, "r");
  frontend.splice_source("c1", 9, 0, "l");
  frontend.splice_source("c1", 10, 0, "d");
  syncHandles(frontend, daemon);
  assertEquals(getSource(daemon, "c1"), "hello world");
  assertEquals(getSource(frontend, "c1"), "hello world");
  frontend.free();
  daemon.free();
});

Deno.test(
  "interleaved: frontend types while daemon writes metadata (no source conflict)",
  () => {
    const frontend = makeHandle("interleave-2", "c1");
    const daemon = NotebookHandle.load(frontend.save());
    syncHandles(frontend, daemon);

    // Frontend types
    frontend.splice_source("c1", 0, 0, "print('hi')");
    // Daemon writes a non-source change (doesn't touch source)
    daemon.add_uv_dependency("numpy");

    // Sync both ways
    syncHandles(frontend, daemon);

    // Source should be intact on both sides
    assertEquals(getSource(frontend, "c1"), "print('hi')");
    assertEquals(getSource(daemon, "c1"), "print('hi')");
    frontend.free();
    daemon.free();
  },
);

// ── 10. Text attributions ────────────────────────────────────────────

Deno.test(
  "attributions: splice_source produces insert attribution on sync",
  () => {
    const frontend = makeHandle("attr-1", "c1", "hello");
    const daemon = NotebookHandle.load(frontend.save());
    syncHandles(frontend, daemon);

    // Frontend inserts " world"
    frontend.splice_source("c1", 5, 0, " world");

    // Sync via frame to get attribution events
    const events = syncViaFrame(frontend, daemon);
    assertExists(events);

    const syncEvent = events.find(
      // deno-lint-ignore no-explicit-any
      (e: any) => e.type === "sync_applied" && e.changed,
    );
    assertExists(syncEvent);
    assert(Array.isArray(syncEvent.attributions));
    assert(syncEvent.attributions.length > 0);

    // Find the attribution for our cell
    // deno-lint-ignore no-explicit-any
    const attr = syncEvent.attributions.find((a: any) => a.cell_id === "c1");
    assertExists(attr);
    assertEquals(attr.text, " world");
    assertEquals(attr.index, 5);
    assertEquals(attr.deleted, 0);
    frontend.free();
    daemon.free();
  },
);

Deno.test(
  "attributions: splice_source deletion produces delete attribution",
  () => {
    const frontend = makeHandle("attr-2", "c1", "hello world");
    const daemon = NotebookHandle.load(frontend.save());
    syncHandles(frontend, daemon);

    // Frontend deletes " world"
    frontend.splice_source("c1", 5, 6, "");

    // The sync protocol may need multiple rounds to deliver changes
    // (first round can be a heads-only exchange on some platforms).
    // Collect events across up to 3 rounds of frame-based sync.
    // deno-lint-ignore no-explicit-any
    const allEvents: any[] = [];
    for (let round = 0; round < 3; round++) {
      const fwdMsg = frontend.flush_local_changes();
      if (fwdMsg) {
        const frame = new Uint8Array(1 + fwdMsg.length);
        frame[0] = 0x00;
        frame.set(fwdMsg, 1);
        const events = daemon.receive_frame(frame);
        if (events) allEvents.push(...events);
      }
      // Send daemon's reply back to frontend so protocol advances
      const replyMsg = daemon.flush_local_changes();
      if (replyMsg) {
        const replyFrame = new Uint8Array(1 + replyMsg.length);
        replyFrame[0] = 0x00;
        replyFrame.set(replyMsg, 1);
        frontend.receive_frame(replyFrame);
      }
      if (
        allEvents.some(
          // deno-lint-ignore no-explicit-any
          (e: any) => e.type === "sync_applied" && e.changed,
        )
      )
        break;
    }

    const syncEvent = allEvents.find(
      // deno-lint-ignore no-explicit-any
      (e: any) => e.type === "sync_applied" && e.changed,
    );
    if (!syncEvent) {
      console.error(
        "No sync_applied event with changed:true. All events:",
        JSON.stringify(allEvents, null, 2),
      );
    }
    assertExists(syncEvent);

    // deno-lint-ignore no-explicit-any
    const attr = syncEvent.attributions.find((a: any) => a.cell_id === "c1");
    assertExists(attr);
    assertEquals(attr.deleted, 6);
    assertEquals(attr.text, "");
    assertEquals(attr.index, 5);
    frontend.free();
    daemon.free();
  },
);

Deno.test(
  "attributions: multiple splices produce multiple attributions",
  () => {
    const frontend = makeHandle("attr-3", "c1");
    const daemon = NotebookHandle.load(frontend.save());
    syncHandles(frontend, daemon);

    // Type three characters
    frontend.splice_source("c1", 0, 0, "a");
    frontend.splice_source("c1", 1, 0, "b");
    frontend.splice_source("c1", 2, 0, "c");

    const events = syncViaFrame(frontend, daemon);
    assertExists(events);

    const syncEvent = events.find(
      // deno-lint-ignore no-explicit-any
      (e: any) => e.type === "sync_applied" && e.changed,
    );
    assertExists(syncEvent);
    assert(syncEvent.attributions.length > 0);

    // The attributions might be coalesced into one insert or kept separate
    // — either way, the daemon doc should have the full text.
    assertEquals(getSource(daemon, "c1"), "abc");
    frontend.free();
    daemon.free();
  },
);

Deno.test("non-source sync: metadata change does not alter source", () => {
  const frontend = makeHandle("attr-4", "c1", "hello");
  const daemon = NotebookHandle.load(frontend.save());
  syncHandles(frontend, daemon);

  // Daemon writes a non-source change
  daemon.add_uv_dependency("numpy");

  // Full sync — change arrives at frontend
  syncHandles(daemon, frontend);

  // Source must be unchanged on both sides
  assertEquals(getSource(frontend, "c1"), "hello");
  assertEquals(getSource(daemon, "c1"), "hello");
  frontend.free();
  daemon.free();
});

// ── 11. Mixed operations ─────────────────────────────────────────────

Deno.test("mixed: splice_source then update_source on same handle", () => {
  const h = makeHandle("mixed-1", "c1");

  // Start with splice
  h.splice_source("c1", 0, 0, "hello");
  assertEquals(getSource(h, "c1"), "hello");

  // Then full replace via update_source
  h.update_source("c1", "goodbye");
  assertEquals(getSource(h, "c1"), "goodbye");

  // Then splice again
  h.splice_source("c1", 7, 0, " world");
  assertEquals(getSource(h, "c1"), "goodbye world");
  h.free();
});

Deno.test("mixed: splice_source then append_source on same handle", () => {
  const h = makeHandle("mixed-2", "c1");

  h.splice_source("c1", 0, 0, "start");
  h.append_source("c1", " middle");
  h.splice_source("c1", 12, 0, " end");
  assertEquals(getSource(h, "c1"), "start middle end");
  h.free();
});

Deno.test("mixed: update_source then splice_source", () => {
  const h = makeHandle("mixed-3", "c1");

  h.update_source("c1", "import numpy as np");
  // Simulate cursor at position 7, user deletes "numpy" and types "pandas"
  h.splice_source("c1", 7, 5, "pandas");
  assertEquals(getSource(h, "c1"), "import pandas as np");

  // Fix "np" to "pd"
  h.splice_source("c1", 17, 2, "pd");
  assertEquals(getSource(h, "c1"), "import pandas as pd");
  h.free();
});

// ── 12. Large document stress test ───────────────────────────────────

Deno.test("stress: build a large document via splices then sync", () => {
  const h = makeHandle("stress-1", "c1");
  const b = NotebookHandle.load(h.save());
  syncHandles(h, b);

  // Build a 10KB document line by line via splices
  const lines: string[] = [];
  let pos = 0;
  for (let i = 0; i < 200; i++) {
    const line = `line ${i}: ${"x".repeat(40)}\n`;
    h.splice_source("c1", pos, 0, line);
    pos += line.length;
    lines.push(line);
  }

  const expected = lines.join("");
  assertEquals(getSource(h, "c1"), expected);

  // Sync the whole thing
  syncHandles(h, b);
  assertEquals(getSource(b, "c1"), expected);

  h.free();
  b.free();
});

Deno.test("stress: concurrent large edits from both sides", () => {
  const a = makeHandle("stress-2", "c1", "original content here");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  // A builds up content at the start
  for (let i = 0; i < 50; i++) {
    a.splice_source("c1", 0, 0, `# comment ${i}\n`);
  }

  // B builds up content at the end
  for (let i = 0; i < 50; i++) {
    const len = getSource(b, "c1").length;
    b.splice_source("c1", len, 0, `\n# footer ${i}`);
  }

  syncHandles(a, b);

  const sourceA = getSource(a, "c1");
  const sourceB = getSource(b, "c1");
  assertEquals(sourceA, sourceB);
  // Both sides' content should be present
  assert(sourceA.includes("# comment 0"));
  assert(sourceA.includes("# comment 49"));
  assert(sourceA.includes("# footer 0"));
  assert(sourceA.includes("# footer 49"));
  assert(sourceA.includes("original content here"));
  a.free();
  b.free();
});

// ── 13. Unicode / multi-byte character handling ──────────────────────

Deno.test("unicode: insert emoji", () => {
  const h = makeHandle("unicode-1", "c1", "hello world");
  // Note: emoji are multi-byte in UTF-8 but single chars in JS/Automerge
  h.splice_source("c1", 5, 0, " 🎉");
  assertEquals(getSource(h, "c1"), "hello 🎉 world");
  h.free();
});

Deno.test("unicode: CJK characters", () => {
  const h = makeHandle("unicode-2", "c1");
  h.splice_source("c1", 0, 0, "你好世界");
  assertEquals(getSource(h, "c1"), "你好世界");
  // Delete middle two characters
  h.splice_source("c1", 1, 2, "");
  assertEquals(getSource(h, "c1"), "你界");
  h.free();
});

Deno.test("unicode: mixed ASCII and multi-byte, sync between handles", () => {
  const a = makeHandle("unicode-3", "c1", "hello");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  a.splice_source("c1", 5, 0, " 世界 🌍");
  syncHandles(a, b);

  assertEquals(getSource(a, "c1"), "hello 世界 🌍");
  assertEquals(getSource(b, "c1"), "hello 世界 🌍");
  a.free();
  b.free();
});

Deno.test("unicode: accented characters and combining marks", () => {
  const h = makeHandle("unicode-4", "c1");
  // Precomposed é (U+00E9)
  h.splice_source("c1", 0, 0, "café");
  assertEquals(getSource(h, "c1"), "café");
  assertEquals(getSource(h, "c1").length, 4);
  h.free();
});

Deno.test("unicode: splice AFTER emoji uses UTF-16 positions", () => {
  // 🐸 is U+1F438 — a surrogate pair in UTF-16 (2 code units), but 1 Unicode code point.
  // In JavaScript, "🐸".length === 2.  If the WASM binding uses code-point indexing
  // instead of UTF-16 indexing, splicing after the emoji lands at the wrong position.
  const h = makeHandle("unicode-5", "c1", "🐸 hello");
  // JS string: 🐸(2) + space(1) + hello(5) = length 8
  assertEquals("🐸 hello".length, 8);
  // Insert "!" right after the space (UTF-16 index 3: 2 for 🐸 + 1 for space)
  h.splice_source("c1", 3, 0, "!");
  assertEquals(getSource(h, "c1"), "🐸 !hello");
  h.free();
});

Deno.test("unicode: delete after emoji uses UTF-16 positions", () => {
  const h = makeHandle("unicode-6", "c1", "🐸ab");
  // UTF-16 positions: 🐸 = [0,1], a = 2, b = 3 → length 4
  assertEquals("🐸ab".length, 4);
  // Delete 'a' (UTF-16 index 2, delete 1)
  h.splice_source("c1", 2, 1, "");
  assertEquals(getSource(h, "c1"), "🐸b");
  h.free();
});

Deno.test("unicode: splice between two emoji", () => {
  const h = makeHandle("unicode-7", "c1", "🐸🔥");
  // UTF-16: 🐸=[0,1] 🔥=[2,3] → length 4
  assertEquals("🐸🔥".length, 4);
  // Insert between the two emoji (UTF-16 index 2)
  h.splice_source("c1", 2, 0, " and ");
  assertEquals(getSource(h, "c1"), "🐸 and 🔥");
  h.free();
});

Deno.test("unicode: replace text after multiple emoji", () => {
  const h = makeHandle("unicode-8", "c1", "🐸🔥⚡ test");
  // UTF-16: 🐸(2) + 🔥(2) + ⚡(1, BMP) + space(1) + test(4) = 10
  assertEquals("🐸🔥⚡ test".length, 10);
  // Replace "test" (starts at UTF-16 index 6, length 4) with "done"
  h.splice_source("c1", 6, 4, "done");
  assertEquals(getSource(h, "c1"), "🐸🔥⚡ done");
  h.free();
});

Deno.test("unicode: sync splice-after-emoji between two handles", () => {
  const a = makeHandle("unicode-9", "c1", "# 🐸 Hello");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  // UTF-16: #(1) + space(1) + 🐸(2) + space(1) + Hello(5) = 10
  assertEquals("# 🐸 Hello".length, 10);

  // Peer A: insert " World" after "Hello" (UTF-16 index 10)
  a.splice_source("c1", 10, 0, " World");
  syncHandles(a, b);
  assertEquals(getSource(a, "c1"), "# 🐸 Hello World");
  assertEquals(getSource(b, "c1"), "# 🐸 Hello World");

  // Peer B: insert "!" after "World" (UTF-16 index 16)
  b.splice_source("c1", 16, 0, "!");
  syncHandles(b, a);
  assertEquals(getSource(a, "c1"), "# 🐸 Hello World!");
  assertEquals(getSource(b, "c1"), "# 🐸 Hello World!");

  a.free();
  b.free();
});

// ── 14. Newline handling ─────────────────────────────────────────────

Deno.test("newlines: insert newline between lines", () => {
  const h = makeHandle("newline-1", "c1", "line1\nline2");
  h.splice_source("c1", 5, 0, "\nnew_line");
  assertEquals(getSource(h, "c1"), "line1\nnew_line\nline2");
  h.free();
});

Deno.test("newlines: delete a newline (joining lines)", () => {
  const h = makeHandle("newline-2", "c1", "line1\nline2");
  h.splice_source("c1", 5, 1, "");
  assertEquals(getSource(h, "c1"), "line1line2");
  h.free();
});

Deno.test("newlines: replace newline with space", () => {
  const h = makeHandle("newline-3", "c1", "a\nb");
  h.splice_source("c1", 1, 1, " ");
  assertEquals(getSource(h, "c1"), "a b");
  h.free();
});

Deno.test("newlines: CRLF handling", () => {
  const h = makeHandle("newline-4", "c1");
  h.splice_source("c1", 0, 0, "line1\r\nline2");
  assertEquals(getSource(h, "c1"), "line1\r\nline2");
  // Delete the \r\n
  h.splice_source("c1", 5, 2, "");
  assertEquals(getSource(h, "c1"), "line1line2");
  h.free();
});

Deno.test("newlines: trailing newline preserved through sync", () => {
  const a = makeHandle("newline-5", "c1");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  a.splice_source("c1", 0, 0, "code()\n");
  syncHandles(a, b);
  assertEquals(getSource(b, "c1"), "code()\n");
  a.free();
  b.free();
});

// ── 15. Delete-all then retype (re-execution pattern) ────────────────

Deno.test("retype: clear cell and rewrite completely", () => {
  const h = makeHandle("retype-1", "c1", "import os\nos.getcwd()");
  const oldLen = getSource(h, "c1").length;

  // Select all → delete
  h.splice_source("c1", 0, oldLen, "");
  assertEquals(getSource(h, "c1"), "");

  // Retype
  const newCode = "import sys\nprint(sys.version)";
  for (let i = 0; i < newCode.length; i++) {
    h.splice_source("c1", i, 0, newCode[i]);
  }
  assertEquals(getSource(h, "c1"), newCode);
  h.free();
});

Deno.test("retype: clear and retype syncs correctly", () => {
  const a = makeHandle("retype-2", "c1", "old code");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  // Clear on A
  a.splice_source("c1", 0, 8, "");
  syncHandles(a, b);
  assertEquals(getSource(b, "c1"), "");

  // Retype on A
  a.splice_source("c1", 0, 0, "new code");
  syncHandles(a, b);
  assertEquals(getSource(b, "c1"), "new code");
  a.free();
  b.free();
});

Deno.test("retype: clear on A while B is typing (concurrent)", () => {
  const a = makeHandle("retype-3", "c1", "shared base");
  const b = NotebookHandle.load(a.save());
  syncHandles(a, b);

  // A clears everything
  a.splice_source("c1", 0, getSource(a, "c1").length, "");
  // B appends (hasn't seen the clear yet)
  b.splice_source("c1", getSource(b, "c1").length, 0, " extra");

  syncHandles(a, b);

  const sourceA = getSource(a, "c1");
  const sourceB = getSource(b, "c1");
  assertEquals(sourceA, sourceB);
  // B's " extra" should survive since it was a concurrent insert.
  // The delete of "shared base" and the insert of " extra" are
  // independent operations in the CRDT.
  assert(
    sourceA.includes("extra"),
    `Expected "extra" to survive concurrent clear, got "${sourceA}"`,
  );
  a.free();
  b.free();
});

// ── 16. Equivalence: splice_source vs update_source ──────────────────
// Verify that building a string via splices produces the same CRDT result
// as setting it via update_source, so consumers don't see differences.

Deno.test("equivalence: splice-built source matches update_source", () => {
  const target = "import time\ntime.sleep(1)\nprint('done')";

  // Build via splice (character by character)
  const spliced = makeHandle("equiv-1a", "c1");
  for (let i = 0; i < target.length; i++) {
    spliced.splice_source("c1", i, 0, target[i]);
  }

  // Build via update_source (full replacement)
  const updated = makeHandle("equiv-1b", "c1");
  updated.update_source("c1", target);

  assertEquals(getSource(spliced, "c1"), target);
  assertEquals(getSource(updated, "c1"), target);

  // Both should produce identical cell snapshots
  const snapA = JSON.parse(spliced.get_cells_json());
  const snapB = JSON.parse(updated.get_cells_json());
  assertEquals(snapA[0].source, snapB[0].source);
  assertEquals(snapA[0].source, target);

  spliced.free();
  updated.free();
});

// ── 17. Multiple cells ───────────────────────────────────────────────

Deno.test("multi-cell: splice_source on different cells independently", () => {
  const h = new NotebookHandle("multi-1");
  h.add_cell(0, "c1", "code");
  h.add_cell(1, "c2", "code");
  h.add_cell(2, "c3", "markdown");

  h.splice_source("c1", 0, 0, "cell one");
  h.splice_source("c2", 0, 0, "cell two");
  h.splice_source("c3", 0, 0, "# cell three");

  assertEquals(getSource(h, "c1"), "cell one");
  assertEquals(getSource(h, "c2"), "cell two");
  assertEquals(getSource(h, "c3"), "# cell three");

  // Modify c2 without affecting others
  h.splice_source("c2", 5, 3, "2");
  assertEquals(getSource(h, "c1"), "cell one");
  assertEquals(getSource(h, "c2"), "cell 2");
  assertEquals(getSource(h, "c3"), "# cell three");
  h.free();
});

Deno.test("multi-cell: splice on deleted cell returns false", () => {
  const h = new NotebookHandle("multi-2");
  h.add_cell(0, "c1", "code");
  h.splice_source("c1", 0, 0, "hello");
  h.delete_cell("c1");
  const ok = h.splice_source("c1", 0, 0, "should fail");
  assertEquals(ok, false);
  h.free();
});

// ── 18. Save / load round-trip ───────────────────────────────────────

Deno.test("persistence: splice_source survives save/load round-trip", () => {
  const h = makeHandle("persist-1", "c1");
  h.splice_source("c1", 0, 0, "import ");
  h.splice_source("c1", 7, 0, "numpy ");
  h.splice_source("c1", 13, 0, "as np");
  assertEquals(getSource(h, "c1"), "import numpy as np");

  const bytes = h.save();
  const loaded = NotebookHandle.load(bytes);
  assertEquals(getSource(loaded, "c1"), "import numpy as np");

  // Further splices on loaded handle work
  loaded.splice_source("c1", 18, 0, "\nnp.array([1,2,3])");
  assertEquals(
    getSource(loaded, "c1"),
    "import numpy as np\nnp.array([1,2,3])",
  );

  h.free();
  loaded.free();
});

// ── 19. Changeset correctness ────────────────────────────────────────

Deno.test("changeset: splice_source flags source as changed", () => {
  const daemon = makeHandle("cs-1", "c1", "hello");
  const frontend = NotebookHandle.load(daemon.save());
  syncHandles(daemon, frontend);

  // Frontend splices
  frontend.splice_source("c1", 5, 0, " world");

  // Sync via frame to check changeset
  const events = syncViaFrame(frontend, daemon);
  assertExists(events);

  const syncEvent = events.find(
    // deno-lint-ignore no-explicit-any
    (e: any) => e.type === "sync_applied" && e.changed,
  );
  assertExists(syncEvent);
  assertExists(syncEvent.changeset);

  // The changeset should flag source as changed for c1
  // deno-lint-ignore no-explicit-any
  const changedCell = syncEvent.changeset.changed.find(
    // deno-lint-ignore no-explicit-any
    (c: any) => c.cell_id === "c1",
  );
  assertExists(changedCell);
  assertEquals(changedCell.fields.source, true);
  daemon.free();
  frontend.free();
});

Deno.test("changeset: splice_source does NOT flag outputs", () => {
  const daemon = makeHandle("cs-2", "c1", "hello");
  const frontend = NotebookHandle.load(daemon.save());
  syncHandles(daemon, frontend);

  frontend.splice_source("c1", 5, 0, "!");

  const events = syncViaFrame(frontend, daemon);
  assertExists(events);

  const syncEvent = events.find(
    // deno-lint-ignore no-explicit-any
    (e: any) => e.type === "sync_applied" && e.changed,
  );
  assertExists(syncEvent);

  // deno-lint-ignore no-explicit-any
  const changedCell = syncEvent.changeset.changed.find(
    // deno-lint-ignore no-explicit-any
    (c: any) => c.cell_id === "c1",
  );
  assertExists(changedCell);
  assertEquals(changedCell.fields.source, true);
  // ChangedFields serialization omits false values — absent means unchanged
  assertEquals(changedCell.fields.outputs, undefined);
  assertEquals(changedCell.fields.execution_count, undefined);
  daemon.free();
  frontend.free();
});

// ── 20. The exact reproduction case ──────────────────────────────────
// Simulate the scenario that caused the text corruption bug:
// User types while daemon sync triggers materialization.

Deno.test("regression: typing 'import time' survives concurrent sync", () => {
  // Frontend types code. Daemon makes non-source changes concurrently.
  // After sync, the frontend's source should be intact.
  const frontend = makeHandle("regression-1", "c1");
  const daemon = NotebookHandle.load(frontend.save());
  syncHandles(frontend, daemon);

  const text = "import time\ntime.sleep(0)";

  // Simulate typing character by character with periodic syncs
  for (let i = 0; i < text.length; i++) {
    frontend.splice_source("c1", i, 0, text[i]);

    // Every 5 characters, sync (simulating debounced sync)
    if (i % 5 === 4) {
      syncHandles(frontend, daemon);
      // Daemon might write metadata (simulating non-source CRDT changes
      // that would trigger materialization in the real app)
      if (i === 9) {
        daemon.add_uv_dependency("requests");
      }
    }
  }

  // Final sync
  syncHandles(frontend, daemon);

  // Source must be exactly what was typed — no corruption
  assertEquals(getSource(frontend, "c1"), text);
  assertEquals(getSource(daemon, "c1"), text);

  frontend.free();
  daemon.free();
});

Deno.test(
  "regression: rapid typing with concurrent non-source changes doesn't corrupt",
  () => {
    const frontend = makeHandle("regression-2", "c1");
    const daemon = NotebookHandle.load(frontend.save());
    syncHandles(frontend, daemon);

    // Type the first line
    const line1 = "import time";
    for (let i = 0; i < line1.length; i++) {
      frontend.splice_source("c1", i, 0, line1[i]);
    }

    // Sync
    syncHandles(frontend, daemon);

    // Daemon writes a non-source change (simulating an output broadcast
    // that would trigger materialization in the real app)
    daemon.add_uv_dependency("pandas");

    // Sync — daemon's metadata change arrives at frontend
    syncHandles(frontend, daemon);

    // Frontend continues typing (this is the critical moment — in the old code,
    // the materialization of a non-source change could trigger a stale value prop)
    const line2 = "\ntime.sleep(0)";
    const offset = getSource(frontend, "c1").length;
    for (let i = 0; i < line2.length; i++) {
      frontend.splice_source("c1", offset + i, 0, line2[i]);
    }

    syncHandles(frontend, daemon);

    const expected = "import time\ntime.sleep(0)";
    assertEquals(getSource(frontend, "c1"), expected);
    assertEquals(getSource(daemon, "c1"), expected);

    frontend.free();
    daemon.free();
  },
);
