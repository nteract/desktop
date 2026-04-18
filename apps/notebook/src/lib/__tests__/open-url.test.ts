// @vitest-environment jsdom
import type { NotebookHost } from "@nteract/notebook-host";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { openUrl, setOpenUrlHost } from "../open-url";

const opened: string[] = [];

function makeHost(): NotebookHost {
  return {
    externalLinks: {
      open: vi.fn(async (url: string) => {
        opened.push(url);
      }),
    },
  } as unknown as NotebookHost;
}

beforeEach(() => {
  opened.length = 0;
  setOpenUrlHost(makeHost());
});

afterEach(() => {
  setOpenUrlHost(null);
});

describe("openUrl", () => {
  it("forwards http URLs to host.externalLinks.open", async () => {
    await openUrl("https://example.com/path");
    expect(opened).toEqual(["https://example.com/path"]);
  });

  it("trims whitespace before opening", async () => {
    await openUrl("   https://example.com/  ");
    expect(opened).toEqual(["https://example.com/"]);
  });

  it("refuses javascript: URLs", async () => {
    // biome-ignore lint/suspicious/noExplicitAny: spying on console.error
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    await openUrl("javascript:alert(1)");
    expect(opened).toEqual([]);
    spy.mockRestore();
  });

  it("refuses data: URLs", async () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    await openUrl("data:text/html,<script>alert(1)</script>");
    expect(opened).toEqual([]);
    spy.mockRestore();
  });

  it("refuses file: URLs", async () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    await openUrl("file:///etc/passwd");
    expect(opened).toEqual([]);
    spy.mockRestore();
  });

  it("refuses obviously invalid URLs", async () => {
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    await openUrl("not a url");
    expect(opened).toEqual([]);
    spy.mockRestore();
  });

  it("passes mailto: through", async () => {
    await openUrl("mailto:hello@example.com");
    expect(opened).toEqual(["mailto:hello@example.com"]);
  });

  it("drops URLs silently when host isn't registered yet", async () => {
    setOpenUrlHost(null);
    const spy = vi.spyOn(console, "error").mockImplementation(() => {});
    await openUrl("https://example.com");
    expect(opened).toEqual([]);
    spy.mockRestore();
  });
});
