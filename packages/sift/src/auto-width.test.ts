import { describe, expect, it } from "vite-plus/test";
import { autoWidth } from "./auto-width";

describe("autoWidth", () => {
  it("returns minimum width for short column names", () => {
    expect(autoWidth("x", "numeric")).toBeGreaterThanOrEqual(100);
    expect(autoWidth("y", "boolean")).toBeGreaterThanOrEqual(90);
    expect(autoWidth("z", "timestamp")).toBeGreaterThanOrEqual(130);
    expect(autoWidth("a", "categorical")).toBeGreaterThanOrEqual(120);
  });

  it("widens for long column names", () => {
    const shortWidth = autoWidth("id", "numeric");
    const longWidth = autoWidth("very_long_column_name_here", "numeric");
    expect(longWidth).toBeGreaterThan(shortWidth);
  });

  it("caps categorical columns at 280px", () => {
    const width = autoWidth(
      "this_is_an_extremely_long_categorical_column_name_that_goes_on_forever",
      "categorical",
    );
    expect(width).toBeLessThanOrEqual(280);
  });

  it("boolean columns have lowest minimum", () => {
    const boolW = autoWidth("x", "boolean");
    const numW = autoWidth("x", "numeric");
    const catW = autoWidth("x", "categorical");
    const tsW = autoWidth("x", "timestamp");
    expect(boolW).toBeLessThanOrEqual(numW);
    expect(boolW).toBeLessThanOrEqual(catW);
    expect(boolW).toBeLessThanOrEqual(tsW);
  });

  it("timestamp columns have highest minimum", () => {
    const tsW = autoWidth("x", "timestamp");
    expect(tsW).toBeGreaterThanOrEqual(130);
  });

  it("handles empty string column name", () => {
    const width = autoWidth("", "numeric");
    expect(width).toBeGreaterThanOrEqual(100);
  });

  it("handles unicode column names", () => {
    const width = autoWidth("日付", "timestamp");
    expect(width).toBeGreaterThanOrEqual(130);
  });
});
