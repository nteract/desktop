import { describe, expect, it } from "vite-plus/test";
import {
  BIN_COUNT,
  BooleanAccumulator,
  CategoricalAccumulator,
  formatCell,
  isNullSentinel,
  NumericAccumulator,
  refineColumnType,
  TimestampAccumulator,
} from "./accumulators";
import type {
  BooleanColumnSummary,
  CategoricalColumnSummary,
  NumericColumnSummary,
  TimestampColumnSummary,
} from "./table";

describe("NumericAccumulator", () => {
  it("computes min/max and bins for normal data", () => {
    const acc = new NumericAccumulator();
    const data = [10, 20, 30, 40, 50, 15, 25, 35, 45, 5];
    acc.add(data, 0, data.length);
    const result = acc.snapshot() as NumericColumnSummary;
    expect(result).not.toBeNull();
    expect(result.kind).toBe("numeric");
    expect(result.min).toBe(5);
    expect(result.max).toBe(50);
    expect(result.bins).toHaveLength(BIN_COUNT);
    expect(result.bins.reduce((s, b) => s + b.count, 0)).toBe(data.length);
  });

  it("marks monotonically increasing data as index", () => {
    const acc = new NumericAccumulator();
    const data = [1, 2, 3, 4, 5];
    acc.add(data, 0, data.length);
    const result = acc.snapshot(5) as any;
    expect(result).not.toBeNull();
    expect(result.isIndex).toBe(true);
  });

  it("excludes NaN, Infinity, -Infinity, null from bins", () => {
    const acc = new NumericAccumulator();
    const data: (number | null)[] = [10, NaN, 20, Infinity, -Infinity, null, 30, 15, 25];
    acc.add(data, 0, data.length);
    const result = acc.snapshot() as NumericColumnSummary;
    expect(result).not.toBeNull();
    // Only 10, 20, 30, 15, 25 in bins = 5 finite values
    expect(result.bins.reduce((s, b) => s + b.count, 0)).toBe(5);
    expect(result.min).toBe(10);
    expect(result.max).toBe(30);
  });

  it("returns non-null when monotonic but has special values", () => {
    const acc = new NumericAccumulator();
    const data: (number | null)[] = [1, 2, 3, null];
    acc.add(data, 0, data.length);
    // Monotonic but has null → should still produce histogram
    const result = acc.snapshot();
    expect(result).not.toBeNull();
  });

  it("returns null when only special values (no finite data)", () => {
    const acc = new NumericAccumulator();
    const data: (number | null)[] = [NaN, Infinity, null];
    acc.add(data, 0, data.length);
    expect(acc.snapshot()).toBeNull();
  });

  it("handles all identical values (not an index)", () => {
    const acc = new NumericAccumulator();
    const data = [42, 42, 42, 42, 42, 42, 42, 42, 42, 42];
    acc.add(data, 0, data.length);
    const result = acc.snapshot(10) as any;
    expect(result).not.toBeNull();
    expect(result.kind).toBe("numeric");
    expect(result.isIndex).toBeFalsy();
  });

  it("handles incremental batches", () => {
    const acc = new NumericAccumulator();
    const batch1 = [10, 20, 30];
    const batch2 = [5, 40, 25];
    acc.add(batch1, 0, batch1.length);
    acc.add(batch2, 0, batch2.length);
    const result = acc.snapshot() as NumericColumnSummary;
    expect(result).not.toBeNull();
    expect(result.min).toBe(5);
    expect(result.max).toBe(40);
    expect(result.bins.reduce((s, b) => s + b.count, 0)).toBe(6);
  });

  it("handles BigInt values by coercing to Number", () => {
    const acc = new NumericAccumulator();
    const data: unknown[] = [BigInt(10), BigInt(20), BigInt(5), BigInt(30)];
    acc.add(data, 0, data.length);
    const result = acc.snapshot() as NumericColumnSummary;
    expect(result).not.toBeNull();
    expect(result.min).toBe(5);
    expect(result.max).toBe(30);
    expect(result.bins.reduce((s, b) => s + b.count, 0)).toBe(4);
  });
  it("handles single-value column (min === max)", () => {
    const acc = new NumericAccumulator();
    const data = [7];
    acc.add(data, 0, data.length);
    const result = acc.snapshot(1) as NumericColumnSummary;
    expect(result).not.toBeNull();
    expect(result.min).toBe(7);
    expect(result.max).toBe(7);
    // Bins should not crash with zero-width range
    expect(result.bins).toHaveLength(BIN_COUNT);
  });

  it("handles empty data (0 rows)", () => {
    const acc = new NumericAccumulator();
    acc.add([], 0, 0);
    expect(acc.snapshot()).toBeNull();
  });

  it("handles very large numbers", () => {
    const acc = new NumericAccumulator();
    const data = [Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER - 1, 0];
    acc.add(data, 0, data.length);
    const result = acc.snapshot() as NumericColumnSummary;
    expect(result).not.toBeNull();
    expect(result.max).toBe(Number.MAX_SAFE_INTEGER);
    expect(result.min).toBe(0);
  });
});

