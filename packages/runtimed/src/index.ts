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
  InitialLoadPhase,
  NotebookDocPhase,
  RuntimeStatePhase,
  SessionStatus,
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
  KERNEL_ERROR_REASON,
  type KernelActivity,
  type KernelErrorReasonKey,
  type KernelState,
  type ProjectContext,
  type ProjectFile,
  type ProjectFileExtras,
  type ProjectFileKind,
  type ProjectFileParsed,
  type QueueEntry,
  type QueueState,
  type RuntimeLifecycle,
  type RuntimeState,
  type TrustState,
  type TrustStatus,
  diffExecutions,
  getExecutionCountForCell,
} from "./runtime-state";

// Pool state
export { type PoolState, type RuntimePoolState, DEFAULT_POOL_STATE } from "./pool-state";

// Broadcast types
export {
  type CommBroadcast,
  type EnvProgressBroadcast,
  isCommBroadcast,
  isEnvProgressBroadcast,
  type KnownBroadcast,
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
  deriveEnvManager,
  deriveEnvSyncState,
  deriveKernelInfo,
  deriveQueueState,
  deriveRuntimeKind,
  type EnvManager,
  type EnvManagerMetadataInputs,
  type EnvSyncDiff,
  type EnvSyncState,
  isKernelStatus,
  KERNEL_STATUS,
  type KernelInfo,
  type KernelStatus,
  lifecycleToLegacyStatus,
  RUNTIME_STATUS,
  runtimeStatusKey,
  type RuntimeKind,
  type RuntimeStatusKey,
  statusKeyToLegacyStatus,
} from "./derived-state";

// Notebook client
export { NotebookClient, type NotebookClientOptions, SaveNotebookError } from "./notebook-client";
export type {
  CommRequestMessage,
  CompletionItem,
  DependencyGuard,
  GuardedNotebookProvenance,
  HistoryEntry,
  NotebookRequest,
  NotebookResponse,
  SaveErrorKind,
} from "./request-types";

// MIME priority
export { DEFAULT_MIME_PRIORITY } from "./mime-priority";

// Testing
export { DirectTransport } from "./direct-transport";
export type { ServerHandle } from "./direct-transport";
