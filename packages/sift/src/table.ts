import { layout, type PreparedText, prepare } from "@chenglou/pretext";
import {
  animationFrameScheduler,
  distinctUntilChanged,
  interval,
  map,
  scan,
  throttleTime,
} from "rxjs";
import { fitColumnWidths } from "./auto-width";
import { type ColumnAction, mountColumnMenu, unmountColumnMenu } from "./column-menu";
import { renderColumnSummary, unmountColumnSummary } from "./sparkline";
// --- Types ---

export type ColumnType = "numeric" | "categorical" | "timestamp" | "boolean";

export type Column = {
  key: string;
  label: string;
  width: number;
  sortable: boolean;
  numeric: boolean;
  columnType: ColumnType;
};

export type NumericColumnSummary = {
  kind: "numeric";
  min: number;
  max: number;
  bins: { x0: number; x1: number; count: number }[];
  /** Number of distinct finite values seen (capped at tracking limit). */
  uniqueCount?: number;
  /** True if this column looks like an index/ID (suppress histogram). */
  isIndex?: boolean;
  /** Number of null/undefined/NaN values. */
  nullCount?: number;
};

export type CategoryEntry = { label: string; count: number; pct: number };

export type CategoricalColumnSummary = {
  kind: "categorical";
  uniqueCount: number;
  topCategories: CategoryEntry[];
  othersCount: number;
  othersPct: number;
  /** All categories sorted by frequency (descending). Used by the filter popover. */
  allCategories: CategoryEntry[];
  /** Median string length across all unique values. Used for display heuristics. */
  medianTextLength: number;
};

export type BooleanColumnSummary = {
  kind: "boolean";
  trueCount: number;
  falseCount: number;
  nullCount: number;
  total: number;
};

export type TimestampColumnSummary = {
  kind: "timestamp";
  min: number;
  max: number;
  bins: { x0: number; x1: number; count: number }[];
  /** Number of null/undefined values. */
  nullCount?: number;
};

export type ColumnSummary =
  | NumericColumnSummary
  | CategoricalColumnSummary
  | BooleanColumnSummary
  | TimestampColumnSummary
  | null;

export type TableData = {
  columns: Column[];
  rowCount: number;
  getCell: (row: number, col: number) => string;
  getCellRaw: (row: number, col: number) => unknown;
  columnSummaries: ColumnSummary[];
  /** Optional: prefetch visible rows in batch (WASM viewport optimization). */
  prefetchViewport?: (dataRowIndices: number[]) => void;
  /** Optional: cast a column to a different type (WASM type override). */
  castColumn?: (colIndex: number, targetType: ColumnType) => void;
  /** Optional: undo a column cast, restoring original type. Returns the original type. */
  undoCastColumn?: (colIndex: number) => ColumnType;
  /** Optional: check if a column has been cast (can be undone). */
  isColumnCast?: (colIndex: number) => boolean;
  /** Optional: recompute all column summaries (e.g. after a cast changes the data). */
  recomputeSummaries?: () => void;
  /** Optional: return sorted row indices for a column (WASM sort optimization). */
  sortColumn?: (colIndex: number, ascending: boolean) => Uint32Array;
  /** Optional: recompute filtered summaries in WASM (crossfilter fast path). */
  recomputeFilteredSummaries?: (mask: Uint8Array, filteredCount: number) => void;
  /** Optional: apply filters in WASM and return matching row indices. */
  filterRows?: (filters: (ColumnFilter | null)[]) => Uint32Array;
};

// --- Filter types ---

export type RangeFilter = { kind: "range"; min: number; max: number };
export type SetFilter = { kind: "set"; values: Set<string> };
export type BooleanFilter = { kind: "boolean"; value: boolean };
export type ColumnFilter = RangeFilter | SetFilter | BooleanFilter | null;

export type TableEngineState = {
  sort: { column: string; direction: "asc" | "desc" } | null;
  filters: { column: string; filter: ColumnFilter }[];
  filteredCount: number;
  totalCount: number;
};

export type TableEngineOptions = {
  /** Called whenever sort or filter state changes from UI interaction. */
  onChange?: (state: TableEngineState) => void;
};

export type TableEngine = {
  onBatchAppended(): void;
  /** Signal that all batches have been loaded and streaming is complete. */
  setStreamingDone(): void;
  destroy(): void;
  setFilter(colIndex: number, filter: ColumnFilter): void;
  clearFilter(colIndex: number): void;
  clearAllFilters(): void;
  /** Get current sort state. */
  getSort(): { column: string; direction: "asc" | "desc" } | null;
  /** Programmatically sort by column name and direction. Pass null to clear. */
  setSort(column: string, direction: "asc" | "desc"): void;
  /** Get current filter state for all columns. */
  getFilters(): { column: string; filter: ColumnFilter }[];
  /** Get the full explorer state (sort + filters + counts) in a serializable format. */
  getState(): TableEngineState;
};

type SortState = { col: number; dir: "asc" | "desc" } | null;

// --- Constants ---

const FONT = '14px Inter, "Helvetica Neue", Helvetica, Arial, sans-serif';
const LINE_HEIGHT = 20;
const CELL_PAD_H = 24; // 12px each side
const CELL_PAD_V = 16; // 8px top + 8px bottom
const MIN_COL_WIDTH = 60;
const OVERSCAN = 40; // base buffer rows above/below viewport
const OVERSCAN_VELOCITY_SCALE = 3; // extra rows per 10px of scroll delta
const MAX_OVERSCAN = 120; // cap total overscan per side

// --- Table Engine ---

