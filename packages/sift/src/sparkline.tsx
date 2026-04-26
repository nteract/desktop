import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { Popover, PopoverAnchor, PopoverContent, PopoverTrigger } from "./components/ui/popover";
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
const BRUSHING_CLASS = "sift-brushing";

type BrushState = {
  startX: number;
  currentX: number;
};

function clearNativeSelection() {
  document.getSelection()?.removeAllRanges();
}

/**
 * Does a bin with range [x0, x1] overlap the active range filter?
 *
 * Uses strict inequality for the common case of two extents overlapping.
 * The inclusive fallbacks handle two degenerate shapes that can arise
 * from the constant-slice pin behavior (#1859 / #1860):
 *
 * - **Bin is a point** (`x0 === x1`): the bin collapsed to one value;
 *   include it when the filter brackets that value.
 * - **Filter is a point** (`filterMin === filterMax`): the user pinned
 *   a value while the column was collapsed, and the column later
 *   widened. Use half-open `[x0, x1)` to mirror the actual bucketing
 *   in `NumericAccumulator.snapshot` / `store_histogram` (values at
 *   interior boundaries go to the upper bin via floor division). Pass
 *   `isLastBin = true` for the final histogram bucket so `v === max`
 *   still lights up the last bar — the bucketing code clamps that
 *   index into the last bucket too.
 */
