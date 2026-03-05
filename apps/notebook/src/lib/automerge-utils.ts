/**
 * Shared utilities for Automerge notebook document materialization.
 *
 * These functions convert the Automerge document representation (CellSnapshot)
 * into the React-friendly NotebookCell types used by the UI, resolving
 * manifest hashes via the blob store HTTP API.
 *
 * Extracted from useNotebook.ts to be shared by both useNotebook and
 * useAutomergeNotebook hooks.
 */

import type { JupyterOutput, NotebookCell } from "../types";
import { logger } from "./logger";

/**
 * Snapshot of a cell from the Automerge document.
 * Matches the Rust CellSnapshot struct and the TypeScript CellDoc schema.
 */
export interface CellSnapshot {
  id: string;
  cell_type: string;
  source: string;
  execution_count: string;
  outputs: string[];
}

/**
 * Check if a string looks like a manifest hash (64-char hex SHA-256).
 */
function isManifestHash(s: string): boolean {
  return /^[a-f0-9]{64}$/.test(s);
}

/**
 * ContentRef from manifest - content may be inlined or stored in blob store.
 */
type ContentRef = { inline: string } | { blob: string; size: number };

/**
 * Manifest types matching the Rust OutputManifest enum.
 */
type OutputManifest =
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

async function resolveContentRef(
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

async function resolveDataBundle(
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

async function resolveManifest(
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
 * Resolve an output string to a JupyterOutput.
 * Handles both manifest hashes (64-char hex) and raw JSON.
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
      logger.warn("[notebook-sync] Failed to parse output JSON");
      return null;
    }
  }

  if (blobPort === null) {
    logger.warn("[notebook-sync] Manifest hash but no blob port");
    return null;
  }

  try {
    const response = await fetch(
      `http://127.0.0.1:${blobPort}/blob/${outputStr}`,
    );
    if (!response.ok) {
      logger.warn(
        `[notebook-sync] Failed to fetch manifest: ${response.status}`,
      );
      return null;
    }

    const manifestJson = await response.text();
    const manifest = JSON.parse(manifestJson) as OutputManifest;
    const output = await resolveManifest(manifest, blobPort);

    cache.set(outputStr, output);
    return output;
  } catch (e) {
    logger.warn("[notebook-sync] Failed to resolve manifest:", e);
    return null;
  }
}

/**
 * Merge consecutive stream outputs of the same type (stdout/stderr).
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
        const resolvedOutputs = (
          await Promise.all(
            snap.outputs.map((o) => resolveOutput(o, blobPort, cache)),
          )
        ).filter((o): o is JupyterOutput => o !== null);

        const outputs = mergeConsecutiveStreams(resolvedOutputs);

        return {
          id: snap.id,
          cell_type: "code" as const,
          source: snap.source,
          execution_count: Number.isNaN(executionCount) ? null : executionCount,
          outputs,
        };
      }

      return {
        id: snap.id,
        cell_type: snap.cell_type as "markdown" | "raw",
        source: snap.source,
      };
    }),
  );
}