describe("TimestampAccumulator", () => {
  it("computes min/max and bins", () => {
    const acc = new TimestampAccumulator();
    const now = Date.now();
    const data = [now - 1000, now - 500, now - 200, now];
    acc.add(data, 0, data.length);
    const result = acc.snapshot() as TimestampColumnSummary;
    expect(result).not.toBeNull();
    expect(result.kind).toBe("timestamp");
    expect(result.min).toBe(now - 1000);
    expect(result.max).toBe(now);
    expect(result.bins).toHaveLength(BIN_COUNT);
    expect(result.bins.reduce((s, b) => s + b.count, 0)).toBe(4);
  });

  it("returns null for empty data", () => {
    const acc = new TimestampAccumulator();
    expect(acc.snapshot()).toBeNull();
  });

  it("handles single timestamp (min === max)", () => {
    const acc = new TimestampAccumulator();
    const ts = Date.now();
    acc.add([ts], 0, 1);
    const result = acc.snapshot() as TimestampColumnSummary;
    expect(result).not.toBeNull();
    expect(result.min).toBe(ts);
    expect(result.max).toBe(ts);
    expect(result.bins).toHaveLength(BIN_COUNT);
  });

  it("handles null timestamps", () => {
    const acc = new TimestampAccumulator();
    const now = Date.now();
    const data: (number | null)[] = [now, null, now - 1000, null];
    acc.add(data, 0, data.length);
    const result = acc.snapshot() as TimestampColumnSummary;
    expect(result).not.toBeNull();
    expect(result.bins.reduce((s, b) => s + b.count, 0)).toBe(2);
  });
});

describe("CategoricalAccumulator", () => {
  it("returns top 3 categories in descending order", () => {
    const strings = ["A", "B", "A", "C", "A", "B", "D", "A", "B", "C"];
    const acc = new CategoricalAccumulator(strings);
    acc.add([], 0, strings.length);
    const result = acc.snapshot(strings.length) as CategoricalColumnSummary;
    expect(result.kind).toBe("categorical");
    expect(result.topCategories[0].label).toBe("A");
    expect(result.topCategories[0].count).toBe(4);
    expect(result.topCategories[1].label).toBe("B");
    expect(result.topCategories[1].count).toBe(3);
    expect(result.topCategories[2].label).toBe("C");
    expect(result.topCategories[2].count).toBe(2);
  });

  it("aggregates others", () => {
    const strings = ["A", "A", "A", "B", "B", "C", "D", "E"];
    const acc = new CategoricalAccumulator(strings);
    acc.add([], 0, strings.length);
    const result = acc.snapshot(strings.length) as CategoricalColumnSummary;
    // Top 3: A(3), B(2), C(1). Others: D(1) + E(1) = 2
    expect(result.othersCount).toBe(2);
    expect(result.uniqueCount).toBe(5);
  });

  it("computes percentages", () => {
    const strings = ["A", "A", "A", "A", "B", "B", "B", "B", "C", "C"];
    const acc = new CategoricalAccumulator(strings);
    acc.add([], 0, strings.length);
    const result = acc.snapshot(10) as CategoricalColumnSummary;
    expect(result.topCategories[0].pct).toBe(40);
    expect(result.topCategories[1].pct).toBe(40);
    expect(result.topCategories[2].pct).toBe(20);
  });

  it("handles unicode categories", () => {
    const strings = ["日本語", "日本語", "中文", "العربية", "日本語"];
    const acc = new CategoricalAccumulator(strings);
    acc.add([], 0, strings.length);
    const result = acc.snapshot(strings.length) as CategoricalColumnSummary;
    expect(result.topCategories[0].label).toBe("日本語");
    expect(result.topCategories[0].count).toBe(3);
    expect(result.uniqueCount).toBe(3);
  });

  it("handles empty string categories", () => {
    const strings = ["", "", "a", "", "b"];
    const acc = new CategoricalAccumulator(strings);
    acc.add([], 0, strings.length);
    const result = acc.snapshot(strings.length) as CategoricalColumnSummary;
    expect(result.topCategories[0].label).toBe("");
    expect(result.topCategories[0].count).toBe(3);
  });

  it("handles single-value categorical", () => {
    const strings = ["same", "same", "same", "same"];
    const acc = new CategoricalAccumulator(strings);
    acc.add([], 0, strings.length);
    const result = acc.snapshot(strings.length) as CategoricalColumnSummary;
    expect(result.uniqueCount).toBe(1);
    expect(result.othersCount).toBe(0);
  });
});

