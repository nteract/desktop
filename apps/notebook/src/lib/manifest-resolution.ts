import type { JupyterOutput } from "../types";

/**
 * Quick check for binary MIME types — safety net for blob refs that WASM
 * couldn't resolve to Url (blob port not yet set at cold start).
 *
 * The canonical classification lives in Rust (notebook_doc::mime::is_binary_mime).
 * This is intentionally minimal — only covers the common cases that would
 * otherwise break if fetched as text.
 */
function looksLikeBinaryMime(mime: string): boolean {
  if (mime.startsWith("image/") && !mime.endsWith("+xml")) return true;
  if (mime.startsWith("audio/") || mime.startsWith("video/")) return true;
  if (mime === "application/pdf" || mime === "application/octet-stream")
    return true;
  return false;
}

/**
 * A content reference — either inlined data, a URL, or a blob-store hash.
 *
 * These variants match the `ResolvedContentRef` shape emitted by WASM:
 * - `inline`: text content embedded directly
 * - `url`: a pre-resolved URL (e.g., blob server URL for binary content)
 * - `blob`: a blob-store hash for text content that needs fetching
 */
export type ContentRef =
  | { inline: string }
  | { url: string }
  | { blob: string; size: number };

/**
 * An output manifest with content refs that may need blob-store resolution.
 */
interface ManifestCommon {
  /** Daemon-stamped UUID. Always non-empty on the write path. */
  output_id?: string;
}

export type OutputManifest =
  | (ManifestCommon & {
      output_type: "display_data";
      data: Record<string, ContentRef>;
      metadata?: Record<string, unknown>;
      transient?: { display_id?: string };
    })
  | (ManifestCommon & {
      output_type: "execute_result";
      data: Record<string, ContentRef>;
      metadata?: Record<string, unknown>;
      execution_count?: number | null;
      transient?: { display_id?: string };
    })
  | (ManifestCommon & {
      output_type: "stream";
      name: string;
      text: ContentRef;
    })
  | (ManifestCommon & {
      output_type: "error";
      ename: string;
      evalue: string;
      traceback: ContentRef;
      /**
       * Optional rich-traceback sibling. Carries the structured payload
       * the frontend's `TracebackOutput` renders (ename, evalue, frames
       * with source context, highlight markers). Present when the
       * kernel emitted rich via `application/vnd.nteract.traceback+json`
       * OR the daemon synthesized one from an ANSI traceback at load.
       *
       * In-memory only — `.ipynb` save strips the sibling so files stay
       * nbformat-clean (see `resolve_manifest` in `output_store.rs`).
       */
      rich?: ContentRef;
    });

/**
 * Type guard: returns true if `value` looks like a structured OutputManifest
 * (i.e., has ContentRef objects rather than already-resolved primitive data).
 *
 * Distinguishes manifests from raw JupyterOutputs by checking whether the
 * data fields contain ContentRef objects (`{ inline }`, `{ url }`, or `{ blob }`).
 */
export function isOutputManifest(value: unknown): value is OutputManifest {
  if (typeof value !== "object" || value === null) return false;
  const obj = value as Record<string, unknown>;
  if (!("output_type" in obj)) return false;

  switch (obj.output_type) {
    case "stream":
      return isContentRef(obj.text);
    case "error":
      return isContentRef(obj.traceback);
    case "display_data":
    case "execute_result": {
      if (typeof obj.data !== "object" || obj.data === null) return false;
      const entries = Object.values(obj.data as Record<string, unknown>);
      // A manifest's data values are ContentRef objects; a raw output's are strings/primitives
      return entries.length > 0 && entries.every(isContentRef);
    }
    default:
      return false;
  }
}

/** Check if a value is a ContentRef (`{ inline }`, `{ url }`, or `{ blob, size }`). */
function isContentRef(value: unknown): value is ContentRef {
  if (typeof value !== "object" || value === null) return false;
  const obj = value as Record<string, unknown>;
  return (
    ("inline" in obj && typeof obj.inline === "string") ||
    ("url" in obj && typeof obj.url === "string") ||
    ("blob" in obj && typeof obj.blob === "string")
  );
}

/**
 * Resolve a content reference to its string value.
 *
 * - `inline` refs return the embedded string directly.
 * - `url` refs return the pre-resolved URL (e.g., blob server URL for binary content).
 * - `blob` refs: binary MIME types resolve to a blob server URL (the browser
 *   fetches raw bytes directly); text MIME types are fetched as strings.
 *
 * Normally WASM emits `url` for binary types, but a `blob` ref may arrive
 * when the blob port wasn't set on the WASM handle yet (cold start, daemon
 * restart). The mimeType fallback handles that gracefully.
 */