export function binOverlapsFilter(
  x0: number,
  x1: number,
  filterMin: number,
  filterMax: number,
  isLastBin: boolean = false,
): boolean {
  if (x0 === x1) {
    return x0 >= filterMin && x0 <= filterMax;
  }
  if (filterMin === filterMax) {
    const v = filterMin;
    return isLastBin ? x0 <= v && v <= x1 : x0 <= v && v < x1;
  }
  return x1 > filterMin && x0 < filterMax;
}

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
  const hitTargetRef = useRef<HTMLDivElement>(null);
  const brushStateRef = useRef<BrushState | null>(null);
  const stopBrushCaptureRef = useRef<(() => void) | null>(null);
  const [brushState, setBrushState] = useState<BrushState | null>(null);

  useEffect(() => {
    return () => {
      stopBrushCaptureRef.current?.();
    };
  }, []);

  // When min === max (the filtered slice is a single value), the brush's
  // value↔pixel mapping would divide by zero and produce NaN coordinates,
  // which corrupts the active-selection overlay. Treat that case as a
  // degenerate single-value column: any brush result collapses to `min`,
  // and the overlay spans the full width.
  const span = max - min;
  const xToValue = useCallback(
    (px: number) => {
      if (span <= 0) return min;
      return min + (px / width) * span;
    },
    [width, min, span],
  );

  const valueToX = useCallback(
    (v: number) => {
      if (span <= 0) return 0;
      return ((v - min) / span) * width;
    },
    [width, min, span],
  );

  const clientXToBrushX = useCallback(
    (clientX: number) => {
      const rect = hitTargetRef.current!.getBoundingClientRect();
      return Math.max(0, Math.min(width, clientX - rect.left));
    },
    [width],
  );

  const setBrush = useCallback((state: BrushState | null) => {
    brushStateRef.current = state;
    setBrushState(state);
  }, []);

  const finishBrush = useCallback(
    (state: BrushState) => {
      const x0 = Math.min(state.startX, state.currentX);
      const x1 = Math.max(state.startX, state.currentX);
      setBrush(null);

      // If the drag was tiny, treat as a click → clear filter
      if (x1 - x0 < 3) {
        onFilter(null);
        return;
      }

      // Constant-slice column (span === 0): xToValue maps every pixel to
      // `min`, so v0 === v1 === min === max and the "entire range" check
      // below would fire on every real drag and clear the filter. Let the
      // user pin the single value instead so their filter survives
      // subsequent changes on other columns.
      if (span <= 0) {
        onFilter({ kind: "range", min, max });
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
    },
    [setBrush, onFilter, span, min, max, xToValue],
  );

  const stopBrushCapture = useCallback(() => {
    stopBrushCaptureRef.current?.();
    stopBrushCaptureRef.current = null;
  }, []);

  const startBrushCapture = useCallback(
    (pointerId: number) => {
      stopBrushCapture();

      const overlay = document.createElement("div");
      overlay.className = "sift-brush-overlay";
      document.body.appendChild(overlay);
      document.documentElement.classList.add(BRUSHING_CLASS);
      clearNativeSelection();
      let active = true;
      let releaseTimer: number | null = null;

      const blockSelection = (event: Event) => {
        event.preventDefault();
        event.stopPropagation();
        clearNativeSelection();
      };

      const onSelectionChange = () => {
        if (active) clearNativeSelection();
      };

      const shouldHandlePointer = (event: PointerEvent) => event.pointerId === pointerId;

      const cleanup = () => {
        if (!active) return;
        active = false;
        if (releaseTimer !== null) window.clearTimeout(releaseTimer);
        document.removeEventListener("selectstart", blockSelection, true);
        document.removeEventListener("dragstart", blockSelection, true);
        document.removeEventListener("selectionchange", onSelectionChange, true);
        document.removeEventListener("pointermove", onMove, true);
        document.removeEventListener("pointerup", onUp, true);
        document.removeEventListener("pointercancel", onCancel, true);
        overlay.remove();
        document.documentElement.classList.remove(BRUSHING_CLASS);
        clearNativeSelection();
      };

      const releaseAfterSafariSelectionSettles = () => {
        clearNativeSelection();
        requestAnimationFrame(clearNativeSelection);
        releaseTimer = window.setTimeout(cleanup, 150);
      };

      const onMove = (event: PointerEvent) => {
        if (!shouldHandlePointer(event)) return;
        event.preventDefault();
        event.stopPropagation();
        clearNativeSelection();

        const state = brushStateRef.current;
        if (!state) return;
        setBrush({ ...state, currentX: clientXToBrushX(event.clientX) });
      };

      const onUp = (event: PointerEvent) => {
        if (!shouldHandlePointer(event)) return;
        event.preventDefault();
        event.stopPropagation();

        const state = brushStateRef.current;
        clearNativeSelection();
        if (state) finishBrush(state);
        releaseAfterSafariSelectionSettles();
      };

      const onCancel = (event: PointerEvent) => {
        if (!shouldHandlePointer(event)) return;
        event.preventDefault();
        event.stopPropagation();
        setBrush(null);
        releaseAfterSafariSelectionSettles();
      };

      document.addEventListener("selectstart", blockSelection, true);
      document.addEventListener("dragstart", blockSelection, true);
      document.addEventListener("selectionchange", onSelectionChange, true);
      document.addEventListener("pointermove", onMove, true);
      document.addEventListener("pointerup", onUp, true);
      document.addEventListener("pointercancel", onCancel, true);

      stopBrushCaptureRef.current = cleanup;
    },
    [clientXToBrushX, finishBrush, setBrush, stopBrushCapture],
  );

  const onPointerDown = useCallback(
    (e: React.PointerEvent) => {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      const x = clientXToBrushX(e.clientX);
      setBrush({ startX: x, currentX: x });
      startBrushCapture(e.pointerId);
    },
    [clientXToBrushX, setBrush, startBrushCapture],
  );

  // Render brush rect for active selection
  let brushRect = null;
  if (brushState) {
    const x = Math.min(brushState.startX, brushState.currentX);
    const w = Math.abs(brushState.currentX - brushState.startX);
    brushRect = (
      <div
        className="sift-brush-selection"
        style={{
          left: x,
          width: w,
          opacity: 0.2,
        }}
      />
    );
  } else if (activeFilter) {
    // When the filtered slice has collapsed to a single value (span === 0),
    // `valueToX` can't express a range anymore. If the active filter
    // brackets that single value, the entire column is "selected" — show
    // a full-width overlay so the user still sees their filter is active.
    // Otherwise the filter has already excluded everything here (unlikely
    // but harmless), and a zero-width rect is fine.
    //
    // Separately, when the *filter* itself is a point (min === max — the
    // pin created by the span-0 branch above) but the column has since
    // widened, `valueToX(min) - valueToX(max)` is 0 pixels and the
    // overlay disappears. Paint a 2px marker at the pinned value so the
    // filter stays visible. Clamp so the marker sits flush against the
    // right edge when the pin coincides with `max`.
    const POINT_MARKER_WIDTH = 2;
    let x: number;
    let w: number;
    if (span <= 0) {
      const covered = activeFilter.min <= min && activeFilter.max >= max;
      x = 0;
      w = covered ? width : 0;
    } else if (activeFilter.min === activeFilter.max) {
      const center = valueToX(activeFilter.min);
      x = Math.max(0, Math.min(width - POINT_MARKER_WIDTH, center - POINT_MARKER_WIDTH / 2));
      w = POINT_MARKER_WIDTH;
    } else {
      x = valueToX(activeFilter.min);
      w = valueToX(activeFilter.max) - x;
    }
    brushRect = (
      <div
        className="sift-brush-selection sift-brush-selection-active"
        style={{
          left: x,
          width: w,
        }}
      />
    );
  }

  return (
    <div
      ref={hitTargetRef}
      className="sift-brush-hit-target"
      style={{
        width,
        height: CHART_HEIGHT,
      }}
      onPointerDown={onPointerDown}
    >
      {brushRect}
    </div>
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

  const isIntegerColumn = Number.isInteger(summary.min) && Number.isInteger(summary.max);
  const lowLabel = isIntegerColumn
    ? String(Math.round((lowBin.x0 + lowBin.x1) / 2))
    : formatNum((lowBin.x0 + lowBin.x1) / 2);
  const highLabel = isIntegerColumn
    ? String(Math.round((highBin.x0 + highBin.x1) / 2))
    : formatNum((highBin.x0 + highBin.x1) / 2);

  // Determine which segment is "active" based on range filter. The high
  // bin's right edge coincides with `summary.max`, so we mark it as the
  // last bin so `v === max` pins keep it active (mirrors the bucketing
  // clamp in `NumericAccumulator.snapshot`).
  const lowActive =
    !activeFilter || binOverlapsFilter(lowBin.x0, lowBin.x1, activeFilter.min, activeFilter.max);
  const highActive =
    !activeFilter ||
    binOverlapsFilter(
      highBin.x0,
      highBin.x1,
      activeFilter.min,
      activeFilter.max,
      highBin.x1 === summary.max,
    );

  return (
    <div className="sift-bool-summary">
      <div className="sift-bool-bar">
        <div
          className="sift-bool-true sift-bool-clickable"
          style={{
            width: `${lowPct}%`,
            opacity: activeFilter && !lowActive ? 0.3 : 1,
          }}
          onClick={() => {
            if (activeFilter && activeFilter.min === lowBin.x0 && activeFilter.max === lowBin.x1) {
              onFilter(null);
            } else {
              onFilter({ kind: "range", min: lowBin.x0, max: lowBin.x1 });
            }
          }}
        />
        <div
          className="sift-bool-false sift-bool-clickable"
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
      <div className="sift-bool-labels">
        <span>
          <strong>{lowLabel}</strong> <span className="sift-pct">{lowPct}%</span>
        </span>
        <span>
          <strong>{highLabel}</strong> <span className="sift-pct">{highPct}%</span>
        </span>
      </div>
    </div>
  );
}

// --- High-cardinality unique count display ---

/** Renders a simple unique count for high-cardinality columns with long text. */
function HighCardinalityText({ summary }: { summary: CategoricalColumnSummary }) {
  return (
    <div className="sift-cat-summary">
      <span className="sift-th-range">{summary.uniqueCount.toLocaleString()} unique values</span>
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
  const isIntegerColumn = Number.isInteger(summary.min) && Number.isInteger(summary.max);

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
    <div className="sift-cat-summary">
      {items.map((item) => {
        // Highlight bar if its range overlaps the active range filter.
        // Mark the last bin (x1 === summary.max) so `v === max` pins
        // stay lit; mirrors the bucketing clamp in the accumulator.
        const isActive =
          !activeFilter ||
          binOverlapsFilter(
            item.x0,
            item.x1,
            activeFilter.min,
            activeFilter.max,
            item.x1 === summary.max,
          );
        return (
          <div
            key={item.label}
            className="sift-cat-row sift-cat-clickable"
            style={{ opacity: activeFilter && !isActive ? 0.3 : 1 }}
            onClick={() => {
              if (activeFilter && activeFilter.min === item.x0 && activeFilter.max === item.x1) {
                onFilter(null);
              } else {
                onFilter({ kind: "range", min: item.x0, max: item.x1 });
              }
            }}
          >
            <div className="sift-cat-bar-track">
              <div className="sift-cat-bar-fill" style={{ width: `${item.pct}%` }} />
            </div>
            <span className="sift-cat-label">{item.label}</span>
            <span className="sift-cat-pct">{item.pct}%</span>
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
        <span className="sift-th-range">{formatNumRange(summary.min, summary.max)}</span>
      </div>
    );
  }

  // Binary numeric (exactly 2 unique values like 0/1): show as ratio bar
  // Use unfiltered summary so the bar always shows both values, even when one is filtered out
  const sourceSummary = unfilteredSummary ?? summary;
  if (sourceSummary.uniqueCount !== undefined && sourceSummary.uniqueCount === 2) {
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
    ? "color-mix(in srgb, var(--sift-accent) 20%, transparent)"
    : "color-mix(in srgb, var(--sift-accent) 70%, transparent)";
  const dimFill = "color-mix(in srgb, var(--sift-accent) 12%, transparent)";
  const activeFill = "color-mix(in srgb, var(--sift-accent) 70%, transparent)";

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
              // Per-bin highlight: bins overlapping the filter range are bright, others dimmed.
              // See `binOverlapsFilter` for the zero-width bin / zero-width filter cases
              // that fall out of the constant-slice pin behavior (#1859 / #1860).
              let fill = baseFill;
              if (isFiltered) {
                fill = binOverlapsFilter(
                  bin.x0,
                  bin.x1,
                  activeFilter.min,
                  activeFilter.max,
                  i === numBins - 1,
                )
                  ? activeFill
                  : dimFill;
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
                    transition: "y 200ms ease-out, height 200ms ease-out, fill 150ms ease-out",
                  }}
                />
              );
            })}
        </svg>
        {hasOverlay && (
          <VisibleOverlay bins={summary.bins} visibleBins={visibleBins} width={width} />
        )}
        <BrushLayer
          width={width}
          min={summary.min}
          max={summary.max}
          activeFilter={activeFilter}
          onFilter={onFilter}
        />
      </div>
      {/*
        Always show the range label. The earlier "hide when integer span
        <= 1" heuristic was a carry-over from the binary (0/1) case, which
        already early-returns into `BinaryNumericRatioBar` via the
        `uniqueCount === 2` branch above. Past that branch, single-valued
        integer columns (e.g., a filtered slice where every row is 0)
        were falling into this suppression and rendering no textual
        label at all - with no ratio bar either, since `uniqueCount`
        is 1 in that case.
      */}
      <span className="sift-th-range">{formatNumRange(summary.min, summary.max)}</span>
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

  return <span className="sift-th-profile">{parts.join(" · ")}</span>;
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
            fill="var(--sift-accent)"
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
  exclusionSet,
  onFilter,
}: {
  allCategories: CategoryEntry[];
  activeSet: Set<string> | null;
  exclusionSet: Set<string> | null;
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

  const allSelected = activeSet === null && exclusionSet === null;
  const selectedCount = exclusionSet
    ? allCategories.length - exclusionSet.size
    : activeSet
      ? activeSet.size
      : allCategories.length;

  function toggleItem(label: string) {
    // Case 1: No filter active (null) — start exclusion filter
    if (activeSet === null && exclusionSet === null) {
      onFilter({ kind: "not-in", values: new Set([label]) });
      return;
    }

    // Case 2: Exclusion filter active — add/remove from exclusion set
    if (exclusionSet !== null) {
      if (exclusionSet.has(label)) {
        // Re-checking an excluded item: remove from exclusion set
        const next = new Set(exclusionSet);
        next.delete(label);
        if (next.size === 0) {
          // All items re-checked: collapse to "no filter"
          onFilter(null);
        } else {
          onFilter({ kind: "not-in", values: next });
        }
      } else {
        // Unchecking a non-excluded item: add to exclusion set
        const next = new Set(exclusionSet);
        next.add(label);
        // If everything is excluded, collapse to empty-set filter
        if (next.size >= allCategories.length) {
          onFilter({ kind: "set", values: new Set<string>() });
        } else {
          onFilter({ kind: "not-in", values: next });
        }
      }
      return;
    }

    // Case 3: Inclusion filter active — add/remove from inclusion set
    if (activeSet !== null) {
      if (activeSet.has(label)) {
        const next = new Set(activeSet);
        next.delete(label);
        onFilter({ kind: "set", values: next });
      } else {
        const next = new Set(activeSet);
        next.add(label);
        if (next.size >= allCategories.length) {
          onFilter(null);
        } else {
          onFilter({ kind: "set", values: next });
        }
      }
    }
  }

  function selectAll() {
    onFilter(null);
  }
  function clearAll() {
    onFilter({ kind: "set", values: new Set<string>() });
  }

  return (
    <div className="sift-cat-popover">
      <input
        ref={inputRef}
        className="sift-cat-popover-search"
        type="text"
        placeholder={`Search ${allCategories.length} values…`}
        value={searchInput}
        onChange={(e) => {
          setSearchInput(e.target.value);
          setScrollTop(0);
        }}
      />
      <div className="sift-cat-popover-actions">
        <button onClick={selectAll} className="sift-cat-popover-btn">
          All
        </button>
        <button onClick={clearAll} className="sift-cat-popover-btn">
          None
        </button>
        <span className="sift-cat-popover-count">{selectedCount} selected</span>
      </div>
      <div
        ref={listRef}
        className="sift-cat-popover-list"
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
            const checked =
              allSelected ||
              (exclusionSet ? !exclusionSet.has(cat.label) : (activeSet?.has(cat.label) ?? false));
            return (
              <label
                key={cat.label}
                className="sift-cat-popover-row"
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
                  className="sift-cat-popover-check"
                />
                <span className="sift-cat-popover-label">{cat.label}</span>
                <span className="sift-cat-popover-pct">{cat.pct}%</span>
              </label>
            );
          })}
        </div>
      </div>
      {filtered.length === 0 && <div className="sift-cat-popover-empty">No matches</div>}
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

  const items = summary.topCategories.map((c) => ({
    label: c.label,
    count: c.count,
    pct: c.pct,
    isOthers: false,
  }));

  // Whether the column has an "others" bucket at all is a property of
  // the *unfiltered* data, not the current filter. Without this, any
  // filter that narrows the set to only the top categories — or an
  // all-empty "None" selection — unmounts the trigger and leaves the
  // user with no button to reopen the popover and adjust their filter.
  // Fall back to the current summary's lists when the unfiltered
  // snapshot isn't available.
  const unfilteredUniqueCount = unfilteredAllCategories?.length ?? summary.allCategories.length;
  const hasOthersTrigger = unfilteredUniqueCount > summary.topCategories.length;
  if (hasOthersTrigger) {
    items.push({
      label: `${unfilteredUniqueCount - summary.topCategories.length} others`,
      count: summary.othersCount,
      pct: summary.othersPct,
      isOthers: true,
    });
  }

  const activeSet = activeFilter?.kind === "set" ? activeFilter.values : null;
  const exclusionSet = activeFilter?.kind === "not-in" ? activeFilter.values : null;

  return (
    <Popover open={popoverOpen} onOpenChange={setPopoverOpen}>
      {/*
        Anchor the popover to the whole category summary area rather than
        the "others" row itself. The summary box is a stable position
        directly below the column heading, so the popover stays pinned
        there even as individual category bars reflow with the filter.
        Wrapping the same div makes its position track the column's
        scroll/layout while the trigger remains the "others" row click.
      */}
      <PopoverAnchor asChild>
        <div className="sift-cat-summary">
          {items.map((item) => {
            const row = (
              <div
                key={item.label}
                className={`sift-cat-row sift-cat-clickable`}
                style={{
                  opacity: (() => {
                    if (item.isOthers) return 1;
                    if (exclusionSet) return exclusionSet.has(item.label) ? 0.3 : 1;
                    if (activeSet) return activeSet.has(item.label) ? 1 : 0.3;
                    return 1;
                  })(),
                }}
                onClick={
                  item.isOthers
                    ? undefined
                    : () => {
                        // Top-category bars use "click to filter to this"
                        // semantics (the bar highlight / dim affordance signals
                        // selection, not a checkbox). Unlike the popover rows
                        // inside CategoryPopoverContent — those are checkboxes
                        // and need the implicit-full-set subtract behavior —
                        // clicking a bar while nothing is filtered should
                        // filter to that single category.
                        if (exclusionSet) {
                          if (exclusionSet.has(item.label)) {
                            const next = new Set(exclusionSet);
                            next.delete(item.label);
                            onFilter(next.size > 0 ? { kind: "not-in", values: next } : null);
                          } else {
                            const next = new Set(exclusionSet);
                            next.add(item.label);
                            onFilter({ kind: "not-in", values: next });
                          }
                        } else if (activeSet?.has(item.label)) {
                          const next = new Set(activeSet);
                          next.delete(item.label);
                          onFilter(next.size > 0 ? { kind: "set", values: next } : null);
                        } else {
                          const next = new Set(activeSet ?? []);
                          next.add(item.label);
                          onFilter({ kind: "set", values: next });
                        }
                      }
                }
              >
                <div className="sift-cat-bar-track">
                  <div className="sift-cat-bar-fill" style={{ width: `${item.pct}%` }} />
                </div>
                <span className="sift-cat-label">
                  {item.isOthers ? item.label + " ▾" : truncate(item.label, 16)}
                </span>
                <span className="sift-cat-pct">{item.pct}%</span>
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
      </PopoverAnchor>
      <PopoverContent side="bottom" align="start">
        <CategoryPopoverContent
          allCategories={unfilteredAllCategories ?? summary.allCategories}
          activeSet={activeSet}
          exclusionSet={exclusionSet}
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
  const truePct = summary.total > 0 ? (summary.trueCount / summary.total) * 100 : 0;
  const falsePct = summary.total > 0 ? (summary.falseCount / summary.total) * 100 : 0;
  const nullPct = summary.total > 0 ? (summary.nullCount / summary.total) * 100 : 0;
  const activeValue = activeFilter?.kind === "boolean" ? activeFilter.value : null;
  const hasNulls = summary.nullCount > 0;

  return (
    <div className="sift-bool-summary">
      <div className="sift-bool-bar">
        <div
          className="sift-bool-true sift-bool-clickable"
          style={{
            width: `${truePct}%`,
            opacity: activeValue === false ? 0.3 : 1,
          }}
          onClick={() => onFilter(activeValue === true ? null : { kind: "boolean", value: true })}
        />
        <div
          className="sift-bool-false sift-bool-clickable"
          style={{
            width: `${falsePct}%`,
            opacity: activeValue === true ? 0.3 : 1,
          }}
          onClick={() => onFilter(activeValue === false ? null : { kind: "boolean", value: false })}
        />
        {hasNulls && (
          <div
            className="sift-bool-null"
            style={{ width: `${nullPct}%` }}
            title={`${summary.nullCount} null values`}
          />
        )}
      </div>
      <div className="sift-bool-labels">
        <span>Yes {nonNull > 0 ? truePct.toFixed(0) : 0}%</span>
        {hasNulls && <span className="sift-bool-null-label">{nullPct.toFixed(0)}% null</span>}
        <span>No {nonNull > 0 ? falsePct.toFixed(0) : 0}%</span>
      </div>
    </div>
  );
}

// --- Timestamp histogram ---

function formatDateRange(minMs: number, maxMs: number, timezone?: string | null): [string, string] {
  const tz = timezone ?? "UTC";
  const min = new Date(minMs);
  const max = new Date(maxMs);
  const spanDays = (maxMs - minMs) / (1000 * 60 * 60 * 24);

  if (maxMs === minMs) {
    const fmt = (d: Date) =>
      d.toLocaleString(undefined, {
        year: "numeric",
        month: "short",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
        timeZone: tz,
        timeZoneName: "short",
      });
    return [fmt(min), fmt(max)];
  }

  if (spanDays < 1) {
    const fmt = (d: Date) =>
      d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", timeZone: tz });
    return [fmt(min), fmt(max)];
  }
  if (spanDays > 730) {
    return [
      min.toLocaleDateString(undefined, { year: "numeric", timeZone: tz }),
      max.toLocaleDateString(undefined, { year: "numeric", timeZone: tz }),
    ];
  }
  if (spanDays > 60) {
    return [
      min.toLocaleDateString(undefined, { year: "numeric", month: "short", timeZone: tz }),
      max.toLocaleDateString(undefined, { year: "numeric", month: "short", timeZone: tz }),
    ];
  }
  return [
    min.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      timeZone: tz,
    }),
    max.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      timeZone: tz,
    }),
  ];
}

