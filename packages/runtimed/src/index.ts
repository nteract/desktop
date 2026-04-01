/**
 * runtimed — transport-agnostic notebook client library.
 *
 * Sync with the runtimed daemon from any JS runtime (Tauri, browser,
 * Node, Deno) without framework-specific dependencies.
 */

// Core
export { SyncEngine } from "./sync-engine";
export type { SyncEngineOptions, SyncEngineLogger } from "./sync-engine";

// Transport
export type { NotebookTransport, FrameListener } from "./transport";
export { FrameType, type FrameTypeValue } from "./transport";

// Handle
export type {
  SyncableHandle,
  FrameEvent,
  TextAttribution,
} from "./handle";

// Cell changeset
export {
  type CellChangeset,
  type ChangedCell,
  type ChangedFields,
  mergeChangesets,
} from "./cell-changeset";

// Runtime state
export {
  type CommDocEntry,
  DEFAULT_RUNTIME_STATE,
  type EnvState,
  type ExecutionState,
  type ExecutionTransition,
  type KernelState,
  type QueueEntry,
  type QueueState,
  type RuntimeState,
  type TrustState,
  diffExecutions,
} from "./runtime-state";

// Pool state
export {
  type PoolState,
  type RuntimePoolState,
  DEFAULT_POOL_STATE,
} from "./pool-state";

// Testing
export { DirectTransport } from "./direct-transport";
export type { ServerHandle } from "./direct-transport";
