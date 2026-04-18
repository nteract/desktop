import type { NotebookHost } from "@nteract/notebook-host";
import { useSyncExternalStore } from "react";
import { logger } from "./logger";

// ---------------------------------------------------------------------------
// Blob port store — single source of truth for the daemon's blob server port.
//
// The blob port is set once on daemon connect and only changes on reconnect.
// All consumers read synchronously from this store instead of re-awaiting
// a promise on every access.
//
// Usage:
//   - React components: `const port = useBlobPort()`
//   - Non-React code:   `const port = getBlobPort()`
//   - On connect/reconnect: `refreshBlobPort()`
// ---------------------------------------------------------------------------

let _blobPort: number | null = null;
let _refreshPromise: Promise<number | null> | null = null;

// Generation counter — incremented on every reset. Refresh results from
// a previous generation are discarded to prevent a stale port from a
// pre-reset fetch overwriting the reset state.
let _generation = 0;

const _subscribers = new Set<() => void>();

// Module-level host reference for fetching the blob port. Wired at boot
// by main.tsx via `setBlobPortHost(host)`. Kept as a reference so the
// module doesn't reach for Tauri directly; any host implementation
// (Tauri, Electron, future browser) provides `host.blobs.port()`.
let _host: NotebookHost | null = null;

/** Install the `NotebookHost` this module fetches the blob port from. */
export function setBlobPortHost(host: NotebookHost | null): void {
  _host = host;
}

function emit(): void {
  for (const cb of _subscribers) cb();
}

function subscribe(callback: () => void): () => void {
  _subscribers.add(callback);
  return () => _subscribers.delete(callback);
}

function getSnapshot(): number | null {
  return _blobPort;
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/**
 * Get the current blob port synchronously. Returns `null` if not yet resolved.
 */
export function getBlobPort(): number | null {
  return _blobPort;
}

/**
 * Fetch the blob port from the daemon and update the store.
 *
 * Call on initial connect and on `daemon:ready` / reconnect events.
 * Deduplicates concurrent calls — only one IPC request in flight at a time.
 * If `resetBlobPort()` is called while a refresh is in flight, the stale
 * result is discarded (generation counter check).
 * Returns the resolved port (or null on failure).
 */
export async function refreshBlobPort(): Promise<number | null> {
  if (_refreshPromise) return _refreshPromise;

  const gen = _generation;

  _refreshPromise = (async () => {
    // Retry a few times — the blob server may not be ready immediately
    // after daemon startup.
    for (let attempt = 1; attempt <= 5; attempt++) {
      // Bail early if a reset occurred while we were retrying.
      if (_generation !== gen) return _blobPort;

      try {
        if (!_host) {
          throw new Error("blob-port: no NotebookHost configured — call setBlobPortHost at boot");
        }
        const port = await _host.blobs.port();

        // Discard result if a reset happened since we started.
        if (_generation !== gen) return _blobPort;

        _blobPort = port;
        emit();
        return port;
      } catch (e) {
        if (attempt < 5) {
          await new Promise((r) => setTimeout(r, 500));
        } else {
          logger.warn(
            `[blob-port] Failed to get blob port after 5 attempts:`,
            e,
          );
        }
      }
    }
    return null;
  })();

  try {
    return await _refreshPromise;
  } finally {
    // Only clear the dedup promise if we're still the current generation.
    // A reset during our flight already cleared it.
    if (_generation === gen) {
      _refreshPromise = null;
    }
  }
}

/**
 * Reset the blob port (e.g., on daemon disconnect). Forces the next
 * `refreshBlobPort()` call to fetch fresh. Any in-flight refresh from
 * a prior generation will be discarded when it resolves.
 */
export function resetBlobPort(): void {
  _generation++;
  _blobPort = null;
  _refreshPromise = null;
  emit();
}

/**
 * React hook — subscribes to the blob port store.
 * Re-renders only when the port value changes.
 */
export function useBlobPort(): number | null {
  return useSyncExternalStore(subscribe, getSnapshot);
}

// ---------------------------------------------------------------------------
// Test helpers — only exported for unit tests
// ---------------------------------------------------------------------------

/** @internal Reset all state for test isolation. */
export function _testReset(): void {
  _blobPort = null;
  _refreshPromise = null;
  _generation = 0;
  _subscribers.clear();
}

/** @internal Get the current generation (for verifying reset behavior). */
export function _testGetGeneration(): number {
  return _generation;
}
