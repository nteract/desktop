import { act } from "react";
import { afterEach, describe, expect, it, vi } from "vite-plus/test";
import { binOverlapsFilter, renderColumnSummary, unmountColumnSummary } from "./sparkline";

function pointerEvent(type: string, clientX: number) {
  const event = new Event(type, { bubbles: true, cancelable: true });
  Object.defineProperties(event, {
    button: { value: 0 },
    clientX: { value: clientX },
    clientY: { value: 0 },
    pointerId: { value: 1 },
    pointerType: { value: "mouse" },
  });
  return event;
}

afterEach(() => {
  document.querySelector(".sift-brush-overlay")?.remove();
  document.documentElement.classList.remove("sift-brushing");
});

describe("binOverlapsFilter", () => {
  it("treats overlapping ranges as active (normal case)", () => {
    // Filter [5, 15] overlaps bin [0, 10] and bin [10, 20]
    expect(binOverlapsFilter(0, 10, 5, 15)).toBe(true);
    expect(binOverlapsFilter(10, 20, 5, 15)).toBe(true);
  });

  it("excludes bins that touch only at the boundary (strict)", () => {
    // Filter [10, 20] and bin [0, 10]: x1 === filter.min, strict overlap is false
    expect(binOverlapsFilter(0, 10, 10, 20)).toBe(false);
    // Filter [0, 10] and bin [10, 20]: x0 === filter.max
    expect(binOverlapsFilter(10, 20, 0, 10)).toBe(false);
  });

  it("excludes bins entirely outside the filter", () => {
    expect(binOverlapsFilter(0, 5, 10, 20)).toBe(false);
    expect(binOverlapsFilter(25, 30, 10, 20)).toBe(false);
  });

  it("point-bin (x0 === x1) is inclusive against the filter range", () => {
    // Constant-slice histogram: bin collapses to a single value.
    expect(binOverlapsFilter(7, 7, 0, 10)).toBe(true);
    expect(binOverlapsFilter(0, 0, 0, 10)).toBe(true);
    expect(binOverlapsFilter(10, 10, 0, 10)).toBe(true);
    expect(binOverlapsFilter(11, 11, 0, 10)).toBe(false);
  });

  it("point-filter (min === max) lands in one bin at interior boundaries", () => {
    // This is the #1860 case: user pinned a value while the column was
    // collapsed, then cleared the other filter so the column widened.
    // Mirror the half-open [x0, x1) bucketing in NumericAccumulator —
    // at an interior boundary (e.g. v = 10 for bins [0,10) and [10,20)),
    // the pinned value should light the *upper* bin only, not both.
    expect(binOverlapsFilter(0, 10, 5, 5)).toBe(true); // interior of bin
    expect(binOverlapsFilter(0, 10, 0, 0)).toBe(true); // left edge (inclusive)
    expect(binOverlapsFilter(0, 10, 10, 10)).toBe(false); // right edge — upper bin wins
    expect(binOverlapsFilter(10, 20, 10, 10)).toBe(true); // same boundary from the upper side
    expect(binOverlapsFilter(0, 10, 11, 11)).toBe(false);
    expect(binOverlapsFilter(0, 10, -1, -1)).toBe(false);
  });

  it("point-filter hitting the last bin's upper edge stays inclusive", () => {
    // `v === summary.max` clamps into the last bucket in the accumulator,
    // so passing `isLastBin = true` makes the upper edge inclusive.
    expect(binOverlapsFilter(90, 100, 100, 100, true)).toBe(true);
    // Without the flag the last bin behaves like any interior bin.
    expect(binOverlapsFilter(90, 100, 100, 100, false)).toBe(false);
  });

  it("point-bin and point-filter at the same value overlap", () => {
    expect(binOverlapsFilter(5, 5, 5, 5)).toBe(true);
    expect(binOverlapsFilter(5, 5, 6, 6)).toBe(false);
  });
});

describe("histogram brushing", () => {
  it("captures document-level drag and blocks native text selection", async () => {
    const onFilter = vi.fn();
    const container = document.createElement("div");
    document.body.appendChild(container);

    try {
      await act(async () => {
        renderColumnSummary(
          container,
          {
            kind: "numeric",
            min: 0,
            max: 100,
            bins: [
              { x0: 0, x1: 50, count: 10 },
              { x0: 50, x1: 100, count: 5 },
            ],
          },
          100,
          undefined,
          undefined,
          onFilter,
        );
      });

      const brushTarget = container.querySelector<HTMLDivElement>(".sift-brush-hit-target");
      expect(brushTarget).toBeTruthy();

      Object.defineProperty(brushTarget, "getBoundingClientRect", {
        configurable: true,
        value: () => ({
          x: 0,
          y: 0,
          top: 0,
          left: 0,
          right: 100,
          bottom: 48,
          width: 100,
          height: 48,
          toJSON: () => ({}),
        }),
      });

      const down = pointerEvent("pointerdown", 20);
      await act(async () => {
        brushTarget!.dispatchEvent(down);
      });

      expect(down.defaultPrevented).toBe(true);
      expect(document.documentElement.classList.contains("sift-brushing")).toBe(true);
      expect(document.querySelector(".sift-brush-overlay")).toBeTruthy();

      const selectStart = new Event("selectstart", { bubbles: true, cancelable: true });
      document.dispatchEvent(selectStart);
      expect(selectStart.defaultPrevented).toBe(true);

      await act(async () => {
        document.dispatchEvent(pointerEvent("pointermove", 80));
        document.dispatchEvent(pointerEvent("pointerup", 80));
      });

      expect(document.querySelector(".sift-brush-overlay")).toBeTruthy();
      await new Promise((resolve) => setTimeout(resolve, 180));

      expect(document.querySelector(".sift-brush-overlay")).toBeNull();
      expect(document.documentElement.classList.contains("sift-brushing")).toBe(false);
      expect(onFilter).toHaveBeenCalledWith({ kind: "range", min: 20, max: 80 });
    } finally {
      unmountColumnSummary(container);
      container.remove();
    }
  });
});