export async function resolveContentRef(
  ref: ContentRef,
  blobPort: number,
  mimeType?: string,
): Promise<string> {
  if ("inline" in ref) {
    return ref.inline;
  }
  if ("url" in ref) {
    return ref.url;
  }
  // Safety net: binary blob refs that WASM couldn't resolve to Url
  // (blob port wasn't set yet). Construct the URL directly.
  if (mimeType && looksLikeBinaryMime(mimeType)) {
    return `http://127.0.0.1:${blobPort}/blob/${ref.blob}`;
  }
  // Text blob ref — fetch content from blob server
  const response = await fetch(`http://127.0.0.1:${blobPort}/blob/${ref.blob}`);
  if (!response.ok) {
    throw new Error(`Failed to fetch blob ${ref.blob}: ${response.status}`);
  }
  return response.text();
}

/**
 * Resolve a content reference synchronously, returning null if async
 * work (blob fetch) would be required.
 *
 * Resolves:
 * - Inline refs → the embedded string
 * - URL refs → the pre-resolved URL
 * - Binary blob refs → blob server URL (safety net for cold start)
 *
 * Returns null for text blob refs (require HTTP fetch).
 */
function resolveContentRefSync(
  ref: ContentRef,
  blobPort: number,
  mimeType?: string,
): string | null {
  if ("inline" in ref) {
    return ref.inline;
  }
  if ("url" in ref) {
    return ref.url;
  }
  // Safety net: binary blob refs that WASM couldn't resolve to Url
  if (mimeType && looksLikeBinaryMime(mimeType)) {
    return `http://127.0.0.1:${blobPort}/blob/${ref.blob}`;
  }
  // Text blob ref — needs async fetch
  return null;
}

/**
 * Resolve a MIME-type → ContentRef map to a fully hydrated data bundle.
 *
 * URL refs (binary content) pass through as URLs for the browser to fetch
 * directly. JSON MIME types are auto-parsed. Text MIME types are returned
 * as strings.
 */
export async function resolveDataBundle(
  data: Record<string, ContentRef>,
  blobPort: number,
): Promise<Record<string, unknown>> {
  const entries = Object.entries(data);
  const contents = await Promise.all(
    entries.map(([mimeType, ref]) =>
      resolveContentRef(ref, blobPort, mimeType),
    ),
  );
  const resolved: Record<string, unknown> = {};
  for (let i = 0; i < entries.length; i++) {
    const [mimeType] = entries[i];
    const content = contents[i];
    if (mimeType.includes("json")) {
      try {
        resolved[mimeType] = JSON.parse(content);
      } catch {
        resolved[mimeType] = content;
      }
    } else {
      resolved[mimeType] = content;
    }
  }
  return resolved;
}

/**
 * Synchronously resolve a data bundle. Returns null if any blob
 * fetch would be required.
 */
function resolveDataBundleSync(
  data: Record<string, ContentRef>,
  blobPort: number,
): Record<string, unknown> | null {
  const resolved: Record<string, unknown> = {};
  for (const [mimeType, ref] of Object.entries(data)) {
    const content = resolveContentRefSync(ref, blobPort, mimeType);
    if (content === null) return null;
    if (mimeType.includes("json")) {
      try {
        resolved[mimeType] = JSON.parse(content);
      } catch {
        resolved[mimeType] = content;
      }
    } else {
      resolved[mimeType] = content;
    }
  }
  return resolved;
}

/**
 * Resolve an output manifest into a fully hydrated JupyterOutput.
 */
export async function resolveManifest(
  manifest: OutputManifest,
  blobPort: number,
): Promise<JupyterOutput> {
  const output_id = manifest.output_id;
  switch (manifest.output_type) {
    case "display_data": {
      const data = await resolveDataBundle(manifest.data, blobPort);
      return {
        output_id,
        output_type: "display_data",
        data,
        metadata: manifest.metadata ?? {},
        display_id: manifest.transient?.display_id,
      };
    }
    case "execute_result": {
      const data = await resolveDataBundle(manifest.data, blobPort);
      return {
        output_id,
        output_type: "execute_result",
        data,
        metadata: manifest.metadata ?? {},
        execution_count: manifest.execution_count ?? null,
        display_id: manifest.transient?.display_id,
      };
    }
    case "stream": {
      const text = await resolveContentRef(manifest.text, blobPort);
      return {
        output_id,
        output_type: "stream",
        name: manifest.name as "stdout" | "stderr",
        text,
      };
    }
    case "error": {
      const tracebackJson = await resolveContentRef(manifest.traceback, blobPort);
      const traceback = JSON.parse(tracebackJson) as string[];
      const rich = await resolveRich(manifest.rich, blobPort);
      return {
        output_id,
        output_type: "error",
        ename: manifest.ename,
        evalue: manifest.evalue,
        traceback,
        rich,
      };
    }
  }
}

