/**
 * TypeScript schema for the Automerge notebook document.
 *
 * Mirrors the Rust `NotebookDoc` schema defined in
 * `crates/runtimed/src/notebook_doc.rs` (lines 10-24).
 *
 * Document structure:
 * ```
 * ROOT/
 *   notebook_id: string
 *   cells: List<CellDoc>
 *   metadata: Map { runtime: string, notebook_metadata: string }
 * ```
 */

/**
 * A single cell in the Automerge document.
 *
 * - `source` is an Automerge `Text` CRDT, enabling character-level merging
 *   across concurrent editors. In the TypeScript API it appears as a `string`.
 * - `execution_count` is JSON-encoded: a number string like `"5"` or `"null"`.
 * - `outputs` is a list of JSON-encoded Jupyter output objects, or manifest
 *   hashes (64-char hex SHA-256) that resolve via the blob store.
 */
export interface CellDoc {
  id: string;
  cell_type: "code" | "markdown" | "raw";
  source: string;
  execution_count: string;
  outputs: string[];
}

/**
 * The root schema of the Automerge notebook document.
 */
export interface NotebookSchema {
  notebook_id: string;
  cells: CellDoc[];
  metadata: {
    runtime: string;
    notebook_metadata: string;
  };
}
