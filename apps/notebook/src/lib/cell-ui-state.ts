import { useMemo, useSyncExternalStore } from "react";
import type { FindMatch } from "../hooks/useGlobalFind";
import { getCellIdsSnapshot, subscribeIds } from "./notebook-cells";

// ---------------------------------------------------------------------------
// Transient UI state store for per-cell rendering state.
//
// This store holds state that changes frequently (focus, execution, search)
// but is NOT persisted in the CRDT. Components subscribe to only the slices
// they need via hooks, so e.g. focusing cell B does not re-render cell A.
//
// Same useSyncExternalStore pattern as notebook-cells.ts.
// ---------------------------------------------------------------------------

// ── Internal state ──────────────────────────────────────────────────────

let _focusedCellId: string | null = null;
let _executingCellIds: Set<string> = new Set();
let _queuedCellIds: Set<string> = new Set();
let _searchQuery: string | undefined; // eslint-disable-line -- intentionally uninitialized alongside siblings
let _searchCurrentMatch: FindMatch | null = null;

// ── Subscribers ─────────────────────────────────────────────────────────

const _focusSubscribers = new Set<() => void>();
const _executingSubscribers = new Set<() => void>();
const _queuedSubscribers = new Set<() => void>();
const _searchQuerySubscribers = new Set<() => void>();
const _searchMatchSubscribers = new Set<() => void>();

function emit(subs: Set<() => void>): void {
  for (const cb of subs) cb();
}

// Subscriber sets that need deferred notification.
const _pendingEmits = new Set<Set<() => void>>();
let _emitScheduled = false;

function deferEmit(subs: Set<() => void>): void {
  _pendingEmits.add(subs);
  if (!_emitScheduled) {
    _emitScheduled = true;
    queueMicrotask(() => {
      _emitScheduled = false;
      const batch = [..._pendingEmits];
      _pendingEmits.clear();
      for (const s of batch) emit(s);
    });
  }
}

function setsEqual(a: Set<string>, b: Set<string>): boolean {
  if (a.size !== b.size) return false;
  for (const v of a) if (!b.has(v)) return false;
  return true;
}

// ── Setters ─────────────────────────────────────────────────────────────
//
// Two-phase update pattern for compatibility with React's render rules:
//
// 1. Assign the variable immediately — so useSyncExternalStore snapshot
//    functions return the current value during the same render cycle.
// 2. Defer subscriber notification via queueMicrotask — so React doesn't
//    see a cross-component setState during render. Subscribers re-render
//    on the next microtask, which is still before the browser paints.

export function setFocusedCellId(id: string | null): void {
  if (id === _focusedCellId) return;
  _focusedCellId = id;
  deferEmit(_focusSubscribers);
}

export function setExecutingCellIds(ids: Set<string>): void {
  if (setsEqual(_executingCellIds, ids)) return;
  _executingCellIds = ids;
  deferEmit(_executingSubscribers);
}

export function setQueuedCellIds(ids: Set<string>): void {
  if (setsEqual(_queuedCellIds, ids)) return;
  _queuedCellIds = ids;
  deferEmit(_queuedSubscribers);
}

export function setSearchQuery(query: string | undefined): void {
  if (query === _searchQuery) return;
  _searchQuery = query;
  deferEmit(_searchQuerySubscribers);
}

export function setSearchCurrentMatch(match: FindMatch | null): void {
  if (_searchCurrentMatch === match) return;
  _searchCurrentMatch = match;
  deferEmit(_searchMatchSubscribers);
}

// ── Snapshot readers (non-reactive) ─────────────────────────────────────

export function getFocusedCellId(): string | null {
  return _focusedCellId;
}

// ── Hooks ───────────────────────────────────────────────────────────────

/** Subscribe to the focused cell ID. */
export function useFocusedCellId(): string | null {
  return useSyncExternalStore(subscribeFocus, getFocusSnapshot);
}

