import { describe, expect, it } from "vitest";
import {
  columnFiltersToPredicates,
  type ExplorerState,
  engineStateToExplorerState,
  explorerStateToJSON,
  type FilterPredicate,
  predicateToEnglish,
  predicateToPandas,
  predicateToSQL,
} from "./filter-schema";
import type { TableEngineState } from "./table";

describe("predicateToSQL", () => {
  it("between", () => {
    expect(
      predicateToSQL({ column: "score", op: "between", value: [50, 80] }),
    ).toBe('"score" BETWEEN 50 AND 80');
  });

  it("eq string", () => {
    expect(predicateToSQL({ column: "name", op: "eq", value: "Alice" })).toBe(
      `"name" = 'Alice'`,
    );
  });

  it("eq null → IS NULL", () => {
    expect(predicateToSQL({ column: "x", op: "eq", value: null })).toBe(
      '"x" IS NULL',
    );
  });

  it("neq null → IS NOT NULL", () => {
    expect(predicateToSQL({ column: "x", op: "neq", value: null })).toBe(
      '"x" IS NOT NULL',
    );
  });

  it("in", () => {
    expect(
      predicateToSQL({ column: "dept", op: "in", value: ["A", "B"] }),
    ).toBe(`"dept" IN ('A', 'B')`);
  });

  it("not_in", () => {
    expect(predicateToSQL({ column: "dept", op: "not_in", value: ["A"] })).toBe(
      `"dept" NOT IN ('A')`,
    );
  });

  it("gt / gte / lt / lte", () => {
    expect(predicateToSQL({ column: "x", op: "gt", value: 5 })).toBe('"x" > 5');
    expect(predicateToSQL({ column: "x", op: "gte", value: 5 })).toBe(
      '"x" >= 5',
    );
    expect(predicateToSQL({ column: "x", op: "lt", value: 5 })).toBe('"x" < 5');
    expect(predicateToSQL({ column: "x", op: "lte", value: 5 })).toBe(
      '"x" <= 5',
    );
  });

  it("is_null / is_not_null", () => {
    expect(predicateToSQL({ column: "x", op: "is_null" })).toBe('"x" IS NULL');
    expect(predicateToSQL({ column: "x", op: "is_not_null" })).toBe(
      '"x" IS NOT NULL',
    );
  });

  it("contains", () => {
    expect(
      predicateToSQL({ column: "name", op: "contains", value: "test" }),
    ).toBe(`"name" LIKE '%test%'`);
  });

  it("escapes single quotes in string values", () => {
    expect(predicateToSQL({ column: "name", op: "eq", value: "O'Brien" })).toBe(
      `"name" = 'O''Brien'`,
    );
  });

  it("escapes double quotes in column names", () => {
    expect(predicateToSQL({ column: 'col"name', op: "eq", value: 1 })).toBe(
      `"col""name" = 1`,
    );
  });

  it("compound and", () => {
    const p: FilterPredicate = {
      op: "and",
      args: [
        { column: "a", op: "gt", value: 1 },
        { column: "b", op: "lt", value: 10 },
      ],
    };
    expect(predicateToSQL(p)).toBe('("a" > 1 AND "b" < 10)');
  });

  it("compound or", () => {
    const p: FilterPredicate = {
      op: "or",
      args: [
        { column: "x", op: "eq", value: "a" },
        { column: "x", op: "eq", value: "b" },
      ],
    };
    expect(predicateToSQL(p)).toBe(`("x" = 'a' OR "x" = 'b')`);
  });

  it("not", () => {
    expect(
      predicateToSQL({ op: "not", arg: { column: "x", op: "eq", value: 1 } }),
    ).toBe('NOT ("x" = 1)');
  });

  it("nested compound", () => {
    const p: FilterPredicate = {
      op: "not",
      arg: {
        op: "and",
        args: [
          { column: "a", op: "between", value: [1, 5] },
          { column: "b", op: "eq", value: "x" },
        ],
      },
    };
    expect(predicateToSQL(p)).toBe(`NOT (("a" BETWEEN 1 AND 5 AND "b" = 'x'))`);
  });
});

