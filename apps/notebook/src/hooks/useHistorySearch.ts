import { invoke } from "@tauri-apps/api/core";
import { useCallback, useRef, useState } from "react";

export interface HistoryEntry {
  session: number;
  line: number;
  source: string;
}

// MRU cache for search queries (pattern -> entries)
// Uses Map iteration order: oldest at start, newest at end
const MAX_CACHE_SIZE = 20;
const searchCache = new Map<string, HistoryEntry[]>();

function getCacheKey(pattern: string | undefined): string {
  return pattern ?? "__tail__";
}

function getCachedResult(pattern: string | undefined): HistoryEntry[] | null {
  const key = getCacheKey(pattern);
  const result = searchCache.get(key);
  if (result) {
    // Move to end (most recently used) - delete and re-add
    searchCache.delete(key);
    searchCache.set(key, result);
    return result;
  }
  return null;
}

function setCacheResult(pattern: string | undefined, entries: HistoryEntry[]) {
  const key = getCacheKey(pattern);
  // Remove if exists (will re-add at end)
  searchCache.delete(key);
  // Evict oldest if at capacity
  if (searchCache.size >= MAX_CACHE_SIZE) {
    const oldestKey = searchCache.keys().next().value;
    if (oldestKey !== undefined) {
      searchCache.delete(oldestKey);
    }
  }
  searchCache.set(key, entries);
}

// Alias for backward compatibility
function getTailCache(): HistoryEntry[] {
  return getCachedResult(undefined) ?? [];
}

export function useHistorySearch() {
  const [entries, setEntries] = useState<HistoryEntry[]>(getTailCache);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // Track the current search pattern to avoid race conditions
  const currentSearchRef = useRef<string | undefined>(undefined);

  const searchHistory = useCallback(async (pattern?: string) => {
    // Track this search request
    currentSearchRef.current = pattern;

    // Check cache first - if we have results, show them immediately
    const cached = getCachedResult(pattern);
    if (cached) {
      setEntries(cached);
      // Still fetch fresh results in background, but don't show loading
      // This gives instant feedback for typo corrections / backspace
    }

    setIsLoading(true);
    setError(null);

    try {
      const results = await invoke<HistoryEntry[]>("get_history_via_daemon", {
        pattern: pattern || null,
        n: 100,
      });

      // Only update if this is still the current search (avoid race conditions)
      if (currentSearchRef.current === pattern) {
        // Don't replace good entries with empty kernel results — the kernel
        // glob search may legitimately return nothing for very narrow patterns
        // while client-side filtering of the tail still has useful matches.
        if (results.length > 0 || !pattern) {
          setEntries(results);
          setCacheResult(pattern, results);
        }
      }
    } catch (e) {
      const errorMsg = e instanceof Error ? e.message : String(e);
      // Only update error if this is still the current search
      if (currentSearchRef.current === pattern) {
        setError(errorMsg);
        // Don't clear entries on error - keep showing what we have
      }
    } finally {
      // Only clear loading if this is still the current search
      if (currentSearchRef.current === pattern) {
        setIsLoading(false);
      }
    }
  }, []);

  const clearEntries = useCallback(() => {
    // Reset to tail cache (or empty if no cache)
    setEntries(getTailCache());
    setError(null);
    currentSearchRef.current = undefined;
  }, []);

  return { entries, isLoading, error, searchHistory, clearEntries };
}
