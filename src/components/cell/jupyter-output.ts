/**
 * Jupyter output types based on the nbformat spec.
 *
 * Extracted to a standalone module so consumers can import the type without
 * pulling in the full OutputArea component (which brings in iframe-libraries,
 * heavy raw-string library imports, etc.).
 */
/**
 * Common fields on every nbformat output. `output_id` is a stable
 * daemon-stamped UUID — always non-empty on the daemon write path,
 * optional here only to tolerate render paths that build outputs
 * locally (e.g. markdown previews) without a backing manifest.
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
    })
  | (OutputCommon & {
      output_type: "stream";
      name: "stdout" | "stderr";
      text: string | string[];
    })
  | (OutputCommon & {
      output_type: "error";
      ename: string;
      evalue: string;
      traceback: string[];
    });
