import { logger } from "../lib/logger";
import { isOutputManifest, type OutputManifest, resolveManifest } from "../lib/manifest-resolution";
import type { JupyterOutput } from "../types";

/**
 * Resolve an output value to a JupyterOutput.
 *
 * Accepts either:
 * - A structured OutputManifest object (from WASM layer or parsed JSON)
 * - A raw JupyterOutput object (already resolved, no ContentRefs)
 * - A JSON string (legacy path — parsed then resolved)
 *
 * For structured manifests, resolves ContentRefs: inline data is extracted
 * directly, blob refs are fetched from the blob server. The caller no
 * longer needs to fetch the manifest itself — it arrives pre-parsed from
 * the WASM layer or daemon broadcast.
 */
export async function resolveOutputValue(
  output: unknown,
  blobPort: number,
): Promise<JupyterOutput | null> {
  // Structured manifest from WASM — resolve ContentRefs (inline + blob)
  if (isOutputManifest(output)) {
    try {
      return await resolveManifest(output as OutputManifest, blobPort);
    } catch (e) {
      logger.warn("[manifest-resolver] Failed to resolve manifest:", e);
      return null;
    }
  }

  // Object with output_type but no ContentRefs — already a raw JupyterOutput
  if (typeof output === "object" && output !== null && "output_type" in output) {
    return output as JupyterOutput;
  }

  // String path: parse JSON, then check if it's a manifest or raw output
  if (typeof output === "string") {
    try {
      const parsed = JSON.parse(output);
      if (isOutputManifest(parsed)) {
        try {
          return await resolveManifest(parsed as OutputManifest, blobPort);
        } catch (e) {
          logger.warn("[manifest-resolver] Failed to resolve parsed manifest:", e);
          return null;
        }
      }
      // Already a resolved JupyterOutput
      return parsed as JupyterOutput;
    } catch {
      logger.warn("[manifest-resolver] Failed to parse output string as JSON");
      return null;
    }
  }

  logger.warn("[manifest-resolver] Unrecognized output type:", typeof output);
  return null;
}
