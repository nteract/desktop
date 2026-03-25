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
  type RuntimeState,
  type KernelState,
  type QueueEntry,
  type QueueState,
  type EnvState,
  type TrustState,
  type ExecutionState,
  type ExecutionTransition,
  DEFAULT_RUNTIME_STATE,
  diffExecutions,
} from "./runtime-state";

// Testing
export { DirectTransport } from "./direct-transport";
export type { ServerHandle } from "./direct-transport";
