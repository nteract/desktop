/**
 * WASM integration tests — real Automerge frames through the full pipeline.
 *
 * These tests use two real WASM NotebookHandles (server + client) connected
 * via DirectTransport, with the client wired through SyncEngine. Every frame
 * is a real Automerge sync message — no mocked FrameEvents.
 *
 * This catches bugs that mock-based tests miss:
 * - WASM demux producing unexpected changeset shapes
 * - Sync state machine getting stuck (handshake never completing)
 * - Changeset field flags not matching actual document mutations
 * - Coalescing merging changesets incorrectly for real frame sequences
 * - Concurrent edit conflicts between server and client
 *
 * NOTE: Output-writing tests (append_output) are not possible here because
 * the WASM handle doesn't expose output mutation APIs — those are daemon-only.
 * Output materialization is tested via the mock-based frame-pipeline tests.
 */

import { firstValueFrom, timeout } from "rxjs";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import type { CellChangeset } from "../src/cell-changeset";
import { type WasmHarness, createWasmHarness, initWasm } from "./wasm-harness";

// ── Test suite ──────────────────────────────────────────────────────

describe("WASM integration: real frames through SyncEngine", () => {
  let h: WasmHarness;

  beforeEach(async () => {
    h = await createWasmHarness();
  });

  afterEach(() => {
    h.dispose();
  });

  // ── Initial sync handshake ──────────────────────────────────────

  describe("initial sync handshake", () => {
    it("completes initial sync when server has content", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "print('hello')");

      // startAndCompleteSync handles the full multi-round handshake
      await h.startAndCompleteSync();
    });

    it("completes initial sync for empty notebook", async () => {
      // Even an empty notebook (just metadata) should complete sync
      await h.startAndCompleteSync();
    });

    it("client can read cells after initial sync", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "x = 42");
      h.serverAddCell("cell-2", "markdown");
      h.serverUpdateSource("cell-2", "# Title");

      await h.startAndCompleteSync();

      expect(h.client.cell_count()).toBe(2);
      expect(h.client.get_cell_source("cell-1")).toBe("x = 42");
      expect(h.client.get_cell_source("cell-2")).toBe("# Title");
    });

    it("client sees cell types correctly", async () => {
      h.serverAddCell("code-cell", "code");
      h.serverAddCell("md-cell", "markdown");

      await h.startAndCompleteSync();

      expect(h.client.get_cell_type("code-cell")).toBe("code");
      expect(h.client.get_cell_type("md-cell")).toBe("markdown");
    });

    it("client sees execution count after sync", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverSetExecutionCount("cell-1", "5");

      await h.startAndCompleteSync();

      expect(h.client.get_cell_execution_count("cell-1")).toBe("5");
    });
  });

  // ── Cell changes via real sync frames ───────────────────────────

  describe("cell changes from sync frames", () => {
    it("emits changeset when server updates cell source", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "original");
      await h.startAndCompleteSync();

      const changesetPromise = firstValueFrom(
        h.engine.cellChanges$.pipe(timeout(1000)),
      );

      h.serverUpdateSource("cell-1", "modified");
      h.pushAndFlush();

      const cs = await changesetPromise;
      expect(cs).not.toBeNull();
      expect(cs!.changed.length).toBeGreaterThanOrEqual(1);

      const cell1Change = cs!.changed.find((c) => c.cell_id === "cell-1");
      expect(cell1Change).toBeDefined();
      expect(cell1Change!.fields.source).toBe(true);
    });

    it("emits changeset with execution_count flag when server sets it", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "1 + 1");
      await h.startAndCompleteSync();

      const changesetPromise = firstValueFrom(
        h.engine.cellChanges$.pipe(timeout(1000)),
      );

      h.serverSetExecutionCount("cell-1", "1");
      h.pushAndFlush();

      const cs = await changesetPromise;
      expect(cs).not.toBeNull();

      const cell1Change = cs!.changed.find((c) => c.cell_id === "cell-1");
      expect(cell1Change).toBeDefined();
      expect(cell1Change!.fields.execution_count).toBe(true);
    });

    it("reports structural changes when server adds a new cell", async () => {
      h.serverAddCell("cell-1", "code");
      await h.startAndCompleteSync();

      const changesetPromise = firstValueFrom(
        h.engine.cellChanges$.pipe(timeout(1000)),
      );

      h.serverAddCell("cell-2", "markdown");
      h.pushAndFlush();

      const cs = await changesetPromise;
      // Structural change: either null (full materialization) or has added[]
      if (cs !== null) {
        expect(cs.added.length + (cs.order_changed ? 1 : 0)).toBeGreaterThan(
          0,
        );
      }
      expect(h.client.cell_count()).toBe(2);
    });

    it("reports structural changes when server deletes a cell", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverAddCell("cell-2", "code");
      await h.startAndCompleteSync();

      const changesetPromise = firstValueFrom(
        h.engine.cellChanges$.pipe(timeout(1000)),
      );

      h.server.delete_cell("cell-2");
      h.pushAndFlush();

      const cs = await changesetPromise;
      if (cs !== null) {
        expect(cs.removed).toContain("cell-2");
      }
      expect(h.client.cell_count()).toBe(1);
    });

    it("source-only change does NOT set outputs flag", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "original");
      await h.startAndCompleteSync();

      const changesetPromise = firstValueFrom(
        h.engine.cellChanges$.pipe(timeout(1000)),
      );

      h.serverUpdateSource("cell-1", "changed");
      h.pushAndFlush();

      const cs = await changesetPromise;
      expect(cs).not.toBeNull();
      const cell1Change = cs!.changed.find((c) => c.cell_id === "cell-1");
      expect(cell1Change).toBeDefined();
      expect(cell1Change!.fields.source).toBe(true);
      // Output flag should NOT be set for a source-only change
      expect(cell1Change!.fields.outputs).toBeUndefined();
    });
  });

  // ── Coalescing window ──────────────────────────────────────────

  describe("coalescing", () => {
    it("coalesces rapid source changes within 32ms window", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "v0");
      await h.startAndCompleteSync();

      const emissions: (CellChangeset | null)[] = [];
      const sub = h.engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      // Rapid-fire source changes — all within 32ms
      h.serverUpdateSource("cell-1", "v1");
      h.pushToClient();
      h.serverUpdateSource("cell-1", "v2");
      h.pushToClient();
      h.serverUpdateSource("cell-1", "v3");
      h.pushToClient();

      // Advance past coalescing window
      h.advanceBy(50);

      // Should be coalesced into a single emission
      expect(emissions.length).toBe(1);
      expect(h.client.get_cell_source("cell-1")).toBe("v3");

      sub.unsubscribe();
    });

    it("coalesces changes to different cells within same window", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverAddCell("cell-2", "code");
      h.serverUpdateSource("cell-1", "a");
      h.serverUpdateSource("cell-2", "b");
      await h.startAndCompleteSync();

      const emissions: (CellChangeset | null)[] = [];
      const sub = h.engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      h.serverUpdateSource("cell-1", "a-modified");
      h.pushToClient();
      h.serverUpdateSource("cell-2", "b-modified");
      h.pushToClient();

      h.advanceBy(50);

      // Single coalesced emission covering both cells
      expect(emissions.length).toBe(1);
      if (emissions[0] !== null) {
        const changedIds = emissions[0].changed.map((c) => c.cell_id).sort();
        expect(changedIds).toContain("cell-1");
        expect(changedIds).toContain("cell-2");
      }

      sub.unsubscribe();
    });

    it("emits separate batches for changes separated by coalescing windows", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "start");
      await h.startAndCompleteSync();

      const emissions: (CellChangeset | null)[] = [];
      const sub = h.engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      // First batch
      h.serverUpdateSource("cell-1", "batch1");
      h.pushToClient();
      h.advanceBy(50);

      // Second batch (after coalescing window)
      h.serverUpdateSource("cell-1", "batch2");
      h.pushToClient();
      h.advanceBy(50);

      expect(emissions.length).toBe(2);
      sub.unsubscribe();
    });

    it("merged changeset has correct field union for multi-field changes", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "start");
      await h.startAndCompleteSync();

      const emissions: (CellChangeset | null)[] = [];
      const sub = h.engine.cellChanges$.subscribe((cs) => emissions.push(cs));

      // Change source in one frame
      h.serverUpdateSource("cell-1", "new source");
      h.pushToClient();
      // Change execution count in another frame within same window
      h.serverSetExecutionCount("cell-1", "3");
      h.pushToClient();

      h.advanceBy(50);

      expect(emissions.length).toBe(1);
      if (emissions[0] !== null) {
        const cell1Change = emissions[0].changed.find(
          (c) => c.cell_id === "cell-1",
        );
        expect(cell1Change).toBeDefined();
        expect(cell1Change!.fields.source).toBe(true);
        expect(cell1Change!.fields.execution_count).toBe(true);
      }

      sub.unsubscribe();
    });
  });

  // ── Concurrent edits (client + server) ──────────────────────────

  describe("concurrent edits", () => {
    it("client and server edits to different cells converge", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverAddCell("cell-2", "code");
      h.serverUpdateSource("cell-1", "server-orig");
      h.serverUpdateSource("cell-2", "server-orig");
      await h.startAndCompleteSync();

      // Client edits cell-1 locally
      h.client.update_source("cell-1", "client-edit");

      // Server edits cell-2
      h.serverUpdateSource("cell-2", "server-edit");

      // Sync both ways — flush client, push server, repeat
      h.engine.flush();
      h.syncUntilConverged();
      h.advanceBy(50);

      // Both should converge
      expect(h.client.get_cell_source("cell-1")).toBe("client-edit");
      expect(h.client.get_cell_source("cell-2")).toBe("server-edit");
      expect(h.server.get_cell_source("cell-1")).toBe("client-edit");
      expect(h.server.get_cell_source("cell-2")).toBe("server-edit");
    });

    it("concurrent edits to same cell merge via Automerge text CRDT", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "hello world");
      await h.startAndCompleteSync();

      // Client appends to the source
      h.client.splice_source("cell-1", 11, 0, " from client");

      // Server also appends (before seeing client's edit)
      h.server.splice_source("cell-1", 11, 0, " from server");

      // Sync both directions
      h.engine.flush();
      h.syncUntilConverged();
      h.advanceBy(50);

      // Both should have merged — exact merge depends on Automerge's
      // text CRDT, but both strings should be present
      const clientSource = h.client.get_cell_source("cell-1");
      const serverSource = h.server.get_cell_source("cell-1");

      expect(clientSource).toBe(serverSource);
      expect(clientSource).toContain("from client");
      expect(clientSource).toContain("from server");
    });

    it("client adds cell while server adds cell — both survive", async () => {
      h.serverAddCell("cell-1", "code");
      await h.startAndCompleteSync();

      // Both sides add cells concurrently
      h.client.add_cell(1, "client-cell", "markdown");
      h.client.update_source("client-cell", "# From client");

      h.server.add_cell(1, "server-cell", "code");
      h.server.update_source("server-cell", "# From server");

      h.engine.flush();
      h.syncUntilConverged();
      h.advanceBy(50);

      // Both cells should exist on both sides
      expect(h.client.cell_count()).toBe(3);
      expect(h.server.cell_count()).toBe(3);
      expect(h.client.get_cell_source("client-cell")).toBe("# From client");
      expect(h.client.get_cell_source("server-cell")).toBe("# From server");
    });

    it("concurrent delete + edit: edit wins on non-deleted cell", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverAddCell("cell-2", "code");
      h.serverUpdateSource("cell-1", "keep me");
      h.serverUpdateSource("cell-2", "delete me");
      await h.startAndCompleteSync();

      // Client edits cell-1
      h.client.update_source("cell-1", "client changed this");

      // Server deletes cell-2
      h.server.delete_cell("cell-2");

      h.engine.flush();
      h.syncUntilConverged();
      h.advanceBy(50);

      expect(h.client.cell_count()).toBe(1);
      expect(h.server.cell_count()).toBe(1);
      expect(h.client.get_cell_source("cell-1")).toBe("client changed this");
      expect(h.server.get_cell_source("cell-1")).toBe("client changed this");
    });
  });

  // ���─ Broadcast routing ──────────────────────────────────────────

  describe("broadcast routing", () => {
    it("routes broadcast frames to broadcasts$ observable", async () => {
      h.engine.start();

      const broadcastPromise = firstValueFrom(
        h.engine.broadcasts$.pipe(timeout(1000)),
      );

      h.transport.pushBroadcast({
        type: "kernel_status",
        status: "busy",
      });

      const payload = await broadcastPromise;
      expect(payload).toEqual({
        type: "kernel_status",
        status: "busy",
      });
    });

    it("routes multiple broadcasts in order", () => {
      h.engine.start();

      const payloads: unknown[] = [];
      const sub = h.engine.broadcasts$.subscribe((p) => payloads.push(p));

      h.transport.pushBroadcast({ seq: 1 });
      h.transport.pushBroadcast({ seq: 2 });
      h.transport.pushBroadcast({ seq: 3 });

      expect(payloads).toEqual([{ seq: 1 }, { seq: 2 }, { seq: 3 }]);
      sub.unsubscribe();
    });

    it("broadcasts and sync frames can interleave", async () => {
      h.serverAddCell("cell-1", "code");
      await h.startAndCompleteSync();

      const broadcasts: unknown[] = [];
      const changesets: (CellChangeset | null)[] = [];
      const bSub = h.engine.broadcasts$.subscribe((p) => broadcasts.push(p));
      const cSub = h.engine.cellChanges$.subscribe((cs) =>
        changesets.push(cs),
      );

      // Interleave: broadcast, sync, broadcast
      h.transport.pushBroadcast({ type: "execution_started" });
      h.serverUpdateSource("cell-1", "executed");
      h.pushToClient();
      h.transport.pushBroadcast({ type: "execution_done" });

      h.advanceBy(50);

      // 2 explicit broadcasts + possible text_attribution from sync frame
      expect(broadcasts.length).toBeGreaterThanOrEqual(2);
      expect(changesets.length).toBeGreaterThan(0);

      bSub.unsubscribe();
      cSub.unsubscribe();
    });
  });

  // ── Metadata sync ──────────────────────────────────────────────

  describe("metadata sync", () => {
    it("server metadata changes sync to client", async () => {
      await h.startAndCompleteSync();

      h.server.set_metadata("runtime", "deno");
      h.pushAndFlush();

      expect(h.client.get_metadata("runtime")).toBe("deno");
    });

    it("client metadata changes sync to server", async () => {
      await h.startAndCompleteSync();

      h.client.set_metadata("runtime", "deno");
      // Flush client changes via engine
      h.engine.flush();
      h.syncUntilConverged();

      expect(h.server.get_metadata("runtime")).toBe("deno");
    });

    it("concurrent metadata changes merge", async () => {
      await h.startAndCompleteSync();

      // Both sides set different metadata keys
      h.client.set_metadata("custom_a", "from-client");
      h.server.set_metadata("custom_b", "from-server");

      h.engine.flush();
      h.syncUntilConverged();

      expect(h.client.get_metadata("custom_a")).toBe("from-client");
      expect(h.client.get_metadata("custom_b")).toBe("from-server");
      expect(h.server.get_metadata("custom_a")).toBe("from-client");
      expect(h.server.get_metadata("custom_b")).toBe("from-server");
    });
  });

  // ── Sync reply atomicity ────────────────────────��─────────────

  describe("sync reply behavior", () => {
    it("client sends sync replies back via transport", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "hello");

      h.engine.start();
      h.engine.flush();
      h.pushToClient();

      // The engine should have sent sync reply frames back
      const syncReplies = h.transport.sentFrames.filter(
        (f) => f.frameType === 0x00,
      );
      expect(syncReplies.length).toBeGreaterThan(0);
    });

    it("transport failure triggers cancel_last_flush recovery", async () => {
      h.serverAddCell("cell-1", "code");
      await h.startAndCompleteSync();

      // Clear tracked frames
      h.transport.clearSentFrames();

      // Enable failure mode
      h.transport.simulateFailure = true;

      // Server makes a change — the inline sync reply will fail
      h.serverUpdateSource("cell-1", "after-failure");
      h.pushToClient();

      expect(h.transport.sendFailureCount).toBeGreaterThan(0);

      // Disable failure mode and resync
      h.transport.simulateFailure = false;
      h.engine.resetAndResync();
      h.syncUntilConverged();
      h.advanceBy(50);

      // Client should eventually get the content
      expect(h.client.get_cell_source("cell-1")).toBe("after-failure");
    });
  });

  // ── Cell ordering and structural operations ────────────────────

  describe("cell ordering", () => {
    it("cell ordering is preserved after sync", async () => {
      h.serverAddCell("a", "code");
      h.serverAddCell("b", "markdown");
      h.serverAddCell("c", "code");
      await h.startAndCompleteSync();

      const clientIds = h.client.get_cell_ids() as string[];
      const serverIds = h.server.get_cell_ids() as string[];
      expect(clientIds).toEqual(serverIds);
    });

    it("cell move changes ordering on client", async () => {
      h.serverAddCell("a", "code");
      h.serverAddCell("b", "code");
      h.serverAddCell("c", "code");
      await h.startAndCompleteSync();

      const originalIds = h.client.get_cell_ids() as string[];
      expect(originalIds).toEqual(["a", "b", "c"]);

      // Move "c" to the front (after nothing = first position)
      h.server.move_cell("c");
      h.pushAndFlush();

      const newIds = h.client.get_cell_ids() as string[];
      expect(newIds[0]).toBe("c");
      expect(newIds.length).toBe(3);
    });

    it("cell add + delete + add cycle maintains consistency", async () => {
      await h.startAndCompleteSync();

      // Add a cell
      h.serverAddCell("ephemeral", "code");
      h.serverUpdateSource("ephemeral", "temporary");
      h.pushAndFlush();
      expect(h.client.cell_count()).toBe(1);

      // Delete it
      h.server.delete_cell("ephemeral");
      h.pushAndFlush();
      expect(h.client.cell_count()).toBe(0);

      // Add a new one with different content
      h.serverAddCell("permanent", "code");
      h.serverUpdateSource("permanent", "keeper");
      h.pushAndFlush();
      expect(h.client.cell_count()).toBe(1);
      expect(h.client.get_cell_source("permanent")).toBe("keeper");
    });

    it("insert between existing cells works", async () => {
      h.serverAddCell("first", "code");
      h.serverAddCell("last", "code");
      await h.startAndCompleteSync();

      // Insert "middle" after "first" (at index 1)
      h.server.add_cell(1, "middle", "code");
      h.pushAndFlush();

      const ids = h.client.get_cell_ids() as string[];
      expect(ids).toEqual(["first", "middle", "last"]);
    });
  });

  // ── Stress scenarios ──────────────────────────────────────────

  describe("stress scenarios", () => {
    it(
      "handles notebook with many cells",
      async () => {
        for (let i = 0; i < 50; i++) {
          h.serverAddCell(`cell-${i}`, i % 3 === 0 ? "markdown" : "code");
          h.serverUpdateSource(`cell-${i}`, `content of cell ${i}`);
        }

        await h.startAndCompleteSync();

        expect(h.client.cell_count()).toBe(50);
        expect(h.client.get_cell_source("cell-0")).toBe("content of cell 0");
        expect(h.client.get_cell_source("cell-49")).toBe("content of cell 49");
      },
      15000,
    );

    it("rapid source edits all arrive correctly", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "v0");
      await h.startAndCompleteSync();

      // 20 rapid source changes
      for (let i = 1; i <= 20; i++) {
        h.serverUpdateSource("cell-1", `v${i}`);
        h.pushToClient();
      }
      h.advanceBy(50);

      expect(h.client.get_cell_source("cell-1")).toBe("v20");
    });

    it("many concurrent cell additions from both sides", async () => {
      await h.startAndCompleteSync();

      // Client adds 5 cells, server adds 5 cells, concurrently
      for (let i = 0; i < 5; i++) {
        h.client.add_cell(i, `client-${i}`, "code");
        h.client.update_source(`client-${i}`, `from client ${i}`);
        h.server.add_cell(i, `server-${i}`, "code");
        h.server.update_source(`server-${i}`, `from server ${i}`);
      }

      h.engine.flush();
      h.syncUntilConverged();
      h.advanceBy(50);

      // All 10 cells should exist
      expect(h.client.cell_count()).toBe(10);
      expect(h.server.cell_count()).toBe(10);

      // Verify all content is present
      for (let i = 0; i < 5; i++) {
        expect(h.client.get_cell_source(`client-${i}`)).toBe(
          `from client ${i}`,
        );
        expect(h.client.get_cell_source(`server-${i}`)).toBe(
          `from server ${i}`,
        );
      }
    });
  });

  // ── Document save/load round-trip ──────────────────────────────

  describe("save/load", () => {
    it("client document round-trips through save/load", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "x = 42");
      h.serverSetExecutionCount("cell-1", "1");
      h.serverAddCell("cell-2", "markdown");
      h.serverUpdateSource("cell-2", "# Results");

      await h.startAndCompleteSync();

      // Save client doc
      const saved = h.client.save();
      expect(saved.length).toBeGreaterThan(0);

      // Load into a new handle
      const Handle = await initWasm();
      const loaded = Handle.load(saved);

      expect(loaded.cell_count()).toBe(2);
      expect(loaded.get_cell_source("cell-1")).toBe("x = 42");
      expect(loaded.get_cell_execution_count("cell-1")).toBe("1");
      expect(loaded.get_cell_source("cell-2")).toBe("# Results");
      expect(loaded.get_cell_type("cell-2")).toBe("markdown");

      loaded.free();
    });

    it("saved doc from client can be loaded by a new client and synced", async () => {
      h.serverAddCell("cell-1", "code");
      h.serverUpdateSource("cell-1", "original");
      await h.startAndCompleteSync();

      const saved = h.client.save();
      const Handle = await initWasm();
      const loaded = Handle.load(saved);

      // The loaded doc should be a valid starting point
      expect(loaded.cell_count()).toBe(1);
      expect(loaded.get_cell_source("cell-1")).toBe("original");

      loaded.free();
    });
  });
});
