/**
 * Tests for anywidget-view helpers that don't require the full iframe +
 * Jupyter comm pipeline. Focused on `injectCSS` because it grew a URL
 * branch (see PR that restores `_css` URL passthrough). Other exports
 * like `loadESM` rely on dynamic `import()` and are better covered by
 * integration tests in the isolated iframe harness.
 */

import { afterEach, describe, expect, it } from "vite-plus/test";
import { injectCSS } from "../anywidget-view";

describe("injectCSS", () => {
  afterEach(() => {
    document.head.querySelectorAll("[data-widget-id]").forEach((node) => node.remove());
  });

  it("renders raw CSS into a <style> element and cleans up on dispose", () => {
    const cleanup = injectCSS("m1", ".foo { color: red; }");
    const el = document.head.querySelector<HTMLStyleElement>('style[data-widget-id="m1"]');
    expect(el).not.toBeNull();
    expect(el?.textContent).toBe(".foo { color: red; }");
    cleanup();
    expect(document.head.querySelector('[data-widget-id="m1"]')).toBeNull();
  });

  it("renders http:// URL as a <link rel=stylesheet> and cleans up", () => {
    const url = "http://127.0.0.1:1234/blob/cafebabe";
    const cleanup = injectCSS("m2", url);
    const el = document.head.querySelector<HTMLLinkElement>('link[data-widget-id="m2"]');
    expect(el).not.toBeNull();
    expect(el?.rel).toBe("stylesheet");
    expect(el?.href).toBe(url);
    // Belt-and-suspenders: no <style> element should be created for the URL path.
    expect(document.head.querySelector('style[data-widget-id="m2"]')).toBeNull();
    cleanup();
    expect(document.head.querySelector('[data-widget-id="m2"]')).toBeNull();
  });

  it("renders https:// URL as a <link rel=stylesheet>", () => {
    const url = "https://cdn.example.com/widget.css";
    const cleanup = injectCSS("m3", url);
    const el = document.head.querySelector<HTMLLinkElement>('link[data-widget-id="m3"]');
    expect(el).not.toBeNull();
    expect(el?.href).toBe(url);
    cleanup();
  });

  it("distinct model ids produce independent nodes", () => {
    const c1 = injectCSS("m4", ".a {}");
    const c2 = injectCSS("m5", "http://x/blob/h");
    expect(document.head.querySelector('style[data-widget-id="m4"]')).not.toBeNull();
    expect(document.head.querySelector('link[data-widget-id="m5"]')).not.toBeNull();
    c1();
    expect(document.head.querySelector('[data-widget-id="m4"]')).toBeNull();
    expect(document.head.querySelector('link[data-widget-id="m5"]')).not.toBeNull();
    c2();
  });
});
