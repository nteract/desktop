/**
 * Tests for WidgetUpdateManager — CRDT-first widget state persistence.
 *
 * Post-A2 semantics: no optimistic store update, no debounce, no echo
 * suppression. Every update goes straight to the injected CRDT writer
 * and the widget store is updated by the commChanges$ projection
 * (verified end-to-end in `packages/runtimed/tests/widget-sync-stall.test.ts`).
 *
 * The manager's remaining responsibilities are narrow: bootstrap
 * fallback when the writer isn't registered yet, and mirroring binary
 * buffers into the local widget model since the CRDT doesn't carry
 * ArrayBuffers.
 */

import { describe, expect, it } from "vite-plus/test";
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
  describe("happy path", () => {
    it("routes every update straight to the CRDT writer", () => {
      const { manager, writerCalls } = setup();

      manager.updateAndPersist("comm-1", { value: 10 });
      manager.updateAndPersist("comm-1", { value: 20 });
      manager.updateAndPersist("comm-1", { value: 30 });

      expect(writerCalls).toEqual([
        { commId: "comm-1", patch: { value: 10 } },
        { commId: "comm-1", patch: { value: 20 } },
        { commId: "comm-1", patch: { value: 30 } },
      ]);
    });

    it("does not touch the local store on the happy path", () => {
      // The widget store is driven by the CRDT projection (via
      // `engine.projectLocalState()` after `set_comm_state_batch`).
      // The manager itself must not write to the store — otherwise
      // we'd be back to the pre-A2 dual-source drift.
      const { store, manager } = setup();
      const beforeValue = store.getModel("comm-1")?.state.value;

      manager.updateAndPersist("comm-1", { value: 42 });

      // Store state is unchanged — projection happens downstream.
      expect(store.getModel("comm-1")?.state.value).toBe(beforeValue);
    });

    it("passes disjoint patches through untouched", () => {
      const { manager, writerCalls } = setup();

      manager.updateAndPersist("comm-1", { value: 42 });
      manager.updateAndPersist("comm-1", { description: "updated" });

      expect(writerCalls).toEqual([
        { commId: "comm-1", patch: { value: 42 } },
        { commId: "comm-1", patch: { description: "updated" } },
      ]);
    });

    it("keeps different comms independent", () => {
      const { store, manager, writerCalls } = setup();
      store.createModel("comm-2", { value: 0 });

      manager.updateAndPersist("comm-1", { value: 1 });
      manager.updateAndPersist("comm-2", { value: 2 });

      expect(writerCalls).toEqual([
        { commId: "comm-1", patch: { value: 1 } },
        { commId: "comm-2", patch: { value: 2 } },
      ]);
    });
  });

  describe("bootstrap queue", () => {
    it("queues patches when writer isn't ready and mirrors to local store", () => {
      // Early session state: CRDT writer hasn't been registered yet
      // (App.tsx's `setCrdtCommWriter` useEffect hasn't run). The
      // update is queued so nothing is lost; the store is mirrored
      // so the UI doesn't stall during bootstrap.
      const { store, manager, writerCalls } = setup({ writerAvailable: false });

      manager.updateAndPersist("comm-1", { value: 42 });

      expect(writerCalls).toHaveLength(0);
      expect(store.getModel("comm-1")?.state.value).toBe(42);
    });

    it("drains queued patches into the writer on the next update", () => {
      // Typical bootstrap race: user clicks a widget → writer isn't
      // registered yet → patch queued. A later call (e.g. model
      // echo or another interaction) finds the writer registered —
      // drain the queue before processing the new write.
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

      // Bootstrap write — queued.
      manager.updateAndPersist("comm-1", { value: 42 });
      expect(writerCalls).toHaveLength(0);

      // Writer becomes available.
      writerAvailable = true;
      manager.updateAndPersist("comm-1", { description: "ready" });

      // Queued patch flushes first, then the new one.
      expect(writerCalls).toEqual([
        { commId: "comm-1", patch: { value: 42 } },
        { commId: "comm-1", patch: { description: "ready" } },
      ]);
    });

    it("coalesces multiple bootstrap writes on the same comm", () => {
      const { manager, writerCalls } = setup({ writerAvailable: false });
      manager.updateAndPersist("comm-1", { value: 1 });
      manager.updateAndPersist("comm-1", { value: 2 });
      manager.updateAndPersist("comm-1", { description: "hello" });

      // Drain by providing a writer on the next call.
      const writer = (commId: string, patch: Record<string, unknown>) => {
        writerCalls.push({ commId, patch });
      };
      const managerRef = manager as unknown as {
        getCrdtWriter: () => typeof writer | null;
      };
      managerRef.getCrdtWriter = () => writer;
      manager.updateAndPersist("comm-2", { value: 99 });

      // The coalesced patch has the last value for `value` plus the
      // description — last-wins merge on key collision.
      expect(writerCalls).toEqual([
        { commId: "comm-1", patch: { value: 2, description: "hello" } },
        { commId: "comm-2", patch: { value: 99 } },
      ]);
    });
  });

  describe("binary buffers", () => {
    it("sends patch through CRDT writer and buffers into local store", () => {
      // CRDT doesn't carry ArrayBuffers; keep the legacy behavior of
      // stashing buffers on the local widget model so anywidgets can
      // read back from `model.buffers`. Kernel delivery of the buffers
      // themselves is handled elsewhere (SendComm RPC).
      const { store, manager, writerCalls } = setup();
      const buffer = new ArrayBuffer(8);

      manager.updateAndPersist("comm-1", { value: 42 }, [buffer]);

      expect(writerCalls).toEqual([{ commId: "comm-1", patch: { value: 42 } }]);
      expect(store.getModel("comm-1")?.buffers).toContain(buffer);
    });
  });

  describe("lifecycle", () => {
    it("reset / dispose / clearComm are all no-ops post-A2", () => {
      // Kept on the API so the calling sites in App.tsx don't have
      // to change. Assert they don't throw — no state to reset.
      const { manager } = setup();
      expect(() => manager.reset()).not.toThrow();
      expect(() => manager.clearComm("comm-1")).not.toThrow();
      expect(() => manager.dispose()).not.toThrow();
    });
  });
});
