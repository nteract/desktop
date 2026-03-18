import { useCallback, useRef } from "react";
import { useBlobPort } from "../lib/blob-port";
import { logger } from "../lib/logger";
import type { OutputManifest } from "../lib/manifest-resolution";
import { isManifestHash, resolveManifest } from "../lib/manifest-resolution";
import type { JupyterOutput } from "../types";

/**
 * Resolve an output string to a JupyterOutput.
 *
 * The output string may be:
 * - A blob hash (64-char hex) pointing to an output manifest
 * - Raw Jupyter output JSON (for backward compatibility)
 *
 * This is a standalone function for use outside React hooks (e.g., event handlers).
 */
export async function resolveOutputString(
  outputStr: string,
  blobPort: number,
): Promise<JupyterOutput | null> {
  // If it doesn't look like a blob hash, try parsing as raw JSON
  if (!isManifestHash(outputStr)) {
    try {
      return JSON.parse(outputStr) as JupyterOutput;
    } catch {
      logger.warn("[manifest-resolver] Failed to parse output as JSON");
      return null;
    }
  }

  // It's a blob hash - fetch manifest and resolve
  try {
    const response = await fetch(
      `http://127.0.0.1:${blobPort}/blob/${outputStr}`,
    );
    if (!response.ok) {
      logger.warn(
        `[manifest-resolver] Failed to fetch manifest: ${response.status}`,
      );
      return null;
    }

    const manifestJson = await response.text();
    const manifest = JSON.parse(manifestJson) as OutputManifest;
    return resolveManifest(manifest, blobPort);
  } catch (e) {
    logger.warn("[manifest-resolver] Failed to resolve manifest:", e);
    return null;
  }
}

/**
 * Hook for resolving output manifests from the blob store.
 *
 * This hook fetches the blob server port from the daemon and provides
 * a function to resolve manifest hashes to full Jupyter outputs.
 * Results are cached to avoid redundant fetches.
 */
export function useManifestResolver() {
  const blobPort = useBlobPort();
  const cacheRef = useRef<Map<string, JupyterOutput>>(new Map());
  const pendingRef = useRef<Map<string, Promise<JupyterOutput | null>>>(
    new Map(),
  );

  /**
   * Resolve an output string to a JupyterOutput.
   *
   * The output string may be:
   * - A blob hash (64-char hex) pointing to an output manifest
   * - Raw Jupyter output JSON (for backward compatibility during transition)
   *
   * Returns null if resolution fails.
   */
  const resolveOutput = useCallback(
    async (outputStr: string): Promise<JupyterOutput | null> => {
      // Check cache
      const cached = cacheRef.current.get(outputStr);
      if (cached) {
        return cached;
      }

      // Check for in-flight request
      const pending = pendingRef.current.get(outputStr);
      if (pending) {
        return pending;
      }

      // If it doesn't look like a blob hash, try parsing as raw JSON
      if (!isManifestHash(outputStr)) {
        try {
          const output = JSON.parse(outputStr) as JupyterOutput;
          cacheRef.current.set(outputStr, output);
          return output;
        } catch {
          logger.warn("[manifest-resolver] Failed to parse output as JSON");
          return null;
        }
      }

      // Need blob port for manifest resolution
      if (blobPort === null) {
        logger.debug("[manifest-resolver] Blob port not available yet");
        return null;
      }

      // Create the promise and store it to dedupe concurrent requests
      const promise = (async () => {
        try {
          // Fetch manifest from blob store
          const response = await fetch(
            `http://127.0.0.1:${blobPort}/blob/${outputStr}`,
          );
          if (!response.ok) {
            logger.warn(
              `[manifest-resolver] Failed to fetch manifest: ${response.status}`,
            );
            return null;
          }

          const manifestJson = await response.text();
          const manifest = JSON.parse(manifestJson) as OutputManifest;
          const output = await resolveManifest(manifest, blobPort);

          // Cache the result
          cacheRef.current.set(outputStr, output);
          return output;
        } catch (e) {
          logger.warn("[manifest-resolver] Failed to resolve manifest:", e);
          return null;
        } finally {
          // Remove from pending
          pendingRef.current.delete(outputStr);
        }
      })();

      pendingRef.current.set(outputStr, promise);
      return promise;
    },
    [blobPort],
  );

  return { resolveOutput, blobPort };
}
