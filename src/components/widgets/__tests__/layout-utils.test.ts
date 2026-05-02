/**
 * Tests for Layout model → CSS extraction, focused on the length-coercion
 * path shared with `toCssLength`. `ipywidgets.Layout` declares every sizing
 * trait as `CUnicode`, so `Layout(width=64)` arrives as `"64"` and would
 * collapse the widget to its intrinsic size without coercion.
 */

import { describe, expect, it } from "vite-plus/test";
import {
  extractChildGridStyles,
  extractContainerGridStyles,
  extractGeneralStyles,
} from "../layout-utils";

describe("extractGeneralStyles length coercion", () => {
  it("coerces bare numeric width/height strings to pixels", () => {
    const style = extractGeneralStyles({ width: "64", height: "64" });
    expect(style.width).toBe("64px");
    expect(style.height).toBe("64px");
  });

  it("coerces numeric width/height values to pixels", () => {
    const style = extractGeneralStyles({ width: 128, height: 96 });
    expect(style.width).toBe("128px");
    expect(style.height).toBe("96px");
  });

  it("preserves already-qualified lengths verbatim", () => {
    const style = extractGeneralStyles({
      width: "50%",
      height: "10rem",
      min_width: "64px",
      max_width: "calc(100% - 20px)",
    });
    expect(style.width).toBe("50%");
    expect(style.height).toBe("10rem");
    expect(style.minWidth).toBe("64px");
    expect(style.maxWidth).toBe("calc(100% - 20px)");
  });

  it("coerces min/max width and height", () => {
    const style = extractGeneralStyles({
      min_width: "10",
      max_width: "200",
      min_height: "20",
      max_height: "300",
    });
    expect(style.minWidth).toBe("10px");
    expect(style.maxWidth).toBe("200px");
    expect(style.minHeight).toBe("20px");
    expect(style.maxHeight).toBe("300px");
  });

  it("skips empty and null length values", () => {
    const style = extractGeneralStyles({
      width: "",
      height: null as unknown as string,
      min_width: undefined as unknown as string,
    });
    expect(style.width).toBeUndefined();
    expect(style.height).toBeUndefined();
    expect(style.minWidth).toBeUndefined();
  });

  it("leaves non-length string properties untouched", () => {
    const style = extractGeneralStyles({
      display: "flex",
      overflow: "hidden",
      margin: "10px",
      width: "64",
    });
    expect(style.display).toBe("flex");
    expect(style.overflow).toBe("hidden");
    expect(style.margin).toBe("10px");
    expect(style.width).toBe("64px");
  });
});

describe("extractContainerGridStyles", () => {
  it("extracts grid container properties in camelCase", () => {
    const style = extractContainerGridStyles({
      grid_template_columns: "1fr 2fr",
      grid_gap: "8px",
    });
    expect(style.gridTemplateColumns).toBe("1fr 2fr");
    expect(style.gridGap).toBe("8px");
  });
});

describe("extractChildGridStyles", () => {
  it("extracts grid child placement in camelCase", () => {
    const style = extractChildGridStyles({
      grid_area: "header",
      grid_row: "1 / 3",
    });
    expect(style.gridArea).toBe("header");
    expect(style.gridRow).toBe("1 / 3");
  });
});
