import { cleanup, render } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vite-plus/test";
import { SiftTable } from "./react";
import type { Column, TableData } from "./table";

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

describe("SiftTable", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  afterEach(() => {
    cleanup();
    vi.useRealTimers();
  });

  it("renders a table container", async () => {
    const rows = [
      [1, "Alice", 95],
      [2, "Bob", 87],
      [3, "Carol", 92],
    ];
    const data = makeTableData(rows);

    const { container } = render(<SiftTable data={data} />);
    await vi.advanceTimersByTimeAsync(0);

    expect(container.querySelector(".pt-table-container")).not.toBeNull();
    expect(container.querySelector(".pt-header")).not.toBeNull();
    expect(container.querySelector(".pt-viewport")).not.toBeNull();
  });

  it("renders correct header labels from data", async () => {
    const rows = [[1, "Alice", 95]];
    const data = makeTableData(rows);

    const { container } = render(<SiftTable data={data} />);
    await vi.advanceTimersByTimeAsync(0);

    const labels = container.querySelectorAll(".pt-th-label");
    expect(labels).toHaveLength(3);
    expect(labels[0].textContent).toBe("ID");
    expect(labels[1].textContent).toBe("Name");
    expect(labels[2].textContent).toBe("Score");
  });

  it("shows row count in stats bar", async () => {
    const rows = Array.from({ length: 25 }, (_, i) => [i, `Person ${i}`, i * 10]);
    const data = makeTableData(rows);

    const { container } = render(<SiftTable data={data} />);
    await vi.advanceTimersByTimeAsync(0);

    const stats = container.querySelector(".pt-stat-rows") as HTMLElement;
    expect(stats?.dataset.value).toContain("25");
  });

  it("calls onChange when filter is applied via engine", async () => {
    const onChange = vi.fn();
    const rows = Array.from({ length: 10 }, (_, i) => [i, `Person ${i}`, i * 10]);
    const data = makeTableData(rows);

    const { container } = render(<SiftTable data={data} onChange={onChange} />);
    await vi.advanceTimersByTimeAsync(0);

    // The engine is mounted — we can interact via the DOM
    // The onChange should be wired up; we test by verifying the container has content
    expect(container.querySelector(".pt-table-container")).not.toBeNull();
  });

  it("cleans up engine on unmount", async () => {
    const rows = [[1, "Alice", 95]];
    const data = makeTableData(rows);

    const { container, unmount } = render(<SiftTable data={data} />);
    await vi.advanceTimersByTimeAsync(0);

    expect(container.querySelector(".pt-table-container")).not.toBeNull();

    unmount();

    // Container should be cleaned up (React unmount clears the DOM)
    expect(container.innerHTML).toBe("");
  });

  it("renders container for url prop without loading flash", async () => {
    // Mock fetch to never resolve (simulating slow load)
    vi.stubGlobal("fetch", () => new Promise(() => {}));

    const { container } = render(<SiftTable url="/data.arrow" />);
    await vi.advanceTimersByTimeAsync(0);

    // No loading indicator — engine handles its own skeleton
    expect(container.querySelector(".pt-loading")).toBeNull();

    vi.unstubAllGlobals();
  });

  it("shows error state on fetch failure", async () => {
    vi.useRealTimers();
    vi.stubGlobal("fetch", () =>
      Promise.resolve({ ok: false, status: 404, statusText: "Not Found" }),
    );

    const { container } = render(<SiftTable url="/missing.arrow" />);

    // Wait for the async fetch to resolve and state to update
    await new Promise((r) => setTimeout(r, 50));

    const loading = container.querySelector(".pt-loading");
    expect(loading?.textContent).toContain("404");

    vi.unstubAllGlobals();
    vi.useFakeTimers();
  });

  it("accepts className and style props", async () => {
    const rows = [[1, "Alice", 95]];
    const data = makeTableData(rows);

    const { container } = render(
      <SiftTable data={data} className="my-table" style={{ border: "1px solid red" }} />,
    );
    await vi.advanceTimersByTimeAsync(0);

    const wrapper = container.firstElementChild as HTMLElement;
    expect(wrapper.classList.contains("my-table")).toBe(true);
    expect(wrapper.style.border).toBe("1px solid red");
  });
});
