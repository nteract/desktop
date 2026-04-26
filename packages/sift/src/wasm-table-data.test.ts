import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { createWasmTableData } from "./wasm-table-data";

const predicateModule = vi.hoisted(() => ({
  num_rows: vi.fn(() => 2),
  num_cols: vi.fn(() => 4),
  col_names: vi.fn(() => ["id", "name", "score", "active"]),
  col_type: vi.fn((_handle: number, col: number) =>
    col === 0 || col === 2 ? "numeric" : col === 3 ? "boolean" : "categorical",
  ),
  col_timezone: vi.fn(() => null),
  store_viewport_cells: vi.fn((_handle: number, rows: Uint32Array) => {
    const requested = Array.from(rows);
    expect(requested).toEqual([1, 0]);
    return {
      rows: requested,
      strings: ["2", "Bob", "88", "No", "1", "Alice", "", "Yes"],
      numeric_values: [2, null, 88, null, 1, null, null, null],
      nulls: [false, false, false, false, false, false, true, false],
    };
  }),
  is_null: vi.fn(() => {
    throw new Error("is_null should not be called during batched prefetch");
  }),
  get_cell_string: vi.fn(() => {
    throw new Error("get_cell_string should not be called during batched prefetch");
  }),
  get_cell_f64: vi.fn(() => {
    throw new Error("get_cell_f64 should not be called during batched prefetch");
  }),
}));

vi.mock("./predicate", () => ({
  getModuleSync: () => predicateModule,
}));

describe("createWasmTableData", () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  it("prefetches visible cells with one batched WASM call", () => {
    const { tableData, prefetchViewport } = createWasmTableData(7);

    prefetchViewport([1, 0]);

    expect(predicateModule.store_viewport_cells).toHaveBeenCalledTimes(1);
    expect(predicateModule.store_viewport_cells.mock.calls[0][0]).toBe(7);
    expect(Array.from(predicateModule.store_viewport_cells.mock.calls[0][1])).toEqual([1, 0]);

    expect(tableData.getCell(1, 1)).toBe("Bob");
    expect(tableData.getCellRaw(1, 0)).toBe(2);
    expect(tableData.getCellRaw(1, 2)).toBe(88);
    expect(tableData.getCellRaw(1, 3)).toBe(false);
    expect(tableData.getCell(0, 2)).toBe("");
    expect(tableData.getCellRaw(0, 2)).toBeNull();
    expect(tableData.getCellRaw(0, 3)).toBe(true);

    prefetchViewport([1]);
    expect(predicateModule.store_viewport_cells).toHaveBeenCalledTimes(1);
    expect(predicateModule.is_null).not.toHaveBeenCalled();
    expect(predicateModule.get_cell_string).not.toHaveBeenCalled();
    expect(predicateModule.get_cell_f64).not.toHaveBeenCalled();
  });
});
