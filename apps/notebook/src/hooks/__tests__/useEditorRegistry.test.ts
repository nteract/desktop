import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { startFocusedCellResnap } from "../useEditorRegistry";

type ResizeObserverCallback = ConstructorParameters<typeof ResizeObserver>[0];

class FakeResizeObserver {
  static instances: FakeResizeObserver[] = [];

  observe = vi.fn();
  disconnect = vi.fn();

  constructor(private readonly callback: ResizeObserverCallback) {
    FakeResizeObserver.instances.push(this);
  }

  trigger() {
    this.callback([], this as unknown as ResizeObserver);
  }
}

describe("startFocusedCellResnap", () => {
  let originalResizeObserver: typeof window.ResizeObserver | undefined;
  let originalMutationObserver: typeof window.MutationObserver | undefined;

  beforeEach(() => {
    FakeResizeObserver.instances = [];

    originalResizeObserver = window.ResizeObserver;
    originalMutationObserver = window.MutationObserver;

    Object.defineProperty(window, "ResizeObserver", {
      configurable: true,
      value: FakeResizeObserver,
    });
    Object.defineProperty(window, "MutationObserver", {
      configurable: true,
      value: undefined,
    });
  });

  afterEach(() => {
    Object.defineProperty(window, "ResizeObserver", {
      configurable: true,
      value: originalResizeObserver,
    });
    Object.defineProperty(window, "MutationObserver", {
      configurable: true,
      value: originalMutationObserver,
    });
    document.body.replaceChildren();
  });

  it("snaps the focused cell back into view while the DOM is settling", async () => {
    const list = document.createElement("div");
    list.dataset.notebookScrollContainer = "true";
    const scrollListener = vi.fn();
    const resizeListener = vi.fn();
    list.addEventListener("scroll", scrollListener);
    window.addEventListener("resize", resizeListener);
    const cell = document.createElement("section");
    const scrollIntoView = vi.fn();
    cell.scrollIntoView = scrollIntoView;
    list.append(cell);
    document.body.append(list);

    const cleanup = startFocusedCellResnap(cell);

    expect(FakeResizeObserver.instances).toHaveLength(1);
    expect(FakeResizeObserver.instances[0].observe).toHaveBeenCalledWith(list);

    FakeResizeObserver.instances[0].trigger();
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

    expect(scrollIntoView).toHaveBeenCalledWith({ block: "nearest", behavior: "auto" });
    expect(scrollListener).toHaveBeenCalled();
    expect(resizeListener).toHaveBeenCalled();

    cleanup();
    window.removeEventListener("resize", resizeListener);
  });

  it("disconnects automatically after the resnap window closes", async () => {
    const cell = document.createElement("section");
    const scrollIntoView = vi.fn();
    cell.scrollIntoView = scrollIntoView;
    document.body.append(cell);

    startFocusedCellResnap(cell, { durationMs: 1 });
    await new Promise((resolve) => setTimeout(resolve, 5));

    expect(FakeResizeObserver.instances[0].disconnect).toHaveBeenCalled();

    FakeResizeObserver.instances[0].trigger();
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

    expect(scrollIntoView).not.toHaveBeenCalled();
  });

  it("stops resnapping when focus moves elsewhere", async () => {
    const cell = document.createElement("section");
    const scrollIntoView = vi.fn();
    cell.scrollIntoView = scrollIntoView;
    document.body.append(cell);

    const cleanup = startFocusedCellResnap(cell);
    cleanup();

    FakeResizeObserver.instances[0].trigger();
    await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));

    expect(scrollIntoView).not.toHaveBeenCalled();
    expect(FakeResizeObserver.instances[0].disconnect).toHaveBeenCalled();
  });
});
