/**
 * Tests for createAFMModelProxy — the AFM-compatible model proxy for anywidgets.
 */

import { describe, expect, it, vi } from "vite-plus/test";
import { createAFMModelProxy } from "../anywidget-view";
import { createWidgetStore } from "../widget-store";

function makeProxy(state: Record<string, unknown>) {
  const store = createWidgetStore();
  const commId = "test-comm";
  store.createModel(commId, state);
  const sendUpdate = vi.fn();
  const sendCustom = vi.fn();
  const model = store.getModel(commId)!;
  const proxy = createAFMModelProxy(
    model,
    store,
    { sendUpdate, sendCustom },
    () => store.getModel(commId)?.state ?? {},
  );
  return { proxy, store, sendUpdate, sendCustom };
}

describe("createAFMModelProxy", () => {
  describe("get", () => {
    it("returns primitive values directly", () => {
      const { proxy } = makeProxy({ count: 42, label: "hello", flag: true });
      expect(proxy.get("count")).toBe(42);
      expect(proxy.get("label")).toBe("hello");
      expect(proxy.get("flag")).toBe(true);
    });

    it("returns cloned objects that can be mutated without affecting the store", () => {
      const originalData = { x: [1, 2, 3], y: [4, 5, 6] };
      const { proxy, store } = makeProxy({ _data: [originalData] });

      const data = proxy.get("_data") as Array<{ x: number[]; y: number[] }>;
      // Mutate the returned value (Plotly.js does this)
      data[0].x.push(99);

      // Store should be unaffected
      const storeData = store.getModel("test-comm")!.state._data as Array<{
        x: number[];
      }>;
      expect(storeData[0].x).toEqual([1, 2, 3]);
    });

    it("returns cloned arrays that can be mutated", () => {
      const { proxy, store } = makeProxy({ items: [1, 2, 3] });

      const items = proxy.get("items") as number[];
      items.push(4);

      const storeItems = store.getModel("test-comm")!.state.items as number[];
      expect(storeItems).toEqual([1, 2, 3]);
    });

    it("allows mutation of frozen source objects via cloning", () => {
      const frozenLayout = Object.freeze({
        title: Object.freeze({ text: "Test" }),
        margin: Object.freeze({ l: 50, r: 50 }),
      });
      const { proxy } = makeProxy({ _layout: frozenLayout });

      const layout = proxy.get("_layout") as Record<string, unknown>;
      // This would throw "Attempted to assign to readonly property"
      // without the structuredClone fix
      expect(() => {
        (layout as Record<string, unknown>).title = { text: "Modified" };
      }).not.toThrow();
    });

    it("returns undefined for missing keys", () => {
      const { proxy } = makeProxy({ value: 1 });
      expect(proxy.get("nonexistent")).toBeUndefined();
    });

    it("returns null without cloning", () => {
      const { proxy } = makeProxy({ empty: null });
      expect(proxy.get("empty")).toBeNull();
    });

    it("returns pending changes over store state", () => {
      const { proxy } = makeProxy({ value: 1 });
      proxy.set("value", 2);
      expect(proxy.get("value")).toBe(2);
    });
  });

  describe("set + save_changes", () => {
    it("buffers changes until save_changes is called", () => {
      const { proxy, sendUpdate } = makeProxy({ value: 0 });
      proxy.set("value", 42);
      expect(sendUpdate).not.toHaveBeenCalled();
    });

    it("routes save_changes through sendUpdate as a plain state patch", () => {
      const { proxy, sendUpdate } = makeProxy({ value: 0, other: "a" });
      proxy.set("value", 42);
      proxy.set("other", "b");
      proxy.save_changes();
      expect(sendUpdate).toHaveBeenCalledTimes(1);
      expect(sendUpdate).toHaveBeenCalledWith("test-comm", { value: 42, other: "b" });
    });

    it("is a no-op when there are no pending changes", () => {
      const { proxy, sendUpdate } = makeProxy({ value: 0 });
      proxy.save_changes();
      expect(sendUpdate).not.toHaveBeenCalled();
    });

    it("clears pending changes after save", () => {
      const { proxy, sendUpdate } = makeProxy({ value: 0 });
      proxy.set("value", 1);
      proxy.save_changes();
      proxy.save_changes();
      expect(sendUpdate).toHaveBeenCalledTimes(1);
    });
  });

  describe("send", () => {
    it("routes custom messages through sendCustom with buffers", () => {
      const { proxy, sendCustom, sendUpdate } = makeProxy({ value: 0 });
      const buf = new ArrayBuffer(4);
      proxy.send({ type: "ping" }, undefined, [buf]);
      expect(sendCustom).toHaveBeenCalledTimes(1);
      expect(sendCustom).toHaveBeenCalledWith("test-comm", { type: "ping" }, [buf]);
      expect(sendUpdate).not.toHaveBeenCalled();
    });
  });
});