/** Returns true only when this specific cell is focused. */
export function useIsCellFocused(cellId: string): boolean {
  const subscribe = useMemo(() => subscribeFocusFor(cellId), [cellId]);
  const getSnapshot = useMemo(() => getIsFocusedSnapshot(cellId), [cellId]);
  return useSyncExternalStore(subscribe, getSnapshot);
}

/** Returns true only when this specific cell is executing. */
export function useIsCellExecuting(cellId: string): boolean {
  const subscribe = useMemo(() => subscribeExecutingFor(cellId), [cellId]);
  const getSnapshot = useMemo(() => getIsExecutingSnapshot(cellId), [cellId]);
  return useSyncExternalStore(subscribe, getSnapshot);
}

/** Returns true only when this specific cell is queued. */
export function useIsCellQueued(cellId: string): boolean {
  const subscribe = useMemo(() => subscribeQueuedFor(cellId), [cellId]);
  const getSnapshot = useMemo(() => getIsQueuedSnapshot(cellId), [cellId]);
  return useSyncExternalStore(subscribe, getSnapshot);
}

/** Subscribe to the search query string. */
export function useSearchQuery(): string | undefined {
  return useSyncExternalStore(subscribeSearchQuery, getSearchQuerySnapshot);
}

/** Subscribe to the current search match. */
export function useSearchCurrentMatch(): FindMatch | null {
  return useSyncExternalStore(subscribeSearchMatch, getSearchMatchSnapshot);
}

/** Returns true when this cell is immediately before the focused cell. */
export function useIsPreviousCellFromFocused(cellId: string): boolean {
  const subscribe = useMemo(() => subscribeNeighborFor(cellId), [cellId]);
  const getSnapshot = useMemo(() => getIsPreviousSnapshot(cellId), [cellId]);
  return useSyncExternalStore(subscribe, getSnapshot);
}

/** Returns true when this cell is immediately after the focused cell. */
export function useIsNextCellFromFocused(cellId: string): boolean {
  const subscribe = useMemo(() => subscribeNeighborFor(cellId), [cellId]);
  const getSnapshot = useMemo(() => getIsNextSnapshot(cellId), [cellId]);
  return useSyncExternalStore(subscribe, getSnapshot);
}

/**
 * Returns the active source search offset for this cell, or -1.
 * Only re-renders when the search match changes.
 */
export function useSearchActiveOffset(cellId: string): number {
  const subscribe = useMemo(() => subscribeSearchMatchFor(cellId), [cellId]);
  const getSnapshot = useMemo(
    () => getSearchActiveOffsetSnapshot(cellId),
    [cellId],
  );
  return useSyncExternalStore(subscribe, getSnapshot);
}

/**
 * Returns true if any cell in the given group is executing.
 * Re-renders when executing state changes.
 */
export function useIsGroupExecuting(
  groupCellIds: string[] | undefined,
): boolean {
  const subscribe = useMemo(() => subscribeExecutingForGroup(), []);
  const getSnapshot = useMemo(
    () => getIsGroupExecutingSnapshot(groupCellIds),
    [groupCellIds],
  );
  return useSyncExternalStore(subscribe, getSnapshot);
}

// ── Subscription helpers ────────────────────────────────────────────────

function subscribeFocus(cb: () => void): () => void {
  _focusSubscribers.add(cb);
  return () => _focusSubscribers.delete(cb);
}

function getFocusSnapshot(): string | null {
  return _focusedCellId;
}

// Per-cell focus: subscribes to the global focus change, but snapshot
// returns a boolean — useSyncExternalStore only re-renders when the
// boolean value changes (i.e. this cell gains or loses focus).
function subscribeFocusFor(_cellId: string): (cb: () => void) => () => void {
  return (cb: () => void) => {
    _focusSubscribers.add(cb);
    return () => _focusSubscribers.delete(cb);
  };
}

// Per-cell neighbor: subscribes to both focus changes AND structural changes
// (cell add/delete/reorder). Without the structural subscription, inserting
// or deleting a cell next to the focused cell wouldn't update the dimming.
function subscribeNeighborFor(_cellId: string): (cb: () => void) => () => void {
  return (cb: () => void) => {
    _focusSubscribers.add(cb);
    const unsubIds = subscribeIds(cb);
    return () => {
      _focusSubscribers.delete(cb);
      unsubIds();
    };
  };
}

