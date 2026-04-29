/**
 * Pool state types — matches the Rust `PoolDoc` schema in `notebook-doc/src/pool_state.rs`.
 *
 * The daemon writes pool stats to a global Automerge document. These types
 * represent the deserialized snapshot from the WASM `PoolDoc::read_state()`.
 */

/** State of a single runtime pool (UV, Conda, or Pixi). */
export interface RuntimePoolState {
  available: number;
  warming: number;
  pool_size: number;
  /** Human-readable error message (undefined if healthy). */
  error?: string;
  /** Package that failed to install (undefined if not identified). */
  failed_package?: string;
  /** Error classification: "timeout", "invalid_package", "import_error", "setup_failed". */
  error_kind?: string;
  /** Number of consecutive failures (0 if healthy). */
  consecutive_failures: number;
  /** Seconds until next retry (0 if retry is imminent or healthy). */
  retry_in_secs: number;
}

/** Full pool state snapshot from the PoolDoc. */
export interface PoolState {
  uv: RuntimePoolState;
  conda: RuntimePoolState;
  pixi: RuntimePoolState;
}

const DEFAULT_RUNTIME_POOL: RuntimePoolState = {
  available: 0,
  warming: 0,
  pool_size: 0,
  consecutive_failures: 0,
  retry_in_secs: 0,
};

export const DEFAULT_POOL_STATE: PoolState = {
  uv: { ...DEFAULT_RUNTIME_POOL },
  conda: { ...DEFAULT_RUNTIME_POOL },
  pixi: { ...DEFAULT_RUNTIME_POOL },
};
