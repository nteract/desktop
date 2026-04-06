import type { JupyterOutput } from "../types";

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
export type OutputManifest =
  | {
      output_type: "display_data";
      data: Record<string, ContentRef>;
      metadata?: Record<string, unknown>;
      transient?: { display_id?: string };
    }
  | {
      output_type: "execute_result";
      data: Record<string, ContentRef>;
      metadata?: Record<string, unknown>;
      execution_count?: number | null;
      transient?: { display_id?: string };
    }
  | {
      output_type: "stream";
      name: string;
      text: ContentRef;
    }
  | {
      output_type: "error";
      ename: string;
      evalue: string;
      traceback: ContentRef;
    };

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
 * - `blob` refs fetch text content from the blob server.
 */
export async function resolveContentRef(
  ref: ContentRef,
  blobPort: number,
  _mimeType?: string,
): Promise<string> {
  if ("inline" in ref) {
    return ref.inline;
  }
  if ("url" in ref) {
    return ref.url;
  }
  // Blob ref — fetch text content from blob server
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
 *
 * Returns null for blob refs (require HTTP fetch).
 */
function resolveContentRefSync(
  ref: ContentRef,
  _blobPort: number,
  _mimeType?: string,
): string | null {
  if ("inline" in ref) {
    return ref.inline;
  }
  if ("url" in ref) {
    return ref.url;
  }
  // Blob ref — needs async fetch
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
  switch (manifest.output_type) {
    case "display_data": {
      const data = await resolveDataBundle(manifest.data, blobPort);
      return {
        output_type: "display_data",
        data,
        metadata: manifest.metadata ?? {},
        display_id: manifest.transient?.display_id,
      };
    }
    case "execute_result": {
      const data = await resolveDataBundle(manifest.data, blobPort);
      return {
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
        output_type: "stream",
        name: manifest.name as "stdout" | "stderr",
        text,
      };
    }
    case "error": {
      const tracebackJson = await resolveContentRef(
        manifest.traceback,
        blobPort,
      );
      const traceback = JSON.parse(tracebackJson) as string[];
      return {
        output_type: "error",
        ename: manifest.ename,
        evalue: manifest.evalue,
        traceback,
      };
    }
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
  switch (manifest.output_type) {
    case "display_data": {
      const data = resolveDataBundleSync(manifest.data, blobPort);
      if (data === null) return null;
      return {
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
        output_type: "stream",
        name: manifest.name as "stdout" | "stderr",
        text,
      };
    }
    case "error": {
      const tracebackJson = resolveContentRefSync(manifest.traceback, blobPort);
      if (tracebackJson === null) return null;
      const traceback = JSON.parse(tracebackJson) as string[];
      return {
        output_type: "error",
        ename: manifest.ename,
        evalue: manifest.evalue,
        traceback,
      };
    }
  }
}
