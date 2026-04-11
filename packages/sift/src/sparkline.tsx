import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import {
  Popover,
  PopoverContent,
  PopoverTrigger,
} from "./components/ui/popover";
import type {
  BooleanColumnSummary,
  CategoricalColumnSummary,
  CategoryEntry,
  ColumnFilter,
  NumericColumnSummary,
  RangeFilter,
  TimestampColumnSummary,
} from "./table";

type NonNullSummary =
  | NumericColumnSummary
  | CategoricalColumnSummary
  | BooleanColumnSummary
  | TimestampColumnSummary;

type FilterCallback = (filter: ColumnFilter) => void;

const CHART_HEIGHT = 48;

// --- Histogram brush layer ---

function BrushLayer({
  width,
  min,
  max,
  activeFilter,
  onFilter,
}: {
  width: number;
  min: number;
  max: number;
  activeFilter?: RangeFilter | null;
  onFilter: FilterCallback;
}) {
  const svgRef = useRef<SVGSVGElement>(null);
  const [brushState, setBrushState] = useState<{
    startX: number;
    currentX: number;
  } | null>(null);

  const xToValue = useCallback(
    (px: number) => {
      return min + (px / width) * (max - min);
    },
    [width, min, max],
  );

  const valueToX = useCallback(
    (v: number) => {
      return ((v - min) / (max - min)) * width;
    },
    [width, min, max],
  );

  const onPointerDown = useCallback((e: React.PointerEvent) => {
    const svg = svgRef.current!;
    svg.setPointerCapture(e.pointerId);
    const rect = svg.getBoundingClientRect();
    const x = e.clientX - rect.left;
    setBrushState({ startX: x, currentX: x });
  }, []);

  const onPointerMove = useCallback(
    (e: React.PointerEvent) => {
      if (!brushState) return;
      const rect = svgRef.current!.getBoundingClientRect();
      const x = Math.max(0, Math.min(width, e.clientX - rect.left));
      setBrushState({ ...brushState, currentX: x });
    },
    [brushState, width],
  );

  const onPointerUp = useCallback(() => {
    if (!brushState) return;
    const x0 = Math.min(brushState.startX, brushState.currentX);
    const x1 = Math.max(brushState.startX, brushState.currentX);
    setBrushState(null);

    // If the drag was tiny, treat as a click → clear filter
    if (x1 - x0 < 3) {
      onFilter(null);
      return;
    }

    const v0 = xToValue(x0);
    const v1 = xToValue(x1);

    // If the entire range is selected, clear instead of filtering
    if (v0 <= min && v1 >= max) {
      onFilter(null);
      return;
    }

    onFilter({ kind: "range", min: v0, max: v1 });
  }, [brushState, xToValue, onFilter, min, max]);

  // Render brush rect for active selection
  let brushRect = null;
  if (brushState) {
    const x = Math.min(brushState.startX, brushState.currentX);
    const w = Math.abs(brushState.currentX - brushState.startX);
    brushRect = (
      <rect
        x={x}
        y={0}
        width={w}
        height={CHART_HEIGHT}
        fill="var(--accent)"
        opacity={0.2}
      />
    );
  } else if (activeFilter) {
    const x = valueToX(activeFilter.min);
    const w = valueToX(activeFilter.max) - x;
    brushRect = (
      <rect
        x={x}
        y={0}
        width={w}
        height={CHART_HEIGHT}
        fill="var(--accent)"
        opacity={0.15}
        rx={2}
      />
    );
  }

  return (
    <svg
      ref={svgRef}
      width={width}
      height={CHART_HEIGHT}
      viewBox={`0 0 ${width} ${CHART_HEIGHT}`}
      style={{
        position: "absolute",
        top: 0,
        left: 0,
        cursor: "crosshair",
        touchAction: "none",
      }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
    >
      {brushRect}
    </svg>
  );
}

// --- Binary numeric ratio bar (0/1 or two-value columns) ---

/** Renders numeric columns with exactly 2 unique values as a boolean-style ratio bar. */
function BinaryNumericRatioBar({
  summary,
  activeFilter,
  onFilter,
}: {
  summary: NumericColumnSummary;
  activeFilter?: RangeFilter | null;
  onFilter: FilterCallback;
}) {
  // Find the two values and their counts from bins
  const nonEmpty = summary.bins.filter((b) => b.count > 0);
  const lowBin = nonEmpty[0];
  const highBin = nonEmpty[nonEmpty.length - 1];
  if (!lowBin || !highBin) return null;

  const total = lowBin.count + highBin.count;
  const lowPct = Math.round((lowBin.count / total) * 1000) / 10;
  const highPct = Math.round((highBin.count / total) * 1000) / 10;

  const isIntegerColumn =
    Number.isInteger(summary.min) && Number.isInteger(summary.max);
  const lowLabel = isIntegerColumn
    ? String(Math.round((lowBin.x0 + lowBin.x1) / 2))
    : formatNum((lowBin.x0 + lowBin.x1) / 2);
  const highLabel = isIntegerColumn
    ? String(Math.round((highBin.x0 + highBin.x1) / 2))
    : formatNum((highBin.x0 + highBin.x1) / 2);

  // Determine which segment is "active" based on range filter
  const lowActive =
    !activeFilter ||
    (lowBin.x1 > activeFilter.min && lowBin.x0 < activeFilter.max);
  const highActive =
    !activeFilter ||
    (highBin.x1 > activeFilter.min && highBin.x0 < activeFilter.max);

  return (
    <div className="pt-bool-summary">
      <div className="pt-bool-bar">
        <div
          className="pt-bool-true pt-bool-clickable"
          style={{
            width: `${lowPct}%`,
            opacity: activeFilter && !lowActive ? 0.3 : 1,
          }}
          onClick={() => {
            if (
              activeFilter &&
              activeFilter.min === lowBin.x0 &&
              activeFilter.max === lowBin.x1
            ) {
              onFilter(null);
            } else {
              onFilter({ kind: "range", min: lowBin.x0, max: lowBin.x1 });
            }
          }}
        />
        <div
          className="pt-bool-false pt-bool-clickable"
          style={{
            width: `${highPct}%`,
            opacity: activeFilter && !highActive ? 0.3 : 1,
          }}
          onClick={() => {
            if (
              activeFilter &&
              activeFilter.min === highBin.x0 &&
              activeFilter.max === highBin.x1
            ) {
              onFilter(null);
            } else {
              onFilter({ kind: "range", min: highBin.x0, max: highBin.x1 });
            }
          }}
        />
      </div>
      <div className="pt-bool-labels">
        <span>
          <strong>{lowLabel}</strong> <span className="pt-pct">{lowPct}%</span>
        </span>
        <span>
          <strong>{highLabel}</strong>{" "}
          <span className="pt-pct">{highPct}%</span>
        </span>
      </div>
    </div>
  );
}

// --- High-cardinality unique count display ---

/** Renders a simple unique count for high-cardinality columns with long text. */
function HighCardinalityText({
  summary,
}: {
  summary: CategoricalColumnSummary;
}) {
  return (
    <div className="pt-cat-summary">
      <span className="pt-th-range">
        {summary.uniqueCount.toLocaleString()} unique values
      </span>
    </div>
  );
}

// --- Low-cardinality numeric bars ---

/** Renders numeric columns with few unique values as categorical-style bars. */
function LowCardinalityNumericBars({
  summary,
  activeFilter,
  onFilter,
}: {
  summary: NumericColumnSummary;
  activeFilter?: RangeFilter | null;
  onFilter: FilterCallback;
}) {
  // Check if this looks like an integer column
  const isIntegerColumn =
    Number.isInteger(summary.min) && Number.isInteger(summary.max);

  const entries = summary.bins
    .filter((b) => b.count > 0)
    .map((b) => {
      const mid = (b.x0 + b.x1) / 2;
      const label = isIntegerColumn ? String(Math.round(mid)) : formatNum(mid);
      return { label, count: b.count, x0: b.x0, x1: b.x1 };
    });

  const total = entries.reduce((s, e) => s + e.count, 0);
  const items = entries.map((e) => ({
    ...e,
    pct: Math.round((e.count / total) * 1000) / 10,
  }));

  return (
    <div className="pt-cat-summary">
      {items.map((item) => {
        // Highlight bar if its range overlaps the active range filter
        const isActive =
          !activeFilter ||
          (item.x1 > activeFilter.min && item.x0 < activeFilter.max);
        return (
          <div
            key={item.label}
            className="pt-cat-row pt-cat-clickable"
            style={{ opacity: activeFilter && !isActive ? 0.3 : 1 }}
            onClick={() => {
              if (
                activeFilter &&
                activeFilter.min === item.x0 &&
                activeFilter.max === item.x1
              ) {
                onFilter(null);
              } else {
                onFilter({ kind: "range", min: item.x0, max: item.x1 });
              }
            }}
          >
            <div className="pt-cat-bar-track">
              <div
                className="pt-cat-bar-fill"
                style={{ width: `${item.pct}%` }}
              />
            </div>
            <span className="pt-cat-label">{item.label}</span>
            <span className="pt-cat-pct">{item.pct}%</span>
          </div>
        );
      })}
    </div>
  );
}

// --- Numeric histogram ---

function NumericHistogram({
  summary,
  unfilteredSummary,
  width,
  visibleBins,
  activeFilter,
  onFilter,
}: {
  summary: NumericColumnSummary;
  unfilteredSummary?: NumericColumnSummary;
  width: number;
  visibleBins?: number[];
  activeFilter?: RangeFilter | null;
  onFilter: FilterCallback;
}) {
  // Index/ID columns: just show the range, no histogram
  if (summary.isIndex) {
    return (
      <div>
        <span className="pt-th-range">
          {formatNum(summary.min)} – {formatNum(summary.max)}
        </span>
      </div>
    );
  }

  // Binary numeric (exactly 2 unique values like 0/1): show as ratio bar
  // Use unfiltered summary so the bar always shows both values, even when one is filtered out
  const sourceSummary = unfilteredSummary ?? summary;
  if (
    sourceSummary.uniqueCount !== undefined &&
    sourceSummary.uniqueCount === 2
  ) {
    return (
      <>
        <BinaryNumericRatioBar
          summary={sourceSummary}
          activeFilter={activeFilter}
          onFilter={onFilter}
        />
        <NumericProfile summary={summary} />
      </>
    );
  }

  // Low-cardinality: show as categorical bars instead of histogram
  if (summary.uniqueCount !== undefined && summary.uniqueCount < 10) {
    return (
      <>
        <LowCardinalityNumericBars
          summary={summary}
          activeFilter={activeFilter}
          onFilter={onFilter}
        />
        <NumericProfile summary={summary} />
      </>
    );
  }

  const hasOverlay = visibleBins && Math.max(...visibleBins) > 0;
  const isFiltered = !!activeFilter;
  const maxCount = Math.max(...summary.bins.map((b) => b.count));
  const numBins = summary.bins.length;
  const gap = 1;
  const barW = Math.max(1, (width - (numBins - 1) * gap) / numBins);
  const baseFill = hasOverlay
    ? "rgba(149, 95, 59, 0.2)"
    : "rgba(149, 95, 59, 0.7)";
  const dimFill = "rgba(149, 95, 59, 0.12)";
  const activeFill = "rgba(149, 95, 59, 0.7)";

  return (
    <div>
      <div style={{ position: "relative", width, height: CHART_HEIGHT }}>
        <svg
          width={width}
          height={CHART_HEIGHT}
          viewBox={`0 0 ${width} ${CHART_HEIGHT}`}
          style={{ display: "block" }}
        >
          {maxCount > 0 &&
            summary.bins.map((bin, i) => {
              if (bin.count <= 0) return null;
              const x = i * (barW + gap);
              const h = (bin.count / maxCount) * CHART_HEIGHT;
              // Per-bin highlight: bins overlapping the filter range are bright, others dimmed
              let fill = baseFill;
              if (isFiltered) {
                const binOverlaps =
                  bin.x1 > activeFilter.min && bin.x0 < activeFilter.max;
                fill = binOverlaps ? activeFill : dimFill;
              }
              return (
                <rect
                  key={i}
                  x={x}
                  y={CHART_HEIGHT - h}
                  width={barW}
                  height={h}
                  fill={fill}
                  style={{
                    transition:
                      "y 200ms ease-out, height 200ms ease-out, fill 150ms ease-out",
                  }}
                />
              );
            })}
        </svg>
        {hasOverlay && (
          <VisibleOverlay
            bins={summary.bins}
            visibleBins={visibleBins}
            width={width}
          />
        )}
        <BrushLayer
          width={width}
          min={summary.min}
          max={summary.max}
          activeFilter={activeFilter}
          onFilter={onFilter}
        />
      </div>
      {/* Hide range for binary-like columns where the ratio bar says it all */}
      {!(
        Number.isInteger(summary.min) &&
        Number.isInteger(summary.max) &&
        summary.max - summary.min <= 1
      ) && (
        <span className="pt-th-range">
          {formatNum(summary.min)} – {formatNum(summary.max)}
        </span>
      )}
      <NumericProfile summary={summary} />
    </div>
  );
}

function NumericProfile({ summary }: { summary: NumericColumnSummary }) {
  const totalInBins = summary.bins.reduce((s, b) => s + b.count, 0);
  // Infer null count: total rows in bins is the non-null count,
  // so nulls = explicit nullCount OR can't be computed without total rows
  const nulls = summary.nullCount ?? 0;
  const total = totalInBins + nulls;

  const parts: string[] = [];
  if (nulls > 0 && total > 0) {
    const pct = Math.round((nulls / total) * 100);
    parts.push(`${pct}% null`);
  }
  if (summary.uniqueCount !== undefined) {
    parts.push(`${summary.uniqueCount.toLocaleString()} distinct`);
  }
  if (parts.length === 0) return null;

  return <span className="pt-th-profile">{parts.join(" · ")}</span>;
}

function VisibleOverlay({
  bins,
  visibleBins,
  width,
}: {
  bins: NumericColumnSummary["bins"];
  visibleBins: number[];
  width: number;
}) {
  const visMax = Math.max(...visibleBins);
  if (visMax === 0) return null;

  const barW = Math.max(1, (width - (bins.length - 1)) / bins.length);

  return (
    <svg
      width={width}
      height={CHART_HEIGHT}
      viewBox={`0 0 ${width} ${CHART_HEIGHT}`}
      style={{ position: "absolute", top: 0, left: 0, pointerEvents: "none" }}
    >
      {visibleBins.map((count, i) => {
        if (count <= 0) return null;
        const x = i * (barW + 1);
        const h = (count / visMax) * CHART_HEIGHT;
        return (
          <rect
            key={i}
            x={x}
            y={CHART_HEIGHT - h}
            width={barW}
            height={h}
            fill="var(--accent)"
            opacity={0.85}
          />
        );
      })}
    </svg>
  );
}

// --- Category filter popover ---

const POPOVER_ROW_HEIGHT = 30;
const POPOVER_MAX_VISIBLE = 8;

function CategoryPopoverContent({
  allCategories,
  activeSet,
  onFilter,
}: {
  allCategories: CategoryEntry[];
  activeSet: Set<string> | null;
  onFilter: FilterCallback;
}) {
  const [searchInput, setSearchInput] = useState("");
  const [search, setSearch] = useState("");
  const listRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);

  // Pre-lowercase all labels once (avoids 89k toLowerCase() calls per keystroke)
  const lowercased = useMemo(
    () => allCategories.map((c) => c.label.toLowerCase()),
    [allCategories],
  );

  // Debounce search to avoid filtering 89k entries on every keystroke
  useEffect(() => {
    const timer = setTimeout(() => setSearch(searchInput), 80);
    return () => clearTimeout(timer);
  }, [searchInput]);

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const filtered = useMemo(() => {
    if (!search) return allCategories;
    const q = search.toLowerCase();
    const result: CategoryEntry[] = [];
    for (let i = 0; i < allCategories.length; i++) {
      if (lowercased[i].includes(q)) result.push(allCategories[i]);
    }
    return result;
  }, [allCategories, lowercased, search]);

  // Simple virtual scroll: track scroll offset
  const [scrollTop, setScrollTop] = useState(0);
  const totalHeight = filtered.length * POPOVER_ROW_HEIGHT;
  const visibleCount = POPOVER_MAX_VISIBLE;
  const first = Math.floor(scrollTop / POPOVER_ROW_HEIGHT);
  const last = Math.min(filtered.length - 1, first + visibleCount + 1);

  const allSelected = activeSet === null;
  const selectedCount = activeSet ? activeSet.size : allCategories.length;

  function toggleItem(label: string) {
    if (activeSet?.has(label)) {
      const next = new Set(activeSet);
      next.delete(label);
      onFilter(next.size > 0 ? { kind: "set", values: next } : null);
    } else {
      const next = new Set(activeSet ?? []);
      next.add(label);
      onFilter({ kind: "set", values: next });
    }
  }

  function selectAll() {
    onFilter(null);
  }
  function clearAll() {
    onFilter({ kind: "set", values: new Set<string>() });
  }

  return (
    <div className="pt-cat-popover">
      <input
        ref={inputRef}
        className="pt-cat-popover-search"
        type="text"
        placeholder={`Search ${allCategories.length} values…`}
        value={searchInput}
        onChange={(e) => {
          setSearchInput(e.target.value);
          setScrollTop(0);
        }}
      />
      <div className="pt-cat-popover-actions">
        <button onClick={selectAll} className="pt-cat-popover-btn">
          All
        </button>
        <button onClick={clearAll} className="pt-cat-popover-btn">
          None
        </button>
        <span className="pt-cat-popover-count">{selectedCount} selected</span>
      </div>
      <div
        ref={listRef}
        className="pt-cat-popover-list"
        style={{
          height: Math.min(filtered.length, visibleCount) * POPOVER_ROW_HEIGHT,
        }}
        onScroll={(e) => setScrollTop((e.target as HTMLElement).scrollTop)}
      >
        <div style={{ height: totalHeight, position: "relative" }}>
          {Array.from({ length: last - first + 1 }, (_, i) => {
            const idx = first + i;
            const cat = filtered[idx];
            if (!cat) return null;
            const checked = allSelected || (activeSet?.has(cat.label) ?? false);
            return (
              <label
                key={cat.label}
                className="pt-cat-popover-row"
                style={{
                  position: "absolute",
                  top: idx * POPOVER_ROW_HEIGHT,
                  left: 0,
                  right: 0,
                  height: POPOVER_ROW_HEIGHT,
                }}
              >
                <input
                  type="checkbox"
                  checked={checked}
                  onChange={() => toggleItem(cat.label)}
                  className="pt-cat-popover-check"
                />
                <span className="pt-cat-popover-label">{cat.label}</span>
                <span className="pt-cat-popover-pct">{cat.pct}%</span>
              </label>
            );
          })}
        </div>
      </div>
      {filtered.length === 0 && (
        <div className="pt-cat-popover-empty">No matches</div>
      )}
    </div>
  );
}

