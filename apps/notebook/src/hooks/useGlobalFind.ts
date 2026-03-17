import { useCallback, useMemo, useState } from "react";
import {
  getNotebookCellsSnapshot,
  useSourceVersion,
} from "../lib/notebook-cells";

/** A single search match location. */
export interface FindMatch {
  /** Cell ID containing the match */
  cellId: string;
  /** Index of the cell in the notebook */
  cellIndex: number;
  /** Whether this match is in the cell source or output */
  type: "source" | "output";
  /** For source matches: character offset within the source text. For output matches: local index within this cell's output matches. */
  offset: number;
  /** For source matches: length of the match. For output matches: 0. */
  length: number;
}

/** State and actions exposed by the useGlobalFind hook. */
export interface GlobalFindState {
  /** Whether the find bar is open */
  isOpen: boolean;
  /** Current search query */
  query: string;
  /** All matches found */
  matches: FindMatch[];
  /** Index of the currently active match */
  currentMatchIndex: number;
  /** The currently active match (or null) */
  currentMatch: FindMatch | null;
  /** Open the find bar */
  open: () => void;
  /** Close the find bar and clear search */
  close: () => void;
  /** Update the search query */
  setQuery: (query: string) => void;
  /** Navigate to the next match */
  nextMatch: () => void;
  /** Navigate to the previous match */
  prevMatch: () => void;
  /**
   * Report the number of search matches found in a cell's output.
   * Called by OutputArea when iframe reports search_results or in-DOM highlighting completes.
   */
  reportOutputMatchCount: (cellId: string, count: number) => void;
}

/**
 * Find all occurrences of a query in text (case-insensitive).
 */
function findInText(
  text: string,
  query: string,
): { offset: number; length: number }[] {
  if (!query || !text) return [];
  const matches: { offset: number; length: number }[] = [];
  const lowerText = text.toLowerCase();
  const lowerQuery = query.toLowerCase();
  let pos = lowerText.indexOf(lowerQuery, 0);
  while (pos !== -1) {
    matches.push({ offset: pos, length: query.length });
    pos = lowerText.indexOf(lowerQuery, pos + query.length);
  }
  return matches;
}

/**
 * Hook for managing global find state across the notebook.
 *
 * Source matches are computed directly from cell source text.
 * Output matches are reported asynchronously by OutputArea components
 * (via iframe postMessage search_results or in-DOM highlight counts).
 */
export function useGlobalFind(cellIds: string[]): GlobalFindState {
  const sourceVersion = useSourceVersion();
  const [isOpen, setIsOpen] = useState(false);
  const [query, setQueryState] = useState("");
  const [currentMatchIndex, setCurrentMatchIndex] = useState(0);
  const [outputMatchCounts, setOutputMatchCounts] = useState<
    Map<string, number>
  >(new Map());

  const reportOutputMatchCount = useCallback(
    (cellId: string, count: number) => {
      setOutputMatchCounts((prev) => {
        // Bail out when count is 0 and cellId isn't tracked — avoids creating
        // a new Map on every output change when find is inactive (#1)
        if (count === 0 && !prev.has(cellId)) return prev;
        if (prev.get(cellId) === count) return prev;
        const next = new Map(prev);
        if (count === 0) {
          next.delete(cellId);
        } else {
          next.set(cellId, count);
        }
        return next;
      });
    },
    [],
  );

  // Compute all matches: source matches directly, output matches from reported counts.
  // Reads cells imperatively — recomputes on query change and structural changes
  // (cellIds), not on every source keystroke.
  const matches = useMemo(() => {
    if (!query) return [];
    // Depend on cellIds (structural changes) and sourceVersion (source edits)
    // to recompute when cells change. We read cells imperatively for the actual data.
    void cellIds;
    void sourceVersion;
    const cells = getNotebookCellsSnapshot();
    const allMatches: FindMatch[] = [];

    for (let i = 0; i < cells.length; i++) {
      const cell = cells[i];

      // Source matches (computed directly)
      const sourceMatches = findInText(cell.source, query);
      for (const m of sourceMatches) {
        allMatches.push({
          cellId: cell.id,
          cellIndex: i,
          type: "source",
          offset: m.offset,
          length: m.length,
        });
      }

      // Output matches (from reported counts)
      const outputCount = outputMatchCounts.get(cell.id) || 0;
      for (let j = 0; j < outputCount; j++) {
        allMatches.push({
          cellId: cell.id,
          cellIndex: i,
          type: "output",
          offset: j,
          length: 0,
        });
      }
    }

    return allMatches;
  }, [query, cellIds, sourceVersion, outputMatchCounts]);

  const currentMatch =
    matches.length > 0 && currentMatchIndex >= 0
      ? (matches[currentMatchIndex % matches.length] ?? null)
      : null;

  const open = useCallback(() => {
    setIsOpen(true);
  }, []);

  const close = useCallback(() => {
    setIsOpen(false);
    setQueryState("");
    setCurrentMatchIndex(0);
    setOutputMatchCounts(new Map());
  }, []);

  const setQuery = useCallback((newQuery: string) => {
    setQueryState(newQuery);
    setCurrentMatchIndex(0);
    // Clear stale output counts — OutputArea will re-report for the new query
    setOutputMatchCounts(new Map());
  }, []);

  const nextMatch = useCallback(() => {
    if (matches.length === 0) return;
    setCurrentMatchIndex((prev) => (prev + 1) % matches.length);
  }, [matches.length]);

  const prevMatch = useCallback(() => {
    if (matches.length === 0) return;
    setCurrentMatchIndex(
      (prev) => (prev - 1 + matches.length) % matches.length,
    );
  }, [matches.length]);

  return {
    isOpen,
    query,
    matches,
    currentMatchIndex:
      matches.length > 0 ? currentMatchIndex % matches.length : -1,
    currentMatch,
    open,
    close,
    setQuery,
    nextMatch,
    prevMatch,
    reportOutputMatchCount,
  };
}
