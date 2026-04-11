/**
 * Tests for WidgetUpdateManager — debounced CRDT persistence + echo suppression.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { createWidgetStore, type WidgetStore } from "../widget-store";
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

  // Pre-create a model so updateModel works
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

  // ── Debouncing ──────────────────────────────────────────────────

  describe("debouncing", () => {
    it("updates store immediately", () => {
      const { store, manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });

      expect(store.getModel("comm-1")?.state.value).toBe(42);
    });

    it("debounces CRDT writes at 50ms", () => {
      const { manager, writerCalls } = setup();

      manager.updateAndPersist("comm-1", { value: 10 });
      manager.updateAndPersist("comm-1", { value: 20 });
      manager.updateAndPersist("comm-1", { value: 30 });

      // No CRDT writes yet
      expect(writerCalls).toHaveLength(0);

      // Advance past debounce
      vi.advanceTimersByTime(50);

      // Single merged write
      expect(writerCalls).toHaveLength(1);
      expect(writerCalls[0]).toEqual({
        commId: "comm-1",
        patch: { value: 30 },
      });
    });

    it("merges multiple keys in debounce window", () => {
      const { manager, writerCalls } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });
      manager.updateAndPersist("comm-1", { description: "updated" });

      vi.advanceTimersByTime(50);

      expect(writerCalls).toHaveLength(1);
      expect(writerCalls[0].patch).toEqual({
        value: 42,
        description: "updated",
      });
    });

    it("debounces independently per comm", () => {
      const { store, manager, writerCalls } = setup();
      store.createModel("comm-2", { value: 0 });

      manager.updateAndPersist("comm-1", { value: 10 });

      vi.advanceTimersByTime(30);

      manager.updateAndPersist("comm-2", { value: 20 });

      vi.advanceTimersByTime(20);

      // comm-1 flushed at t=50, comm-2 still pending
      expect(writerCalls).toHaveLength(1);
      expect(writerCalls[0].commId).toBe("comm-1");

      vi.advanceTimersByTime(30);

      // comm-2 flushed at t=80
      expect(writerCalls).toHaveLength(2);
      expect(writerCalls[1].commId).toBe("comm-2");
    });

    it("resets debounce timer on new update", () => {
      const { manager, writerCalls } = setup();

      manager.updateAndPersist("comm-1", { value: 10 });
      vi.advanceTimersByTime(40);

      // Another update before 50ms — resets the timer
      manager.updateAndPersist("comm-1", { value: 20 });
      vi.advanceTimersByTime(40);

      // Still no flush (only 40ms since last update)
      expect(writerCalls).toHaveLength(0);

      vi.advanceTimersByTime(10);

      // Now flushed with the latest value
      expect(writerCalls).toHaveLength(1);
      expect(writerCalls[0].patch).toEqual({ value: 20 });
    });

    it("flushes immediately for binary buffers", () => {
      const { manager, writerCalls } = setup();

      const buffer = new ArrayBuffer(8);
      manager.updateAndPersist("comm-1", { value: 42 }, [buffer]);

      // Flushed immediately, no debounce
      expect(writerCalls).toHaveLength(1);
      expect(writerCalls[0].patch).toEqual({ value: 42 });
    });
  });

  // ── Echo suppression ────────────────────────────────────────────

  describe("echo suppression", () => {
    it("suppresses echoes for optimistic keys", () => {
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });

      const result = manager.shouldSuppressEcho("comm-1", { value: 10 });
      expect(result).toBeNull();
    });

    it("passes through non-optimistic keys", () => {
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });

      const result = manager.shouldSuppressEcho("comm-1", {
        value: 10,
        description: "from kernel",
      });
      expect(result).toEqual({ description: "from kernel" });
    });

    it("passes everything when no optimistic keys", () => {
      const { manager } = setup();

      const result = manager.shouldSuppressEcho("comm-1", {
        value: 10,
        description: "from kernel",
      });
      expect(result).toEqual({ value: 10, description: "from kernel" });
    });

    it("clears optimistic keys after flush", () => {
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });

      // During debounce window — suppressed
      expect(manager.shouldSuppressEcho("comm-1", { value: 10 })).toBeNull();

      // Flush
      vi.advanceTimersByTime(50);

      // After flush — passes through
      const result = manager.shouldSuppressEcho("comm-1", { value: 10 });
      expect(result).toEqual({ value: 10 });
    });

    it("suppresses during continuous drag", () => {
      const { manager } = setup();

      // Simulate continuous slider drag
      manager.updateAndPersist("comm-1", { value: 10 });
      vi.advanceTimersByTime(16);
      manager.updateAndPersist("comm-1", { value: 15 });
      vi.advanceTimersByTime(16);
      manager.updateAndPersist("comm-1", { value: 20 });

      // Stale echo from earlier value — suppressed
      expect(manager.shouldSuppressEcho("comm-1", { value: 5 })).toBeNull();

      // Non-value keys still pass through
      expect(manager.shouldSuppressEcho("comm-1", { value: 5, _view_name: "x" })).toEqual({
        _view_name: "x",
      });
    });
  });

  // ── clearComm ──────────────────────────────────────────────────

  describe("clearComm", () => {
    it("cancels pending flush", () => {
      const { manager, writerCalls } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });
      manager.clearComm("comm-1");

      vi.advanceTimersByTime(50);

      expect(writerCalls).toHaveLength(0);
    });

    it("clears optimistic keys", () => {
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });
      manager.clearComm("comm-1");

      // Echo passes through after clearComm
      const result = manager.shouldSuppressEcho("comm-1", { value: 10 });
      expect(result).toEqual({ value: 10 });
    });
  });

  // ── reset ──────────────────────────────────────────────────────

  describe("reset", () => {
    it("cancels all pending flushes", () => {
      const { store, manager, writerCalls } = setup();
      store.createModel("comm-2", { value: 0 });

      manager.updateAndPersist("comm-1", { value: 10 });
      manager.updateAndPersist("comm-2", { value: 20 });
      manager.reset();

      vi.advanceTimersByTime(50);

      expect(writerCalls).toHaveLength(0);
    });

    it("clears all optimistic keys", () => {
      const { manager } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });
      manager.reset();

      const result = manager.shouldSuppressEcho("comm-1", { value: 10 });
      expect(result).toEqual({ value: 10 });
    });
  });

  // ── Writer unavailable ─────────────────────────────────────────

  describe("writer unavailable", () => {
    it("retries flush when CRDT writer is null", () => {
      let writerAvailable = false;
      const writerCalls: Array<{
        commId: string;
        patch: Record<string, unknown>;
      }> = [];
      const store = createWidgetStore();
      store.createModel("comm-1", { value: 0 });

      const manager = new WidgetUpdateManager({
        getStore: () => store,
        getCrdtWriter: () =>
          writerAvailable
            ? (commId: string, patch: Record<string, unknown>) => {
                writerCalls.push({ commId, patch });
              }
            : null,
      });

      manager.updateAndPersist("comm-1", { value: 42 });

      // First flush attempt — writer not available, retries
      vi.advanceTimersByTime(50);
      expect(writerCalls).toHaveLength(0);

      // Make writer available
      writerAvailable = true;

      // Retry fires after another 50ms
      vi.advanceTimersByTime(50);
      expect(writerCalls).toHaveLength(1);
      expect(writerCalls[0].patch).toEqual({ value: 42 });
    });

    it("keeps optimistic keys while retrying", () => {
      const store = createWidgetStore();
      store.createModel("comm-1", { value: 0 });

      const manager = new WidgetUpdateManager({
        getStore: () => store,
        getCrdtWriter: () => null,
      });

      manager.updateAndPersist("comm-1", { value: 42 });
      vi.advanceTimersByTime(50);

      // Still optimistic since flush failed
      expect(manager.shouldSuppressEcho("comm-1", { value: 10 })).toBeNull();
    });
  });
});
