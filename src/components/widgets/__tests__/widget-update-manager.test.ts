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

    it("flags an identical projected value as an echo of the pending local write", () => {
      // Continuous drag → user writes value=42 → daemon lags and
      // projects the previous value back. The App-level subscriber
      // uses isEchoOfPendingWrite to drop just the matching echo.
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });

      expect(manager.isEchoOfPendingWrite("comm-1", "value", 42)).toBe(true);
      expect(manager.isEchoOfPendingWrite("comm-1", "description", "test")).toBe(false);
    });

    it("still accepts authoritative updates that differ from the pending local value", () => {
      // Kernel-side validators (value clamp, normalization) and
      // collaborative peers can write authoritative values that
      // don't match what we last wrote. Those must land in the
      // store; otherwise the frontend stays permanently divergent.
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });

      // Daemon echoes a clamped value — not ours.
      expect(manager.isEchoOfPendingWrite("comm-1", "value", 100)).toBe(false);
    });

    it("matches by structural equality for object/array values", () => {
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: [1, 2, 3] });

      // Different reference, same shape → still our echo.
      expect(manager.isEchoOfPendingWrite("comm-1", "value", [1, 2, 3])).toBe(true);
      // Different content → authoritative.
      expect(manager.isEchoOfPendingWrite("comm-1", "value", [1, 2, 4])).toBe(false);
    });

    it("recognizes older in-flight values as echoes during rapid bursts", () => {
      // A rapid drag writes 10 → 11 → 12 inside one throttle window.
      // projectLocalState fires a microtask per writer call, so the
      // projection of 10 (from the leading-edge CRDT write) can land
      // after the store is already at 12. The filter must still
      // recognize the 10 echo as ours, not treat it as authoritative.
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 10 });
      manager.updateAndPersist("comm-1", { value: 11 });
      manager.updateAndPersist("comm-1", { value: 12 });

      // consume-on-match: each call drains the matching entry.
      expect(manager.isEchoOfPendingWrite("comm-1", "value", 10)).toBe(true);
      expect(manager.isEchoOfPendingWrite("comm-1", "value", 11)).toBe(true);
      expect(manager.isEchoOfPendingWrite("comm-1", "value", 12)).toBe(true);
      // Authoritative values (kernel validator, peer edit) not in
      // the history still pass through.
      expect(manager.isEchoOfPendingWrite("comm-1", "value", 99)).toBe(false);
    });

    it("recordLocalWrite lets a direct writer share the echo history", () => {
      // anywidget `save_changes()` reaches the CRDT writer directly
      // (bypasses updateAndPersist to preserve AFM synchronous
      // model.get() semantics). `recordLocalWrite` exposes markPending
      // so those writes get the same echo suppression, otherwise
      // their stale projections would rewind the store mid-burst.
      const { manager } = setup();

      manager.recordLocalWrite("comm-1", { value: 77 });

      expect(manager.isEchoOfPendingWrite("comm-1", "value", 77)).toBe(true);
      expect(manager.isEchoOfPendingWrite("comm-1", "value", 77)).toBe(false);
    });

    it("consumes matches so a peer writing the same value afterward lands", () => {
      // Collaborative case: we write 10, our own echo arrives and
      // gets consumed. If a peer later writes 10 as an authoritative
      // update, it must not be suppressed — history is already
      // empty once our own echo has been absorbed.
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 10 });
      // First call represents our own projected echo.
      expect(manager.isEchoOfPendingWrite("comm-1", "value", 10)).toBe(true);
      // Second call represents a peer write of the same value — not
      // suppressed because the matching entry was consumed.
      expect(manager.isEchoOfPendingWrite("comm-1", "value", 10)).toBe(false);
    });

    it("clears pending-key marks after the TTL elapses", () => {
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });
      expect(manager.isEchoOfPendingWrite("comm-1", "value", 42)).toBe(true);

      vi.advanceTimersByTime(600); // past PENDING_TTL_MS (500)
      expect(manager.isEchoOfPendingWrite("comm-1", "value", 42)).toBe(false);
    });

    it("clears pending keys on clearComm (comm_close)", () => {
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });
      manager.clearComm("comm-1");

      expect(manager.isEchoOfPendingWrite("comm-1", "value", 42)).toBe(false);
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

    it("flushes pending throttled scalar patches before a buffered update", () => {
      // Ordering matters: a throttle-pending scalar patch must not
      // land after a buffered update for the same comm, otherwise
      // it would overwrite the newer buffered state and reorder
      // widget protocols that expect monotonic deltas.
      const { manager, writerCalls } = setup();
      const buf = new ArrayBuffer(4);

      manager.updateAndPersist("comm-1", { value: 1 }); // leading, fires
      manager.updateAndPersist("comm-1", { value: 2 }); // pending (throttled)
      manager.updateAndPersist("comm-1", { state: "after" }, [buf]);

      // Expect: leading scalar, then flushed-early pending scalar,
      // then buffered update — in that exact order.
      expect(writerCalls).toEqual([
        { commId: "comm-1", patch: { value: 1 } },
        { commId: "comm-1", patch: { value: 2 } },
        { commId: "comm-1", patch: { state: "after" } },
      ]);

      // Advance past the trailing window — no additional writes,
      // because `fireTrailingNow` already cleared the pending state.
      vi.advanceTimersByTime(60);
      expect(writerCalls).toHaveLength(3);
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
