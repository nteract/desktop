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
export type { SyncableHandle, FrameEvent, TextAttribution } from "./handle";

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
  getExecutionCountForCell,
} from "./runtime-state";

// Pool state
export {
  type PoolState,
  type RuntimePoolState,
  DEFAULT_POOL_STATE,
} from "./pool-state";

// Broadcast types
export {
  type CommBroadcast,
  type DisplayUpdateBroadcast,
  isCommBroadcast,
  isDisplayUpdateBroadcast,
  isKernelErrorBroadcast,
  isOutputBroadcast,
  isOutputsClearedBroadcast,
  isRuntimeStateSnapshotBroadcast,
  type KernelErrorBroadcast,
  type KnownBroadcast,
  type OutputBroadcast,
  type OutputsClearedBroadcast,
  type RuntimeStateSnapshotBroadcast,
} from "./broadcast-types";

// Comm diffing
export {
  type CommChanges,
  type CommDiffResult,
  type CommDiffState,
  detectOutputManifestHashes,
  detectUnresolvedOutputs,
  diffComms,
  isManifestHash,
  type OutputManifestHashes,
  type ResolvedComm,
  type UnresolvedOutputs,
} from "./comm-diff";

// Derived state
export {
  type DaemonQueueState,
  deriveEnvSyncState,
  deriveKernelInfo,
  deriveQueueState,
  type EnvSyncDiff,
  type EnvSyncState,
  isKernelStatus,
  KERNEL_STATUS,
  kernelStatus$,
  type KernelInfo,
  type KernelStatus,
  throttleBusyStatus,
} from "./derived-state";

// Notebook client
export { NotebookClient, type NotebookClientOptions } from "./notebook-client";
export type {
  CommRequestMessage,
  NotebookRequest,
  NotebookResponse,
} from "./request-types";

// MIME priority
export { DEFAULT_MIME_PRIORITY } from "./mime-priority";

// Testing
export { DirectTransport } from "./direct-transport";
export type { ServerHandle } from "./direct-transport";
