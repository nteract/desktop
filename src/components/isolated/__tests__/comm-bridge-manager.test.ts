/**
 * Tests for comm-bridge-manager.ts - Widget communication bridge for isolated iframes.
 *
 * The CommBridgeManager proxies widget communication between the parent window's
 * widget store and isolated iframes. It handles message buffering, state sync,
 * and echo prevention.
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  WidgetModel,
  WidgetStore,
} from "@/components/widgets/widget-store";
import {
  CommBridgeManager,
  createCommBridgeManager,
} from "../comm-bridge-manager";
import type { IsolatedFrameHandle } from "../isolated-frame";

// Helper to create a mock WidgetStore
function createMockStore(initialModels: Map<string, WidgetModel> = new Map()): {
  store: WidgetStore;
  triggerChange: () => void;
  listeners: Set<() => void>;
  customMessageListeners: Map<
    string,
    Set<(content: Record<string, unknown>, buffers?: DataView[]) => void>
  >;
} {
  const models = initialModels;
  const listeners = new Set<() => void>();
  const customMessageListeners = new Map<
    string,
    Set<(content: Record<string, unknown>, buffers?: DataView[]) => void>
  >();

  const store: WidgetStore = {
    subscribe: vi.fn((listener) => {
      listeners.add(listener);
      return () => listeners.delete(listener);
    }),
    getSnapshot: vi.fn(() => models),
    getModel: vi.fn((id) => models.get(id)),
    createModel: vi.fn(),
    updateModel: vi.fn(),
    deleteModel: vi.fn(),
    wasModelClosed: vi.fn(() => false),
    subscribeToKey: vi.fn(() => () => {}),
    emitCustomMessage: vi.fn(),
    subscribeToCustomMessage: vi.fn((commId, callback) => {
      if (!customMessageListeners.has(commId)) {
        customMessageListeners.set(commId, new Set());
      }
      customMessageListeners.get(commId)!.add(callback);
      return () => customMessageListeners.get(commId)?.delete(callback);
    }),
  };

  return {
    store,
    triggerChange: () => listeners.forEach((l) => l()),
    listeners,
    customMessageListeners,
  };
}

// Helper to create a mock IsolatedFrameHandle
function createMockFrame(): {
  frame: IsolatedFrameHandle;
  sendCalls: unknown[];
} {
  const sendCalls: unknown[] = [];
  const frame: IsolatedFrameHandle = {
    send: vi.fn((msg) => sendCalls.push(msg)),
    render: vi.fn(),
    eval: vi.fn(),
    setTheme: vi.fn(),
    clear: vi.fn(),
    search: vi.fn(),
    searchNavigate: vi.fn(),
    isReady: true,
    isIframeReady: true,
  };
  return { frame, sendCalls };
}

// Helper to create a WidgetModel
function createModel(
  id: string,
  state: Record<string, unknown> = {},
  modelModule = "@jupyter-widgets/controls",
): WidgetModel {
  return {
    id,
    state: { _model_name: "TestModel", _model_module: modelModule, ...state },
    buffers: [],
    modelName: "TestModel",
    modelModule,
  };
}

describe("CommBridgeManager", () => {
  let mockStore: ReturnType<typeof createMockStore>;
  let mockFrame: ReturnType<typeof createMockFrame>;
  let sendUpdate: ReturnType<typeof vi.fn>;
  let sendCustom: ReturnType<typeof vi.fn>;
  let closeComm: ReturnType<typeof vi.fn>;

  beforeEach(() => {
    mockStore = createMockStore();
    mockFrame = createMockFrame();
    sendUpdate = vi.fn();
    sendCustom = vi.fn();
    closeComm = vi.fn();
    vi.clearAllMocks();
  });

  function createManager(storeOverride?: WidgetStore) {
    return new CommBridgeManager({
      frame: mockFrame.frame,
      store: storeOverride ?? mockStore.store,
      sendUpdate,
      sendCustom,
      closeComm,
    });
  }

  describe("construction", () => {
    it("sends bridge_ready on construction", () => {
      createManager();

      expect(mockFrame.frame.send).toHaveBeenCalledWith({
        type: "bridge_ready",
      });
    });

    it("subscribes to store changes", () => {
      createManager();

      expect(mockStore.store.subscribe).toHaveBeenCalled();
    });
  });

  describe("message buffering before widget_ready", () => {
    it("buffers comm_open messages until widget_ready", () => {
      const manager = createManager();

      manager.sendCommOpen("comm-1", "jupyter.widget", { value: 1 });

      // Should not have sent comm_open yet (only bridge_ready)
      expect(mockFrame.sendCalls).toHaveLength(1);
      expect(mockFrame.sendCalls[0]).toEqual({ type: "bridge_ready" });
    });

    it("buffers comm_msg messages until widget_ready", () => {
      const manager = createManager();

      manager.sendCommMsg("comm-1", "update", { value: 2 });

      // Only bridge_ready should be sent
      expect(mockFrame.sendCalls).toHaveLength(1);
    });

    it("buffers comm_close messages until widget_ready", () => {
      const manager = createManager();

      manager.sendCommClose("comm-1");

      // Only bridge_ready should be sent
      expect(mockFrame.sendCalls).toHaveLength(1);
    });

    it("flushes all buffered messages in order on widget_ready", () => {
      const manager = createManager();

      // Buffer several messages
      manager.sendCommOpen("comm-1", "jupyter.widget", { value: 1 });
      manager.sendCommMsg("comm-1", "update", { value: 2 });
      manager.sendCommClose("comm-1");

      // Trigger widget_ready
      manager.handleIframeMessage({ type: "widget_ready" });

      // Should have: bridge_ready, comm_sync, then buffered messages
      expect(mockFrame.sendCalls.length).toBeGreaterThanOrEqual(4);

      // Find the buffered messages (after comm_sync)
      const commOpenIdx = mockFrame.sendCalls.findIndex(
        (msg: unknown) => (msg as { type: string }).type === "comm_open",
      );
      const commMsgIdx = mockFrame.sendCalls.findIndex(
        (msg: unknown) => (msg as { type: string }).type === "comm_msg",
      );
      const commCloseIdx = mockFrame.sendCalls.findIndex(
        (msg: unknown) => (msg as { type: string }).type === "comm_close",
      );

      // All should exist and be in order
      expect(commOpenIdx).toBeGreaterThan(-1);
      expect(commMsgIdx).toBeGreaterThan(commOpenIdx);
      expect(commCloseIdx).toBeGreaterThan(commMsgIdx);
    });

    it("sends messages directly after widget_ready", () => {
      const manager = createManager();

      // First trigger widget_ready
      manager.handleIframeMessage({ type: "widget_ready" });

      const callsBeforeSend = mockFrame.sendCalls.length;

      // Now send a message - should go directly
      manager.sendCommOpen("comm-2", "jupyter.widget", { value: 10 });

      expect(mockFrame.sendCalls.length).toBe(callsBeforeSend + 1);
      expect(mockFrame.sendCalls[mockFrame.sendCalls.length - 1]).toEqual({
        type: "comm_open",
        payload: {
          commId: "comm-2",
          targetName: "jupyter.widget",
          state: { value: 10 },
          buffers: undefined,
        },
      });
    });
  });

  describe("handleWidgetReady", () => {
    it("sends comm_sync with all existing models from store", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1", { value: 1 })],
        ["comm-2", createModel("comm-2", { value: 2 })],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });

      const commSync = mockFrame.sendCalls.find(
        (msg: unknown) => (msg as { type: string }).type === "comm_sync",
      ) as { type: string; payload: { models: unknown[] } };

      expect(commSync).toBeDefined();
      expect(commSync.payload.models).toHaveLength(2);
    });

    it("tracks sent models to avoid duplicates in syncModels", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1", { value: 1 })],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      // Trigger widget_ready
      manager.handleIframeMessage({ type: "widget_ready" });
      const callsAfterReady = mockFrame.sendCalls.length;

      // Trigger store change - model already sent, no state change
      storeWithModels.triggerChange();

      // Should not send comm_open again (model already tracked as sent)
      const newCommOpens = mockFrame.sendCalls
        .slice(callsAfterReady)
        .filter(
          (msg: unknown) => (msg as { type: string }).type === "comm_open",
        );
      expect(newCommOpens).toHaveLength(0);
    });

    it("subscribes to custom messages for each model", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1")],
        ["comm-2", createModel("comm-2")],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });

      expect(
        storeWithModels.store.subscribeToCustomMessage,
      ).toHaveBeenCalledWith("comm-1", expect.any(Function));
      expect(
        storeWithModels.store.subscribeToCustomMessage,
      ).toHaveBeenCalledWith("comm-2", expect.any(Function));
    });
  });

  describe("handleIframeMessage", () => {
    it("processes widget_ready message", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1")],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });

      // Should have sent comm_sync with the model
      const commSync = mockFrame.sendCalls.find(
        (msg: unknown) => (msg as { type: string }).type === "comm_sync",
      );
      expect(commSync).toBeDefined();
    });

    it("processes widget_comm_msg with update method", () => {
      const manager = createManager();
      manager.handleIframeMessage({ type: "widget_ready" });

      manager.handleIframeMessage({
        type: "widget_comm_msg",
        payload: {
          commId: "comm-1",
          method: "update",
          data: { value: 42 },
        },
      });

      expect(mockStore.store.updateModel).toHaveBeenCalledWith(
        "comm-1",
        { value: 42 },
        undefined,
      );
      expect(sendUpdate).toHaveBeenCalledWith(
        "comm-1",
        { value: 42 },
        undefined,
      );
    });

    it("processes widget_comm_msg with custom method", () => {
      const manager = createManager();
      manager.handleIframeMessage({ type: "widget_ready" });

      manager.handleIframeMessage({
        type: "widget_comm_msg",
        payload: {
          commId: "comm-1",
          method: "custom",
          data: { action: "reset" },
        },
      });

      // Custom messages don't update store, they go directly to kernel
      expect(mockStore.store.updateModel).not.toHaveBeenCalled();
      expect(sendCustom).toHaveBeenCalledWith(
        "comm-1",
        { action: "reset" },
        undefined,
      );
    });

    it("processes widget_comm_close message", () => {
      const manager = createManager();
      manager.handleIframeMessage({ type: "widget_ready" });

      manager.handleIframeMessage({
        type: "widget_comm_close",
        payload: { commId: "comm-1" },
      });

      expect(mockStore.store.deleteModel).toHaveBeenCalledWith("comm-1");
      expect(closeComm).toHaveBeenCalledWith("comm-1");
    });

    it("ignores unknown message types", () => {
      const manager = createManager();

      // Should not throw
      manager.handleIframeMessage({ type: "unknown_type" } as never);

      // Store should not be affected
      expect(mockStore.store.updateModel).not.toHaveBeenCalled();
      expect(mockStore.store.deleteModel).not.toHaveBeenCalled();
    });
  });

  describe("syncModels (state synchronization)", () => {
    it("sends comm_open for new models in store", () => {
      const manager = createManager();
      manager.handleIframeMessage({ type: "widget_ready" });

      // Simulate adding a new model to the store
      const newModel = createModel("new-comm", { value: 100 });
      (mockStore.store.getSnapshot as ReturnType<typeof vi.fn>).mockReturnValue(
        new Map([["new-comm", newModel]]),
      );

      // Trigger store change
      mockStore.triggerChange();

      // Should have sent comm_open for the new model
      const commOpen = mockFrame.sendCalls.find(
        (msg: unknown) =>
          (msg as { type: string; payload?: { commId: string } }).type ===
            "comm_open" &&
          (msg as { payload: { commId: string } }).payload.commId ===
            "new-comm",
      );
      expect(commOpen).toBeDefined();
    });

    it("sends comm_close for deleted models", () => {
      // Start with a model
      const models = new Map<string, WidgetModel>([
        ["comm-to-delete", createModel("comm-to-delete")],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });

      // Now remove the model
      (
        storeWithModels.store.getSnapshot as ReturnType<typeof vi.fn>
      ).mockReturnValue(new Map());
      storeWithModels.triggerChange();

      // Should have sent comm_close
      const commClose = mockFrame.sendCalls.find(
        (msg: unknown) =>
          (msg as { type: string; payload?: { commId: string } }).type ===
            "comm_close" &&
          (msg as { payload: { commId: string } }).payload.commId ===
            "comm-to-delete",
      );
      expect(commClose).toBeDefined();
    });

    it("sends comm_msg for changed model state", () => {
      // Start with a model
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1", { value: 1 })],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });

      // Update the model's state
      const updatedModel = createModel("comm-1", { value: 99 });
      (
        storeWithModels.store.getSnapshot as ReturnType<typeof vi.fn>
      ).mockReturnValue(new Map([["comm-1", updatedModel]]));
      storeWithModels.triggerChange();

      // Should have sent comm_msg with update
      const commMsg = mockFrame.sendCalls.find(
        (msg: unknown) =>
          (
            msg as {
              type: string;
              payload?: { commId: string; method: string };
            }
          ).type === "comm_msg" &&
          (msg as { payload: { commId: string } }).payload.commId === "comm-1",
      );
      expect(commMsg).toBeDefined();
    });

    it("only sends delta for changed keys", () => {
      // Start with a model with multiple keys
      const models = new Map<string, WidgetModel>([
        [
          "comm-1",
          createModel("comm-1", {
            value: 1,
            label: "test",
            other: "unchanged",
          }),
        ],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });
      const callsAfterReady = mockFrame.sendCalls.length;

      // Update only the value key
      const updatedModel = createModel("comm-1", {
        value: 2,
        label: "test",
        other: "unchanged",
      });
      (
        storeWithModels.store.getSnapshot as ReturnType<typeof vi.fn>
      ).mockReturnValue(new Map([["comm-1", updatedModel]]));
      storeWithModels.triggerChange();

      // Find the comm_msg that was sent
      const commMsg = mockFrame.sendCalls
        .slice(callsAfterReady)
        .find(
          (msg: unknown) => (msg as { type: string }).type === "comm_msg",
        ) as {
        type: string;
        payload: { data: Record<string, unknown> };
      };

      expect(commMsg).toBeDefined();
      // Should only contain the changed key
      expect(commMsg.payload.data).toHaveProperty("value", 2);
      expect(commMsg.payload.data).not.toHaveProperty("label");
      expect(commMsg.payload.data).not.toHaveProperty("other");
    });

    it("does not send update for unchanged state", () => {
      // Start with a model
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1", { value: 1 })],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });
      const callsAfterReady = mockFrame.sendCalls.length;

      // Trigger change but with same state (no actual changes)
      storeWithModels.triggerChange();

      // Should not have sent any new comm_msg
      const newCommMsgs = mockFrame.sendCalls
        .slice(callsAfterReady)
        .filter(
          (msg: unknown) => (msg as { type: string }).type === "comm_msg",
        );
      expect(newCommMsgs).toHaveLength(0);
    });
  });

  describe("isProcessingIframeUpdate flag (echo prevention)", () => {
    it("prevents echoing iframe updates back to iframe", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1", { value: 1 })],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });
      const callsAfterReady = mockFrame.sendCalls.length;

      // Simulate iframe sending an update
      manager.handleIframeMessage({
        type: "widget_comm_msg",
        payload: {
          commId: "comm-1",
          method: "update",
          data: { value: 42 },
        },
      });

      // The store.subscribe callback should be suppressed during this update
      // so no comm_msg should be sent back to iframe
      const newCommMsgs = mockFrame.sendCalls
        .slice(callsAfterReady)
        .filter(
          (msg: unknown) => (msg as { type: string }).type === "comm_msg",
        );
      expect(newCommMsgs).toHaveLength(0);
    });

    it("flag is reset after update even if error occurs", () => {
      const manager = createManager();
      manager.handleIframeMessage({ type: "widget_ready" });

      // Make updateModel throw
      (
        mockStore.store.updateModel as ReturnType<typeof vi.fn>
      ).mockImplementation(() => {
        throw new Error("Test error");
      });

      // This should not leave the flag stuck on
      expect(() => {
        manager.handleIframeMessage({
          type: "widget_comm_msg",
          payload: {
            commId: "comm-1",
            method: "update",
            data: { value: 42 },
          },
        });
      }).toThrow("Test error");

      // Flag should be reset, so subsequent store changes should trigger sync
      const callsAfterError = mockFrame.sendCalls.length;

      // Add a new model
      const newModel = createModel("new-comm", { value: 100 });
      (mockStore.store.getSnapshot as ReturnType<typeof vi.fn>).mockReturnValue(
        new Map([["new-comm", newModel]]),
      );
      mockStore.triggerChange();

      // Should have sent comm_open (flag was reset)
      expect(mockFrame.sendCalls.length).toBeGreaterThan(callsAfterError);
    });
  });

  describe("custom messages", () => {
    it("converts DataView buffers to ArrayBuffer", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1")],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });

      // Get the callback that was registered
      const subscribeCall = (
        storeWithModels.store.subscribeToCustomMessage as ReturnType<
          typeof vi.fn
        >
      ).mock.calls[0];
      const callback = subscribeCall[1] as (
        content: Record<string, unknown>,
        buffers?: DataView[],
      ) => void;

      // Simulate receiving a custom message with DataView buffers
      const buffer = new ArrayBuffer(8);
      const dataView = new DataView(buffer);
      callback({ action: "draw" }, [dataView]);

      // Find the comm_msg sent with custom
      const commMsg = mockFrame.sendCalls.find(
        (msg: unknown) =>
          (msg as { type: string; payload?: { method: string } }).type ===
            "comm_msg" &&
          (msg as { payload: { method: string } }).payload.method === "custom",
      ) as { payload: { buffers: ArrayBuffer[] } };

      expect(commMsg).toBeDefined();
      // The buffer should be converted to ArrayBuffer
      expect(commMsg.payload.buffers?.[0]).toBe(buffer);
    });

    it("handles undefined buffers gracefully", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1")],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });

      // Get the callback
      const subscribeCall = (
        storeWithModels.store.subscribeToCustomMessage as ReturnType<
          typeof vi.fn
        >
      ).mock.calls[0];
      const callback = subscribeCall[1] as (
        content: Record<string, unknown>,
        buffers?: DataView[],
      ) => void;

      // Call with undefined buffers - should not throw
      expect(() => callback({ action: "draw" }, undefined)).not.toThrow();
    });

    it("does not double-subscribe to same model", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1")],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      // Trigger widget_ready twice
      manager.handleIframeMessage({ type: "widget_ready" });
      manager.handleIframeMessage({ type: "widget_ready" });

      // Should only have subscribed once per model
      const subscribeCalls = (
        storeWithModels.store.subscribeToCustomMessage as ReturnType<
          typeof vi.fn
        >
      ).mock.calls.filter((call: unknown[]) => call[0] === "comm-1");

      expect(subscribeCalls).toHaveLength(1);
    });

    it("unsubscribes when model is deleted", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-to-delete", createModel("comm-to-delete")],
      ]);
      const storeWithModels = createMockStore(models);

      // Track unsubscribe calls
      const unsubscribeFn = vi.fn();
      (
        storeWithModels.store.subscribeToCustomMessage as ReturnType<
          typeof vi.fn
        >
      ).mockReturnValue(unsubscribeFn);

      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });

      // Now delete the model
      (
        storeWithModels.store.getSnapshot as ReturnType<typeof vi.fn>
      ).mockReturnValue(new Map());
      storeWithModels.triggerChange();

      // Unsubscribe should have been called
      expect(unsubscribeFn).toHaveBeenCalled();
    });
  });

  describe("dispose", () => {
    it("unsubscribes from store", () => {
      const manager = createManager();

      manager.dispose();

      // The listener should be removed
      expect(mockStore.listeners.size).toBe(0);
    });

    it("unsubscribes all custom message listeners", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1")],
        ["comm-2", createModel("comm-2")],
      ]);
      const storeWithModels = createMockStore(models);

      const unsubscribeFns = [vi.fn(), vi.fn()];
      let callCount = 0;
      (
        storeWithModels.store.subscribeToCustomMessage as ReturnType<
          typeof vi.fn
        >
      ).mockImplementation(() => {
        return unsubscribeFns[callCount++];
      });

      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });
      manager.dispose();

      // Both unsubscribe functions should have been called
      expect(unsubscribeFns[0]).toHaveBeenCalled();
      expect(unsubscribeFns[1]).toHaveBeenCalled();
    });

    it("clears message buffer", () => {
      const manager = createManager();

      // Buffer some messages
      manager.sendCommOpen("comm-1", "jupyter.widget", {});
      manager.sendCommMsg("comm-1", "update", {});

      manager.dispose();

      // Now trigger widget_ready - buffered messages should be gone
      manager.handleIframeMessage({ type: "widget_ready" });

      // Should not have sent the buffered messages
      const commOpen = mockFrame.sendCalls.find(
        (msg: unknown) =>
          (msg as { type: string; payload?: { commId: string } }).type ===
            "comm_open" &&
          (msg as { payload: { commId: string } }).payload?.commId === "comm-1",
      );
      expect(commOpen).toBeUndefined();
    });

    it("clears sent models tracking", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1")],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });
      manager.dispose();

      // After dispose and re-widget_ready, it should sync all models again
      manager.handleIframeMessage({ type: "widget_ready" });

      const commSyncs = mockFrame.sendCalls.filter(
        (msg: unknown) => (msg as { type: string }).type === "comm_sync",
      );

      // Should have two comm_sync messages (before and after dispose)
      expect(commSyncs).toHaveLength(2);
    });

    it("resets widget ready flag", () => {
      const manager = createManager();

      manager.handleIframeMessage({ type: "widget_ready" });
      manager.dispose();

      // After dispose, messages should be buffered again
      manager.sendCommOpen("comm-1", "jupyter.widget", {});

      // Find if comm_open was sent directly
      const directCommOpen = mockFrame.sendCalls.find(
        (msg: unknown) =>
          (msg as { type: string; payload?: { commId: string } }).type ===
            "comm_open" &&
          (msg as { payload: { commId: string } }).payload?.commId === "comm-1",
      );

      // Should not have been sent (should be buffered)
      expect(directCommOpen).toBeUndefined();
    });
  });

  describe("getChangedKeys", () => {
    // Note: getChangedKeys is private, so we test it indirectly through syncModels

    it("detects added keys", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1", { existing: 1 })],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });
      const callsAfterReady = mockFrame.sendCalls.length;

      // Add a new key
      const updatedModel = createModel("comm-1", {
        existing: 1,
        newKey: "added",
      });
      (
        storeWithModels.store.getSnapshot as ReturnType<typeof vi.fn>
      ).mockReturnValue(new Map([["comm-1", updatedModel]]));
      storeWithModels.triggerChange();

      const commMsg = mockFrame.sendCalls
        .slice(callsAfterReady)
        .find(
          (msg: unknown) => (msg as { type: string }).type === "comm_msg",
        ) as {
        payload: { data: Record<string, unknown> };
      };

      expect(commMsg).toBeDefined();
      expect(commMsg.payload.data).toHaveProperty("newKey", "added");
    });

    it("detects removed keys", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1", { toRemove: 1, keep: 2 })],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });
      const callsAfterReady = mockFrame.sendCalls.length;

      // Remove a key (by not including it in new state)
      const updatedModel = createModel("comm-1", { keep: 2 });
      (
        storeWithModels.store.getSnapshot as ReturnType<typeof vi.fn>
      ).mockReturnValue(new Map([["comm-1", updatedModel]]));
      storeWithModels.triggerChange();

      const commMsg = mockFrame.sendCalls
        .slice(callsAfterReady)
        .find(
          (msg: unknown) => (msg as { type: string }).type === "comm_msg",
        ) as {
        payload: { data: Record<string, unknown> };
      };

      expect(commMsg).toBeDefined();
      // The removed key should be in the delta with undefined value
      expect("toRemove" in commMsg.payload.data).toBe(true);
    });

    it("detects changed primitive values", () => {
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1", { value: 1 })],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });
      const callsAfterReady = mockFrame.sendCalls.length;

      // Change value
      const updatedModel = createModel("comm-1", { value: 999 });
      (
        storeWithModels.store.getSnapshot as ReturnType<typeof vi.fn>
      ).mockReturnValue(new Map([["comm-1", updatedModel]]));
      storeWithModels.triggerChange();

      const commMsg = mockFrame.sendCalls
        .slice(callsAfterReady)
        .find(
          (msg: unknown) => (msg as { type: string }).type === "comm_msg",
        ) as {
        payload: { data: Record<string, unknown> };
      };

      expect(commMsg).toBeDefined();
      expect(commMsg.payload.data).toHaveProperty("value", 999);
    });

    it("does not send update for object-only reference changes with same content", () => {
      const obj1 = { nested: 1 };
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1", { obj: obj1 })],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });
      const callsAfterReady = mockFrame.sendCalls.length;

      // New object reference (even with same content)
      const obj2 = { nested: 1 };
      const updatedModel = createModel("comm-1", { obj: obj2 });
      (
        storeWithModels.store.getSnapshot as ReturnType<typeof vi.fn>
      ).mockReturnValue(new Map([["comm-1", updatedModel]]));
      storeWithModels.triggerChange();

      const commMsg = mockFrame.sendCalls
        .slice(callsAfterReady)
        .find(
          (msg: unknown) => (msg as { type: string }).type === "comm_msg",
        ) as {
        payload: { data: Record<string, unknown> };
      };

      expect(commMsg).toBeUndefined();
    });

    it("returns empty array when no changes (same reference)", () => {
      const obj = { nested: 1 };
      const models = new Map<string, WidgetModel>([
        ["comm-1", createModel("comm-1", { obj })],
      ]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });
      const callsAfterReady = mockFrame.sendCalls.length;

      // Same object reference
      const updatedModel = createModel("comm-1", { obj });
      (
        storeWithModels.store.getSnapshot as ReturnType<typeof vi.fn>
      ).mockReturnValue(new Map([["comm-1", updatedModel]]));
      storeWithModels.triggerChange();

      // Should not have sent any comm_msg
      const commMsgs = mockFrame.sendCalls
        .slice(callsAfterReady)
        .filter(
          (msg: unknown) => (msg as { type: string }).type === "comm_msg",
        );

      expect(commMsgs).toHaveLength(0);
    });

    it("detects in-place mutations on nested state values", () => {
      const outputs: Array<Record<string, unknown>> = [];
      const model = createModel("comm-1", { outputs });
      const models = new Map<string, WidgetModel>([["comm-1", model]]);
      const storeWithModels = createMockStore(models);
      const manager = new CommBridgeManager({
        frame: mockFrame.frame,
        store: storeWithModels.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      manager.handleIframeMessage({ type: "widget_ready" });
      const callsAfterReady = mockFrame.sendCalls.length;

      // Mutate outputs in-place (same array reference)
      outputs.push({
        output_type: "stream",
        name: "stdout",
        text: "clicked",
      });
      storeWithModels.triggerChange();

      const commMsg = mockFrame.sendCalls
        .slice(callsAfterReady)
        .find(
          (msg: unknown) => (msg as { type: string }).type === "comm_msg",
        ) as
        | {
            type: string;
            payload: {
              commId: string;
              method: string;
              data: Record<string, unknown>;
            };
          }
        | undefined;

      expect(commMsg).toBeDefined();
      expect(commMsg!.payload.commId).toBe("comm-1");
      expect(commMsg!.payload.method).toBe("update");
      expect(commMsg!.payload.data.outputs).toBeDefined();
    });
  });

  describe("createCommBridgeManager factory", () => {
    it("creates a CommBridgeManager instance", () => {
      const manager = createCommBridgeManager({
        frame: mockFrame.frame,
        store: mockStore.store,
        sendUpdate,
        sendCustom,
        closeComm,
      });

      expect(manager).toBeInstanceOf(CommBridgeManager);
    });
  });
});
