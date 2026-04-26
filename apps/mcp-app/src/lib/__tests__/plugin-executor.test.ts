// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { installPluginFromUrl } from "../plugin-executor";

describe("installPluginFromUrl", () => {
  afterEach(() => {
    vi.restoreAllMocks();
    document.head.replaceChildren();
  });

  it("waits for plugin CSS before resolving", async () => {
    const appended: Element[] = [];
    vi.spyOn(document.head, "appendChild").mockImplementation((node: Node) => {
      appended.push(node as Element);
      return node;
    });

    const installPromise = installPluginFromUrl(
      "http://localhost/plugins/sift.js",
      "http://localhost/plugins/sift.css",
    );

    const link = appended.find((el): el is HTMLLinkElement => el.tagName === "LINK");
    const script = appended.find((el): el is HTMLScriptElement => el.tagName === "SCRIPT");
    expect(link?.href).toBe("http://localhost/plugins/sift.css");
    expect(script?.src).toBe("http://localhost/plugins/sift.js");

    let resolved = false;
    installPromise.then(() => {
      resolved = true;
    });

    script?.dispatchEvent(new Event("load"));
    await Promise.resolve();
    expect(resolved).toBe(false);

    link?.dispatchEvent(new Event("load"));
    await installPromise;
    expect(resolved).toBe(true);
  });

  it("rejects when plugin CSS fails to load", async () => {
    const appended: Element[] = [];
    vi.spyOn(document.head, "appendChild").mockImplementation((node: Node) => {
      appended.push(node as Element);
      return node;
    });

    const installPromise = installPluginFromUrl(
      "http://localhost/plugins/sift.js",
      "http://localhost/plugins/sift.css",
    );

    const link = appended.find((el): el is HTMLLinkElement => el.tagName === "LINK");
    const script = appended.find((el): el is HTMLScriptElement => el.tagName === "SCRIPT");

    script?.dispatchEvent(new Event("load"));
    link?.dispatchEvent(new Event("error"));

    await expect(installPromise).rejects.toThrow("Failed to load plugin stylesheet");
  });
});