describe("predicateToPandas", () => {
  it("between", () => {
    expect(
      predicateToPandas({ column: "score", op: "between", value: [50, 80] }),
    ).toBe('df["score"].between(50, 80)');
  });

  it("eq null → isna()", () => {
    expect(predicateToPandas({ column: "x", op: "eq", value: null })).toBe(
      'df["x"].isna()',
    );
  });

  it("eq boolean → Python True/False", () => {
    expect(predicateToPandas({ column: "v", op: "eq", value: true })).toBe(
      '(df["v"] == True)',
    );
  });

  it("in", () => {
    expect(
      predicateToPandas({ column: "d", op: "in", value: ["A", "B"] }),
    ).toBe('df["d"].isin(["A", "B"])');
  });

  it("not_in", () => {
    expect(predicateToPandas({ column: "d", op: "not_in", value: ["A"] })).toBe(
      '~df["d"].isin(["A"])',
    );
  });

  it("contains", () => {
    expect(
      predicateToPandas({ column: "n", op: "contains", value: "test" }),
    ).toBe('df["n"].str.contains("test")');
  });

  it("custom dfName", () => {
    expect(
      predicateToPandas({ column: "x", op: "gt", value: 5 }, "my_df"),
    ).toBe('(my_df["x"] > 5)');
  });

  it("compound and", () => {
    const p: FilterPredicate = {
      op: "and",
      args: [
        { column: "a", op: "gt", value: 1 },
        { column: "b", op: "lt", value: 10 },
      ],
    };
    expect(predicateToPandas(p)).toBe('((df["a"] > 1) & (df["b"] < 10))');
  });

  it("not", () => {
    expect(
      predicateToPandas({ op: "not", arg: { column: "x", op: "is_null" } }),
    ).toBe('~(df["x"].isna())');
  });
});

describe("predicateToEnglish", () => {
  it("between", () => {
    expect(
      predicateToEnglish({ column: "score", op: "between", value: [50, 80] }),
    ).toBe("score is between 50 and 80");
  });

  it("eq null", () => {
    expect(predicateToEnglish({ column: "x", op: "eq", value: null })).toBe(
      "x is null",
    );
  });

  it("in", () => {
    expect(
      predicateToEnglish({ column: "dept", op: "in", value: ["A", "B"] }),
    ).toBe("dept is one of A, B");
  });

  it("contains", () => {
    expect(
      predicateToEnglish({ column: "name", op: "contains", value: "test" }),
    ).toBe('name contains "test"');
  });

  it("compound and", () => {
    const p: FilterPredicate = {
      op: "and",
      args: [
        { column: "a", op: "gt", value: 1 },
        { column: "b", op: "eq", value: "x" },
      ],
    };
    expect(predicateToEnglish(p)).toBe("a > 1 and b is x");
  });
});

describe("columnFiltersToPredicates", () => {
  const cols = [{ key: "score" }, { key: "dept" }, { key: "verified" }];

  it("range → between", () => {
    const result = columnFiltersToPredicates(cols, [
      { kind: "range", min: 10, max: 50 },
      null,
      null,
    ]);
    expect(result).toEqual([
      { column: "score", op: "between", value: [10, 50] },
    ]);
  });

  it("set → in", () => {
    const result = columnFiltersToPredicates(cols, [
      null,
      { kind: "set", values: new Set(["A", "B"]) },
      null,
    ]);
    expect(result).toEqual([{ column: "dept", op: "in", value: ["A", "B"] }]);
  });

  it("boolean → eq", () => {
    const result = columnFiltersToPredicates(cols, [
      null,
      null,
      { kind: "boolean", value: true },
    ]);
    expect(result).toEqual([{ column: "verified", op: "eq", value: true }]);
  });

  it("skips null filters", () => {
    expect(columnFiltersToPredicates(cols, [null, null, null])).toEqual([]);
  });

  it("multiple filters", () => {
    const result = columnFiltersToPredicates(cols, [
      { kind: "range", min: 0, max: 100 },
      { kind: "set", values: new Set(["X"]) },
      null,
    ]);
    expect(result).toHaveLength(2);
  });
});

