/**
 * Tests for frame-html.ts HTML generation.
 *
 * These tests verify:
 * 1. Generated HTML has proper structure
 * 2. CSP meta tag is present
 * 3. Message handler validates source
 */

import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { createFrameBlobUrl, generateFrameHtml } from "../frame-html";

describe("generateFrameHtml", () => {
  let html: string;

  beforeAll(() => {
    html = generateFrameHtml({ darkMode: false });
  });

  it("generates valid HTML document", () => {
    expect(html).toContain("<!DOCTYPE html>");
    expect(html).toContain("<html");
    expect(html).toContain("</html>");
    expect(html).toContain("<head>");
    expect(html).toContain("</head>");
    expect(html).toContain("<body>");
    expect(html).toContain("</body>");
  });

  it("includes Content-Security-Policy meta tag", () => {
    expect(html).toContain('http-equiv="Content-Security-Policy"');
  });

  it("includes viewport meta tag", () => {
    expect(html).toContain('name="viewport"');
  });

  /**
   * SECURITY: Message handler must validate event.source.
   * This prevents accepting messages from windows other than the parent.
   */
  it("message handler validates event.source", () => {
    // The handler should check event.source === window.parent
    expect(html).toContain("event.source !== window.parent");
  });

  it("sets up ready message listener", () => {
    expect(html).toContain("addEventListener");
    expect(html).toContain("message");
  });

  it("sends ready message on load", () => {
    // Ready uses legacy format (bootstrap signal before JSON-RPC transport exists)
    expect(html).toContain("sendLegacy('ready'");
    expect(html).toContain("postMessage");
  });

  describe("dark mode", () => {
    it("bakes theme-correct background to prevent flash", () => {
      // --bg-primary is always transparent; the notebook background shows through
      const darkHtml = generateFrameHtml({ darkMode: true });
      expect(darkHtml).toContain("--bg-primary: transparent");
      expect(darkHtml).toContain("--bg-secondary: #1a1a1a");

      const lightHtml = generateFrameHtml({ darkMode: false });
      expect(lightHtml).toContain("--bg-primary: transparent");
      expect(lightHtml).toContain("--bg-secondary: #f5f5f5");

      // Cream uses warm tones
      const creamDarkHtml = generateFrameHtml({ darkMode: true, colorTheme: "cream" });
      expect(creamDarkHtml).toContain("--bg-secondary: #242120");
      expect(creamDarkHtml).toContain("--text-primary: #e8e2dc");
      expect(creamDarkHtml).toContain("--border-color: #3a3533");

      const creamLightHtml = generateFrameHtml({ darkMode: false, colorTheme: "cream" });
      expect(creamLightHtml).toContain("--bg-secondary: #f0ede7");
      expect(creamLightHtml).toContain("--text-primary: #1e1a18");
      expect(creamLightHtml).toContain("--border-color: #d8cec3");
    });

    it("uses dark text colors when darkMode is true", () => {
      const darkHtml = generateFrameHtml({ darkMode: true });
      expect(darkHtml).toContain("--text-primary: #e0e0e0");
      expect(darkHtml).toContain("--border-color: #333333");
    });

    it("uses light text colors when darkMode is false", () => {
      const lightHtml = generateFrameHtml({ darkMode: false });
      expect(lightHtml).toContain("--text-primary: #1a1a1a");
      expect(lightHtml).toContain("--border-color: #e0e0e0");
    });
  });
});

describe("createFrameBlobUrl", () => {
  let mockCreateObjectURL: ReturnType<typeof vi.fn>;
  let mockRevokeObjectURL: ReturnType<typeof vi.fn>;
  let urlCounter = 0;

  beforeEach(() => {
    urlCounter = 0;
    mockCreateObjectURL = vi.fn(() => `blob:mock-${++urlCounter}`);
    mockRevokeObjectURL = vi.fn();
    vi.stubGlobal("URL", {
      ...URL,
      createObjectURL: mockCreateObjectURL,
      revokeObjectURL: mockRevokeObjectURL,
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("returns a blob: URL", () => {
    const url = createFrameBlobUrl({ darkMode: false });
    expect(url).toMatch(/^blob:/);
    expect(mockCreateObjectURL).toHaveBeenCalledWith(expect.any(Blob));
  });

  it("creates unique URLs each call", () => {
    const url1 = createFrameBlobUrl({ darkMode: false });
    const url2 = createFrameBlobUrl({ darkMode: false });
    expect(url1).not.toBe(url2);
    expect(mockCreateObjectURL).toHaveBeenCalledTimes(2);
  });
});