// --- Categorical bars (click to filter) ---

function CategoricalBars({
  summary,
  unfilteredAllCategories,
  activeFilter,
  onFilter,
}: {
  summary: CategoricalColumnSummary;
  unfilteredAllCategories?: CategoryEntry[];
  activeFilter?: ColumnFilter;
  onFilter: FilterCallback;
}) {
  const [popoverOpen, setPopoverOpen] = useState(false);

  const items = [
    ...summary.topCategories.map((c) => ({
      label: c.label,
      count: c.count,
      pct: c.pct,
      isOthers: false,
    })),
  ];
  if (summary.othersCount > 0) {
    items.push({
      label: `${summary.uniqueCount - summary.topCategories.length} others`,
      count: summary.othersCount,
      pct: summary.othersPct,
      isOthers: true,
    });
  }

  const activeSet = activeFilter?.kind === "set" ? activeFilter.values : null;

  return (
    <Popover open={popoverOpen} onOpenChange={setPopoverOpen}>
      <div className="pt-cat-summary">
        {items.map((item) => {
          const isActive = activeSet ? activeSet.has(item.label) : true;
          const row = (
            <div
              key={item.label}
              className={`pt-cat-row pt-cat-clickable`}
              style={{
                opacity: activeSet && !isActive && !item.isOthers ? 0.3 : 1,
              }}
              onClick={
                item.isOthers
                  ? undefined
                  : () => {
                      if (activeSet?.has(item.label)) {
                        const next = new Set(activeSet);
                        next.delete(item.label);
                        onFilter(
                          next.size > 0 ? { kind: "set", values: next } : null,
                        );
                      } else {
                        const next = new Set(activeSet ?? []);
                        next.add(item.label);
                        onFilter({ kind: "set", values: next });
                      }
                    }
              }
            >
              <div className="pt-cat-bar-track">
                <div
                  className="pt-cat-bar-fill"
                  style={{ width: `${item.pct}%` }}
                />
              </div>
              <span className="pt-cat-label">
                {item.isOthers ? item.label + " ▾" : truncate(item.label, 16)}
              </span>
              <span className="pt-cat-pct">{item.pct}%</span>
            </div>
          );
          if (item.isOthers) {
            return (
              <PopoverTrigger key={item.label} asChild>
                {row}
              </PopoverTrigger>
            );
          }
          return row;
        })}
      </div>
      <PopoverContent side="bottom" align="start">
        <CategoryPopoverContent
          allCategories={unfilteredAllCategories ?? summary.allCategories}
          activeSet={activeSet}
          onFilter={onFilter}
        />
      </PopoverContent>
    </Popover>
  );
}

