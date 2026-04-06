/**
 * Resolve widget state blob references to blob URLs.
 *
 * Widget state values that exceed the inline threshold are stored in the
 * daemon's blob store and referenced in the CRDT as ContentRef objects:
 *
 *   {"blob": "<hash>", "size": N, "media_type": "text/javascript"}
 *
 * This module replaces them with HTTP blob URLs that the browser or iframe
 * can load directly. Binary buffers and executable content (_esm, _css)
 * become URL strings; the iframe fetches binary URLs as ArrayBuffers.
 *
 * Inline ContentRefs ({"inline": value}) are unwrapped to their inner value.
 */

import { getBlobPort } from "./blob-port";

/** ContentRef blob reference: {"blob": hash, "size": N, "media_type"?: string} */
function isContentRef(
  value: unknown,
): value is { blob: string; size: number; media_type?: string } {
  if (typeof value !== "object" || value === null) return false;
  const obj = value as Record<string, unknown>;
  return typeof obj.blob === "string" && typeof obj.size === "number";
}

/** ContentRef inline reference: {"inline": string | object} */
function isInlineRef(value: unknown): value is { inline: unknown } {
  if (typeof value !== "object" || value === null) return false;
  return "inline" in (value as Record<string, unknown>);
}

/**
 * Resolve all ContentRef references in a widget state object.
 *
 * Walks the state recursively:
 *
 * - ContentRef blob → blob URL string
 * - ContentRef inline → unwrapped inline value
 * - Plain values → pass through unchanged
 *
 * Returns the resolved state and the paths where blob URL replacements
 * occurred (for the iframe to know which values to fetch as ArrayBuffers).
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
  // ContentRef blob reference — resolve to URL
  if (isContentRef(value)) {
    bufferPaths.push([...currentPath]);
    return `http://127.0.0.1:${blobPort}/blob/${value.blob}`;
  }

  // ContentRef inline reference — unwrap the value
  if (isInlineRef(value)) {
    return value.inline;
  }

  // Recurse into arrays
  if (Array.isArray(value)) {
    return value.map((item, i) =>
      walkAndReplace(item, [...currentPath, String(i)], bufferPaths, blobPort),
    );
  }

  // Recurse into objects
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

  // Plain values pass through
  return value;
}
