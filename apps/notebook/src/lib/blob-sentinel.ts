/**
 * Resolve {"$blob": "<hash>"} sentinels in widget state objects.
 *
 * Widget binary buffers are stored in the blob store and referenced
 * as sentinels in the CRDT state. This module resolves them to
 * ArrayBuffers via the blob HTTP server before passing to widgets.
 */

import { getBlobPort, refreshBlobPort } from "./blob-port";
import { logger } from "./logger";

/** Cache of resolved blobs by hash. */
const blobCache = new Map<string, ArrayBuffer>();

/** Check if a value is a blob sentinel. */
function isBlobSentinel(value: unknown): value is { $blob: string } {
  return (
    typeof value === "object" &&
    value !== null &&
    "$blob" in value &&
    typeof (value as Record<string, unknown>).$blob === "string"
  );
}

/**
 * Resolve all blob sentinels in a widget state object.
 *
 * Walks the state recursively, replacing {"$blob": "<hash>"} with
 * resolved ArrayBuffers. Returns the state with sentinels replaced
 * and an array of resolved buffers (for the Jupyter buffer protocol).
 *
 * Uses a cache to avoid re-fetching the same blob.
 */
export async function resolveBlobSentinels(
  state: Record<string, unknown>,
): Promise<{
  resolvedState: Record<string, unknown>;
  buffers: ArrayBuffer[];
  bufferPaths: string[][];
}> {
  const buffers: ArrayBuffer[] = [];
  const bufferPaths: string[][] = [];
  const resolvedState = { ...state };

  // Collect all sentinel paths and hashes
  const sentinels: { path: string[]; hash: string }[] = [];
  collectSentinels(resolvedState, [], sentinels);

  if (sentinels.length === 0) {
    return { resolvedState, buffers: [], bufferPaths: [] };
  }

  let blobPort = getBlobPort();
  if (blobPort === null) {
    blobPort = await refreshBlobPort();
  }
  if (blobPort === null) {
    logger.warn(
      "[blob-sentinel] No blob port available, cannot resolve sentinels",
    );
    return { resolvedState, buffers: [], bufferPaths: [] };
  }

  // Resolve all blobs in parallel
  const resolved = await Promise.all(
    sentinels.map(async ({ hash }) => {
      const cached = blobCache.get(hash);
      if (cached) return cached;

      try {
        const response = await fetch(
          `http://127.0.0.1:${blobPort}/blob/${hash}`,
        );
        if (!response.ok) {
          logger.warn(
            `[blob-sentinel] Failed to fetch blob ${hash.slice(0, 16)}...: ${response.status}`,
          );
          return null;
        }
        const buffer = await response.arrayBuffer();
        blobCache.set(hash, buffer);
        return buffer;
      } catch (e) {
        logger.warn(
          `[blob-sentinel] Failed to fetch blob ${hash.slice(0, 16)}...:`,
          e,
        );
        return null;
      }
    }),
  );

  // Replace sentinels with resolved buffers
  for (let i = 0; i < sentinels.length; i++) {
    const buffer = resolved[i];
    if (!buffer) continue;

    const { path } = sentinels[i];
    buffers.push(buffer);
    bufferPaths.push(path);

    // Navigate to parent and replace sentinel with buffer
    let current: Record<string, unknown> = resolvedState;
    for (let j = 0; j < path.length - 1; j++) {
      const next = current[path[j]];
      if (typeof next !== "object" || next === null) break;
      current = next as Record<string, unknown>;
    }
    const lastKey = path[path.length - 1];
    if (lastKey) {
      current[lastKey] = buffer;
    }
  }

  return { resolvedState, buffers, bufferPaths };
}

/** Recursively collect blob sentinels with their paths. */
function collectSentinels(
  obj: Record<string, unknown>,
  currentPath: string[],
  out: { path: string[]; hash: string }[],
): void {
  for (const [key, value] of Object.entries(obj)) {
    if (isBlobSentinel(value)) {
      out.push({ path: [...currentPath, key], hash: value.$blob });
    } else if (
      typeof value === "object" &&
      value !== null &&
      !ArrayBuffer.isView(value)
    ) {
      collectSentinels(
        value as Record<string, unknown>,
        [...currentPath, key],
        out,
      );
    }
  }
}