export function createTable(
  container: HTMLElement,
  data: TableData,
  options?: TableEngineOptions,
): TableEngine {
  const { columns } = data;

  // Mutable row count — grows as batches arrive
  let rowCount = data.rowCount;

  // Filter state — one per column, null = no filter
  const filters: ColumnFilter[] = columns.map(() => null);
  let filteredCount = rowCount;

  // Unfiltered summaries (saved so we can restore when filters are cleared)
  let unfilteredSummaries: ColumnSummary[] = [...data.columnSummaries];

  // Sort state
  let sortState: SortState = null;

  // Pinned columns + visual ordering
  const pinnedColumns = new Set<number>([0]); // first column pinned by default
  // Visual column order: pinned columns first, then the rest
  let visualOrder: number[] = computeVisualOrder();

  function computeVisualOrder(): number[] {
    const pinned = [...pinnedColumns].sort((a, b) => a - b);
    const unpinned = columns.map((_, i) => i).filter((i) => !pinnedColumns.has(i));
    return [...pinned, ...unpinned];
  }

  // viewIndices: sorted position → data row (filtered + sorted)
  let viewIndices: Int32Array;

  // Growable typed arrays with capacity doubling
  let capacity = Math.max(8192, rowCount);
  let rowHeights = new Float64Array(capacity);
  let rowPositions = new Float64Array(capacity + 1);
  viewIndices = new Int32Array(capacity);
  for (let i = 0; i < rowCount; i++) viewIndices[i] = i;

  let totalHeight = 0;
  let heightsDirty = true;

  function growBuffers(needed: number) {
    if (needed <= capacity) return;
    while (capacity < needed) capacity *= 2;
    const newHeights = new Float64Array(capacity);
    newHeights.set(rowHeights.subarray(0, rowCount));
    rowHeights = newHeights;
    const newPositions = new Float64Array(capacity + 1);
    newPositions.set(rowPositions.subarray(0, rowCount + 1));
    rowPositions = newPositions;
    const newSorted = new Int32Array(capacity);
    newSorted.set(viewIndices.subarray(0, rowCount));
    viewIndices = newSorted;
  }

  // Per-cell cache: prepared text + last laid-out width/height
  type CellCache = {
    prepared: PreparedText;
    lastWidth: number;
    lastHeight: number;
  };
  const cellCaches: (CellCache[] | null)[] = [];

  function prepareCellRow(r: number): CellCache[] {
    const row = Array.from<CellCache>({ length: columns.length });
    for (let c = 0; c < columns.length; c++) {
      row[c] = {
        prepared: prepare(data.getCell(r, c), FONT),
        lastWidth: -1,
        lastHeight: 0,
      };
    }
    return row;
  }

  // Cells are prepared lazily when they enter the viewport.
  // computeRowHeight() handles null caches with an estimated height.

  // Column widths — header-based floor, refined by sampled cell data
  const colWidths = columns.map((c) => c.width);
  fitColumnWidths(data, colWidths);

  // --- Compute heights ---

  function computeRowHeight(sortedRow: number): number {
    const dataRow = viewIndices[sortedRow];
    const cache = cellCaches[dataRow];
    if (!cache) return LINE_HEIGHT + CELL_PAD_V; // estimate for unprepared rows

    let maxH = LINE_HEIGHT;
    for (let c = 0; c < columns.length; c++) {
      const cell = cache[c];
      const cellW = Math.max(1, colWidths[c] - CELL_PAD_H);
      if (cell.lastWidth !== cellW) {
        const { height } = layout(cell.prepared, cellW, LINE_HEIGHT);
        cell.lastHeight = height;
        cell.lastWidth = cellW;
      }
      if (cell.lastHeight > maxH) maxH = cell.lastHeight;
    }
    return maxH + CELL_PAD_V;
  }

  function recomputeAllHeights() {
    for (let i = 0; i < filteredCount; i++) {
      rowHeights[i] = computeRowHeight(i);
    }
    rebuildPositions();
    heightsDirty = false;
  }

  function rebuildPositions() {
    rowPositions[0] = 0;
    for (let i = 0; i < filteredCount; i++) {
      rowPositions[i + 1] = rowPositions[i] + rowHeights[i];
    }
    totalHeight = rowPositions[filteredCount];
  }

  function rowAtOffset(offset: number): number {
    let lo = 0,
      hi = filteredCount;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (rowPositions[mid + 1] <= offset) lo = mid + 1;
      else hi = mid;
    }
    return lo;
  }

  // --- Filter + Sort ---

  function rowPassesFilters(dataRow: number): boolean {
    for (let c = 0; c < columns.length; c++) {
      const f = filters[c];
      if (!f) continue;
      const raw = data.getCellRaw(dataRow, c);
      switch (f.kind) {
        case "range": {
          if (raw == null) return false;
          const v = Number(raw);
          if (!Number.isFinite(v)) return false;
          if (v < f.min || v > f.max) return false;
          break;
        }
        case "set": {
          const s = data.getCell(dataRow, c);
          if (!f.values.has(s)) return false;
          break;
        }
        case "boolean": {
          if (Boolean(raw) !== f.value) return false;
          break;
        }
      }
    }
    return true;
  }

  function hasActiveFilters(): boolean {
    return filters.some((f) => f !== null);
  }

  function applyFilterAndSort() {
    // Step 1: filter
    const filtered: number[] = [];
    if (hasActiveFilters()) {
      if (data.filterRows) {
        // WASM fast path: apply all filters in Rust, no per-cell FFI
        const wasmResult = data.filterRows(filters);
        for (let i = 0; i < wasmResult.length; i++) filtered.push(wasmResult[i]);
      } else {
        for (let i = 0; i < rowCount; i++) {
          if (rowPassesFilters(i)) filtered.push(i);
        }
      }
    } else {
      for (let i = 0; i < rowCount; i++) filtered.push(i);
    }
    filteredCount = filtered.length;

    // Step 2: sort the filtered set
    if (sortState) {
      const { col, dir } = sortState;
      if (data.sortColumn) {
        // WASM fast path: get sorted indices for all rows, then intersect with filter
        const sortedAll = data.sortColumn(col, dir === "asc");
        if (hasActiveFilters()) {
          const filterSet = new Set(filtered);
          filtered.length = 0;
          for (let i = 0; i < sortedAll.length; i++) {
            if (filterSet.has(sortedAll[i])) filtered.push(sortedAll[i]);
          }
        } else {
          filtered.length = 0;
          for (let i = 0; i < sortedAll.length; i++) {
            filtered.push(sortedAll[i]);
          }
        }
      } else {
        // JS fallback: sort the filtered set with comparators
        const colType = columns[col].columnType;
        const isNumeric = colType === "numeric" || colType === "timestamp";
        filtered.sort((a, b) => {
          let cmp: number;
          if (isNumeric) {
            const rawA = data.getCellRaw(a, col);
            const rawB = data.getCellRaw(b, col);
            const va = rawA == null ? NaN : Number(rawA);
            const vb = rawB == null ? NaN : Number(rawB);
            const aOk = Number.isFinite(va) || va === Infinity || va === -Infinity;
            const bOk = Number.isFinite(vb) || vb === Infinity || vb === -Infinity;
            if (!aOk && !bOk) cmp = 0;
            else if (!aOk) return 1;
            else if (!bOk) return -1;
            else cmp = va - vb;
          } else if (colType === "boolean") {
            const rawA = data.getCellRaw(a, col);
            const rawB = data.getCellRaw(b, col);
            if (rawA == null && rawB == null) cmp = 0;
            else if (rawA == null) return 1;
            else if (rawB == null) return -1;
            else cmp = (rawA ? 1 : 0) - (rawB ? 1 : 0);
          } else {
            const rawA = data.getCellRaw(a, col);
            const rawB = data.getCellRaw(b, col);
            if (rawA == null && rawB == null) cmp = 0;
            else if (rawA == null) return 1;
            else if (rawB == null) return -1;
            else {
              const sa = data.getCell(a, col);
              const sb = data.getCell(b, col);
              cmp = sa < sb ? -1 : sa > sb ? 1 : 0;
            }
          }
          return dir === "asc" ? cmp : -cmp;
        });
      }
    }

    // Step 3: write to viewIndices
    if (filtered.length > capacity) growBuffers(filtered.length);
    for (let i = 0; i < filtered.length; i++) {
      viewIndices[i] = filtered[i];
    }

    heightsDirty = true;
  }

  // --- DOM ---

  container.innerHTML = "";
  container.classList.add("sift-table-container");
  container.setAttribute("role", "grid");
  container.setAttribute("aria-label", "Data table");
  container.setAttribute("aria-rowcount", String(rowCount));
  container.setAttribute("aria-colcount", String(columns.length));

  // Streaming state — progress bar is appended after stats bar below
  let streaming = true;
  const progressBar = document.createElement("div");
  progressBar.className = "sift-progress-bar";
  progressBar.innerHTML = '<div class="sift-progress-bar-fill"></div>';

  // Header — lives inside the scroll content so it scrolls
  // horizontally with the data. position: sticky keeps it at top.
  const headerEl = document.createElement("div");
  headerEl.className = "sift-header";

  const headerRowEl = document.createElement("div");
  headerRowEl.className = "sift-header-row";
  headerRowEl.setAttribute("role", "row");

  const summaryContainers: HTMLDivElement[] = [];

  for (let c = 0; c < columns.length; c++) {
    const th = document.createElement("div");
    th.className = "sift-th";
    th.setAttribute("role", "columnheader");
    th.setAttribute("aria-colindex", String(c + 1));
    th.style.width = colWidths[c] + "px";
    th.dataset.col = String(c);

    const topRow = document.createElement("div");
    topRow.className = "sift-th-top";

    const label = document.createElement("span");
    label.className = "sift-th-label";
    label.textContent = columns[c].label;
    topRow.appendChild(label);

    if (columns[c].sortable) {
      const arrow = document.createElement("span");
      arrow.className = "sift-sort-arrow";
      arrow.textContent = "";
      topRow.appendChild(arrow);
      topRow.style.cursor = "pointer";
      topRow.addEventListener("click", () => onSortClick(c));
    }

    th.appendChild(topRow);

    // Type icon — hide for index columns (empty label = hidden index)
    if (columns[c].label) {
      const typeIcon = document.createElement("span");
      typeIcon.className = "sift-type-icon";
      typeIcon.textContent =
        columns[c].columnType === "numeric"
          ? "#"
          : columns[c].columnType === "boolean"
            ? "◉"
            : columns[c].columnType === "timestamp"
              ? "◷"
              : "Aa";
      typeIcon.title = columns[c].columnType;
      th.appendChild(typeIcon);
    }

    const summaryEl = document.createElement("div");
    summaryEl.className = "sift-th-summary";
    // Prevent clicks on summary charts (filter interactions) from triggering sort
    summaryEl.addEventListener("click", (e) => e.stopPropagation());
    th.appendChild(summaryEl);
    summaryContainers.push(summaryEl);

    // Keyboard shortcuts on column headers
    th.setAttribute("tabindex", "0");
    th.addEventListener("keydown", (e) => {
      if (e.key === "p" || e.key === "P") {
        e.preventDefault();
        const action: ColumnAction = pinnedColumns.has(c) ? { kind: "unpin" } : { kind: "pin" };
        handleColumnAction(c, action);
      } else if (e.key === "Enter" && columns[c].sortable) {
        e.preventDefault();
        onSortClick(c);
      } else if (e.key === "ArrowRight") {
        e.preventDefault();
        const ths = Array.from(headerRowEl.children) as HTMLDivElement[];
        const curVi = ths.indexOf(th);
        if (curVi < ths.length - 1) (ths[curVi + 1] as HTMLElement).focus();
      } else if (e.key === "ArrowLeft") {
        e.preventDefault();
        const ths = Array.from(headerRowEl.children) as HTMLDivElement[];
        const curVi = ths.indexOf(th);
        if (curVi > 0) (ths[curVi - 1] as HTMLElement).focus();
      }
    });

    // Context menu: right-click (desktop) + long-press (mobile)
    function openColumnMenu(x: number, y: number) {
      mountColumnMenu(
        {
          colIndex: c,
          colName: columns[c].label,
          colType: columns[c].columnType,
          isPinned: pinnedColumns.has(c),
          isCast: data.isColumnCast ? data.isColumnCast(c) : false,
          isStreaming: streaming,
          sortDirection: sortState?.col === c ? sortState.dir : null,
          x,
          y,
        },
        handleColumnAction,
      );
    }

    th.addEventListener("contextmenu", (e) => {
      e.preventDefault();
      openColumnMenu(e.clientX, e.clientY);
    });

    // Long-press for touch devices (500ms threshold)
    let longPressTimer: ReturnType<typeof setTimeout> | null = null;
    th.addEventListener(
      "touchstart",
      (e) => {
        const touch = e.touches[0];
        const startX = touch.clientX;
        const startY = touch.clientY;
        longPressTimer = setTimeout(() => {
          longPressTimer = null;
          openColumnMenu(startX, startY);
        }, 500);
      },
      { passive: true },
    );
    th.addEventListener(
      "touchmove",
      () => {
        if (longPressTimer) {
          clearTimeout(longPressTimer);
          longPressTimer = null;
        }
      },
      { passive: true },
    );
    th.addEventListener("touchend", () => {
      if (longPressTimer) {
        clearTimeout(longPressTimer);
        longPressTimer = null;
      }
    });

    if (c < columns.length - 1) {
      const handle = document.createElement("div");
      handle.className = "sift-resize-handle";
      handle.addEventListener("pointerdown", (e) => onResizeStart(e, c));
      th.appendChild(handle);
    }

    headerRowEl.appendChild(th);
  }

  headerEl.appendChild(headerRowEl);
  // Header is appended to scroll content (not container) so it scrolls
  // horizontally with data. position: sticky; top: 0 keeps it visible.

  // Create stable filter callbacks per column
  const filterCallbacks: ((filter: ColumnFilter) => void)[] = columns.map(
    (_, c) => (filter: ColumnFilter) => setFilter(c, filter),
  );

  function renderSummary(c: number, visibleBins?: number[]) {
    const summary = data.columnSummaries[c];
    if (summary) {
      const unfiltered = hasActiveFilters() ? (unfilteredSummaries[c] ?? undefined) : undefined;
      renderColumnSummary(
        summaryContainers[c],
        summary,
        colWidths[c] - CELL_PAD_H,
        visibleBins,
        filters[c],
        filterCallbacks[c],
        unfiltered,
      );
    }
  }

  function renderAllSummaries() {
    for (let c = 0; c < columns.length; c++) renderSummary(c);
  }
  renderAllSummaries();

  // Scroll viewport
  const viewport = document.createElement("div");
  viewport.className = "sift-viewport";

  const scrollContent = document.createElement("div");
  scrollContent.className = "sift-scroll-content";
  // Set min-width so horizontal scroll position is preserved when pool rows are hidden
  function updateScrollContentWidth() {
    const totalW = colWidths.reduce((s, w) => s + w, 0);
    scrollContent.style.minWidth = totalW + "px";
  }
  updateScrollContentWidth();

  const rowPool = document.createElement("div");
  rowPool.className = "sift-row-pool";

  scrollContent.appendChild(headerEl); // Header inside scroll content for natural H scroll
  scrollContent.appendChild(rowPool);

  // Empty state (shown when filters exclude all rows)
  const emptyEl = document.createElement("div");
  emptyEl.className = "sift-empty-state";
  emptyEl.style.display = "none";

  const emptyText = document.createElement("div");
  emptyText.className = "sift-empty-text";
  emptyText.textContent = "No matching rows";

  const emptyClearBtn = document.createElement("button");
  emptyClearBtn.className = "sift-empty-clear";
  emptyClearBtn.textContent = "Clear all filters";
  emptyClearBtn.addEventListener("click", () => clearAllFilters());

  emptyEl.appendChild(emptyText);
  emptyEl.appendChild(emptyClearBtn);

  // Empty state goes inside scroll content (after header, before row pool)
  scrollContent.appendChild(emptyEl);

  viewport.appendChild(scrollContent);
  container.appendChild(viewport);

  // Stats bar
  const statsEl = document.createElement("div");
  statsEl.className = "sift-stats";

  // ARIA live region for screen reader announcements (filter changes, streaming)
  const ariaLive = document.createElement("div");
  ariaLive.setAttribute("aria-live", "polite");
  ariaLive.setAttribute("role", "status");
  ariaLive.className = "sr-only";
  ariaLive.style.cssText =
    "position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0,0,0,0)";
  container.appendChild(ariaLive);

  function makeStatSpan(className: string): HTMLSpanElement {
    const el = document.createElement("span");
    el.className = `sift-stat-value ${className}`;
    return el;
  }

  // Status indicator (streaming dot → checkmark)
  const statusIndicator = document.createElement("span");
  statusIndicator.className = "sift-status-indicator sift-status-streaming";
  statusIndicator.textContent = "●";
  statusIndicator.title = "Loading data…";

  const statRows = makeStatSpan("sift-stat-rows");
  // Debug stats (DOM rows, FPS) — hidden by default
  const debugGroup = document.createElement("span");
  debugGroup.className = "sift-debug-group";
  debugGroup.style.display = "none";
  const statDom = makeStatSpan("sift-stat-dom");
  const statFrame = makeStatSpan("sift-stat-frame");

  // Reusable odometer — rolling digit strips for any numeric display
  type OdometerSlot = {
    el: HTMLSpanElement;
    strip: HTMLSpanElement | null;
    current: string;
    pos: number;
  };

  function createOdometer(host: HTMLElement): {
    update: (text: string) => void;
  } {
    host.classList.add("sift-odometer");
    const slots: OdometerSlot[] = [];
    // Track the previous numeric value to determine roll direction
    let prevNumericValue = 0;

    // Strip layout: 9-0-1-2-3-4-5-6-7-8-9-0 (12 positions)
    // Canonical positions 1-10 map to digits 0-9
    // Position 0 = extra 9 (for wrapping down past 0)
    // Position 11 = extra 0 (for wrapping up past 9)
    const STRIP_DIGITS = [9, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 0];

    function createDigitStrip(): HTMLSpanElement {
      const strip = document.createElement("span");
      strip.className = "sift-odo-strip";
      for (const d of STRIP_DIGITS) {
        const digit = document.createElement("span");
        digit.className = "sift-odo-num";
        digit.textContent = String(d);
        strip.appendChild(digit);
      }
      return strip;
    }

    // Canonical position for a digit (1-10)
    function canonicalPos(d: number): number {
      return d + 1;
    }

    function update(text: string) {
      // Extract numeric value from text to determine direction
      const numStr = text.replace(/[^0-9]/g, "");
      const numericValue = numStr ? parseInt(numStr) : 0;
      const increasing = numericValue >= prevNumericValue;
      prevNumericValue = numericValue;

      while (slots.length < text.length) {
        const el = document.createElement("span");
        el.className = "sift-odo-slot";
        host.appendChild(el);
        slots.push({ el, strip: null, current: "", pos: -1 });
      }
      while (slots.length > text.length) {
        const removed = slots.pop()!;
        host.removeChild(removed.el);
      }

      for (let i = 0; i < text.length; i++) {
        const ch = text[i];
        const slot = slots[i];

        if (ch === slot.current) continue;

        const isDigit = ch >= "0" && ch <= "9";

        if (isDigit) {
          if (!slot.strip) {
            slot.el.textContent = "";
            slot.strip = createDigitStrip();
            slot.el.appendChild(slot.strip);
            slot.pos = -1;
          }
          const target = parseInt(ch);
          const prev = slot.current >= "0" && slot.current <= "9" ? parseInt(slot.current) : -1;

          let targetPos: number;
          if (prev === -1 || slot.pos === -1) {
            // First time — go directly to canonical position
            targetPos = canonicalPos(target);
          } else if (increasing && prev === 9 && target === 0) {
            // Wrapping up: 9→0, roll to position 11 (extra 0 at bottom)
            targetPos = 11;
          } else if (!increasing && prev === 0 && target === 9) {
            // Wrapping down: 0→9, roll to position 0 (extra 9 at top)
            targetPos = 0;
          } else {
            targetPos = canonicalPos(target);
          }

          slot.strip.style.transform = `translateY(${-targetPos * 1.2}em)`;
          slot.pos = targetPos;

          // After wrap transitions, snap back to canonical position
          if (targetPos === 0 || targetPos === 11) {
            const canonical = canonicalPos(target);
            const strip = slot.strip;
            const onEnd = () => {
              strip.removeEventListener("transitionend", onEnd);
              // Disable transition, snap, re-enable
              strip.style.transition = "none";
              strip.style.transform = `translateY(${-canonical * 1.2}em)`;
              slot.pos = canonical;
              // Force reflow then restore transition
              void strip.offsetHeight;
              strip.style.transition = "";
            };
            slot.strip.addEventListener("transitionend", onEnd);
          }
        } else {
          if (slot.strip) {
            slot.el.removeChild(slot.strip);
            slot.strip = null;
            slot.pos = -1;
          }
          slot.el.textContent = ch;
        }
        slot.current = ch;
      }
      // Expose visible text for testing (textContent includes hidden strip digits)
      host.dataset.value = text;
    }

    return { update };
  }

  const rowsOdometer = createOdometer(statRows);

  function updateRowCountDisplay() {
    if (hasActiveFilters()) {
      rowsOdometer.update(`${filteredCount.toLocaleString()} of ${rowCount.toLocaleString()} rows`);
    } else {
      rowsOdometer.update(`${rowCount.toLocaleString()} rows`);
    }
    // Keep ARIA row count in sync
    container.setAttribute("aria-rowcount", String(filteredCount + 1)); // +1 for header row
  }
  updateRowCountDisplay();

  // Fullscreen toggle — only shown if the Fullscreen API is available AND
  // permitted in the current context (not the case in some embeddings like
  // MCP App iframes without the right Permissions Policy).
  const fullscreenSupported =
    (document.fullscreenEnabled ?? (document as any).webkitFullscreenEnabled ?? false) &&
    !!(container.requestFullscreen ?? (container as any).webkitRequestFullscreen);
  const fullscreenBtn = document.createElement("button");
  fullscreenBtn.className = "sift-fullscreen-btn";
  fullscreenBtn.title = "Toggle fullscreen";
  fullscreenBtn.textContent = "⛶";
  if (!fullscreenSupported) {
    fullscreenBtn.style.display = "none";
  }
  fullscreenBtn.addEventListener("click", () => {
    const fsElement =
      document.fullscreenElement ?? (document as any).webkitFullscreenElement ?? null;
    if (fsElement === container) {
      (document.exitFullscreen ?? (document as any).webkitExitFullscreen)?.call(document);
    } else {
      (container.requestFullscreen ?? (container as any).webkitRequestFullscreen)?.call(container);
    }
  });

  // Update button label on fullscreen change
  function onFullscreenChange() {
    const fsElement =
      document.fullscreenElement ?? (document as any).webkitFullscreenElement ?? null;
    const isFS = fsElement === container;
    fullscreenBtn.textContent = isFS ? "⛶" : "⛶";
    fullscreenBtn.title = isFS ? "Exit fullscreen" : "Toggle fullscreen";
    // Trigger re-render since dimensions changed
    heightsDirty = true;
    scheduleRender();
  }
  document.addEventListener("fullscreenchange", onFullscreenChange);
  document.addEventListener("webkitfullscreenchange", onFullscreenChange);

  const filterPillsEl = document.createElement("div");
  filterPillsEl.className = "sift-filter-pills";

  const statsSpacer = document.createElement("div");
  statsSpacer.style.flex = "1";

  // Debug toggle button
  const debugBtn = document.createElement("button");
  debugBtn.className = "sift-debug-btn";
  debugBtn.title = "Toggle debug stats";
  debugBtn.textContent = "⚙";
  debugBtn.addEventListener("click", () => {
    const visible = debugGroup.style.display !== "none";
    debugGroup.style.display = visible ? "none" : "";
    debugBtn.classList.toggle("sift-debug-active", !visible);
  });

  debugGroup.append(sep(), statDom, sep(), statFrame);
  statsEl.append(
    statusIndicator,
    statRows,
    debugGroup,
    filterPillsEl,
    statsSpacer,
    debugBtn,
    fullscreenBtn,
  );
  container.appendChild(statsEl);

  // Streaming progress bar — at the bottom of the table, below the stats bar
  container.appendChild(progressBar);

  // Expand columns to fill container width when there are few columns
  {
    const containerW = viewport.clientWidth;
    const totalW = colWidths.reduce((s, w) => s + w, 0);
    if (containerW > 0 && totalW < containerW) {
      const scale = containerW / totalW;
      const ths = headerRowEl.children as HTMLCollectionOf<HTMLDivElement>;
      for (let c = 0; c < columns.length; c++) {
        colWidths[c] = Math.round(colWidths[c] * scale);
        ths[c].style.width = colWidths[c] + "px";
      }
      updateScrollContentWidth();
      heightsDirty = true;
    }
  }

  function rebuildFilterPills() {
    filterPillsEl.innerHTML = "";
    for (let c = 0; c < columns.length; c++) {
      const f = filters[c];
      if (!f) continue;
      const pill = document.createElement("span");
      pill.className = "sift-filter-pill";

      let text = columns[c].label + ": ";
      switch (f.kind) {
        case "range": {
          const colType = columns[c].columnType;
          if (f.min === f.max) {
            // Single value — no range display needed
            if (colType === "timestamp") {
              text += new Date(f.min).toLocaleDateString(undefined, {
                month: "short",
                day: "numeric",
                year: "2-digit",
              });
            } else {
              text += f.min.toLocaleString(undefined, {
                maximumFractionDigits: 1,
              });
            }
          } else if (colType === "timestamp") {
            const fmt = (v: number) =>
              new Date(v).toLocaleDateString(undefined, {
                month: "short",
                day: "numeric",
                year: "2-digit",
              });
            text += `${fmt(f.min)} – ${fmt(f.max)}`;
          } else {
            const minStr = f.min.toLocaleString(undefined, {
              maximumFractionDigits: 1,
            });
            const maxStr = f.max.toLocaleString(undefined, {
              maximumFractionDigits: 1,
            });
            text += minStr === maxStr ? minStr : `${minStr} – ${maxStr}`;
          }
          break;
        }
        case "set":
          text += [...f.values].map((v) => (v.length > 12 ? v.slice(0, 11) + "…" : v)).join(", ");
          break;
        case "boolean":
          text += f.value ? "Yes" : "No";
          break;
      }

      const label = document.createElement("span");
      label.textContent = text;

      const closeBtn = document.createElement("button");
      closeBtn.className = "sift-filter-pill-x";
      closeBtn.textContent = "×";
      closeBtn.addEventListener("click", (e) => {
        e.stopPropagation();
        clearFilter(c);
      });

      pill.append(label, closeBtn);
      filterPillsEl.appendChild(pill);
    }
  }

  function sep(): HTMLSpanElement {
    const s = document.createElement("span");
    s.className = "sift-stat-sep";
    s.textContent = " · ";
    return s;
  }

  let prevDom = "";

  // RxJS FPS: frame counter on animationFrameScheduler
  const fpsOdometer = createOdometer(statFrame);

  const fps$ = interval(0, animationFrameScheduler).pipe(
    scan<number, { prevTime: number; deltas: number[] }>(
      (state, _) => {
        const now = performance.now();
        if (state.prevTime > 0) {
          const delta = now - state.prevTime;
          const deltas = [...state.deltas, delta].slice(-30);
          return { prevTime: now, deltas };
        }
        return { prevTime: now, deltas: [] };
      },
      { prevTime: 0, deltas: [] },
    ),
    map(({ deltas }) => {
      if (deltas.length === 0) return "–";
      const avg = deltas.reduce((a, b) => a + b, 0) / deltas.length;
      const raw = 1000 / avg;
      // Stabilize: round to nearest 5 above 60fps to avoid jitter
      const fps = raw >= 60 ? Math.round(raw / 5) * 5 : Math.round(raw);
      return String(fps);
    }),
  );
  const fpsSub = fps$
    .pipe(
      map((fpsStr) => `${fpsStr} fps`),
      distinctUntilChanged(),
      throttleTime(400, animationFrameScheduler, { trailing: true }),
    )
    .subscribe((text) => {
      fpsOdometer.update(text);
    });

  function updateStat(el: HTMLSpanElement, value: string, prev: string): string {
    if (value !== prev) {
      el.textContent = value;
      el.classList.remove("sift-stat-flash");
      void el.offsetWidth;
      el.classList.add("sift-stat-flash");
    }
    return value;
  }

  // --- Visible range overlay for header sparklines ---

  let lastVisFirst = -1;
  let lastVisLast = -1;

  function updateVisibleOverlays(visFirst: number, visLast: number) {
    for (let c = 0; c < columns.length; c++) {
      const summary = data.columnSummaries[c];
      if (!summary || (summary.kind !== "numeric" && summary.kind !== "timestamp")) continue;

      const bins = summary.bins;
      const visibleBins = Array.from<number>({ length: bins.length }).fill(0);
      const binWidth = (summary.max - summary.min) / bins.length || 1;

      for (let r = visFirst; r <= visLast; r++) {
        const dataRow = viewIndices[r];
        const raw = data.getCellRaw(dataRow, c);
        if (raw == null) continue;
        const v = Number(raw);
        if (!Number.isFinite(v)) continue;
        let idx = Math.floor((v - summary.min) / binWidth);
        if (idx >= bins.length) idx = bins.length - 1;
        if (idx < 0) idx = 0;
        visibleBins[idx]++;
      }

      renderSummary(c, visibleBins);
    }
  }

  // --- Row pool ---

  type PooledRow = {
    el: HTMLDivElement;
    cells: HTMLDivElement[];
    assignedRow: number;
  };

  const pool: PooledRow[] = [];

  function getPooledRow(): PooledRow {
    for (const pr of pool) {
      if (pr.assignedRow === -1) return pr;
    }
    const el = document.createElement("div");
    el.className = "sift-row";
    el.setAttribute("role", "row");
    const cells: HTMLDivElement[] = [];
    // Create cells in data order (cells[c] = column c)
    for (let c = 0; c < columns.length; c++) {
      const cell = document.createElement("div");
      cell.className = "sift-cell";
      cell.setAttribute("role", "gridcell");
      cell.setAttribute("aria-colindex", String(c + 1));
      cell.style.width = colWidths[c] + "px";
      cells.push(cell);
    }
    // Append in visual order
    for (const c of visualOrder) {
      el.appendChild(cells[c]);
    }
    rowPool.appendChild(el);
    const pr: PooledRow = { el, cells, assignedRow: -1 };
    pool.push(pr);
    return pr;
  }

  // --- Cell rendering (type-aware) ---

  function renderCell(cellEl: HTMLDivElement, dataRow: number, colIndex: number) {
    const col = columns[colIndex];
    const raw = data.getCellRaw(dataRow, colIndex);
    const str = data.getCell(dataRow, colIndex);

    // Clear previous content
    cellEl.textContent = "";
    cellEl.className = "sift-cell";

    // Null values get a distinct badge regardless of column type
    if (raw == null) {
      const badge = document.createElement("span");
      badge.className = "sift-badge sift-badge-null";
      badge.textContent = "null";
      cellEl.appendChild(badge);
      return;
    }

    switch (col.columnType) {
      case "boolean": {
        const badge = document.createElement("span");
        badge.className = raw ? "sift-badge sift-badge-true" : "sift-badge sift-badge-false";
        badge.textContent = raw ? "Yes" : "No";
        cellEl.appendChild(badge);
        break;
      }
      case "timestamp": {
        cellEl.textContent = str;
        cellEl.classList.add("sift-cell-timestamp");
        break;
      }
      default:
        if (
          Array.isArray(raw) &&
          raw.length > 0 &&
          raw.every((item) => !Array.isArray(item) && (typeof item !== "object" || item === null))
        ) {
          cellEl.classList.add("sift-cell-list");
          for (const item of raw) {
            const badge = document.createElement("span");
            badge.className = "sift-badge sift-badge-list-item";
            badge.textContent = item == null ? "null" : String(item);
            cellEl.appendChild(badge);
          }
        } else {
          cellEl.textContent = str;
        }
    }
  }

  // --- Render loop ---

  let lastScrollTop = -1;
  let lastViewportHeight = -1;
  let scheduledRaf: number | null = null;

  function scheduleRender() {
    if (scheduledRaf !== null) return;
    scheduledRaf = requestAnimationFrame(() => {
      scheduledRaf = null;
      render();
    });
  }

  function render() {
    if (heightsDirty) {
      recomputeAllHeights();
      // Account for header height — row pool is absolute inside scroll-content
      const headerH = headerEl.offsetHeight;
      rowPool.style.top = headerH + "px";
      scrollContent.style.height = totalHeight + headerH + "px";
    }

    if (filteredCount === 0) {
      emptyEl.style.display = "";
      rowPool.style.display = "none";
      return;
    }
    emptyEl.style.display = "none";
    rowPool.style.display = "";

    const headerH = headerEl.offsetHeight;
    // Always keep the row pool below the sticky header
    rowPool.style.top = headerH + "px";
    const scrollTop = Math.max(0, viewport.scrollTop - headerH);
    const viewportH = viewport.clientHeight;

    // True visible range (no overscan) — used for header overlays
    const visFirst = rowAtOffset(scrollTop);
    const visLast = Math.min(rowAtOffset(scrollTop + viewportH), filteredCount - 1);

    // Scroll velocity: proportional overscan in the direction of travel
    const scrollDelta = scrollTop - lastScrollTop;
    const scrollingDown = scrollDelta > 0;
    const absDelta = Math.abs(scrollDelta);
    const velocityOverscan = Math.min(
      Math.round((absDelta / 10) * OVERSCAN_VELOCITY_SCALE),
      MAX_OVERSCAN,
    );

    const overscanBefore = OVERSCAN + (scrollingDown ? 0 : velocityOverscan);
    const overscanAfter = OVERSCAN + (scrollingDown ? velocityOverscan : 0);

    const first = Math.max(0, visFirst - overscanBefore);
    const last = Math.min(visLast + overscanAfter, filteredCount - 1);

    // Prefetch visible rows in batch (WASM viewport optimization)
    if (data.prefetchViewport) {
      const visibleDataRows: number[] = [];
      for (let r = first; r <= last; r++) {
        visibleDataRows.push(viewIndices[r]);
      }
      data.prefetchViewport(visibleDataRows);
    }

    for (const pr of pool) {
      if (pr.assignedRow !== -1 && (pr.assignedRow < first || pr.assignedRow > last)) {
        pr.el.style.display = "none";
        pr.assignedRow = -1;
      }
    }

    let lazyPrepared = false;
    for (let r = first; r <= last; r++) {
      const dataRow = viewIndices[r];

      // Lazy-prepare cells on first visibility
      if (!cellCaches[dataRow]) {
        cellCaches[dataRow] = prepareCellRow(dataRow);
        // Recompute this row's height now that we have real measurements
        rowHeights[r] = computeRowHeight(r);
        lazyPrepared = true;
      }

      let existing = false;
      for (const pr of pool) {
        if (pr.assignedRow === r) {
          existing = true;
          break;
        }
      }
      if (existing) continue;

      const pr = getPooledRow();
      pr.assignedRow = r;
      pr.el.style.display = "";
      pr.el.style.transform = `translateY(${rowPositions[r]}px)`;
      pr.el.style.height = rowHeights[r] + "px";
      pr.el.setAttribute("aria-rowindex", String(r + 2)); // 1-based, header is row 1

      if (r % 2 === 1) pr.el.classList.add("sift-row-alt");
      else pr.el.classList.remove("sift-row-alt");

      for (let c = 0; c < columns.length; c++) {
        renderCell(pr.cells[c], dataRow, c);
        pr.cells[c].style.width = colWidths[c] + "px";
        applyCellPinStyle(pr.cells[c], c);
      }
    }

    // If we lazy-prepared any rows, positions shifted — recycle everything
    // and re-render with corrected positions
    if (lazyPrepared) {
      rebuildPositions();
      scrollContent.style.height = totalHeight + "px";
      // Reset all assignments so the next pass positions them correctly
      for (const pr of pool) {
        pr.assignedRow = -1;
        pr.el.style.display = "none";
      }
      // Re-assign with corrected positions (no more lazy-prep this pass since we just did it)
      for (let r = first; r <= last; r++) {
        const dataRow = viewIndices[r];
        const pr = getPooledRow();
        pr.assignedRow = r;
        pr.el.style.display = "";
        pr.el.style.transform = `translateY(${rowPositions[r]}px)`;
        pr.el.style.height = rowHeights[r] + "px";
        pr.el.setAttribute("aria-rowindex", String(r + 2));
        if (r % 2 === 1) pr.el.classList.add("sift-row-alt");
        else pr.el.classList.remove("sift-row-alt");
        for (let c = 0; c < columns.length; c++) {
          renderCell(pr.cells[c], dataRow, c);
          pr.cells[c].style.width = colWidths[c] + "px";
          applyCellPinStyle(pr.cells[c], c);
        }
      }
    }

    if (lastScrollTop === scrollTop && lastViewportHeight === viewportH) {
      for (const pr of pool) {
        if (pr.assignedRow === -1) continue;
        for (let c = 0; c < columns.length; c++) {
          pr.cells[c].style.width = colWidths[c] + "px";
        }
        pr.el.style.height = rowHeights[pr.assignedRow] + "px";
        pr.el.style.transform = `translateY(${rowPositions[pr.assignedRow]}px)`;
      }
    }

    if (visFirst !== lastVisFirst || visLast !== lastVisLast) {
      lastVisFirst = visFirst;
      lastVisLast = visLast;
      updateVisibleOverlays(visFirst, visLast);
    }

    lastScrollTop = scrollTop;
    lastViewportHeight = viewportH;

    const domStr = `${first}–${Math.min(last, filteredCount - 1)} loaded · ${pool.filter((p) => p.assignedRow !== -1).length} DOM rows`;
    prevDom = updateStat(statDom, domStr, prevDom);
  }

  // --- Scroll handler ---

  function onScroll() {
    // Header scrolls naturally with content (it's inside scroll-content)
    scheduleRender();
  }

  viewport.addEventListener("scroll", onScroll, { passive: true });

  window.addEventListener("resize", scheduleRender);

  // --- Mobile tap-row detail sheet ---

  let activeDetailSheet: HTMLDivElement | null = null;

  function typeIconChar(ct: ColumnType): string {
    return ct === "numeric" ? "#" : ct === "boolean" ? "◉" : ct === "timestamp" ? "◷" : "Aa";
  }

  function dismissDetailSheet() {
    if (!activeDetailSheet) return;
    const sheet = activeDetailSheet;
    const backdrop = sheet.previousElementSibling as HTMLElement | null;
    sheet.classList.remove("sift-detail-sheet-open");
    sheet.addEventListener(
      "transitionend",
      () => {
        sheet.remove();
        backdrop?.remove();
      },
      { once: true },
    );
    activeDetailSheet = null;
  }

  function showDetailSheet(viewRow: number) {
    // Dismiss any existing sheet first
    dismissDetailSheet();

    const dataRow = viewIndices[viewRow];

    // Backdrop
    const backdrop = document.createElement("div");
    backdrop.className = "sift-detail-backdrop";
    backdrop.addEventListener("click", dismissDetailSheet);

    // Sheet
    const sheet = document.createElement("div");
    sheet.className = "sift-detail-sheet";

    // Header with row number and close button
    const header = document.createElement("div");
    header.className = "sift-detail-header";

    const title = document.createElement("span");
    title.className = "sift-detail-title";
    title.textContent = `Row ${dataRow + 1}`;

    const closeBtn = document.createElement("button");
    closeBtn.className = "sift-detail-close";
    closeBtn.textContent = "×";
    closeBtn.addEventListener("click", dismissDetailSheet);

    header.appendChild(title);
    header.appendChild(closeBtn);
    sheet.appendChild(header);

    // Column-value list
    const list = document.createElement("div");
    list.className = "sift-detail-list";

    for (let c = 0; c < columns.length; c++) {
      const row = document.createElement("div");
      row.className = "sift-detail-row";

      const nameEl = document.createElement("div");
      nameEl.className = "sift-detail-col-name";

      const icon = document.createElement("span");
      icon.className = "sift-detail-type-icon";
      icon.textContent = typeIconChar(columns[c].columnType);

      const label = document.createElement("span");
      label.textContent = columns[c].label;

      nameEl.appendChild(icon);
      nameEl.appendChild(label);

      const valueEl = document.createElement("div");
      valueEl.className = "sift-detail-col-value";

      const raw = data.getCellRaw(dataRow, c);
      if (raw == null) {
        const badge = document.createElement("span");
        badge.className = "sift-badge sift-badge-null";
        badge.textContent = "null";
        valueEl.appendChild(badge);
      } else if (columns[c].columnType === "boolean") {
        const badge = document.createElement("span");
        badge.className = raw ? "sift-badge sift-badge-true" : "sift-badge sift-badge-false";
        badge.textContent = raw ? "Yes" : "No";
        valueEl.appendChild(badge);
      } else if (
        Array.isArray(raw) &&
        raw.length > 0 &&
        raw.every((item) => !Array.isArray(item) && (typeof item !== "object" || item === null))
      ) {
        valueEl.classList.add("sift-cell-list");
        for (const item of raw) {
          const badge = document.createElement("span");
          badge.className = "sift-badge sift-badge-list-item";
          badge.textContent = item == null ? "null" : String(item);
          valueEl.appendChild(badge);
        }
      } else {
        valueEl.textContent = data.getCell(dataRow, c);
      }

      row.appendChild(nameEl);
      row.appendChild(valueEl);
      list.appendChild(row);
    }

    sheet.appendChild(list);

    document.body.appendChild(backdrop);
    document.body.appendChild(sheet);
    activeDetailSheet = sheet;

    // Trigger slide-up animation on next frame
    requestAnimationFrame(() => {
      sheet.classList.add("sift-detail-sheet-open");
    });
  }

  // Tap detection on rows: click on narrow viewports opens detail sheet.
  // We use pointerdown/pointerup to distinguish taps from scrolls/long-presses.
  let tapStartTime = 0;
  let tapStartX = 0;
  let tapStartY = 0;
  const TAP_MAX_DURATION = 300;
  const TAP_MAX_DISTANCE = 10;

  viewport.addEventListener("pointerdown", (e) => {
    if (window.innerWidth >= 768) return;
    if (e.pointerType !== "touch") return;
    tapStartTime = e.timeStamp;
    tapStartX = e.clientX;
    tapStartY = e.clientY;
  });

  viewport.addEventListener("pointerup", (e) => {
    if (window.innerWidth >= 768) return;
    if (e.pointerType !== "touch") return;

    const duration = e.timeStamp - tapStartTime;
    const dx = Math.abs(e.clientX - tapStartX);
    const dy = Math.abs(e.clientY - tapStartY);
    if (duration > TAP_MAX_DURATION || dx > TAP_MAX_DISTANCE || dy > TAP_MAX_DISTANCE) return;

    // Find which pool row was tapped
    const target = e.target as HTMLElement;
    const rowEl = target.closest(".sift-row") as HTMLDivElement | null;
    if (!rowEl) return;

    for (const pr of pool) {
      if (pr.el === rowEl && pr.assignedRow !== -1) {
        showDetailSheet(pr.assignedRow);
        break;
      }
    }
  });

  // --- Column resize ---

  function onResizeStart(e: PointerEvent, colIndex: number) {
    e.preventDefault();
    e.stopPropagation();
    const handle = e.currentTarget as HTMLDivElement;
    handle.classList.add("dragging");
    handle.setPointerCapture(e.pointerId);
    const startX = e.clientX;
    const startWidth = colWidths[colIndex];

    const onMove = (ev: PointerEvent) => {
      const delta = ev.clientX - startX;
      colWidths[colIndex] = Math.max(MIN_COL_WIDTH, startWidth + delta);
      const ths = headerRowEl.children as HTMLCollectionOf<HTMLDivElement>;
      ths[colIndex].style.width = colWidths[colIndex] + "px";
      renderSummary(colIndex);
      updateScrollContentWidth();
      heightsDirty = true;
      scheduleRender();
    };

    const onUp = () => {
      handle.classList.remove("dragging");
      handle.removeEventListener("pointermove", onMove);
      handle.removeEventListener("pointerup", onUp);
    };

    handle.addEventListener("pointermove", onMove);
    handle.addEventListener("pointerup", onUp);
  }

  // --- Sort ---

  function onSortClick(col: number) {
    if (sortState && sortState.col === col) {
      if (sortState.dir === "asc") sortState = { col, dir: "desc" };
      else sortState = null;
    } else {
      sortState = { col, dir: "asc" };
    }

    updateSortUI();
    applyFilterAndSort();
    // Reset vertical scroll but preserve horizontal position
    viewport.scrollTop = 0;
    for (const pr of pool) {
      pr.assignedRow = -1;
      pr.el.style.display = "none";
    }
    scheduleRender();
    notifyChange();
  }

  function updateSortUI() {
    const ths = headerRowEl.children as HTMLCollectionOf<HTMLDivElement>;
    for (let c = 0; c < columns.length; c++) {
      const arrow = ths[c].querySelector(".sift-sort-arrow");
      if (!arrow) continue;
      if (sortState && sortState.col === c) {
        arrow.textContent = sortState.dir === "asc" ? " ↑" : " ↓";
        ths[c].setAttribute("aria-sort", sortState.dir === "asc" ? "ascending" : "descending");
      } else {
        arrow.textContent = "";
        ths[c].removeAttribute("aria-sort");
      }
    }
  }

  // --- Batch append handler ---

  function onBatchAppended() {
    const newRowCount = data.rowCount;
    growBuffers(newRowCount);

    // Cells will be lazy-prepared when they enter the viewport

    rowCount = newRowCount;
    // Re-filter and re-sort with new data
    applyFilterAndSort();

    // Save the latest unfiltered summaries
    unfilteredSummaries = [...data.columnSummaries];

    // If filters are active, recompute from filtered rows
    if (hasActiveFilters()) {
      recomputeFilteredSummaries();
    }

    // Update row count display
    updateRowCountDisplay();

    // Re-render non-histogram summaries (categorical, boolean) immediately —
    // they don't have a visible-window overlay so there's no flicker concern.
    // Numeric/timestamp histograms are NOT rendered here; instead we reset
    // the overlay tracking so the scheduled render paints them with the
    // correct visible bins, avoiding the all→windowed flicker.
    for (let c = 0; c < columns.length; c++) {
      const summary = data.columnSummaries[c];
      if (summary && summary.kind !== "numeric" && summary.kind !== "timestamp") {
        renderSummary(c);
      }
    }

    heightsDirty = true;
    // Force visible rows to refresh
    for (const pr of pool) {
      pr.assignedRow = -1;
      pr.el.style.display = "none";
    }
    lastVisFirst = -1;
    lastVisLast = -1;
    scheduleRender();
  }

  function setStreamingDone() {
    if (!streaming) return;
    streaming = false;
    progressBar.classList.add("sift-progress-bar-done");
    updateRowCountDisplay();
    // Switch status indicator from streaming dot to checkmark
    statusIndicator.classList.remove("sift-status-streaming");
    statusIndicator.classList.add("sift-status-ready");
    statusIndicator.textContent = "✓";
    statusIndicator.title = "All data loaded";
    // Remove the progress bar from DOM after fade-out transition
    progressBar.addEventListener("transitionend", () => progressBar.remove(), {
      once: true,
    });
  }

  // --- Boot ---

  document.fonts.ready.then(() => {
    for (let r = 0; r < rowCount; r++) {
      const cache = cellCaches[r];
      if (!cache) continue;
      for (let c = 0; c < columns.length; c++) {
        cache[c].prepared = prepare(data.getCell(r, c), FONT);
        cache[c].lastWidth = -1;
      }
    }
    heightsDirty = true;
    scheduleRender();
  });

  scheduleRender();

  // Watch for header height changes (e.g. when React summary charts mount async)
  // so rowPool.style.top stays in sync
  let headerResizeObserver: ResizeObserver | null = null;
  if (typeof ResizeObserver !== "undefined") {
    headerResizeObserver = new ResizeObserver(() => {
      heightsDirty = true;
      scheduleRender();
    });
    headerResizeObserver.observe(headerEl);
  }

  // --- Keyboard navigation ---

  container.tabIndex = 0;
  container.style.outline = "none";

  function onKeyDown(e: KeyboardEvent) {
    const oneRow = LINE_HEIGHT + CELL_PAD_V;
    const pageH = viewport.clientHeight;

    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        viewport.scrollTop += oneRow;
        break;
      case "ArrowUp":
        e.preventDefault();
        viewport.scrollTop -= oneRow;
        break;
      case "PageDown":
        e.preventDefault();
        viewport.scrollTop += pageH;
        break;
      case "PageUp":
        e.preventDefault();
        viewport.scrollTop -= pageH;
        break;
      case "Home":
        e.preventDefault();
        viewport.scrollTop = 0;
        break;
      case "End":
        e.preventDefault();
        viewport.scrollTop = viewport.scrollHeight;
        break;
      case "Escape":
        if (hasActiveFilters()) {
          e.preventDefault();
          clearAllFilters();
        }
        break;
    }
  }

  container.addEventListener("keydown", onKeyDown);

  // --- Destroy ---

  function destroy() {
    // Cancel pending render + FPS observable
    if (scheduledRaf !== null) {
      cancelAnimationFrame(scheduledRaf);
      scheduledRaf = null;
    }
    fpsSub.unsubscribe();
    if (summaryDebounceTimer !== null) clearTimeout(summaryDebounceTimer);

    // Remove event listeners and observers
    headerResizeObserver?.disconnect();
    viewport.removeEventListener("scroll", onScroll);

    container.removeEventListener("keydown", onKeyDown);
    window.removeEventListener("resize", scheduleRender);
    document.removeEventListener("fullscreenchange", onFullscreenChange);
    document.removeEventListener("webkitfullscreenchange", onFullscreenChange);

    // Unmount React roots
    for (const el of summaryContainers) {
      unmountColumnSummary(el);
    }
    unmountColumnMenu();

    // Dismiss detail sheet if open
    dismissDetailSheet();

    // Clear DOM
    container.innerHTML = "";
    container.classList.remove("sift-table-container");
  }

  // --- Filter API ---

  function recomputeFilteredSummaries() {
    if (!hasActiveFilters()) {
      // Restore unfiltered summaries
      data.columnSummaries = [...unfilteredSummaries];
      return;
    }

    // Build byte mask and delegate to WASM
    const mask = new Uint8Array(rowCount);
    for (let i = 0; i < filteredCount; i++) {
      mask[viewIndices[i]] = 1;
    }
    data.recomputeFilteredSummaries!(mask, filteredCount);

    // Carry forward isIndex from unfiltered summaries — WASM doesn't track it
    for (let c = 0; c < columns.length; c++) {
      const unfiltered = unfilteredSummaries[c];
      const filtered = data.columnSummaries[c];
      if (unfiltered && filtered && (unfiltered as any).isIndex) {
        (filtered as any).isIndex = true;
      }
    }
  }

  function filterLabel(f: ColumnFilter, colIndex?: number): string {
    if (!f) return "";
    switch (f.kind) {
      case "range": {
        if (colIndex !== undefined && columns[colIndex].columnType === "timestamp") {
          const fmt = (v: number) =>
            new Date(v).toLocaleDateString(undefined, {
              month: "short",
              day: "numeric",
              year: "2-digit",
            });
          return `${fmt(f.min)} – ${fmt(f.max)}`;
        }
        const minStr = f.min.toLocaleString(undefined, {
          maximumFractionDigits: 1,
        });
        const maxStr = f.max.toLocaleString(undefined, {
          maximumFractionDigits: 1,
        });
        return minStr === maxStr ? minStr : `${minStr} – ${maxStr}`;
      }
      case "set": {
        const vals = [...f.values];
        if (vals.length === 1) return vals[0];
        return `${vals.length} values`;
      }
      case "boolean":
        return f.value ? "Yes" : "No";
    }
  }

  function hiddenLabel(c: number): string | null {
    const full = unfilteredSummaries[c];
    const filtered = data.columnSummaries[c];
    if (!full || !filtered) return null;

    if (full.kind === "categorical" && filtered.kind === "categorical") {
      const hidden = full.uniqueCount - filtered.uniqueCount;
      if (hidden > 0) return `${hidden} values hidden`;
      return null;
    }
    if (full.kind === "boolean" && filtered.kind === "boolean") {
      const anyHidden =
        (full.trueCount > 0 && filtered.trueCount === 0) ||
        (full.falseCount > 0 && filtered.falseCount === 0) ||
        (full.nullCount > 0 && filtered.nullCount === 0);
      return anyHidden ? "Some values hidden" : null;
    }
    // Numeric/timestamp — check if range narrowed
    if (
      (full.kind === "numeric" && filtered.kind === "numeric") ||
      (full.kind === "timestamp" && filtered.kind === "timestamp")
    ) {
      // Binary numeric columns show selection via the ratio bar — no need for "values hidden"
      if (full.kind === "numeric" && (full as any).uniqueCount === 2) return null;
      if (full.min !== filtered.min || full.max !== filtered.max) {
        return "values hidden";
      }
    }
    return null;
  }

  function updateFilteredLabels() {
    const active = hasActiveFilters();
    const ths = headerRowEl.children as HTMLCollectionOf<HTMLDivElement>;
    for (let c = 0; c < columns.length; c++) {
      const th = ths[c];
      const label = th.querySelector(".sift-th-label") as HTMLElement;

      // Remove existing filter line
      const existing = th.querySelector(".sift-filter-line");
      if (existing) existing.remove();

      const f = filters[c];

      if (label) {
        label.style.color = f ? "var(--sift-accent)" : "";
        label.textContent = columns[c].label;
      }

      if (active) {
        const parts: string[] = [];
        if (f) parts.push(filterLabel(f, c));
        const hidden = hiddenLabel(c);
        if (hidden) parts.push(hidden);

        if (parts.length > 0) {
          const line = document.createElement("div");
          line.className = "sift-filter-line";
          line.textContent = parts.join(" · ");
          th.appendChild(line);
        }

        // Detailed hover tooltip showing which filters affect this column
        if (!f) {
          const activeFilters: string[] = [];
          for (let i = 0; i < columns.length; i++) {
            if (filters[i]) {
              activeFilters.push(`${columns[i].label}: ${filterLabel(filters[i], i)}`);
            }
          }
          th.title = `Values filtered by ${activeFilters.join(", ")}`;
        } else {
          th.title = "";
        }
      } else {
        th.title = "";
      }
    }
  }

  // Debounced summary recomputation — expensive, deferred during brush drag
  let summaryDebounceTimer: ReturnType<typeof setTimeout> | null = null;
  const SUMMARY_DEBOUNCE_MS = 120;

  function scheduleSummaryRecompute() {
    if (summaryDebounceTimer !== null) clearTimeout(summaryDebounceTimer);
    summaryDebounceTimer = setTimeout(() => {
      summaryDebounceTimer = null;
      recomputeFilteredSummaries();
      renderAllSummaries();
      updateFilteredLabels();
    }, SUMMARY_DEBOUNCE_MS);
  }

  function onFilterChanged() {
    applyFilterAndSort();
    // Fast path: update rows immediately, defer expensive summary recomputation
    updateRowCountDisplay();
    rebuildFilterPills();
    // Announce to screen readers
    ariaLive.textContent = hasActiveFilters()
      ? `Filtered to ${filteredCount.toLocaleString()} of ${rowCount.toLocaleString()} rows`
      : `${rowCount.toLocaleString()} rows`;
    // WASM path is fast enough to compute summaries synchronously — no flicker.
    // JS fallback still debounces since it's O(rows × cols) with per-cell access.
    if (data.recomputeFilteredSummaries) {
      if (summaryDebounceTimer !== null) {
        clearTimeout(summaryDebounceTimer);
        summaryDebounceTimer = null;
      }
      recomputeFilteredSummaries();
      renderAllSummaries();
      updateFilteredLabels();
    } else {
      scheduleSummaryRecompute();
    }
    viewport.scrollTop = 0;
    for (const pr of pool) {
      pr.assignedRow = -1;
      pr.el.style.display = "none";
    }
    lastVisFirst = -1;
    lastVisLast = -1;
    scheduleRender();
    notifyChange();
  }

  function setFilter(colIndex: number, filter: ColumnFilter) {
    filters[colIndex] = filter;
    onFilterChanged();
  }

  function clearFilter(colIndex: number) {
    filters[colIndex] = null;
    onFilterChanged();
  }

  function clearAllFilters() {
    for (let i = 0; i < filters.length; i++) filters[i] = null;
    onFilterChanged();
  }

  // --- State getters ---

  function getSort(): TableEngineState["sort"] {
    if (!sortState) return null;
    return { column: columns[sortState.col].key, direction: sortState.dir };
  }

  function setSortByName(column: string, direction: "asc" | "desc") {
    const colIndex = columns.findIndex((c) => c.key === column);
    if (colIndex === -1) return;
    sortState = { col: colIndex, dir: direction };

    updateSortUI();
    applyFilterAndSort();
    viewport.scrollTop = 0;
    for (const pr of pool) {
      pr.assignedRow = -1;
      pr.el.style.display = "none";
    }
    scheduleRender();
  }

  function getFilters(): TableEngineState["filters"] {
    const result: TableEngineState["filters"] = [];
    for (let i = 0; i < columns.length; i++) {
      if (filters[i]) {
        result.push({ column: columns[i].key, filter: filters[i] });
      }
    }
    return result;
  }

  function getState(): TableEngineState {
    return {
      sort: getSort(),
      filters: getFilters(),
      filteredCount,
      totalCount: rowCount,
    };
  }

  function notifyChange() {
    options?.onChange?.(getState());
  }

  // --- Column context menu action handler ---

  function handleColumnAction(colIndex: number, action: ColumnAction) {
    switch (action.kind) {
      case "sort":
        if (sortState?.col === colIndex && sortState.dir === action.direction) {
          sortState = null; // toggle off
        } else {
          sortState = { col: colIndex, dir: action.direction };
        }
        updateSortUI();
        applyFilterAndSort();
        viewport.scrollTop = 0;
        for (const pr of pool) {
          pr.assignedRow = -1;
          pr.el.style.display = "none";
        }
        scheduleRender();
        notifyChange();
        break;

      case "pin":
        pinnedColumns.add(colIndex);
        visualOrder = computeVisualOrder();
        reorderColumns();
        updatePinnedStyles();
        break;

      case "unpin":
        pinnedColumns.delete(colIndex);
        visualOrder = computeVisualOrder();
        reorderColumns();
        updatePinnedStyles();
        break;

      case "cast":
        if (streaming) break; // Cast blocked during streaming — schema mismatch would crash WASM
        if (data.castColumn) {
          try {
            data.castColumn(colIndex, action.targetType);
          } catch (e) {
            console.warn("Cast failed:", e);
            break;
          }
          // Update the column metadata
          columns[colIndex].columnType = action.targetType;
          columns[colIndex].numeric = action.targetType === "numeric";
          // Update type icon
          const ths = headerRowEl.children as HTMLCollectionOf<HTMLDivElement>;
          const icon = ths[colIndex].querySelector(".sift-type-icon");
          if (icon) {
            icon.textContent =
              action.targetType === "numeric"
                ? "#"
                : action.targetType === "boolean"
                  ? "◉"
                  : action.targetType === "timestamp"
                    ? "◷"
                    : "Aa";
            icon.setAttribute("title", action.targetType);
          }
          // Recompute summaries from source (WASM or accumulators)
          if (data.recomputeSummaries) data.recomputeSummaries();
          unfilteredSummaries = [...data.columnSummaries];
          if (hasActiveFilters()) recomputeFilteredSummaries();
          renderAllSummaries();
          heightsDirty = true;
          for (const pr of pool) {
            pr.assignedRow = -1;
            pr.el.style.display = "none";
          }
          scheduleRender();
        }
        break;

      case "undo-cast":
        if (streaming) break;
        if (data.undoCastColumn) {
          const restoredType = data.undoCastColumn(colIndex);
          // Update the column metadata
          columns[colIndex].columnType = restoredType;
          columns[colIndex].numeric = restoredType === "numeric";
          // Update type icon
          const thsUndo = headerRowEl.children as HTMLCollectionOf<HTMLDivElement>;
          const iconUndo = thsUndo[colIndex].querySelector(".sift-type-icon");
          if (iconUndo) {
            iconUndo.textContent =
              restoredType === "numeric"
                ? "#"
                : restoredType === "boolean"
                  ? "◉"
                  : restoredType === "timestamp"
                    ? "◷"
                    : "Aa";
            iconUndo.setAttribute("title", restoredType);
          }
          // Recompute summaries from source (WASM or accumulators)
          if (data.recomputeSummaries) data.recomputeSummaries();
          unfilteredSummaries = [...data.columnSummaries];
          if (hasActiveFilters()) recomputeFilteredSummaries();
          renderAllSummaries();
          heightsDirty = true;
          for (const pr of pool) {
            pr.assignedRow = -1;
            pr.el.style.display = "none";
          }
          scheduleRender();
        }
        break;
    }
  }

  // Precompute cumulative left offsets for pinned columns
  let pinnedLeftOffsets: number[] = [];
  function recomputePinnedOffsets() {
    pinnedLeftOffsets = Array.from<number>({ length: columns.length }).fill(-1);
    let cumLeft = 0;
    for (let c = 0; c < columns.length; c++) {
      if (pinnedColumns.has(c)) {
        pinnedLeftOffsets[c] = cumLeft;
        cumLeft += colWidths[c];
      }
    }
  }
  recomputePinnedOffsets();
  // Apply initial pinned styles to header THs (not just cells)
  updatePinnedStyles();

  function applyCellPinStyle(cell: HTMLElement, colIndex: number) {
    if (pinnedColumns.has(colIndex)) {
      cell.style.position = "sticky";
      cell.style.left = pinnedLeftOffsets[colIndex] + "px";
      cell.style.zIndex = "1";
      cell.style.background = "var(--sift-panel)";
      cell.style.boxShadow = "2px 0 4px var(--sift-pin-shadow)";
    } else {
      cell.style.position = "";
      cell.style.left = "";
      cell.style.zIndex = "";
      cell.style.background = "";
      cell.style.boxShadow = "";
    }
  }

  function reorderColumns() {
    // Reorder header TH elements to match visualOrder
    const ths = Array.from(headerRowEl.children) as HTMLDivElement[];
    for (const colIdx of visualOrder) {
      headerRowEl.appendChild(ths[colIdx]);
    }

    // Reorder cells in each pooled row
    for (const pr of pool) {
      for (const colIdx of visualOrder) {
        pr.el.appendChild(pr.cells[colIdx]);
      }
    }

    // Force re-render to update positions
    heightsDirty = true;
    for (const pr of pool) {
      pr.assignedRow = -1;
      pr.el.style.display = "none";
    }
    scheduleRender();
  }

  function updatePinnedStyles() {
    recomputePinnedOffsets();
    // Find the last pinned visual index
    let lastPinnedVi = -1;
    for (let vi = 0; vi < visualOrder.length; vi++) {
      if (pinnedColumns.has(visualOrder[vi])) lastPinnedVi = vi;
    }
    // Iterate in visual order since DOM has been reordered
    const ths = Array.from(headerRowEl.children) as HTMLDivElement[];
    for (let vi = 0; vi < visualOrder.length; vi++) {
      const dataCol = visualOrder[vi];
      const th = ths[vi];
      const handle = th.querySelector(".sift-resize-handle") as HTMLElement | null;
      if (pinnedColumns.has(dataCol)) {
        th.style.position = "sticky";
        th.style.left = pinnedLeftOffsets[dataCol] + "px";
        th.style.zIndex = "6";
        th.style.background = "color-mix(in srgb, var(--sift-panel) 90%, var(--sift-bg) 10%)";
        th.style.boxShadow = vi === lastPinnedVi ? "2px 0 4px var(--sift-pin-shadow)" : "";
        // Hide resize bar on last pinned column (shadow provides the edge)
        if (handle) handle.style.opacity = vi === lastPinnedVi ? "0" : "";
      } else {
        th.style.position = "";
        th.style.left = "";
        th.style.zIndex = "";
        th.style.background = "";
        th.style.boxShadow = "";
        if (handle) handle.style.opacity = "";
      }
    }
  }

  return {
    onBatchAppended,
    setStreamingDone,
    destroy,
    setFilter,
    clearFilter,
    clearAllFilters,
    getSort,
    setSort: setSortByName,
    getFilters,
    getState,
  };
}