/**
 * Resolve the optional rich-traceback ContentRef into the parsed payload.
 *
 * Returns `undefined` when the manifest has no sibling OR the JSON
 * parse fails. The caller (OutputArea) treats `undefined` as "fall
 * back to AnsiErrorOutput", so a malformed payload never blocks
 * rendering — we just lose the rich upgrade for that one output.
 */
async function resolveRich(
  ref: ContentRef | undefined,
  blobPort: number,
): Promise<Record<string, unknown> | undefined> {
  if (!ref) return undefined;
  try {
    const json = await resolveContentRef(ref, blobPort);
    return JSON.parse(json) as Record<string, unknown>;
  } catch {
    return undefined;
  }
}

/**
 * Sync counterpart. Three-state to distinguish "no sibling" (caller
 * finishes resolution) from "sibling exists but blob-backed" (caller
 * must return null so the async path picks it up):
 *
 * - `"absent"`   → no sibling on the manifest, output is complete
 * - `"async"`    → sibling present but needs a blob fetch
 * - `{...}`      → resolved payload (or empty object for a malformed
 *                  blob — async-retrying a parse failure would loop)
 */
type RichSyncResult = "absent" | "async" | Record<string, unknown>;

function resolveRichSync(
  ref: ContentRef | undefined,
  blobPort: number,
): RichSyncResult {
  if (!ref) return "absent";
  const json = resolveContentRefSync(ref, blobPort);
  if (json === null) return "async";
  try {
    return JSON.parse(json) as Record<string, unknown>;
  } catch {
    // Resolved bytes parsed as non-JSON — TracebackOutput's fallback
    // will render the raw object view. Async would just fail the same
    // parse.
    return {};
  }
}

/**
 * Synchronously resolve a manifest into a JupyterOutput.
 *
 * Returns null if any content ref requires an async blob fetch.
 * Inline refs and URL refs are handled synchronously.
 *
 * Use this in the sync materialization path where blob fetches are not
 * available — the async path will pick up unresolved outputs later.
 */
export function resolveManifestSync(
  manifest: OutputManifest,
  blobPort: number,
): JupyterOutput | null {
  const output_id = manifest.output_id;
  switch (manifest.output_type) {
    case "display_data": {
      const data = resolveDataBundleSync(manifest.data, blobPort);
      if (data === null) return null;
      return {
        output_id,
        output_type: "display_data",
        data,
        metadata: manifest.metadata ?? {},
        display_id: manifest.transient?.display_id,
      };
    }
    case "execute_result": {
      const data = resolveDataBundleSync(manifest.data, blobPort);
      if (data === null) return null;
      return {
        output_id,
        output_type: "execute_result",
        data,
        metadata: manifest.metadata ?? {},
        execution_count: manifest.execution_count ?? null,
        display_id: manifest.transient?.display_id,
      };
    }
    case "stream": {
      const text = resolveContentRefSync(manifest.text, blobPort);
      if (text === null) return null;
      return {
        output_id,
        output_type: "stream",
        name: manifest.name as "stdout" | "stderr",
        text,
      };
    }
    case "error": {
      const tracebackJson = resolveContentRefSync(manifest.traceback, blobPort);
      if (tracebackJson === null) return null;
      const traceback = JSON.parse(tracebackJson) as string[];
      const richResult = resolveRichSync(manifest.rich, blobPort);
      // If the rich sibling exists but needs a blob fetch, defer the
      // whole output to the async path. Otherwise callers treat this
      // as fully resolved and never upgrade to rich rendering.
      if (richResult === "async") return null;
      const rich = richResult === "absent" ? undefined : richResult;
      return {
        output_id,
        output_type: "error",
        ename: manifest.ename,
        evalue: manifest.evalue,
        traceback,
        rich,
      };
    }
  }
}
