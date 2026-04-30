/** Cell metadata (arbitrary JSON object, preserves unknown keys) */
export type CellMetadata = Record<string, unknown>;

export interface CodeCell {
  cell_type: "code";
  id: string;
  source: string;
  /**
   * Display count projected from RuntimeStateDoc when available, with the
   * notebook-doc nbformat fallback used only when runtime state is absent.
   */
  execution_count: number | null;
  /**
   * Legacy: after Phase C-lite, the frame pipeline no longer populates
   * this field on incremental output changes. Full materialization and
   * local CRDT mutations still write it, and a few cross-cell readers
   * (drag preview, hidden-group error count) still look at it — those
   * paths recompute on structural changes only and tolerate staleness
   * during a live session. New code should subscribe via
   * `useCellOutputs(cell_id)` from `notebook-outputs.ts` instead.
   */
  outputs: JupyterOutput[];
  metadata: CellMetadata;
}

export interface MarkdownCell {
  cell_type: "markdown";
  id: string;
  source: string;
  metadata: CellMetadata;
  /** Resolved markdown asset refs (`attachment:...`, relative paths) → blob hash */
  resolvedAssets?: Record<string, string>;
}

export interface RawCell {
  cell_type: "raw";
  id: string;
  source: string;
  metadata: CellMetadata;
}

export type NotebookCell = CodeCell | MarkdownCell | RawCell;

/**
 * Common fields on every nbformat output. `output_id` is a stable
 * daemon-stamped UUID — always non-empty on the daemon write path,
 * optional here only so in-flight / local-only outputs typecheck.
 */
interface OutputCommon {
  output_id?: string;
}

export type JupyterOutput =
  | (OutputCommon & {
      output_type: "execute_result" | "display_data";
      data: Record<string, unknown>;
      metadata?: Record<string, unknown>;
      execution_count?: number | null;
      display_id?: string;
    })
  | (OutputCommon & {
      output_type: "stream";
      name: "stdout" | "stderr";
      text: string;
    })
  | (OutputCommon & {
      output_type: "error";
      ename: string;
      evalue: string;
      traceback: string[];
      /**
       * Optional rich-traceback sibling payload (see
       * `src/components/cell/jupyter-output.ts` for the canonical doc).
       * Present when the kernel emitted rich via
       * `application/vnd.nteract.traceback+json` OR the daemon
       * synthesized one from the ANSI traceback at `.ipynb` load.
       */
      rich?: unknown;
    });

export interface KernelspecInfo {
  name: string;
  display_name: string;
  language: string;
}

export interface JupyterMessage {
  header: {
    msg_id: string;
    msg_type: string;
    session: string;
    username: string;
    date: string;
    version: string;
  };
  parent_header?: {
    msg_id: string;
    msg_type: string;
    session: string;
    username: string;
    date: string;
    version: string;
  };
  metadata: Record<string, unknown>;
  content: Record<string, unknown>;
  buffers?: unknown[];
  channel?: string;
  cell_id?: string;
}

// pixi.toml / environment.yml detection types now live in their
// respective hooks (`hooks/usePixiDetection.ts`,
// `hooks/useCondaDependencies.ts`). Both are derived from
// `RuntimeState.project_context` rather than Tauri commands.

// Pool state types removed — pool state now syncs via PoolDoc (Automerge).
// See apps/notebook/src/lib/pool-state.ts for the new types.
