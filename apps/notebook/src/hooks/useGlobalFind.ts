import { useCallback, useMemo, useState } from "react";
import type { NotebookCell } from "../types";

/** A single search match location. */
export interface FindMatch {
  /** Cell ID containing the match */
  cellId: string;
  /** Index of the cell in the notebook */
  cellIndex: number;
  /** Whether this match is in the cell source or output text */
  type: "source" | "output";
  /** Character offset within the searched text */
  offset: number;
  /** Length of the match */
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
}

/**
 * Extract searchable text from a cell's outputs.
 * Returns concatenated text from stream, error, and text/plain outputs.
 */
function extractOutputText(cell: NotebookCell): string {
  if (cell.cell_type !== "code") return "";
  const parts: string[] = [];
  for (const output of cell.outputs) {
    if (output.output_type === "stream") {
      const text = typeof output.text === "string" ? output.text : output.text;
      parts.push(text);
    } else if (output.output_type === "error") {
      parts.push(output.traceback.join("\n"));
    } else if (
      output.output_type === "execute_result" ||
      output.output_type === "display_data"
    ) {
      // Search text/plain representation if available
      const plain = output.data["text/plain"];
      if (typeof plain === "string") {
        parts.push(plain);
      } else if (Array.isArray(plain)) {
        parts.push(plain.join(""));
      }
    }
  }
  return parts.join("\n");
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
 * Searches through cell sources and output text, providing
 * match navigation and the current active match for highlighting.
 */
export function useGlobalFind(cells: NotebookCell[]): GlobalFindState {
  const [isOpen, setIsOpen] = useState(false);
  const [query, setQueryState] = useState("");
  const [currentMatchIndex, setCurrentMatchIndex] = useState(0);

  // Compute all matches whenever query or cells change
  const matches = useMemo(() => {
    if (!query) return [];
    const allMatches: FindMatch[] = [];

    for (let i = 0; i < cells.length; i++) {
      const cell = cells[i];

      // Search cell source
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

      // Search output text
      const outputText = extractOutputText(cell);
      const outputMatches = findInText(outputText, query);
      for (const m of outputMatches) {
        allMatches.push({
          cellId: cell.id,
          cellIndex: i,
          type: "output",
          offset: m.offset,
          length: m.length,
        });
      }
    }

    return allMatches;
  }, [query, cells]);

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
  }, []);

  const setQuery = useCallback((newQuery: string) => {
    setQueryState(newQuery);
    setCurrentMatchIndex(0);
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
  };
}
