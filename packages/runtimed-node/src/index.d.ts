/** @runtimed/node — Node.js bindings and agent-friendly helpers. */

export const enum PackageManager {
  Uv = "uv",
  Conda = "conda",
  Pixi = "pixi",
}

export type Runtime = "python" | "deno" | string;
export type CellType = "code" | "markdown" | "raw";

export interface CreateNotebookOptions {
  runtime?: Runtime;
  workingDir?: string;
  socketPath?: string;
  peerLabel?: string;
  /** Human-readable alias that becomes a peer label if `peerLabel` is omitted. */
  description?: string;
  /** Packages to record before the first kernel launch. */
  dependencies?: string[];
  /** Alias for `dependencies`. */
  deps?: string[];
  /** Alias for `dependencies`. */
  packages?: string[];
  packageManager?: PackageManager;
}

export interface OpenNotebookOptions {
  socketPath?: string;
  peerLabel?: string;
}

export interface RunCellOptions {
  timeoutMs?: number;
  cellType?: CellType;
}

export interface RunOptions extends RunCellOptions {
  /** Install/sync these packages before running this cell. */
  dependencies?: string | string[];
  deps?: string | string[];
  packages?: string | string[];
  /** Defaults to true when dependencies are provided. */
  syncDependencies?: boolean;
  /** Return parsed/enriched outputs. Defaults to true for `run()`. */
  enrich?: boolean;
}

export interface QueueCellOptions {
  cellType?: CellType;
}

export interface WaitExecutionOptions {
  cellId?: string;
  timeoutMs?: number;
}

export interface WaitOptions extends WaitExecutionOptions {
  enrich?: boolean;
}

export interface GetExecutionResultOptions {
  socketPath?: string;
}

export interface QueuedExecution {
  cellId: string;
  executionId: string;
}

export interface JsOutput {
  outputType: string;
  name?: string;
  text?: string;
  /** MIME-keyed JSON. Binary values are base64 strings. */
  dataJson?: string;
  ename?: string;
  evalue?: string;
  traceback?: string[];
  executionCount?: number;
  blobUrlsJson?: string;
  blobPathsJson?: string;
}

export type DecodedMimeValue = string | Uint8Array | unknown;

export interface DecodedOutput extends JsOutput {
  data: Record<string, DecodedMimeValue>;
  blobUrls: Record<string, string>;
  blobPaths: Record<string, string>;
}

export interface CellResult {
  cellId: string;
  executionId: string;
  executionCount?: number;
  status: "done" | "error" | "timeout" | "kernel_error" | string;
  success: boolean;
  outputs: JsOutput[];
}

export interface EnrichedCellResult extends Omit<CellResult, "outputs"> {
  outputs: DecodedOutput[];
  /** Convenience concatenation of streams and text/plain rich results. */
  text: string;
  errors: DecodedOutput[];
  richData: Array<Record<string, DecodedMimeValue>>;
  ok: boolean;
}

export declare class Session {
  get notebookId(): string;
  saveNotebook(path?: string | null): Promise<void>;
  close(): Promise<void>;
  addUvDependency(pkg: string): Promise<void>;
  dependencyFingerprint(): Promise<string | null>;
  approveTrust(observedHeads?: string[] | null): Promise<void>;
  syncEnvironment(): Promise<void>;
  runCell(source: string, options?: RunCellOptions | null): Promise<CellResult>;
  queueCell(source: string, options?: QueueCellOptions | null): Promise<QueuedExecution>;
  waitForExecution(executionId: string, options?: WaitExecutionOptions | null): Promise<CellResult>;

  /** Add dependencies and hot-sync them by default. */
  install(dependencies: string | string[], options?: { sync?: boolean }): Promise<this>;
  /** Run a cell and return decoded outputs plus convenience fields. */
  run(source: string, options?: RunOptions): Promise<EnrichedCellResult>;
  /** Wait for queued work and return decoded outputs plus convenience fields. */
  wait(executionId: string, options?: WaitOptions): Promise<EnrichedCellResult>;
}

export function defaultSocketPath(): string;
export function socketPathForChannel(channel: "stable" | "nightly"): string;
export function createNotebook(options?: CreateNotebookOptions | null): Promise<Session>;
export function openNotebook(
  notebookId: string,
  options?: OpenNotebookOptions | null,
): Promise<Session>;
export function getExecutionResult(
  executionId: string,
  options?: GetExecutionResultOptions | null,
): Promise<CellResult>;

/** One-shot Python execution helper. Creates a notebook, runs code, closes by default. */
export function runPython(
  source: string,
  options?: RunOptions & {
    close?: boolean;
    create?: CreateNotebookOptions;
    packageManager?: PackageManager;
  },
): Promise<EnrichedCellResult>;

export function enhanceSession<T extends Session>(session: T): T;
export function enrichResult(result: CellResult): EnrichedCellResult;
export function outputToObject(output: JsOutput): DecodedOutput;
export function decodeMimeBundle(dataJson?: string): Record<string, DecodedMimeValue>;

export interface ParquetColumnInfo {
  name: string;
  dataType: string;
  nullCount: number;
  statsJson: string;
}
export interface ParquetRowPage {
  columns: string[];
  rows: string[][];
  totalRows: number;
  offset: number;
}
export interface ParquetSummaryResult {
  numRows: number;
  numBytes: number;
  columns: ParquetColumnInfo[];
}
export function readParquetFile(filePath: string, offset: number, limit: number): ParquetRowPage;
export function summarizeParquetFile(filePath: string): ParquetSummaryResult;