function getIsFocusedSnapshot(cellId: string): () => boolean {
  let prev = _focusedCellId === cellId;
  return () => {
    const next = _focusedCellId === cellId;
    if (next !== prev) prev = next;
    return prev;
  };
}

function getIsPreviousSnapshot(cellId: string): () => boolean {
  let prev = false;
  return () => {
    if (!_focusedCellId) {
      if (prev) prev = false;
      return prev;
    }
    const ids = getCellIdsSnapshot();
    const focusedIndex = ids.indexOf(_focusedCellId);
    const next = focusedIndex > 0 && ids[focusedIndex - 1] === cellId;
    if (next !== prev) prev = next;
    return prev;
  };
}

function getIsNextSnapshot(cellId: string): () => boolean {
  let prev = false;
  return () => {
    if (!_focusedCellId) {
      if (prev) prev = false;
      return prev;
    }
    const ids = getCellIdsSnapshot();
    const focusedIndex = ids.indexOf(_focusedCellId);
    const next =
      focusedIndex >= 0 &&
      focusedIndex < ids.length - 1 &&
      ids[focusedIndex + 1] === cellId;
    if (next !== prev) prev = next;
    return prev;
  };
}

function subscribeExecutingFor(
  _cellId: string,
): (cb: () => void) => () => void {
  return (cb: () => void) => {
    _executingSubscribers.add(cb);
    return () => _executingSubscribers.delete(cb);
  };
}

function getIsExecutingSnapshot(cellId: string): () => boolean {
  let prev = _executingCellIds.has(cellId);
  return () => {
    const next = _executingCellIds.has(cellId);
    if (next !== prev) prev = next;
    return prev;
  };
}

function subscribeExecutingForGroup(): (cb: () => void) => () => void {
  return (cb: () => void) => {
    _executingSubscribers.add(cb);
    return () => _executingSubscribers.delete(cb);
  };
}

function getIsGroupExecutingSnapshot(
  groupCellIds: string[] | undefined,
): () => boolean {
  let prev = false;
  return () => {
    const next = groupCellIds?.some((id) => _executingCellIds.has(id)) ?? false;
    if (next !== prev) prev = next;
    return prev;
  };
}

function subscribeQueuedFor(_cellId: string): (cb: () => void) => () => void {
  return (cb: () => void) => {
    _queuedSubscribers.add(cb);
    return () => _queuedSubscribers.delete(cb);
  };
}

function getIsQueuedSnapshot(cellId: string): () => boolean {
  let prev = _queuedCellIds.has(cellId);
  return () => {
    const next = _queuedCellIds.has(cellId);
    if (next !== prev) prev = next;
    return prev;
  };
}

function subscribeSearchQuery(cb: () => void): () => void {
  _searchQuerySubscribers.add(cb);
  return () => _searchQuerySubscribers.delete(cb);
}

function getSearchQuerySnapshot(): string | undefined {
  return _searchQuery;
}

function subscribeSearchMatch(cb: () => void): () => void {
  _searchMatchSubscribers.add(cb);
  return () => _searchMatchSubscribers.delete(cb);
}

function getSearchMatchSnapshot(): FindMatch | null {
  return _searchCurrentMatch;
}

function subscribeSearchMatchFor(
  _cellId: string,
): (cb: () => void) => () => void {
  return (cb: () => void) => {
    _searchMatchSubscribers.add(cb);
    return () => _searchMatchSubscribers.delete(cb);
  };
}

function getSearchActiveOffsetSnapshot(cellId: string): () => number {
  let prev = -1;
  return () => {
    const m = _searchCurrentMatch;
    const next =
      m && m.cellId === cellId && m.type === "source" ? m.offset : -1;
    if (next !== prev) prev = next;
    return prev;
  };
}