describe("explorerStateToJSON", () => {
  it("round-trips through JSON.parse", () => {
    const state: ExplorerState = {
      filters: [{ column: "x", op: "gt", value: 5 }],
      sort: [{ column: "x", direction: "asc" }],
      resultCount: 100,
      totalCount: 1000,
    };
    const json = explorerStateToJSON(state);
    const parsed = JSON.parse(json);
    expect(parsed.filters).toEqual(state.filters);
    expect(parsed.sort).toEqual(state.sort);
    expect(parsed.resultCount).toBe(100);
    expect(parsed.totalCount).toBe(1000);
  });
});

describe("engineStateToExplorerState", () => {
  it("converts empty state", () => {
    const state: TableEngineState = {
      sort: null,
      filters: [],
      filteredCount: 100,
      totalCount: 100,
    };
    const result = engineStateToExplorerState(state);
    expect(result.filters).toEqual([]);
    expect(result.sort).toEqual([]);
    expect(result.resultCount).toBe(100);
    expect(result.totalCount).toBe(100);
  });

  it("converts sort state", () => {
    const state: TableEngineState = {
      sort: { column: "score", direction: "desc" },
      filters: [],
      filteredCount: 50,
      totalCount: 100,
    };
    const result = engineStateToExplorerState(state);
    expect(result.sort).toEqual([{ column: "score", direction: "desc" }]);
  });

  it("converts range filter to between predicate", () => {
    const state: TableEngineState = {
      sort: null,
      filters: [
        { column: "score", filter: { kind: "range", min: 10, max: 50 } },
      ],
      filteredCount: 30,
      totalCount: 100,
    };
    const result = engineStateToExplorerState(state);
    expect(result.filters).toEqual([
      { column: "score", op: "between", value: [10, 50] },
    ]);
  });

  it("converts set filter to in predicate", () => {
    const state: TableEngineState = {
      sort: null,
      filters: [
        {
          column: "dept",
          filter: { kind: "set", values: new Set(["Eng", "Design"]) },
        },
      ],
      filteredCount: 20,
      totalCount: 100,
    };
    const result = engineStateToExplorerState(state);
    expect(result.filters).toHaveLength(1);
    expect(result.filters[0]).toMatchObject({ column: "dept", op: "in" });
    expect((result.filters[0] as any).value).toContain("Eng");
    expect((result.filters[0] as any).value).toContain("Design");
  });

  it("converts boolean filter to eq predicate", () => {
    const state: TableEngineState = {
      sort: null,
      filters: [{ column: "active", filter: { kind: "boolean", value: true } }],
      filteredCount: 60,
      totalCount: 100,
    };
    const result = engineStateToExplorerState(state);
    expect(result.filters).toEqual([
      { column: "active", op: "eq", value: true },
    ]);
  });

  it("round-trips to SQL", () => {
    const state: TableEngineState = {
      sort: { column: "name", direction: "asc" },
      filters: [
        { column: "score", filter: { kind: "range", min: 50, max: 100 } },
        { column: "active", filter: { kind: "boolean", value: true } },
      ],
      filteredCount: 25,
      totalCount: 100,
    };
    const explorer = engineStateToExplorerState(state);
    const sql = explorer.filters.map((f) => predicateToSQL(f));
    expect(sql).toContain('"score" BETWEEN 50 AND 100');
    expect(sql).toContain('"active" = true');
  });

  it("round-trips to JSON", () => {
    const state: TableEngineState = {
      sort: { column: "id", direction: "asc" },
      filters: [
        { column: "dept", filter: { kind: "set", values: new Set(["Eng"]) } },
      ],
      filteredCount: 10,
      totalCount: 50,
    };
    const json = explorerStateToJSON(engineStateToExplorerState(state));
    const parsed = JSON.parse(json);
    expect(parsed.sort).toEqual([{ column: "id", direction: "asc" }]);
    expect(parsed.filters[0].op).toBe("in");
    expect(parsed.resultCount).toBe(10);
  });
});
