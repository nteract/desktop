import { afterEach, describe, expect, it, vi } from "vitest";
import { scrollElementIntoViewIfNeeded } from "../scroll-into-view-if-needed";

function mockRect(rect: Partial<DOMRect>): DOMRect {
  return {
    x: 0,
    y: 0,
    width: 0,
    height: 0,
    top: 0,
    right: 0,
    bottom: 0,
    left: 0,
    toJSON: () => ({}),
    ...rect,
  } as DOMRect;
}

describe("scrollElementIntoViewIfNeeded", () => {
  afterEach(() => {
    document.body.innerHTML = "";
    vi.restoreAllMocks();
  });

  it("scrolls down when the element extends below the container", () => {
    const container = document.createElement("div");
    const element = document.createElement("div");
    container.appendChild(element);
    document.body.appendChild(container);

    vi.spyOn(window, "getComputedStyle").mockReturnValue({
      overflow: "auto",
      overflowY: "auto",
    } as CSSStyleDeclaration);

    Object.defineProperty(container, "scrollTop", {
      value: 100,
      writable: true,
    });
    container.getBoundingClientRect = () =>
      mockRect({ top: 100, bottom: 400, height: 300 });
    element.getBoundingClientRect = () =>
      mockRect({ top: 350, bottom: 460, height: 110 });

    const scrollTo = vi.fn();
    container.scrollTo = scrollTo;

    scrollElementIntoViewIfNeeded(element);

    expect(scrollTo).toHaveBeenCalledWith({
      behavior: "smooth",
      top: 160,
    });
  });

  it("scrolls up when the element sits above the container viewport", () => {
    const container = document.createElement("div");
    const element = document.createElement("div");
    container.appendChild(element);
    document.body.appendChild(container);

    vi.spyOn(window, "getComputedStyle").mockReturnValue({
      overflow: "auto",
      overflowY: "auto",
    } as CSSStyleDeclaration);

    Object.defineProperty(container, "scrollTop", {
      value: 250,
      writable: true,
    });
    container.getBoundingClientRect = () =>
      mockRect({ top: 100, bottom: 400, height: 300 });
    element.getBoundingClientRect = () =>
      mockRect({ top: 60, bottom: 160, height: 100 });

    const scrollTo = vi.fn();
    container.scrollTo = scrollTo;

    scrollElementIntoViewIfNeeded(element);

    expect(scrollTo).toHaveBeenCalledWith({
      behavior: "smooth",
      top: 210,
    });
  });

  it("does not scroll when the element is already visible", () => {
    const container = document.createElement("div");
    const element = document.createElement("div");
    container.appendChild(element);
    document.body.appendChild(container);

    vi.spyOn(window, "getComputedStyle").mockReturnValue({
      overflow: "auto",
      overflowY: "auto",
    } as CSSStyleDeclaration);

    container.getBoundingClientRect = () =>
      mockRect({ top: 100, bottom: 400, height: 300 });
    element.getBoundingClientRect = () =>
      mockRect({ top: 160, bottom: 260, height: 100 });

    const scrollTo = vi.fn();
    container.scrollTo = scrollTo;

    scrollElementIntoViewIfNeeded(element);

    expect(scrollTo).not.toHaveBeenCalled();
  });
});
