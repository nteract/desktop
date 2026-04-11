import { beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { type Column, createTable, type TableData, type TableEngine } from "./table";

// --- Test helpers ---

function makeColumns(): Column[] {
  return [
    {
      key: "id",
      label: "ID",
      width: 80,
      sortable: true,
      numeric: true,
      columnType: "numeric",
    },
    {
      key: "name",
      label: "Name",
      width: 150,
      sortable: true,
      numeric: false,
      columnType: "categorical",
    },
    {
      key: "score",
      label: "Score",
      width: 100,
      sortable: true,
      numeric: true,
      columnType: "numeric",
    },
    {
      key: "active",
      label: "Active",
      width: 80,
      sortable: true,
      numeric: false,
      columnType: "boolean",
    },
  ];
}

function makeTableData(rows: unknown[][]): TableData {
  const columns = makeColumns();
  return {
    columns,
    rowCount: rows.length,
    getCell: (r, c) => String(rows[r][c] ?? ""),
    getCellRaw: (r, c) => rows[r][c],
    columnSummaries: columns.map(() => null),
  };
}

function makeRows(count: number): unknown[][] {
  const rows: unknown[][] = [];
  for (let i = 0; i < count; i++) {
    rows.push([i + 1, `Person ${i}`, Math.round(Math.random() * 100), i % 3 !== 0]);
  }
  return rows;
}

async function flushRAF() {
  await vi.advanceTimersByTimeAsync(0);
}

// --- Tests ---

describe("createTable", () => {
  let container: HTMLDivElement;
  let rows: unknown[][];
  let data: TableData;
  let engine: TableEngine;

  beforeEach(() => {
    vi.useFakeTimers();
    container = document.createElement("div");
    document.body.appendChild(container);
    rows = makeRows(50);
    data = makeTableData(rows);
    engine = createTable(container, data);
  });

  describe("DOM structure", () => {
    it("creates header, viewport, and stats bar", async () => {
      await flushRAF();
      expect(container.querySelector(".pt-header")).not.toBeNull();
      expect(container.querySelector(".pt-viewport")).not.toBeNull();
      expect(container.querySelector(".pt-stats")).not.toBeNull();
    });

    it("renders correct header labels", async () => {
      await flushRAF();
      const labels = container.querySelectorAll(".pt-th-label");
      expect(labels).toHaveLength(4);
      expect(labels[0].textContent).toBe("ID");
      expect(labels[1].textContent).toBe("Name");
      expect(labels[2].textContent).toBe("Score");
      expect(labels[3].textContent).toBe("Active");
    });

    it("shows row count in stats bar", async () => {
      await flushRAF();
      const stats = container.querySelector(".pt-stat-rows") as HTMLElement;
      expect(stats?.dataset.value).toContain("50");
    });
  });

  describe("filtering", () => {
    it("setFilter reduces visible row count", async () => {
      await flushRAF();
      engine.setFilter(2, { kind: "range", min: 0, max: 30 });
      await flushRAF();
      const stats = container.querySelector(".pt-stat-rows") as HTMLElement;
      expect(stats?.dataset.value).toContain("of");
      expect(stats?.dataset.value).toContain("50");
    });

    it("setFilter creates a filter pill", async () => {
      await flushRAF();
      engine.setFilter(2, { kind: "range", min: 10, max: 50 });
      await flushRAF();
      const pills = container.querySelectorAll(".pt-filter-pill");
      expect(pills.length).toBe(1);
      expect(pills[0].textContent).toContain("Score");
    });

    it("clearFilter removes the pill", async () => {
      await flushRAF();
      engine.setFilter(2, { kind: "range", min: 10, max: 50 });
      await flushRAF();
      engine.clearFilter(2);
      await flushRAF();
      const pills = container.querySelectorAll(".pt-filter-pill");
      expect(pills.length).toBe(0);
    });

    it("clearAllFilters removes all pills", async () => {
      await flushRAF();
      engine.setFilter(1, { kind: "set", values: new Set(["Person 1"]) });
      engine.setFilter(3, { kind: "boolean", value: true });
      await flushRAF();
      expect(container.querySelectorAll(".pt-filter-pill").length).toBe(2);
      engine.clearAllFilters();
      await flushRAF();
      expect(container.querySelectorAll(".pt-filter-pill").length).toBe(0);
    });

    it("boolean filter works", async () => {
      await flushRAF();
      engine.setFilter(3, { kind: "boolean", value: true });
      await flushRAF();
      const stats = container.querySelector(".pt-stat-rows") as HTMLElement;
      expect(stats?.dataset.value).toContain("of");
    });

    it("set filter works", async () => {
      await flushRAF();
      engine.setFilter(1, {
        kind: "set",
        values: new Set(["Person 0", "Person 1"]),
      });
      await flushRAF();
      const stats = container.querySelector(".pt-stat-rows") as HTMLElement;
      expect(stats?.dataset.value).toContain("of");
    });
  });

  describe("destroy", () => {
    it("clears container contents", async () => {
      await flushRAF();
      engine.destroy();
      expect(container.innerHTML).toBe("");
    });

    it("removes pt-table-container class", async () => {
      await flushRAF();
      expect(container.classList.contains("pt-table-container")).toBe(true);
      engine.destroy();
      expect(container.classList.contains("pt-table-container")).toBe(false);
    });
  });

  describe("state API", () => {
    it("getSort returns null initially", () => {
      expect(engine.getSort()).toBeNull();
    });

    it("setSort sets sort state", async () => {
      engine.setSort("name", "asc");
      await flushRAF();
      const sort = engine.getSort();
      expect(sort).toEqual({ column: "name", direction: "asc" });
    });

    it("getFilters returns empty array initially", () => {
      expect(engine.getFilters()).toEqual([]);
    });

    it("getFilters returns active filters", async () => {
      engine.setFilter(2, { kind: "range", min: 10, max: 50 });
      await flushRAF();
      const filters = engine.getFilters();
      expect(filters).toHaveLength(1);
      expect(filters[0].column).toBe("score");
      expect(filters[0].filter).toEqual({ kind: "range", min: 10, max: 50 });
    });

    it("getState returns full state", async () => {
      engine.setSort("score", "desc");
      engine.setFilter(3, { kind: "boolean", value: true });
      await flushRAF();
      const state = engine.getState();
      expect(state.sort).toEqual({ column: "score", direction: "desc" });
      expect(state.filters).toHaveLength(1);
      expect(state.totalCount).toBe(50);
      expect(state.filteredCount).toBeLessThanOrEqual(50);
    });

    it("onChange callback fires on filter change", async () => {
      const onChange = vi.fn();
      engine.destroy();
      engine = createTable(container, data, { onChange });
      await flushRAF();

      engine.setFilter(2, { kind: "range", min: 10, max: 50 });
      await flushRAF();
      expect(onChange).toHaveBeenCalled();
      const state = onChange.mock.calls[0][0];
      expect(state.filters).toHaveLength(1);
    });
  });

  describe("sort + filter combined", () => {
    it("sort persists through filter application", async () => {
      await flushRAF();
      engine.setSort("score", "desc");
      await flushRAF();
      engine.setFilter(3, { kind: "boolean", value: true });
      await flushRAF();
      expect(engine.getSort()).toEqual({ column: "score", direction: "desc" });
      expect(engine.getFilters()).toHaveLength(1);
    });

    it("filter persists through sort change", async () => {
      await flushRAF();
      engine.setFilter(2, { kind: "range", min: 0, max: 50 });
      await flushRAF();
      engine.setSort("name", "asc");
      await flushRAF();
      expect(engine.getFilters()).toHaveLength(1);
      expect(engine.getSort()).toEqual({ column: "name", direction: "asc" });
    });

    it("clearAllFilters preserves sort", async () => {
      await flushRAF();
      engine.setSort("id", "asc");
      engine.setFilter(2, { kind: "range", min: 10, max: 50 });
      await flushRAF();
      engine.clearAllFilters();
      await flushRAF();
      expect(engine.getSort()).toEqual({ column: "id", direction: "asc" });
      expect(engine.getFilters()).toEqual([]);
    });
  });

  describe("accessibility", () => {
    it('container has role="grid" and is focusable', async () => {
      await flushRAF();
      expect(container.getAttribute("role")).toBe("grid");
      expect(container.tabIndex).toBe(0);
    });

    it('header cells have role="columnheader"', async () => {
      await flushRAF();
      const headers = container.querySelectorAll(".pt-th");
      expect(headers.length).toBe(4);
      for (const h of headers) {
        expect(h.getAttribute("role")).toBe("columnheader");
      }
    });

    it("sorted column gets aria-sort attribute", async () => {
      await flushRAF();
      engine.setSort("name", "asc");
      await flushRAF();
      const headers = container.querySelectorAll(".pt-th");
      expect(headers[1].getAttribute("aria-sort")).toBe("ascending");
      expect(headers[0].hasAttribute("aria-sort")).toBe(false);
    });
  });

  describe("onBatchAppended", () => {
    it("updates row count when data grows", async () => {
      await flushRAF();
      // Add 10 more rows
      for (let i = 50; i < 60; i++) {
        rows.push([i + 1, `Person ${i}`, Math.round(Math.random() * 100), i % 3 !== 0]);
      }
      data.rowCount = 60;
      engine.onBatchAppended();
      await flushRAF();
      const stats = container.querySelector(".pt-stat-rows") as HTMLElement;
      expect(stats?.dataset.value).toContain("60");
    });
  });

  describe("edge cases", () => {
    it("single-row dataset mounts without crashing", async () => {
      const singleContainer = document.createElement("div");
      document.body.appendChild(singleContainer);
      const singleData = makeTableData([[1, "Solo", 42, true]]);
      const singleEngine = createTable(singleContainer, singleData);
      await flushRAF();
      // Engine mounts: header + stats bar + viewport exist
      expect(singleContainer.querySelector(".pt-header")).not.toBeNull();
      expect(singleContainer.querySelector(".pt-viewport")).not.toBeNull();
      const stats = singleContainer.querySelector(".pt-stat-rows") as HTMLElement;
      expect(stats?.dataset.value).toContain("1");
      singleEngine.destroy();
    });

    it("filter to zero results shows empty state", async () => {
      await flushRAF();
      engine.setFilter(2, { kind: "range", min: 999, max: 1000 });
      await flushRAF();
      const stats = container.querySelector(".pt-stat-rows") as HTMLElement;
      expect(stats?.dataset.value).toContain("0 of");
    });

    it("filter with single-value range (min === max)", async () => {
      await flushRAF();
      const targetScore = rows[0][2] as number;
      engine.setFilter(2, {
        kind: "range",
        min: targetScore,
        max: targetScore,
      });
      await flushRAF();
      const pills = container.querySelectorAll(".pt-filter-pill");
      expect(pills.length).toBe(1);
      // Single-value pill should NOT show range dash
      expect(pills[0].textContent).not.toContain("–");
    });

    it("set filter with empty set filters to zero", async () => {
      await flushRAF();
      engine.setFilter(1, { kind: "set", values: new Set() });
      await flushRAF();
      const stats = container.querySelector(".pt-stat-rows") as HTMLElement;
      expect(stats?.dataset.value).toContain("0 of");
    });

    it("null/NaN/Infinity values in data do not crash engine", async () => {
      const nullContainer = document.createElement("div");
      document.body.appendChild(nullContainer);
      const nullRows: unknown[][] = [
        [1, "Alice", null, true],
        [2, null, 50, false],
        [null, "Bob", NaN, null],
        [4, "Carol", Infinity, true],
        [5, "Dave", -Infinity, false],
      ];
      const nullData = makeTableData(nullRows);
      const nullEngine = createTable(nullContainer, nullData);
      await flushRAF();
      // Engine mounts without throwing
      expect(nullContainer.querySelector(".pt-header")).not.toBeNull();
      expect(nullContainer.querySelector(".pt-viewport")).not.toBeNull();
      nullEngine.destroy();
    });

    it("clearAllFilters restores full count after multiple filters", async () => {
      await flushRAF();
      engine.setFilter(1, { kind: "set", values: new Set(["Person 0"]) });
      engine.setFilter(2, { kind: "range", min: 0, max: 50 });
      engine.setFilter(3, { kind: "boolean", value: true });
      await flushRAF();
      const stats = container.querySelector(".pt-stat-rows") as HTMLElement;
      expect(stats?.dataset.value).toContain("of");

      engine.clearAllFilters();
      await flushRAF();
      expect(stats?.dataset.value).not.toContain("of");
      expect(stats?.dataset.value).toContain("50");
    });
  });
});
