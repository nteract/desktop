/**
 * Jupyter output types based on the nbformat spec.
 *
 * Extracted to a standalone module so consumers can import the type without
 * pulling in the full OutputArea component (which brings in iframe-libraries,
 * heavy raw-string library imports, etc.).
 */
export type JupyterOutput =
  | {
      output_type: "execute_result" | "display_data";
      data: Record<string, unknown>;
      metadata?: Record<string, unknown>;
      execution_count?: number | null;
    }
  | {
      output_type: "stream";
      name: "stdout" | "stderr";
      text: string | string[];
    }
  | {
      output_type: "error";
      ename: string;
      evalue: string;
      traceback: string[];
    };
