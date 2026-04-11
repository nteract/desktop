/**
 * Tests for widget-store.ts - Widget state management for Jupyter widgets.
 *
 * The widget store manages the lifecycle of widget models (comm_open/comm_msg/comm_close)
 * and provides reactive subscriptions for UI updates.
 */

import { describe, expect, it, vi } from "vite-plus/test";
import {
  createWidgetStore,
  isModelRef,
  parseModelRef,
  resolveModelRef,
  type WidgetModel,
} from "../widget-store";

describe("isModelRef", () => {
  it("returns true for valid IPY_MODEL_ reference", () => {
    expect(isModelRef("IPY_MODEL_abc123")).toBe(true);
  });

  it("returns true for IPY_MODEL_ with UUID", () => {
    expect(isModelRef("IPY_MODEL_550e8400-e29b-41d4-a716-446655440000")).toBe(true);
  });

  it("returns false for string without prefix", () => {
    expect(isModelRef("abc123")).toBe(false);
  });

  it("returns false for empty string", () => {
    expect(isModelRef("")).toBe(false);
  });

  it("returns false for null", () => {
    expect(isModelRef(null)).toBe(false);
  });

  it("returns false for undefined", () => {
    expect(isModelRef(undefined)).toBe(false);
  });

  it("returns false for number", () => {
    expect(isModelRef(123)).toBe(false);
  });

  it("returns false for object", () => {
    expect(isModelRef({ id: "IPY_MODEL_abc" })).toBe(false);
  });

  it("returns false for case mismatch", () => {
    expect(isModelRef("ipy_model_abc")).toBe(false);
    expect(isModelRef("IPY_model_abc")).toBe(false);
  });
});

describe("parseModelRef", () => {
  it("extracts model ID from valid reference", () => {
    expect(parseModelRef("IPY_MODEL_abc123")).toBe("abc123");
  });

  it("extracts UUID from reference", () => {
    const uuid = "550e8400-e29b-41d4-a716-446655440000";
    expect(parseModelRef(`IPY_MODEL_${uuid}`)).toBe(uuid);
  });

  it("returns null for string without prefix", () => {
    expect(parseModelRef("abc123")).toBeNull();
  });

  it("returns empty string for IPY_MODEL_ with no ID", () => {
    expect(parseModelRef("IPY_MODEL_")).toBe("");
  });

  it("handles prefix only (edge case)", () => {
    expect(parseModelRef("IPY_MODEL_")).toBe("");
  });
});

describe("resolveModelRef", () => {
  it("resolves reference to model using getModel", () => {
    const mockModel: WidgetModel = {
      id: "abc123",
      state: { value: 42 },
      buffers: [],
      modelName: "IntSliderModel",
      modelModule: "@jupyter-widgets/controls",
    };
    const getModel = vi.fn().mockReturnValue(mockModel);

    const result = resolveModelRef("IPY_MODEL_abc123", getModel);

    expect(getModel).toHaveBeenCalledWith("abc123");
    expect(result).toBe(mockModel);
  });

  it("returns original value for non-reference string", () => {
    const getModel = vi.fn();
    const result = resolveModelRef("plain string", getModel);

    expect(getModel).not.toHaveBeenCalled();
    expect(result).toBe("plain string");
  });

  it("returns original value for number", () => {
    const getModel = vi.fn();
    expect(resolveModelRef(42, getModel)).toBe(42);
    expect(getModel).not.toHaveBeenCalled();
  });

  it("returns original value for object", () => {
    const getModel = vi.fn();
    const obj = { foo: "bar" };
    expect(resolveModelRef(obj, getModel)).toBe(obj);
  });

  it("returns original value for null", () => {
    const getModel = vi.fn();
    expect(resolveModelRef(null, getModel)).toBeNull();
  });

  it("returns undefined from getModel if model not found", () => {
    const getModel = vi.fn().mockReturnValue(undefined);
    const result = resolveModelRef("IPY_MODEL_missing", getModel);

    expect(getModel).toHaveBeenCalledWith("missing");
    expect(result).toBeUndefined();
  });
});

