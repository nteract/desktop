/**
 * Convert {"$blob": "<hash>"} sentinels in widget state to blob URLs.
 *
 * Widget binary buffers are stored in the daemon's blob store and
 * referenced as sentinels in the CRDT state. This module replaces
 * them with HTTP blob URLs that the iframe can fetch directly.
 *
 * The iframe resolves URLs to ArrayBuffers via fetch() — this avoids
 * the DataCloneError from passing non-transferable objects through
 * postMessage.
 */

import { getBlobPort } from "./blob-port";

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
 * Replace all blob sentinels in a widget state object with blob URLs.
 *
 * Walks the state recursively, replacing `{"$blob": "<hash>"}` with
 * `"http://127.0.0.1:{port}/blob/{hash}"`. Returns the modified state
 * and the paths where replacements occurred.
 *
 * This is synchronous — no async fetches. The iframe fetches the
 * actual binary data via the URL.
 */
export function replaceSentinelsWithBlobUrls(state: Record<string, unknown>): {
  state: Record<string, unknown>;
  bufferPaths: string[][];
} {
  const blobPort = getBlobPort();
  if (blobPort === null) {
    return { state, bufferPaths: [] };
  }

  const bufferPaths: string[][] = [];
  const result = walkAndReplace(state, [], bufferPaths, blobPort);
  return {
    state: result as Record<string, unknown>,
    bufferPaths,
  };
}

function walkAndReplace(
  value: unknown,
  currentPath: string[],
  bufferPaths: string[][],
  blobPort: number,
): unknown {
  if (isBlobSentinel(value)) {
    bufferPaths.push([...currentPath]);
    return `http://127.0.0.1:${blobPort}/blob/${value.$blob}`;
  }

  if (Array.isArray(value)) {
    return value.map((item, i) =>
      walkAndReplace(item, [...currentPath, String(i)], bufferPaths, blobPort),
    );
  }

  if (typeof value === "object" && value !== null) {
    const result: Record<string, unknown> = {};
    for (const [key, val] of Object.entries(value)) {
      result[key] = walkAndReplace(
        val,
        [...currentPath, key],
        bufferPaths,
        blobPort,
      );
    }
    return result;
  }

  return value;
}
