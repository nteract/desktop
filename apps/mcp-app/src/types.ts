/** A single output from a cell execution */
export interface CellOutput {
  output_type: "stream" | "error" | "display_data" | "execute_result";
  /** Stream name: "stdout" or "stderr" */
  name?: string;
  /** Stream text content (inline string or blob URL) */
  text?: string;
  /** Error class name */
  ename?: string;
  /** Error message */
  evalue?: string;
  /** Traceback lines (array) or blob URL (string) */
  traceback?: string[] | string;
  /** MIME type → content map (inline string or blob URL) */
  data?: Record<string, string>;
  /** Execution count for execute_result */
  execution_count?: number;
}

/** A cell with its outputs */
export interface CellData {
  cell_id: string;
  cell_type: string;
  source: string;
  outputs: CellOutput[];
  execution_count: number | null;
  status: string;
}

/** The structuredContent shape from nteract MCP tools */
export interface NteractContent {
  cell?: CellData;
  cells?: CellData[];
  /** Daemon HTTP base URL for fetching blob data and plugins */
  blob_base_url?: string;
}