describe("createWidgetStore", () => {
  describe("basic store operations", () => {
    it("returns a store with all required methods", () => {
      const store = createWidgetStore();

      expect(store.subscribe).toBeTypeOf("function");
      expect(store.getSnapshot).toBeTypeOf("function");
      expect(store.getModel).toBeTypeOf("function");
      expect(store.createModel).toBeTypeOf("function");
      expect(store.updateModel).toBeTypeOf("function");
      expect(store.deleteModel).toBeTypeOf("function");
      expect(store.wasModelClosed).toBeTypeOf("function");
      expect(store.subscribeToKey).toBeTypeOf("function");
      expect(store.emitCustomMessage).toBeTypeOf("function");
      expect(store.subscribeToCustomMessage).toBeTypeOf("function");
    });

    it("starts with empty models", () => {
      const store = createWidgetStore();
      expect(store.getSnapshot().size).toBe(0);
    });
  });

  describe("createModel", () => {
    it("creates a model with correct metadata extraction", () => {
      const store = createWidgetStore();

      store.createModel("comm-1", {
        _model_name: "IntSliderModel",
        _model_module: "@jupyter-widgets/controls",
        value: 50,
        min: 0,
        max: 100,
      });

      const model = store.getModel("comm-1");
      expect(model).toBeDefined();
      expect(model?.id).toBe("comm-1");
      expect(model?.modelName).toBe("IntSliderModel");
      expect(model?.modelModule).toBe("@jupyter-widgets/controls");
      expect(model?.state.value).toBe(50);
      expect(model?.buffers).toEqual([]);
    });

    it("uses default values when metadata missing", () => {
      const store = createWidgetStore();

      store.createModel("comm-1", { value: 42 });

      const model = store.getModel("comm-1");
      expect(model?.modelName).toBe("UnknownModel");
      expect(model?.modelModule).toBe("");
    });

    it("stores buffers with the model", () => {
      const store = createWidgetStore();
      const buffer = new ArrayBuffer(8);

      store.createModel("comm-1", { value: null }, [buffer]);

      const model = store.getModel("comm-1");
      expect(model?.buffers).toHaveLength(1);
      expect(model?.buffers[0]).toBe(buffer);
    });

    it("notifies subscribers on create", () => {
      const store = createWidgetStore();
      const listener = vi.fn();

      store.subscribe(listener);
      store.createModel("comm-1", { value: 42 });

      expect(listener).toHaveBeenCalledTimes(1);
    });

    it("updates getSnapshot after create", () => {
      const store = createWidgetStore();

      const before = store.getSnapshot();
      store.createModel("comm-1", { value: 42 });
      const after = store.getSnapshot();

      expect(before.size).toBe(0);
      expect(after.size).toBe(1);
      expect(before).not.toBe(after); // New Map reference
    });
  });

  describe("updateModel", () => {
    it("merges state patch into existing model", () => {
      const store = createWidgetStore();

      store.createModel("comm-1", { value: 50, min: 0, max: 100 });
      store.updateModel("comm-1", { value: 75 });

      const model = store.getModel("comm-1");
      expect(model?.state.value).toBe(75);
      expect(model?.state.min).toBe(0);
      expect(model?.state.max).toBe(100);
    });

    it("replaces buffers when provided", () => {
      const store = createWidgetStore();
      const buffer1 = new ArrayBuffer(4);
      const buffer2 = new ArrayBuffer(8);

      store.createModel("comm-1", { value: null }, [buffer1]);
      store.updateModel("comm-1", {}, [buffer2]);

      const model = store.getModel("comm-1");
      expect(model?.buffers).toHaveLength(1);
      expect(model?.buffers[0]).toBe(buffer2);
    });

    it("preserves buffers when not provided", () => {
      const store = createWidgetStore();
      const buffer = new ArrayBuffer(8);

      store.createModel("comm-1", { value: null }, [buffer]);
      store.updateModel("comm-1", { value: 42 });

      const model = store.getModel("comm-1");
      expect(model?.buffers[0]).toBe(buffer);
    });

    it("does nothing for non-existent model", () => {
      const store = createWidgetStore();
      const listener = vi.fn();

      store.subscribe(listener);
      store.updateModel("missing", { value: 42 });

      expect(listener).not.toHaveBeenCalled();
      expect(store.getModel("missing")).toBeUndefined();
    });

    it("notifies global listeners on update", () => {
      const store = createWidgetStore();
      const listener = vi.fn();

      store.createModel("comm-1", { value: 50 });
      store.subscribe(listener);
      store.updateModel("comm-1", { value: 75 });

      expect(listener).toHaveBeenCalledTimes(1);
    });

    it("creates new Map reference on update", () => {
      const store = createWidgetStore();

      store.createModel("comm-1", { value: 50 });
      const before = store.getSnapshot();
      store.updateModel("comm-1", { value: 75 });
      const after = store.getSnapshot();

      expect(before).not.toBe(after);
    });
  });

  describe("deleteModel", () => {
    it("removes model from store", () => {
      const store = createWidgetStore();

      store.createModel("comm-1", { value: 42 });
      expect(store.getModel("comm-1")).toBeDefined();

      store.deleteModel("comm-1");
      expect(store.getModel("comm-1")).toBeUndefined();
    });

    it("notifies listeners on delete", () => {
      const store = createWidgetStore();
      const listener = vi.fn();

      store.createModel("comm-1", { value: 42 });
      store.subscribe(listener);
      store.deleteModel("comm-1");

      expect(listener).toHaveBeenCalledTimes(1);
    });

    it("does nothing for non-existent model", () => {
      const store = createWidgetStore();
      const listener = vi.fn();

      store.subscribe(listener);
      store.deleteModel("missing");

      expect(listener).not.toHaveBeenCalled();
    });

    it("marks model as closed", () => {
      const store = createWidgetStore();

      store.createModel("comm-1", { value: 42 });
      store.deleteModel("comm-1");

      expect(store.wasModelClosed("comm-1")).toBe(true);
    });
  });

  describe("wasModelClosed", () => {
    it("returns false for model that never existed", () => {
      const store = createWidgetStore();
      expect(store.wasModelClosed("never-existed")).toBe(false);
    });

    it("returns false for existing model", () => {
      const store = createWidgetStore();

      store.createModel("comm-1", { value: 42 });

      expect(store.wasModelClosed("comm-1")).toBe(false);
    });

    it("returns true after model deleted", () => {
      const store = createWidgetStore();

      store.createModel("comm-1", { value: 42 });
      store.deleteModel("comm-1");

      expect(store.wasModelClosed("comm-1")).toBe(true);
    });

    it("returns false after model re-opened", () => {
      const store = createWidgetStore();

      store.createModel("comm-1", { value: 42 });
      store.deleteModel("comm-1");
      expect(store.wasModelClosed("comm-1")).toBe(true);

      store.createModel("comm-1", { value: 100 });
      expect(store.wasModelClosed("comm-1")).toBe(false);
    });
  });

  describe("subscribe / unsubscribe", () => {
    it("returns unsubscribe function", () => {
      const store = createWidgetStore();
      const listener = vi.fn();

      const unsubscribe = store.subscribe(listener);
      expect(unsubscribe).toBeTypeOf("function");
    });

    it("unsubscribe removes listener", () => {
      const store = createWidgetStore();
      const listener = vi.fn();

      const unsubscribe = store.subscribe(listener);
      store.createModel("comm-1", { value: 42 });
      expect(listener).toHaveBeenCalledTimes(1);

      unsubscribe();
      store.createModel("comm-2", { value: 100 });
      expect(listener).toHaveBeenCalledTimes(1); // Not called again
    });

    it("multiple listeners all notified", () => {
      const store = createWidgetStore();
      const listener1 = vi.fn();
      const listener2 = vi.fn();

      store.subscribe(listener1);
      store.subscribe(listener2);
      store.createModel("comm-1", { value: 42 });

      expect(listener1).toHaveBeenCalledTimes(1);
      expect(listener2).toHaveBeenCalledTimes(1);
    });
  });

  describe("subscribeToKey", () => {
    it("notifies callback when specific key changes", () => {
      const store = createWidgetStore();
      const callback = vi.fn();

      store.createModel("comm-1", { value: 50, other: "data" });
      store.subscribeToKey("comm-1", "value", callback);
      store.updateModel("comm-1", { value: 75 });

      expect(callback).toHaveBeenCalledTimes(1);
      expect(callback).toHaveBeenCalledWith(75);
    });

    it("does not notify when different key changes", () => {
      const store = createWidgetStore();
      const callback = vi.fn();

      store.createModel("comm-1", { value: 50, other: "data" });
      store.subscribeToKey("comm-1", "value", callback);
      store.updateModel("comm-1", { other: "changed" });

      expect(callback).not.toHaveBeenCalled();
    });

    it("returns unsubscribe function", () => {
      const store = createWidgetStore();
      const callback = vi.fn();

      store.createModel("comm-1", { value: 50 });
      const unsubscribe = store.subscribeToKey("comm-1", "value", callback);

      store.updateModel("comm-1", { value: 60 });
      expect(callback).toHaveBeenCalledTimes(1);

      unsubscribe();
      store.updateModel("comm-1", { value: 70 });
      expect(callback).toHaveBeenCalledTimes(1); // Not called again
    });

    it("multiple callbacks for same key all notified", () => {
      const store = createWidgetStore();
      const callback1 = vi.fn();
      const callback2 = vi.fn();

      store.createModel("comm-1", { value: 50 });
      store.subscribeToKey("comm-1", "value", callback1);
      store.subscribeToKey("comm-1", "value", callback2);
      store.updateModel("comm-1", { value: 75 });

      expect(callback1).toHaveBeenCalledWith(75);
      expect(callback2).toHaveBeenCalledWith(75);
    });

    it("can subscribe before model exists", () => {
      const store = createWidgetStore();
      const callback = vi.fn();

      store.subscribeToKey("comm-1", "value", callback);
      store.createModel("comm-1", { value: 50 });
      // Create doesn't trigger key listeners
      store.updateModel("comm-1", { value: 75 });

      expect(callback).toHaveBeenCalledWith(75);
    });

    it("cleans up listener maps on unsubscribe", () => {
      const store = createWidgetStore();
      const callback = vi.fn();

      store.createModel("comm-1", { value: 50 });
      const unsubscribe = store.subscribeToKey("comm-1", "value", callback);
      unsubscribe();

      // Should not throw when updating after cleanup
      store.updateModel("comm-1", { value: 60 });
      expect(callback).not.toHaveBeenCalled();
    });
  });

  describe("emitCustomMessage / subscribeToCustomMessage", () => {
    it("delivers message to subscriber", () => {
      const store = createWidgetStore();
      const callback = vi.fn();

      store.createModel("comm-1", { value: 42 });
      store.subscribeToCustomMessage("comm-1", callback);
      store.emitCustomMessage("comm-1", { action: "draw", x: 10 });

      expect(callback).toHaveBeenCalledTimes(1);
      expect(callback).toHaveBeenCalledWith({ action: "draw", x: 10 }, undefined);
    });

    it("converts ArrayBuffer to DataView for anywidget compatibility", () => {
      const store = createWidgetStore();
      const callback = vi.fn();
      const buffer = new ArrayBuffer(8);

      store.createModel("comm-1", { value: 42 });
      store.subscribeToCustomMessage("comm-1", callback);
      store.emitCustomMessage("comm-1", { action: "data" }, [buffer]);

      expect(callback).toHaveBeenCalledTimes(1);
      const [, buffers] = callback.mock.calls[0];
      expect(buffers).toHaveLength(1);
      expect(buffers[0]).toBeInstanceOf(DataView);
      expect(buffers[0].buffer).toBe(buffer);
    });

    it("buffers messages before subscriber exists", () => {
      const store = createWidgetStore();
      const callback = vi.fn();

      store.createModel("comm-1", { value: 42 });
      store.emitCustomMessage("comm-1", { action: "first" });
      store.emitCustomMessage("comm-1", { action: "second" });

      // Subscribe after messages emitted
      store.subscribeToCustomMessage("comm-1", callback);

      // Should receive buffered messages
      expect(callback).toHaveBeenCalledTimes(2);
      expect(callback).toHaveBeenNthCalledWith(1, { action: "first" }, undefined);
      expect(callback).toHaveBeenNthCalledWith(2, { action: "second" }, undefined);
    });

    it("multiple subscribers to same comm receive messages", () => {
      const store = createWidgetStore();
      const callback1 = vi.fn();
      const callback2 = vi.fn();

      store.createModel("comm-1", { value: 42 });
      store.subscribeToCustomMessage("comm-1", callback1);
      store.subscribeToCustomMessage("comm-1", callback2);
      store.emitCustomMessage("comm-1", { action: "test" });

      expect(callback1).toHaveBeenCalledWith({ action: "test" }, undefined);
      expect(callback2).toHaveBeenCalledWith({ action: "test" }, undefined);
    });

    it("second subscriber also receives buffered history", () => {
      const store = createWidgetStore();
      const callback1 = vi.fn();
      const callback2 = vi.fn();

      store.createModel("comm-1", { value: 42 });
      store.subscribeToCustomMessage("comm-1", callback1);
      store.emitCustomMessage("comm-1", { action: "history" });

      // Second subscriber should also get buffered messages
      store.subscribeToCustomMessage("comm-1", callback2);

      expect(callback1).toHaveBeenCalledTimes(1); // Live message only
      expect(callback2).toHaveBeenCalledTimes(1); // Buffer flush
      expect(callback2).toHaveBeenCalledWith({ action: "history" }, undefined);
    });

    it("returns unsubscribe function", () => {
      const store = createWidgetStore();
      const callback = vi.fn();

      store.createModel("comm-1", { value: 42 });
      const unsubscribe = store.subscribeToCustomMessage("comm-1", callback);

      store.emitCustomMessage("comm-1", { action: "first" });
      expect(callback).toHaveBeenCalledTimes(1);

      unsubscribe();
      store.emitCustomMessage("comm-1", { action: "second" });
      expect(callback).toHaveBeenCalledTimes(1); // Not called again
    });

    it("cleans up buffer when model deleted", () => {
      const store = createWidgetStore();
      const callback = vi.fn();

      store.createModel("comm-1", { value: 42 });
      store.emitCustomMessage("comm-1", { action: "buffered" });
      store.deleteModel("comm-1");

      // Re-create and subscribe - should not receive old buffer
      store.createModel("comm-1", { value: 100 });
      store.subscribeToCustomMessage("comm-1", callback);

      expect(callback).not.toHaveBeenCalled();
    });
  });

  describe("getSnapshot immutability", () => {
    it("returns same reference when no changes", () => {
      const store = createWidgetStore();
      store.createModel("comm-1", { value: 42 });

      const snapshot1 = store.getSnapshot();
      const snapshot2 = store.getSnapshot();

      expect(snapshot1).toBe(snapshot2);
    });

    it("returns new reference after change", () => {
      const store = createWidgetStore();
      store.createModel("comm-1", { value: 42 });

      const snapshot1 = store.getSnapshot();
      store.updateModel("comm-1", { value: 50 });
      const snapshot2 = store.getSnapshot();

      expect(snapshot1).not.toBe(snapshot2);
    });
  });
});