// --- Boolean ratio bar (click to filter) ---

function BooleanRatioBar({
  summary,
  activeFilter,
  onFilter,
}: {
  summary: BooleanColumnSummary;
  activeFilter?: ColumnFilter;
  onFilter: FilterCallback;
}) {
  const nonNull = summary.trueCount + summary.falseCount;
  const truePct =
    summary.total > 0 ? (summary.trueCount / summary.total) * 100 : 0;
  const falsePct =
    summary.total > 0 ? (summary.falseCount / summary.total) * 100 : 0;
  const nullPct =
    summary.total > 0 ? (summary.nullCount / summary.total) * 100 : 0;
  const activeValue =
    activeFilter?.kind === "boolean" ? activeFilter.value : null;
  const hasNulls = summary.nullCount > 0;

  return (
    <div className="pt-bool-summary">
      <div className="pt-bool-bar">
        <div
          className="pt-bool-true pt-bool-clickable"
          style={{
            width: `${truePct}%`,
            opacity: activeValue === false ? 0.3 : 1,
          }}
          onClick={() =>
            onFilter(
              activeValue === true ? null : { kind: "boolean", value: true },
            )
          }
        />
        <div
          className="pt-bool-false pt-bool-clickable"
          style={{
            width: `${falsePct}%`,
            opacity: activeValue === true ? 0.3 : 1,
          }}
          onClick={() =>
            onFilter(
              activeValue === false ? null : { kind: "boolean", value: false },
            )
          }
        />
        {hasNulls && (
          <div
            className="pt-bool-null"
            style={{ width: `${nullPct}%` }}
            title={`${summary.nullCount} null values`}
          />
        )}
      </div>
      <div className="pt-bool-labels">
        <span>Yes {nonNull > 0 ? truePct.toFixed(0) : 0}%</span>
        {hasNulls && (
          <span className="pt-bool-null-label">{nullPct.toFixed(0)}% null</span>
        )}
        <span>No {nonNull > 0 ? falsePct.toFixed(0) : 0}%</span>
      </div>
    </div>
  );
}

