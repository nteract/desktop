/**
 * Tests for WidgetUpdateManager — CRDT-first widget state persistence.
 *
 * Post-A2 semantics: no optimistic store update, no echo suppression.
 * Every non-buffer update goes through the injected CRDT writer,
 * throttled at 50ms per comm so continuous slider drags don't flood
 * the CRDT. First tick in a burst fires immediately (instant UI via
 * the projectLocalState → commChanges$ emission); ticks within the
 * throttle window coalesce into a single trailing flush.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { createWidgetStore } from "../widget-store";
import { WidgetUpdateManager } from "../widget-update-manager";

// ── Helpers ──────────────────────────────────────────────────────────

function setup(opts?: { writerAvailable?: boolean }) {
  const store = createWidgetStore();
  const writerCalls: Array<{ commId: string; patch: Record<string, unknown> }> = [];
  const writer = (commId: string, patch: Record<string, unknown>) => {
    writerCalls.push({ commId, patch });
  };

  const writerAvailable = opts?.writerAvailable ?? true;
  const manager = new WidgetUpdateManager({
    getStore: () => store,
    getCrdtWriter: () => (writerAvailable ? writer : null),
  });

  store.createModel("comm-1", { value: 0, description: "test" });

  return { store, manager, writerCalls };
}

// ── Tests ────────────────────────────────────────────────────────────

describe("WidgetUpdateManager", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  describe("throttling", () => {
    it("fires the first tick of a burst immediately", () => {
      const { manager, writerCalls } = setup();

      manager.updateAndPersist("comm-1", { value: 10 });

      expect(writerCalls).toEqual([{ commId: "comm-1", patch: { value: 10 } }]);
    });

    it("coalesces subsequent ticks within the throttle window", () => {
      const { manager, writerCalls } = setup();

      manager.updateAndPersist("comm-1", { value: 10 });
      manager.updateAndPersist("comm-1", { value: 20 });
      manager.updateAndPersist("comm-1", { value: 30 });

      // First tick fired immediately; 20 and 30 are pending.
      expect(writerCalls).toEqual([{ commId: "comm-1", patch: { value: 10 } }]);

      vi.advanceTimersByTime(60);

      // Trailing flush coalesced the in-window writes into one.
      expect(writerCalls).toEqual([
        { commId: "comm-1", patch: { value: 10 } },
        { commId: "comm-1", patch: { value: 30 } },
      ]);
    });

    it("a write after the throttle window fires immediately again", () => {
      const { manager, writerCalls } = setup();

      manager.updateAndPersist("comm-1", { value: 1 });
      vi.advanceTimersByTime(100);
      manager.updateAndPersist("comm-1", { value: 2 });

      expect(writerCalls).toEqual([
        { commId: "comm-1", patch: { value: 1 } },
        { commId: "comm-1", patch: { value: 2 } },
      ]);
    });

    it("keeps different comms independent", () => {
      const { store, manager, writerCalls } = setup();
      store.createModel("comm-2", { value: 0 });

      manager.updateAndPersist("comm-1", { value: 1 });
      manager.updateAndPersist("comm-2", { value: 2 });

      // Both comms get a leading-tick fire — one throttle per comm.
      expect(writerCalls).toEqual([
        { commId: "comm-1", patch: { value: 1 } },
        { commId: "comm-2", patch: { value: 2 } },
      ]);
    });

    it("mirrors every tick into the local store for instant UI feedback", () => {
      // Continuous drags need per-tick store updates so the slider
      // thumb doesn't stutter at the 50 ms throttle boundary. The
      // CRDT projection (via `engine.projectLocalState()`) still fans
      // the same value back through `commChanges$`; the App-level
      // diff check makes that a no-op rather than a redundant update.
      const { store, manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });

      expect(store.getModel("comm-1")?.state.value).toBe(42);
    });

    it("marks written keys as pending so stale projected echoes can be dropped", () => {
      // While the trailing throttle flush is in flight, the daemon's
      // sync frames still carry its pre-flush view of the state. The
      // App-level `commChanges$` subscriber consults
      // `hasPendingKey(commId, key)` to drop those stale echoes, so
      // an in-flight drag value doesn't snap back mid-burst.
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });

      expect(manager.hasPendingKey("comm-1", "value")).toBe(true);
      expect(manager.hasPendingKey("comm-1", "description")).toBe(false);
    });

    it("clears pending-key marks after the TTL elapses", () => {
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });
      expect(manager.hasPendingKey("comm-1", "value")).toBe(true);

      vi.advanceTimersByTime(600); // past PENDING_TTL_MS (500)
      expect(manager.hasPendingKey("comm-1", "value")).toBe(false);
    });

    it("clears pending keys on clearComm (comm_close)", () => {
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });
      manager.clearComm("comm-1");

      expect(manager.hasPendingKey("comm-1", "value")).toBe(false);
    });
  });

  describe("bootstrap queue", () => {
    it("queues and mirrors when writer isn't ready", () => {
      const { store, manager, writerCalls } = setup({ writerAvailable: false });

      manager.updateAndPersist("comm-1", { value: 42 });

      expect(writerCalls).toHaveLength(0);
      expect(store.getModel("comm-1")?.state.value).toBe(42);
    });

    it("drains queued patches on the next update once writer is ready", () => {
      const store = createWidgetStore();
      const writerCalls: Array<{ commId: string; patch: Record<string, unknown> }> = [];
      const writer = (commId: string, patch: Record<string, unknown>) => {
        writerCalls.push({ commId, patch });
      };
      let writerAvailable = false;
      const manager = new WidgetUpdateManager({
        getStore: () => store,
        getCrdtWriter: () => (writerAvailable ? writer : null),
      });
      store.createModel("comm-1", { value: 0 });

      manager.updateAndPersist("comm-1", { value: 42 });
      expect(writerCalls).toHaveLength(0);

      writerAvailable = true;
      manager.updateAndPersist("comm-1", { description: "ready" });

      // Queued patch flushes first, then the new one (leading-tick
      // fires immediately because the throttle state is fresh).
      expect(writerCalls).toEqual([
        { commId: "comm-1", patch: { value: 42 } },
        { commId: "comm-1", patch: { description: "ready" } },
      ]);
    });
  });

  describe("binary buffers", () => {
    it("bypasses the throttle and mirrors buffers to the store", () => {
      const { store, manager, writerCalls } = setup();
      const buf1 = new ArrayBuffer(4);
      const buf2 = new ArrayBuffer(4);

      manager.updateAndPersist("comm-1", { value: 1 }, [buf1]);
      manager.updateAndPersist("comm-1", { value: 2 }, [buf2]);

      // Both writes fired immediately — buffers can't be merged and
      // delaying them would corrupt anywidget model.buffers order.
      expect(writerCalls).toEqual([
        { commId: "comm-1", patch: { value: 1 } },
        { commId: "comm-1", patch: { value: 2 } },
      ]);
      expect(store.getModel("comm-1")?.buffers).toContain(buf2);
    });
  });

  describe("lifecycle", () => {
    it("reset clears pending throttle state", () => {
      const { manager, writerCalls } = setup();

      manager.updateAndPersist("comm-1", { value: 1 });
      manager.updateAndPersist("comm-1", { value: 2 });

      // Reset before the trailing edge fires — the pending write
      // must be dropped so it doesn't reach a (potentially new) kernel.
      manager.reset();
      vi.advanceTimersByTime(100);

      expect(writerCalls).toEqual([{ commId: "comm-1", patch: { value: 1 } }]);
    });

    it("clearComm drops only that comm's state", () => {
      const { store, manager, writerCalls } = setup();
      store.createModel("comm-2", { value: 0 });

      manager.updateAndPersist("comm-1", { value: 1 });
      manager.updateAndPersist("comm-2", { value: 2 });
      manager.updateAndPersist("comm-1", { value: 10 });
      manager.updateAndPersist("comm-2", { value: 20 });

      manager.clearComm("comm-1");
      vi.advanceTimersByTime(100);

      // comm-1's trailing write was dropped; comm-2's fired.
      expect(writerCalls).toEqual([
        { commId: "comm-1", patch: { value: 1 } },
        { commId: "comm-2", patch: { value: 2 } },
        { commId: "comm-2", patch: { value: 20 } },
      ]);
    });

    it("dispose is idempotent and clears everything", () => {
      const { manager } = setup();
      manager.updateAndPersist("comm-1", { value: 1 });
      expect(() => manager.dispose()).not.toThrow();
      expect(() => manager.dispose()).not.toThrow();
    });
  });
});
