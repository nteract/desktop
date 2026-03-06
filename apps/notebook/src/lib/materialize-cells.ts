import type { JupyterOutput, NotebookCell } from "../types";
import { logger } from "./logger";

/**
 * Snapshot of a cell from the Automerge document.
 * Matches the Rust CellSnapshot struct used by both the Tauri sync client
 * and the runtimed-wasm NotebookHandle.
 */
export interface CellSnapshot {
  id: string;
  cell_type: string;
  source: string;
  execution_count: string; // "5" or "null"
  outputs: string[]; // JSON-encoded Jupyter outputs or manifest hashes
}

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

/**
 * Resolve a single output string — either raw JSON or a manifest hash.
 *
 * - If cached, returns the cached value.
 * - If not a manifest hash, parses as raw JSON.
 * - If a manifest hash, fetches from blob store and resolves the manifest.
 */
export async function resolveOutput(
  outputStr: string,
  blobPort: number | null,
  cache: Map<string, JupyterOutput>,
): Promise<JupyterOutput | null> {
  const cached = cache.get(outputStr);
  if (cached) return cached;

  if (!isManifestHash(outputStr)) {
    try {
      const output = JSON.parse(outputStr) as JupyterOutput;
      cache.set(outputStr, output);
      return output;
    } catch {
      logger.warn("[materialize-cells] Failed to parse output JSON");
      return null;
    }
  }

  if (blobPort === null) {
    logger.warn("[materialize-cells] Manifest hash but no blob port");
    return null;
  }

  try {
    const response = await fetch(
      `http://127.0.0.1:${blobPort}/blob/${outputStr}`,
    );
    if (!response.ok) {
      logger.warn(
        `[materialize-cells] Failed to fetch manifest: ${response.status}`,
      );
      return null;
    }

    const manifestJson = await response.text();
    const manifest = JSON.parse(manifestJson) as OutputManifest;
    const output = await resolveManifest(manifest, blobPort);

    cache.set(outputStr, output);
    return output;
  } catch (e) {
    logger.warn("[materialize-cells] Failed to resolve manifest:", e);
    return null;
  }
}

/**
 * Merge consecutive stream outputs sharing the same name (stdout/stderr).
 * Handles both `string` and `string[]` text formats.
 */
export function mergeConsecutiveStreams(
  outputs: JupyterOutput[],
): JupyterOutput[] {
  return outputs.reduce<JupyterOutput[]>((merged, output) => {
    if (output.output_type === "stream" && merged.length > 0) {
      const last = merged[merged.length - 1];
      if (last.output_type === "stream" && last.name === output.name) {
        const lastText = Array.isArray(last.text)
          ? last.text.join("")
          : last.text;
        const outputText = Array.isArray(output.text)
          ? output.text.join("")
          : output.text;
        merged[merged.length - 1] = {
          ...last,
          text: lastText + outputText,
        };
        return merged;
      }
    }
    merged.push(output);
    return merged;
  }, []);
}

/**
 * Convert CellSnapshots to NotebookCells, resolving manifest hashes.
 *
 * This is the primary materialization function shared between `useNotebook`
 * (which receives CellSnapshots from the Tauri sync client) and
 * `useAutomergeNotebook` (which reads them from the WASM NotebookHandle).
 */
export async function cellSnapshotsToNotebookCells(
  snapshots: CellSnapshot[],
  blobPort: number | null,
  cache: Map<string, JupyterOutput>,
): Promise<NotebookCell[]> {
  return Promise.all(
    snapshots.map(async (snap) => {
      const executionCount =
        snap.execution_count === "null"
          ? null
          : Number.parseInt(snap.execution_count, 10);

      if (snap.cell_type === "code") {
        // Resolve all outputs (may be manifest hashes or raw JSON)
        const resolvedOutputs = (
          await Promise.all(
            snap.outputs.map((o) => resolveOutput(o, blobPort, cache)),
          )
        ).filter((o): o is JupyterOutput => o !== null);

        // Merge consecutive stream outputs as a fallback for unmerged data
        const outputs = mergeConsecutiveStreams(resolvedOutputs);

        return {
          id: snap.id,
          cell_type: "code" as const,
          source: snap.source,
          execution_count: Number.isNaN(executionCount) ? null : executionCount,
          outputs,
        };
      }

      // markdown or raw
      return {
        id: snap.id,
        cell_type: snap.cell_type as "markdown" | "raw",
        source: snap.source,
      };
    }),
  );
}
