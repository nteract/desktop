// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";

// Mock the webview module BEFORE importing TauriTransport so the
// transport picks up the mock. getCurrentWebview().listen is what
// we need to exercise — we don't care about any other Tauri API.
const capturedListeners: Array<(event: { payload: number[] }) => void> = [];
const mockUnlisten = vi.fn();

vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({
    listen: vi.fn(async (_event: string, cb: (event: { payload: number[] }) => void) => {
      capturedListeners.push(cb);
      return mockUnlisten;
    }),
  }),
}));

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
  isTauri: () => false, // logger.ts uses this — disable Tauri path in tests
}));

vi.mock("@tauri-apps/plugin-log", () => ({
  attachConsole: vi.fn(),
  debug: vi.fn(),
  info: vi.fn(),
  warn: vi.fn(),
  error: vi.fn(),
}));

const loggerMod = await import("../logger");
const loggerErrorSpy = vi.spyOn(loggerMod.logger, "error");

// Import after the mocks are in place.
const { TauriTransport } = await import("../tauri-transport");

beforeEach(() => {
  capturedListeners.length = 0;
  mockUnlisten.mockReset();
  loggerErrorSpy.mockClear();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe("TauriTransport.onFrame", () => {
  it("survives a throwing frame handler — listener stays alive, error is logged", async () => {
    // Before this fix, an uncaught exception in the handler caused
    // Tauri to silently drop the event listener for the rest of the
    // webview's lifetime. Daemon frames kept arriving; nothing was
    // listening. The webview could only be recovered by reload.
    const transport = new TauriTransport();

    let firstHandlerCalled = 0;
    let secondHandlerCalled = 0;

    transport.onFrame((payload) => {
      firstHandlerCalled++;
      if (firstHandlerCalled === 1) {
        throw new Error("boom — malformed frame");
      }
      secondHandlerCalled += payload.length;
    });

    // Flush the listen-registration promise.
    await Promise.resolve();

    expect(capturedListeners).toHaveLength(1);
    const wrappedCallback = capturedListeners[0];

    // First frame — the user handler throws. The Tauri-facing
    // callback must not re-throw, or Tauri will drop it.
    expect(() => wrappedCallback({ payload: [0x00, 0x01] })).not.toThrow();
    expect(loggerErrorSpy).toHaveBeenCalledTimes(1);
    expect(loggerErrorSpy.mock.calls[0][0]).toContain("notebook:frame handler threw");

    // Second frame — the handler no longer throws (different branch).
    // The listener is still live and delivering events.
    expect(() => wrappedCallback({ payload: [0x00, 0x02, 0x03] })).not.toThrow();
    expect(secondHandlerCalled).toBe(3);
  });
});