// --- Timestamp histogram ---

function formatDateRange(minMs: number, maxMs: number): [string, string] {
  const min = new Date(minMs);
  const max = new Date(maxMs);
  const spanDays = (maxMs - minMs) / (1000 * 60 * 60 * 24);

  if (spanDays < 1) {
    // < 24 hours: show time only
    const fmt = (d: Date) =>
      d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" });
    return [fmt(min), fmt(max)];
  }
  if (spanDays > 730) {
    // > 2 years: just show years
    return [
      min.toLocaleDateString(undefined, { year: "numeric" }),
      max.toLocaleDateString(undefined, { year: "numeric" }),
    ];
  }
  if (spanDays > 60) {
    // > 2 months: month + year
    return [
      min.toLocaleDateString(undefined, { year: "numeric", month: "short" }),
      max.toLocaleDateString(undefined, { year: "numeric", month: "short" }),
    ];
  }
  // < 2 months: full date
  return [
    min.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    }),
    max.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
    }),
  ];
}

function TimestampHistogram({
  summary,
  width,
  visibleBins,
  activeFilter,
  onFilter,
}: {
  summary: TimestampColumnSummary;
  width: number;
  visibleBins?: number[];
  activeFilter?: RangeFilter | null;
  onFilter: FilterCallback;
}) {
  const [minLabel, maxLabel] = formatDateRange(summary.min, summary.max);

  const hasOverlay = visibleBins && Math.max(...visibleBins) > 0;
  const isFiltered = !!activeFilter;
  const maxCount = Math.max(...summary.bins.map((b) => b.count));
  const numBins = summary.bins.length;
  const barW = width / numBins;
  const baseFill = hasOverlay
    ? "rgba(149, 95, 59, 0.18)"
    : "rgba(149, 95, 59, 0.55)";
  const dimFill = "rgba(149, 95, 59, 0.1)";
  const activeFill = "rgba(149, 95, 59, 0.55)";

  // Compute bin boundaries from the linear range
  const binSpan = (summary.max - summary.min) / numBins;

  return (
    <div>
      <div style={{ position: "relative", width, height: CHART_HEIGHT }}>
        <svg
          width={width}
          height={CHART_HEIGHT}
          viewBox={`0 0 ${width} ${CHART_HEIGHT}`}
          style={{ display: "block" }}
        >
          {maxCount > 0 &&
            summary.bins.map((bin, i) => {
              if (bin.count <= 0) return null;
              const x = i * barW;
              const h = (bin.count / maxCount) * CHART_HEIGHT;
              // Per-bin highlight for timestamp bins
              let fill = baseFill;
              if (isFiltered) {
                const binStart = summary.min + i * binSpan;
                const binEnd = binStart + binSpan;
                const binOverlaps =
                  binEnd > activeFilter.min && binStart < activeFilter.max;
                fill = binOverlaps ? activeFill : dimFill;
              }
              return (
                <rect
                  key={i}
                  x={x}
                  y={CHART_HEIGHT - h}
                  width={barW}
                  height={h}
                  fill={fill}
                  style={{
                    transition:
                      "y 200ms ease-out, height 200ms ease-out, fill 150ms ease-out",
                  }}
                />
              );
            })}
        </svg>
        {hasOverlay && (
          <VisibleOverlay
            bins={summary.bins}
            visibleBins={visibleBins}
            width={width}
          />
        )}
        <BrushLayer
          width={width}
          min={summary.min}
          max={summary.max}
          activeFilter={activeFilter}
          onFilter={onFilter}
        />
      </div>
      <span className="pt-th-range">
        {minLabel} – {maxLabel}
      </span>
    </div>
  );
}

