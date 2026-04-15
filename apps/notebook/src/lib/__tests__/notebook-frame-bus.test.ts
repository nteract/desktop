import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import {
  emitBroadcast,
  emitPresence,
  subscribeBroadcast,
  subscribePresence,
} from "../notebook-frame-bus";

/**
 * The frame bus owns module-level Sets, so a leaking subscriber from one
 * test will see dispatches in later tests and fail noisily. Every test
 * tracks its own unsubscribes and cleans up in afterEach.
 */
describe("notebook-frame-bus", () => {
  const cleanups: Array<() => void> = [];
  const track = (unsub: () => void): (() => void) => {
    cleanups.push(unsub);
    return unsub;
  };

  afterEach(() => {
    while (cleanups.length > 0) {
      const u = cleanups.pop();
      u?.();
    }
  });

  describe("broadcast", () => {
    it("delivers payloads to all subscribers", () => {
      const a = vi.fn();
      const b = vi.fn();
      track(subscribeBroadcast(a));
      track(subscribeBroadcast(b));

      emitBroadcast({ type: "kernel_status", status: "idle" });

      expect(a).toHaveBeenCalledWith({ type: "kernel_status", status: "idle" });
      expect(b).toHaveBeenCalledWith({ type: "kernel_status", status: "idle" });
    });

    it("unsubscribe stops delivery without affecting other subscribers", () => {
      const a = vi.fn();
      const b = vi.fn();
      const unsubA = subscribeBroadcast(a);
      track(subscribeBroadcast(b));

      unsubA();
      emitBroadcast("hi");

      expect(a).not.toHaveBeenCalled();
      expect(b).toHaveBeenCalledWith("hi");
    });

    it("unsubscribe is idempotent", () => {
      // If useEffect cleanup fires twice (React 18 strict mode), the second
      // call must not throw or corrupt internal state.
      const a = vi.fn();
      const unsub = subscribeBroadcast(a);
      unsub();
      unsub();

      emitBroadcast("x");
      expect(a).not.toHaveBeenCalled();
    });

    it("registering the same callback twice only delivers once", () => {
      // Sets dedupe by reference — this is how React StrictMode's
      // double-invocation effectively becomes a no-op.
      const a = vi.fn();
      track(subscribeBroadcast(a));
      track(subscribeBroadcast(a));

      emitBroadcast("once");
      expect(a).toHaveBeenCalledTimes(1);
    });

    it("a throwing subscriber does not break dispatch to later subscribers", () => {
      // This is load-bearing: one buggy handler would otherwise take down
      // kernel status, output rendering, env progress — everything that
      // rides the bus.
      const thrower = vi.fn(() => {
        throw new Error("boom");
      });
      const after = vi.fn();
      track(subscribeBroadcast(thrower));
      track(subscribeBroadcast(after));

      expect(() => emitBroadcast({ type: "x" })).not.toThrow();
      expect(thrower).toHaveBeenCalled();
      expect(after).toHaveBeenCalled();
    });

    it("no subscribers is a valid state (no-op emit)", () => {
      expect(() => emitBroadcast("orphan")).not.toThrow();
    });
  });

  describe("presence", () => {
    it("delivers payloads only to presence subscribers, not broadcast", () => {
      // Presence and broadcast share a bus-like pattern but are
      // separate channels. Cross-delivery would flood cursor handlers
      // with kernel status events (and vice versa).
      const broadcast = vi.fn();
      const presence = vi.fn();
      track(subscribeBroadcast(broadcast));
      track(subscribePresence(presence));

      emitPresence({ type: "update" });
      expect(presence).toHaveBeenCalledWith({ type: "update" });
      expect(broadcast).not.toHaveBeenCalled();
    });

    it("broadcast emits do not reach presence subscribers", () => {
      const broadcast = vi.fn();
      const presence = vi.fn();
      track(subscribeBroadcast(broadcast));
      track(subscribePresence(presence));

      emitBroadcast({ type: "kernel_status" });
      expect(broadcast).toHaveBeenCalled();
      expect(presence).not.toHaveBeenCalled();
    });

    it("throwing presence subscriber does not break dispatch", () => {
      const thrower = vi.fn(() => {
        throw new Error("boom");
      });
      const after = vi.fn();
      track(subscribePresence(thrower));
      track(subscribePresence(after));

      expect(() => emitPresence({})).not.toThrow();
      expect(after).toHaveBeenCalled();
    });
  });
});