describe("BooleanAccumulator", () => {
  it("counts true and false", () => {
    const acc = new BooleanAccumulator();
    const data = [true, true, false, true, false];
    acc.add(data, 0, data.length);
    const result = acc.snapshot(data.length) as BooleanColumnSummary;
    expect(result.kind).toBe("boolean");
    expect(result.trueCount).toBe(3);
    expect(result.falseCount).toBe(2);
    expect(result.total).toBe(5);
  });

  it("handles all true", () => {
    const acc = new BooleanAccumulator();
    const data = [true, true, true];
    acc.add(data, 0, data.length);
    const result = acc.snapshot(3) as BooleanColumnSummary;
    expect(result.trueCount).toBe(3);
    expect(result.falseCount).toBe(0);
    expect(result.nullCount).toBe(0);
  });

  it("tracks nulls separately from false", () => {
    const acc = new BooleanAccumulator();
    const data = [true, null, false, null, true, undefined];
    acc.add(data, 0, data.length);
    const result = acc.snapshot(data.length) as BooleanColumnSummary;
    expect(result.trueCount).toBe(2);
    expect(result.falseCount).toBe(1);
    expect(result.nullCount).toBe(3);
    expect(result.total).toBe(6);
  });

  it("handles all-null column", () => {
    const acc = new BooleanAccumulator();
    const data: unknown[] = [null, null, null, undefined];
    acc.add(data, 0, data.length);
    const result = acc.snapshot(data.length) as BooleanColumnSummary;
    expect(result.trueCount).toBe(0);
    expect(result.falseCount).toBe(0);
    expect(result.nullCount).toBe(4);
  });
});

describe("formatCell", () => {
  it("formats boolean as Yes/No", () => {
    expect(formatCell("boolean", true)).toBe("Yes");
    expect(formatCell("boolean", false)).toBe("No");
  });

  it("formats timestamp as date string", () => {
    const result = formatCell("timestamp", 1672531200000);
    // Locale-dependent — just check it produces a non-empty date-like string
    expect(result.length).toBeGreaterThan(0);
    expect(result).toMatch(/\d/);
  });

  it("returns empty string for null", () => {
    expect(formatCell("numeric", null)).toBe("");
    expect(formatCell("categorical", undefined)).toBe("");
  });

  it("stringifies numbers and strings", () => {
    expect(formatCell("numeric", 42)).toBe("42");
    expect(formatCell("categorical", "hello")).toBe("hello");
  });

  it("preserves nested array structure", () => {
    expect(formatCell("categorical", [["fast", "slow"], ["red"]])).toBe(
      '[["fast","slow"],["red"]]',
    );
  });

  it("preserves flat array structure", () => {
    expect(formatCell("categorical", ["fast", "slow", "red"])).toBe('["fast","slow","red"]');
  });

  it("preserves object structure", () => {
    expect(formatCell("categorical", { a: 1 })).toBe('{"a":1}');
  });

  it("handles arrays containing bigint values", () => {
    expect(formatCell("categorical", [1n, 2n, 3n])).toBe("[1,2,3]");
  });
});

describe("isNullSentinel", () => {
  it("recognizes common null sentinels", () => {
    expect(isNullSentinel("?")).toBe(true);
    expect(isNullSentinel("N/A")).toBe(true);
    expect(isNullSentinel("NA")).toBe(true);
    expect(isNullSentinel("n/a")).toBe(true);
    expect(isNullSentinel("NULL")).toBe(true);
    expect(isNullSentinel("None")).toBe(true);
    expect(isNullSentinel("-")).toBe(true);
    expect(isNullSentinel("")).toBe(true);
  });

  it("does not flag normal values", () => {
    expect(isNullSentinel("hello")).toBe(false);
    expect(isNullSentinel("42")).toBe(false);
    expect(isNullSentinel("Private")).toBe(false);
  });
});

describe("refineColumnType", () => {
  it("does not change non-categorical types", () => {
    const result = refineColumnType("numeric", [1, 2, 3]);
    expect(result.type).toBe("numeric");
    expect(result.hasNullSentinels).toBe(false);
  });

  it("refines string dates to timestamp", () => {
    const values = ["2019-06-23", "2018-10-19", "2020-01-15", "2019-07-01", "2021-03-28"];
    const result = refineColumnType("categorical", values);
    expect(result.type).toBe("timestamp");
  });

  it("does not refine mixed strings as timestamp", () => {
    const values = ["hello", "2019-06-23", "world", "foo", "bar"];
    const result = refineColumnType("categorical", values);
    expect(result.type).toBe("categorical");
  });

  it("detects null sentinels in categorical data", () => {
    const values = ["Private", "?", "Self-emp", "?", "Federal-gov"];
    const result = refineColumnType("categorical", values);
    expect(result.hasNullSentinels).toBe(true);
  });

  it("ignores null sentinels when determining date type", () => {
    // Dates + some "?" sentinels should still detect as timestamp
    const values = ["2019-06-23", "?", "2018-10-19", "?", "2020-01-15", "2019-07-01"];
    const result = refineColumnType("categorical", values);
    expect(result.type).toBe("timestamp");
    expect(result.hasNullSentinels).toBe(true);
  });

  it("handles slash-separated dates", () => {
    const values = ["01/15/2020", "12/25/2019", "06/30/2021"];
    const result = refineColumnType("categorical", values);
    expect(result.type).toBe("timestamp");
  });
});