function TimestampHistogram({
  summary,
  width,
  visibleBins,
  activeFilter,
  onFilter,
  timezone,
}: {
  summary: TimestampColumnSummary;
  width: number;
  visibleBins?: number[];
  activeFilter?: RangeFilter | null;
  onFilter: FilterCallback;
  timezone?: string | null;
}) {
  const [minLabel, maxLabel] = formatDateRange(summary.min, summary.max, timezone);

  const hasOverlay = visibleBins && Math.max(...visibleBins) > 0;
  const isFiltered = !!activeFilter;
  const maxCount = Math.max(...summary.bins.map((b) => b.count));
  const numBins = summary.bins.length;
  const barW = width / numBins;
  const baseFill = hasOverlay
    ? "color-mix(in srgb, var(--sift-accent) 18%, transparent)"
    : "color-mix(in srgb, var(--sift-accent) 55%, transparent)";
  const dimFill = "color-mix(in srgb, var(--sift-accent) 10%, transparent)";
  const activeFill = "color-mix(in srgb, var(--sift-accent) 55%, transparent)";

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
              // Per-bin highlight for timestamp bins. See `binOverlapsFilter` for
              // the zero-span / zero-width-filter degenerate cases (#1859 / #1860).
              let fill = baseFill;
              if (isFiltered) {
                const binStart = summary.min + i * binSpan;
                const binEnd = binStart + binSpan;
                fill = binOverlapsFilter(
                  binStart,
                  binEnd,
                  activeFilter.min,
                  activeFilter.max,
                  i === numBins - 1,
                )
                  ? activeFill
                  : dimFill;
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
                    transition: "y 200ms ease-out, height 200ms ease-out, fill 150ms ease-out",
                  }}
                />
              );
            })}
        </svg>
        {hasOverlay && (
          <VisibleOverlay bins={summary.bins} visibleBins={visibleBins} width={width} />
        )}
        <BrushLayer
          width={width}
          min={summary.min}
          max={summary.max}
          activeFilter={activeFilter}
          onFilter={onFilter}
        />
      </div>
      <span className="sift-th-range">
        {summary.min === summary.max ? minLabel : `${minLabel} – ${maxLabel}`}
        {!timezone && (
          <span className="sift-tz-default" title="No timezone in data, displayed as UTC">
            UTC
          </span>
        )}
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
  timezone,
}: {
  summary: NonNullSummary;
  unfilteredSummary?: NonNullSummary;
  width: number;
  visibleBins?: number[];
  activeFilter?: ColumnFilter;
  onFilter: FilterCallback;
  timezone?: string | null;
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
          timezone={timezone}
        />
      );
    case "boolean":
      return <BooleanRatioBar summary={summary} activeFilter={activeFilter} onFilter={onFilter} />;
    case "categorical": {
      // High-cardinality with long text (e.g. track_id, URLs): just show unique count
      if (summary.uniqueCount > 1000 && summary.medianTextLength > 30) {
        return <HighCardinalityText summary={summary} />;
      }
      const unfilteredCategorical =
        unfilteredSummary?.kind === "categorical" ? unfilteredSummary : undefined;
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

/** Format a numeric range, collapsing to a single value when min === max.
 *
 * Compare against the raw values, not the formatted strings — `formatNum`
 * rounds to two decimals, so `0.001`–`0.004` would both render as "0" and
 * falsely collapse. We only want to hide the upper endpoint when the
 * column is genuinely single-valued. */
function formatNumRange(min: number, max: number): string {
  if (min === max) return formatNum(min);
  return `${formatNum(min)} – ${formatNum(max)}`;
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
  timezone?: string | null,
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
      timezone={timezone}
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
