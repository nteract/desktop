/**
 * Tests for JsonRpcTransport — lightweight JSON-RPC 2.0 over postMessage.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { JsonRpcTransport } from "../jsonrpc-transport";

function createMockWindow(): {
  window: Window;
  postMessageCalls: Array<{
    message: unknown;
    origin: string;
    transfer?: Transferable[];
  }>;
} {
  const postMessageCalls: Array<{
    message: unknown;
    origin: string;
    transfer?: Transferable[];
  }> = [];

  const mockWindow = {
    postMessage: vi.fn((message: unknown, origin: string, transfer?: Transferable[]) => {
      postMessageCalls.push({ message, origin, transfer });
    }),
  } as unknown as Window;

  return { window: mockWindow, postMessageCalls };
}

describe("JsonRpcTransport", () => {
  let mockTarget: ReturnType<typeof createMockWindow>;

  beforeEach(() => {
    mockTarget = createMockWindow();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  describe("notify", () => {
    it("sends JSON-RPC 2.0 notification", () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      transport.notify("nteract/renderOutput", {
        mimeType: "text/plain",
        data: "hello",
      });

      expect(mockTarget.postMessageCalls).toHaveLength(1);
      expect(mockTarget.postMessageCalls[0].message).toEqual({
        jsonrpc: "2.0",
        method: "nteract/renderOutput",
        params: { mimeType: "text/plain", data: "hello" },
      });
    });

    it("extracts ArrayBuffers as transferables", () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      const buffer = new ArrayBuffer(16);
      transport.notify("nteract/commMsg", {
        commId: "comm-1",
        buffers: [buffer],
      });

      expect(mockTarget.postMessageCalls[0].transfer).toEqual([buffer]);
    });

    it("deduplicates ArrayBuffer transferables", () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      const buffer = new ArrayBuffer(8);
      transport.notify("test", { a: buffer, b: buffer });

      expect(mockTarget.postMessageCalls[0].transfer).toHaveLength(1);
    });

    it("sends without transfer list when no buffers", () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      transport.notify("nteract/clearOutputs");

      expect(mockTarget.postMessageCalls[0].transfer).toBeUndefined();
    });
  });

  describe("request", () => {
    it("sends JSON-RPC 2.0 request with id", () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      // Don't await — just check it was sent
      transport.request("nteract/search", { query: "test" });

      const sent = mockTarget.postMessageCalls[0].message as {
        jsonrpc: string;
        id: number;
        method: string;
        params: unknown;
      };
      expect(sent.jsonrpc).toBe("2.0");
      expect(sent.id).toBe(1);
      expect(sent.method).toBe("nteract/search");
      expect(sent.params).toEqual({ query: "test" });
    });

    it("increments request ids", () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      transport.request("a");
      transport.request("b");

      const msg1 = mockTarget.postMessageCalls[0].message as { id: number };
      const msg2 = mockTarget.postMessageCalls[1].message as { id: number };
      expect(msg1.id).toBe(1);
      expect(msg2.id).toBe(2);
    });
  });

  describe("receiving", () => {
    it("dispatches notifications to handlers", () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      const received: unknown[] = [];
      transport.onNotification("test/method", (params) => received.push(params));
      transport.start();

      window.dispatchEvent(
        new MessageEvent("message", {
          data: { jsonrpc: "2.0", method: "test/method", params: { x: 1 } },
          source: mockTarget.window,
        }),
      );

      expect(received).toHaveLength(1);
      expect(received[0]).toEqual({ x: 1 });
    });

    it("resolves request promises on response", async () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);
      transport.start();

      const promise = transport.request("test/search", { query: "foo" });

      // Simulate response
      const sentId = (mockTarget.postMessageCalls[0].message as { id: number }).id;
      window.dispatchEvent(
        new MessageEvent("message", {
          data: { jsonrpc: "2.0", id: sentId, result: { count: 5 } },
          source: mockTarget.window,
        }),
      );

      const result = await promise;
      expect(result).toEqual({ count: 5 });
    });

    it("rejects request promises on error response", async () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);
      transport.start();

      const promise = transport.request("test/fail");

      const sentId = (mockTarget.postMessageCalls[0].message as { id: number }).id;
      window.dispatchEvent(
        new MessageEvent("message", {
          data: {
            jsonrpc: "2.0",
            id: sentId,
            error: { code: -32000, message: "oops" },
          },
          source: mockTarget.window,
        }),
      );

      await expect(promise).rejects.toThrow("oops");
    });

    it("ignores messages from wrong source", () => {
      const otherWindow = {} as Window;
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      const received: unknown[] = [];
      transport.onNotification("test", (p) => received.push(p));
      transport.start();

      window.dispatchEvent(
        new MessageEvent("message", {
          data: { jsonrpc: "2.0", method: "test", params: {} },
          source: otherWindow,
        }),
      );

      expect(received).toHaveLength(0);
    });

    it("silently ignores non-JSON-RPC messages", () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      const received: unknown[] = [];
      transport.onNotification("ready", (p) => received.push(p));
      transport.start();

      // Legacy format — should be ignored
      window.dispatchEvent(
        new MessageEvent("message", {
          data: { type: "ready" },
          source: mockTarget.window,
        }),
      );

      expect(received).toHaveLength(0);
    });

    it("handles incoming requests and sends responses", async () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      transport.onRequest("test/eval", (params) => {
        return { success: true, result: String(params) };
      });
      transport.start();

      window.dispatchEvent(
        new MessageEvent("message", {
          data: {
            jsonrpc: "2.0",
            id: 42,
            method: "test/eval",
            params: { code: "1+1" },
          },
          source: mockTarget.window,
        }),
      );

      // Wait for async handler
      await new Promise((r) => setTimeout(r, 0));

      // Should have sent a response
      expect(mockTarget.postMessageCalls).toHaveLength(1);
      expect(mockTarget.postMessageCalls[0].message).toEqual({
        jsonrpc: "2.0",
        id: 42,
        result: {
          success: true,
          result: "[object Object]",
        },
      });
    });

    it("sends error response when request handler throws synchronously", async () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      transport.onRequest("test/throws", () => {
        throw new Error("sync kaboom");
      });
      transport.start();

      window.dispatchEvent(
        new MessageEvent("message", {
          data: {
            jsonrpc: "2.0",
            id: 99,
            method: "test/throws",
            params: {},
          },
          source: mockTarget.window,
        }),
      );

      // Response is sent synchronously for sync throws
      expect(mockTarget.postMessageCalls).toHaveLength(1);
      expect(mockTarget.postMessageCalls[0].message).toEqual({
        jsonrpc: "2.0",
        id: 99,
        error: { code: -32000, message: "sync kaboom" },
      });
    });

    it("handles cyclic objects in collectArrayBuffers without stack overflow", () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      // Create a cyclic object
      const obj: Record<string, unknown> = { a: 1 };
      obj.self = obj;

      // Should not throw
      transport.notify("test", obj);

      expect(mockTarget.postMessageCalls).toHaveLength(1);
    });
  });

  describe("stop", () => {
    it("stops receiving messages", () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      const received: unknown[] = [];
      transport.onNotification("test", (p) => received.push(p));
      transport.start();
      transport.stop();

      window.dispatchEvent(
        new MessageEvent("message", {
          data: { jsonrpc: "2.0", method: "test", params: {} },
          source: mockTarget.window,
        }),
      );

      expect(received).toHaveLength(0);
    });

    it("rejects pending requests", async () => {
      const transport = new JsonRpcTransport(mockTarget.window, mockTarget.window);

      const promise = transport.request("test");
      transport.stop();

      await expect(promise).rejects.toThrow("Transport stopped");
    });
  });
});
