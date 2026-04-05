/**
 * useVirtualCells — compute which cells to render for a virtualized cell list.
 *
 * Uses the cell height cache (pretext-measured heights) and the current scroll
 * position to binary-search the visible range. Returns only the cell IDs that
 * should be mounted in the DOM, plus layout metadata for absolute positioning.
 *
 * Binary search algorithm adapted from @chenglou/pretext's findVisibleRange
 * in the markdown-chat demo.
 */

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useSyncExternalStore,
} from "react";
import {
  getCumulativeOffsets,
  getHeightVersion,
  getTotalHeight,
  setContainerWidth,
  subscribeHeightChanges,
} from "../lib/cell-height-cache";
import { useFocusedCellId } from "../lib/cell-ui-state";

// ── Binary search ──────────────────────────────────────────────────────

/**
 * Find the range of cells visible in the viewport.
 * offsets[i] = top of cell i, offsets[i+1] = bottom of cell i.
 */
function findVisibleRange(
  offsets: Float64Array,
  scrollTop: number,
  viewportHeight: number,
): { start: number; end: number } {
  const count = offsets.length - 1; // number of cells
  if (count === 0) return { start: 0, end: 0 };

  const minY = scrollTop;
  const maxY = scrollTop + viewportHeight;

  // Find first cell whose bottom edge > scrollTop
  let low = 0;
  let high = count;
  while (low < high) {
    const mid = (low + high) >> 1;
    if (offsets[mid + 1] > minY) {
      high = mid;
    } else {
      low = mid + 1;
    }
  }
  const start = low;

  // Find first cell whose top edge >= scrollTop + viewportHeight
  low = start;
  high = count;
  while (low < high) {
    const mid = (low + high) >> 1;
    if (offsets[mid] >= maxY) {
      high = mid;
    } else {
      low = mid + 1;
    }
  }

  return { start, end: low };
}

// ── Scroll state ───────────────────────────────────────────────────────

/** Lightweight scroll position tracking without React re-renders. */
function useScrollPosition(containerRef: React.RefObject<HTMLElement | null>) {
  const scrollTop = useRef(0);
  const clientHeight = useRef(0);
  const version = useRef(0);
  const subscribers = useRef(new Set<() => void>());

  const onScroll = useCallback(() => {
    const el = containerRef.current;
    if (!el) return;
    const newScrollTop = el.scrollTop;
    const newClientHeight = el.clientHeight;
    if (
      newScrollTop === scrollTop.current &&
      newClientHeight === clientHeight.current
    )
      return;
    scrollTop.current = newScrollTop;
    clientHeight.current = newClientHeight;
    version.current++;
    for (const cb of subscribers.current) cb();
  }, [containerRef]);

  const subscribe = useCallback((cb: () => void) => {
    subscribers.current.add(cb);
    return () => subscribers.current.delete(cb);
  }, []);

  const getSnapshot = useCallback(() => version.current, []);

  return { scrollTop, clientHeight, onScroll, subscribe, getSnapshot };
}

// ── Main hook ──────────────────────────────────────────────────────────

interface UseVirtualCellsOptions {
  cellIds: string[];
  scrollContainerRef: React.RefObject<HTMLElement | null>;
  /** Number of cells to render above/below the viewport. Default 4. */
  overscan?: number;
  /** Whether drag is active — disables virtualization. */
  isDragging?: boolean;
  /** Additional cell IDs to always render (search matches, executing cells, etc.). */
  forceInclude?: string[];
}

interface UseVirtualCellsResult {
  /** Cell IDs to actually mount in the DOM. */
  visibleCellIds: Set<string>;
  /** Total scrollable height in px. */
  totalHeight: number;
  /** Map from cellId → top offset in px. */
  cellOffsets: Map<string, number>;
  /** Scroll event handler — attach to the scroll container. */
  onScroll: () => void;
}

export function useVirtualCells({
  cellIds,
  scrollContainerRef,
  overscan = 4,
  isDragging = false,
  forceInclude,
}: UseVirtualCellsOptions): UseVirtualCellsResult {
  const focusedCellId = useFocusedCellId();
  const scroll = useScrollPosition(scrollContainerRef);

  // Subscribe to both scroll changes and height cache changes
  const subscribe = useCallback(
    (cb: () => void) => {
      const unsub1 = scroll.subscribe(cb);
      const unsub2 = subscribeHeightChanges(cb);
      return () => {
        unsub1();
        unsub2();
      };
    },
    [scroll],
  );

  // Combined version — changes when scroll or heights change
  const getSnapshot = useCallback(
    () => scroll.getSnapshot() + getHeightVersion() * 1e9,
    [scroll],
  );

  // Track container width for pretext layout
  useEffect(() => {
    const el = scrollContainerRef.current;
    if (!el) return;
    const ro = new ResizeObserver((entries) => {
      const width = entries[0]?.contentRect.width;
      if (width !== undefined) setContainerWidth(width);
    });
    ro.observe(el);
    // Initial measurement
    setContainerWidth(el.clientWidth);
    return () => ro.disconnect();
  }, [scrollContainerRef]);

  // Subscribe so React re-renders when the visible range changes
  useSyncExternalStore(subscribe, getSnapshot);

  // Compute offsets, visible range, and force-includes
  return useMemo(() => {
    const offsets = getCumulativeOffsets(cellIds);
    const totalHeight = getTotalHeight(cellIds);

    // Build cellId → offset map for all cells (needed for absolute positioning)
    const cellOffsets = new Map<string, number>();
    for (let i = 0; i < cellIds.length; i++) {
      cellOffsets.set(cellIds[i], offsets[i]);
    }

    // During drag, render all cells
    if (isDragging) {
      return {
        visibleCellIds: new Set(cellIds),
        totalHeight,
        cellOffsets,
        onScroll: scroll.onScroll,
      };
    }

    // Binary search for visible range
    const { start, end } = findVisibleRange(
      offsets,
      scroll.scrollTop.current,
      scroll.clientHeight.current || 800, // fallback before first measure
    );

    // Expand by overscan
    const overscanStart = Math.max(0, start - overscan);
    const overscanEnd = Math.min(cellIds.length, end + overscan);

    // Build visible set
    const visibleCellIds = new Set<string>();
    for (let i = overscanStart; i < overscanEnd; i++) {
      visibleCellIds.add(cellIds[i]);
    }

    // Force-include focused cell
    if (focusedCellId) {
      visibleCellIds.add(focusedCellId);
    }

    // Force-include additional cells (search matches, executing, etc.)
    if (forceInclude) {
      for (const id of forceInclude) {
        visibleCellIds.add(id);
      }
    }

    return {
      visibleCellIds,
      totalHeight,
      cellOffsets,
      onScroll: scroll.onScroll,
    };
  }, [cellIds, isDragging, focusedCellId, forceInclude, scroll, overscan]);
}
