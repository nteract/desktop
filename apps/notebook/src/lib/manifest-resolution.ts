import type { JupyterOutput } from "../types";

/**
 * Check if a string looks like a manifest hash (64-char hex SHA-256).
 */
export function isManifestHash(s: string): boolean {
  return /^[a-f0-9]{64}$/.test(s);
}

/**
 * Check if a MIME type represents binary content.
 *
 * Binary MIME types are stored as raw bytes in the blob store (decoded
 * from Jupyter's base64 wire format). The blob HTTP server serves them
 * with the correct Content-Type, so the frontend can use blob URLs
 * directly (e.g., `<img src="http://...">`) instead of base64 data URIs.
 */
export function isBinaryMime(mime: string): boolean {
  if (mime.startsWith("image/")) {
    // SVG is plain XML text in Jupyter, not base64-encoded binary.
    return !mime.endsWith("+xml");
  }
  if (mime.startsWith("audio/") || mime.startsWith("video/")) {
    return true;
  }

  // application/* is binary by default, with carve-outs for text-like formats.
  if (mime.startsWith("application/")) {
    const subtype = mime.slice("application/".length);
    const isText =
      subtype === "json" ||
      subtype === "javascript" ||
      subtype === "ecmascript" ||
      subtype === "xml" ||
      subtype === "xhtml+xml" ||
      subtype === "mathml+xml" ||
      subtype === "sql" ||
      subtype === "graphql" ||
      subtype === "x-latex" ||
      subtype === "x-tex" ||
      subtype.endsWith("+json") ||
      subtype.endsWith("+xml");
    return !isText;
  }

  return false;
}

/**
 * A content reference — either inlined data or a blob-store hash.
 */
export type ContentRef = { inline: string } | { blob: string; size: number };

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
 * Resolve a content reference to its string value.
 *
 * For binary MIME types (images, etc.), blob refs resolve to an HTTP URL
 * pointing at the blob server. The browser fetches the raw bytes directly
 * when rendering (e.g., `<img src="http://...">`) — no base64 round-trip.
 *
 * For text MIME types, blob refs are fetched and returned as strings.
 * Inlined refs always return the embedded string directly.
 */
export async function resolveContentRef(
  ref: ContentRef,
  blobPort: number,
  mimeType?: string,
): Promise<string> {
  if ("inline" in ref) {
    return ref.inline;
  }
  // Binary blob refs resolve to a URL — the blob server serves raw bytes
  // with the correct Content-Type. The browser/iframe fetches directly.
  if (mimeType && isBinaryMime(mimeType)) {
    return `http://127.0.0.1:${blobPort}/blob/${ref.blob}`;
  }
  const response = await fetch(`http://127.0.0.1:${blobPort}/blob/${ref.blob}`);
  if (!response.ok) {
    throw new Error(`Failed to fetch blob ${ref.blob}: ${response.status}`);
  }
  return response.text();
}

/**
 * Resolve a MIME-type → ContentRef map to a fully hydrated data bundle.
 *
 * Binary MIME types resolve to blob URLs (the browser fetches raw bytes
 * directly from the blob server). JSON MIME types are auto-parsed.
 * Text MIME types are returned as strings.
 */
export async function resolveDataBundle(
  data: Record<string, ContentRef>,
  blobPort: number,
): Promise<Record<string, unknown>> {
  const resolved: Record<string, unknown> = {};
  for (const [mimeType, ref] of Object.entries(data)) {
    const content = await resolveContentRef(ref, blobPort, mimeType);
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
