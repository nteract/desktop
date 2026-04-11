// @vitest-environment jsdom
import { clearMocks, mockIPC } from "@tauri-apps/api/mocks";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import {
  _testGetGeneration,
  _testReset,
  getBlobPort,
  refreshBlobPort,
  resetBlobPort,
} from "../blob-port";

// Mock invoke — mockIPC delegates to this so tests can control return values
const mockInvoke = vi.fn();

beforeEach(() => {
  _testReset();
  mockIPC((cmd, args) => mockInvoke(cmd, args));
});

afterEach(() => {
  mockInvoke.mockReset();
  clearMocks();
});

describe("blob-port store", () => {
  it("starts with null", () => {
    expect(getBlobPort()).toBeNull();
  });

  it("refreshBlobPort fetches and caches the port", async () => {
    mockInvoke.mockResolvedValueOnce(12345);
    const port = await refreshBlobPort();
    expect(port).toBe(12345);
    expect(getBlobPort()).toBe(12345);
  });

  it("deduplicates concurrent refresh calls", async () => {
    mockInvoke.mockResolvedValueOnce(9999);
    const [a, b, c] = await Promise.all([
      refreshBlobPort(),
      refreshBlobPort(),
      refreshBlobPort(),
    ]);
    expect(a).toBe(9999);
    expect(b).toBe(9999);
    expect(c).toBe(9999);
    // Only one IPC call
    expect(mockInvoke).toHaveBeenCalledTimes(1);
  });

  it("resetBlobPort clears the port", async () => {
    mockInvoke.mockResolvedValueOnce(12345);
    await refreshBlobPort();
    expect(getBlobPort()).toBe(12345);

    resetBlobPort();
    expect(getBlobPort()).toBeNull();
  });

  it("resetBlobPort increments generation", () => {
    const gen0 = _testGetGeneration();
    resetBlobPort();
    expect(_testGetGeneration()).toBe(gen0 + 1);
  });

  it("discards stale refresh after reset", async () => {
    // Simulate a slow refresh that resolves after a reset
    let resolveInvoke: (v: number) => void;
    mockInvoke.mockReturnValueOnce(
      new Promise<number>((r) => {
        resolveInvoke = r;
      }),
    );

    const refreshPromise = refreshBlobPort();

    // Reset while refresh is in flight
    resetBlobPort();
    expect(getBlobPort()).toBeNull();

    // Now the slow invoke resolves with the OLD port
    resolveInvoke!(54321);
    await refreshPromise;

    // The stale result should be discarded — port stays null
    expect(getBlobPort()).toBeNull();
  });

  it("retries on failure", async () => {
    mockInvoke
      .mockRejectedValueOnce(new Error("not ready"))
      .mockRejectedValueOnce(new Error("not ready"))
      .mockResolvedValueOnce(7777);

    const port = await refreshBlobPort();
    expect(port).toBe(7777);
    expect(mockInvoke).toHaveBeenCalledTimes(3);
  });

  it("allows fresh refresh after reset", async () => {
    mockInvoke.mockResolvedValueOnce(1111);
    await refreshBlobPort();
    expect(getBlobPort()).toBe(1111);

    resetBlobPort();
    expect(getBlobPort()).toBeNull();

    mockInvoke.mockResolvedValueOnce(2222);
    await refreshBlobPort();
    expect(getBlobPort()).toBe(2222);
  });
});
