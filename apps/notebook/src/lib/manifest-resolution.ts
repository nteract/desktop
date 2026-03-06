import type { JupyterOutput } from "../types";

/**
 * Check if a string looks like a manifest hash (64-char hex SHA-256).
 */
export function isManifestHash(s: string): boolean {
  return /^[a-f0-9]{64}$/.test(s);
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
 * Inlined refs return immediately; blob refs are fetched from the blob store.
 */
export async function resolveContentRef(
  ref: ContentRef,
  blobPort: number,
): Promise<string> {
  if ("inline" in ref) {
    return ref.inline;
  }
  const response = await fetch(`http://127.0.0.1:${blobPort}/blob/${ref.blob}`);
  if (!response.ok) {
    throw new Error(`Failed to fetch blob ${ref.blob}: ${response.status}`);
  }
  return response.text();
}

/**
 * Resolve a MIME-type → ContentRef map to a fully hydrated data bundle.
 * JSON MIME types are auto-parsed.
 */
export async function resolveDataBundle(
  data: Record<string, ContentRef>,
  blobPort: number,
): Promise<Record<string, unknown>> {
  const resolved: Record<string, unknown> = {};
  for (const [mimeType, ref] of Object.entries(data)) {
    const content = await resolveContentRef(ref, blobPort);
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
