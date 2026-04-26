import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { findVerticalScrollAncestor, scrollFrameWheelBoundary } from "../scroll-boundary";

function setScrollMetrics(element: HTMLElement, scrollHeight: number, clientHeight: number): void {
  Object.defineProperty(element, "scrollHeight", {
    configurable: true,
    value: scrollHeight,
  });
  Object.defineProperty(element, "clientHeight", {
    configurable: true,
    value: clientHeight,
  });
}

describe("scroll-boundary", () => {
  afterEach(() => {
    document.body.innerHTML = "";
    vi.restoreAllMocks();
  });

  it("finds the nearest vertical scroll ancestor", () => {
    const outer = document.createElement("div");
    outer.style.overflowY = "auto";
    setScrollMetrics(outer, 1000, 200);

    const inner = document.createElement("div");
    const iframe = document.createElement("iframe");
    inner.appendChild(iframe);
    outer.appendChild(inner);
    document.body.appendChild(outer);

    expect(findVerticalScrollAncestor(iframe.parentElement)).toBe(outer);
  });

  it("scrolls the nearest vertical scroll ancestor by the wheel delta", () => {
    const scrollContainer = document.createElement("div");
    scrollContainer.style.overflowY = "auto";
    setScrollMetrics(scrollContainer, 1000, 200);
    scrollContainer.scrollBy = vi.fn();

    const output = document.createElement("div");
    const iframe = document.createElement("iframe");
    output.appendChild(iframe);
    scrollContainer.appendChild(output);
    document.body.appendChild(scrollContainer);

    scrollFrameWheelBoundary(iframe, { deltaY: -160 });

    expect(scrollContainer.scrollBy).toHaveBeenCalledWith({
      top: -160,
      behavior: "auto",
    });
  });

  it("falls back to the owning window when no scroll ancestor exists", () => {
    const iframe = document.createElement("iframe");
    document.body.appendChild(iframe);
    const scrollBy = vi.fn();
    Object.defineProperty(window, "scrollBy", {
      configurable: true,
      value: scrollBy,
    });

    scrollFrameWheelBoundary(iframe, { deltaY: 80 });

    expect(scrollBy).toHaveBeenCalledWith({
      top: 80,
      behavior: "auto",
    });
  });

  it("ignores missing or non-finite deltas", () => {
    const iframe = document.createElement("iframe");
    document.body.appendChild(iframe);
    const scrollBy = vi.fn();
    Object.defineProperty(window, "scrollBy", {
      configurable: true,
      value: scrollBy,
    });

    scrollFrameWheelBoundary(iframe, {});
    scrollFrameWheelBoundary(iframe, { deltaY: Number.NaN });

    expect(scrollBy).not.toHaveBeenCalled();
  });
});