// --- Dispatch ---

function ColumnSummaryChart({
  summary,
  unfilteredSummary,
  width,
  visibleBins,
  activeFilter,
  onFilter,
}: {
  summary: NonNullSummary;
  unfilteredSummary?: NonNullSummary;
  width: number;
  visibleBins?: number[];
  activeFilter?: ColumnFilter;
  onFilter: FilterCallback;
}) {
  switch (summary.kind) {
    case "numeric": {
      const unfilteredNumeric =
        unfilteredSummary?.kind === "numeric" ? unfilteredSummary : undefined;
      return (
        <NumericHistogram
          summary={summary}
          unfilteredSummary={unfilteredNumeric}
          width={width}
          visibleBins={visibleBins}
          activeFilter={activeFilter?.kind === "range" ? activeFilter : null}
          onFilter={onFilter}
        />
      );
    }
    case "timestamp":
      return (
        <TimestampHistogram
          summary={summary}
          width={width}
          visibleBins={visibleBins}
          activeFilter={activeFilter?.kind === "range" ? activeFilter : null}
          onFilter={onFilter}
        />
      );
    case "boolean":
      return (
        <BooleanRatioBar
          summary={summary}
          activeFilter={activeFilter}
          onFilter={onFilter}
        />
      );
    case "categorical": {
      // High-cardinality with long text (e.g. track_id, URLs): just show unique count
      if (summary.uniqueCount > 1000 && summary.medianTextLength > 30) {
        return <HighCardinalityText summary={summary} />;
      }
      const unfilteredCategorical =
        unfilteredSummary?.kind === "categorical"
          ? unfilteredSummary
          : undefined;
      return (
        <CategoricalBars
          summary={summary}
          unfilteredAllCategories={unfilteredCategorical?.allCategories}
          activeFilter={activeFilter}
          onFilter={onFilter}
        />
      );
    }
  }
}

