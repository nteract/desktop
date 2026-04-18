// @vitest-environment jsdom
import type { NotebookHost } from "@nteract/notebook-host";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import {
  _testGetGeneration,
  _testReset,
  getBlobPort,
  refreshBlobPort,
  resetBlobPort,
  setBlobPortHost,
} from "../blob-port";

const mockPort = vi.fn();

/** Minimal host stub: only `blobs.port()` is exercised. */
function makeHost(): NotebookHost {
  return {
    name: "test",
    blobs: { port: mockPort },
  } as unknown as NotebookHost;
}

beforeEach(() => {
  _testReset();
  setBlobPortHost(makeHost());
});

afterEach(() => {
  mockPort.mockReset();
  setBlobPortHost(null);
});

describe("blob-port store", () => {
  it("starts with null", () => {
    expect(getBlobPort()).toBeNull();
  });

  it("refreshBlobPort fetches and caches the port", async () => {
    mockPort.mockResolvedValueOnce(12345);
    const port = await refreshBlobPort();
    expect(port).toBe(12345);
    expect(getBlobPort()).toBe(12345);
  });

  it("deduplicates concurrent refresh calls", async () => {
    mockPort.mockResolvedValueOnce(9999);
    const [a, b, c] = await Promise.all([
      refreshBlobPort(),
      refreshBlobPort(),
      refreshBlobPort(),
    ]);
    expect(a).toBe(9999);
    expect(b).toBe(9999);
    expect(c).toBe(9999);
    expect(mockPort).toHaveBeenCalledTimes(1);
  });

  it("resetBlobPort clears the port", async () => {
    mockPort.mockResolvedValueOnce(12345);
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
    let resolveFetch: (v: number) => void;
    mockPort.mockReturnValueOnce(
      new Promise<number>((r) => {
        resolveFetch = r;
      }),
    );

    const refreshPromise = refreshBlobPort();

    resetBlobPort();
    expect(getBlobPort()).toBeNull();

    resolveFetch!(54321);
    await refreshPromise;

    expect(getBlobPort()).toBeNull();
  });

  it("retries on failure", async () => {
    mockPort
      .mockRejectedValueOnce(new Error("not ready"))
      .mockRejectedValueOnce(new Error("not ready"))
      .mockResolvedValueOnce(7777);

    const port = await refreshBlobPort();
    expect(port).toBe(7777);
    expect(mockPort).toHaveBeenCalledTimes(3);
  });

  it("allows fresh refresh after reset", async () => {
    mockPort.mockResolvedValueOnce(1111);
    await refreshBlobPort();
    expect(getBlobPort()).toBe(1111);

    resetBlobPort();
    expect(getBlobPort()).toBeNull();

    mockPort.mockResolvedValueOnce(2222);
    await refreshBlobPort();
    expect(getBlobPort()).toBe(2222);
  });
});
