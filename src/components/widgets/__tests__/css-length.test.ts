/**
 * Tests for the ipywidgets length coercion used by ImageWidget and VideoWidget.
 *
 * Regression guard: `ipywidgets.Image(width=64)` arrives as the bare string
 * "64" (because `width` is a `CUnicode` trait), which is not a valid CSS
 * length. Without coercion, browsers silently fall back to the image's
 * intrinsic size — e.g. a 1x1 PNG renders invisible.
 */

import { describe, expect, it } from "vite-plus/test";
import { toCssLength } from "../css-length";

describe("toCssLength", () => {
  it("returns undefined for null / undefined / empty", () => {
    expect(toCssLength(null)).toBeUndefined();
    expect(toCssLength(undefined)).toBeUndefined();
    expect(toCssLength("")).toBeUndefined();
    expect(toCssLength("   ")).toBeUndefined();
  });

  it("coerces bare integer strings to pixels (ipywidgets CUnicode case)", () => {
    expect(toCssLength("64")).toBe("64px");
    expect(toCssLength("0")).toBe("0px");
    expect(toCssLength("  128  ")).toBe("128px");
  });

  it("coerces bare numeric values to pixels", () => {
    expect(toCssLength(64)).toBe("64px");
    expect(toCssLength(0)).toBe("0px");
  });

  it("accepts floating point values", () => {
    expect(toCssLength("64.5")).toBe("64.5px");
    expect(toCssLength(64.5)).toBe("64.5px");
  });

  it("preserves already-qualified CSS lengths verbatim", () => {
    expect(toCssLength("64px")).toBe("64px");
    expect(toCssLength("50%")).toBe("50%");
    expect(toCssLength("10rem")).toBe("10rem");
    expect(toCssLength("auto")).toBe("auto");
    expect(toCssLength("fit-content")).toBe("fit-content");
    expect(toCssLength("calc(100% - 20px)")).toBe("calc(100% - 20px)");
  });

  it("rejects non-finite numeric input", () => {
    expect(toCssLength(Number.NaN)).toBeUndefined();
    expect(toCssLength(Number.POSITIVE_INFINITY)).toBeUndefined();
  });
});