// --- Helpers ---

function formatNum(n: number): string {
  if (Number.isInteger(n)) return n.toLocaleString();
  return n.toLocaleString(undefined, { maximumFractionDigits: 2 });
}

function truncate(s: string, max: number): string {
  return s.length > max ? s.slice(0, max - 1) + "…" : s;
}

// --- Mount / update ---

const roots = new WeakMap<HTMLElement, Root>();

export function renderColumnSummary(
  container: HTMLElement,
  summary: NonNullSummary,
  width: number,
  visibleBins?: number[],
  activeFilter?: ColumnFilter,
  onFilter?: FilterCallback,
  unfilteredSummary?: NonNullSummary,
) {
  let root = roots.get(container);
  if (!root) {
    root = createRoot(container);
    roots.set(container, root);
  }
  root.render(
    <ColumnSummaryChart
      summary={summary}
      unfilteredSummary={unfilteredSummary}
      width={width}
      visibleBins={visibleBins}
      activeFilter={activeFilter ?? null}
      onFilter={onFilter ?? (() => {})}
    />,
  );
}

export function unmountColumnSummary(container: HTMLElement) {
  const root = roots.get(container);
  if (root) {
    roots.delete(container);
    // Defer unmount to avoid "synchronously unmount a root while React was
    // already rendering" when destroy() is called from a React effect cleanup
    queueMicrotask(() => root.unmount());
  }
}
