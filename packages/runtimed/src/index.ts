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
export type { FrameListener, NotebookRequestOptions, NotebookTransport } from "./transport";
export {
  FrameType,
  sendAutomergeSyncFrame,
  sendPresenceFrame,
  type FrameTypeValue,
} from "./transport";

// Protocol contract
export {
  DISPLAY_CAPABLE_JUPYTER_OUTPUT_TYPES,
  INITIAL_LOAD_PHASES,
  NOTEBOOK_DOC_PHASES,
  NOTEBOOK_REQUEST_TYPES,
  NOTEBOOK_RESPONSE_RESULTS,
  RUNTIME_STATE_PHASES,
  SESSION_CONTROL_TYPES,
  isDisplayCapableJupyterOutput,
  isDisplayCapableJupyterOutputType,
  isInitialLoadFailed,
  isInitialLoadStreaming,
  type DisplayCapableJupyterOutput,
  type DisplayCapableJupyterOutputType,
  type SessionControlMessage,
} from "./protocol-contract";

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

// Text attribution events
export {
  createTextAttributionEvent,
  isTextAttributionEvent,
  TEXT_ATTRIBUTION_EVENT_TYPE,
  type TextAttributionEvent,
} from "./text-attribution-event";

// Cell changeset
export {
  classifyCellChangesetMaterialization,
  cellChangesetTouchesChrome,
  type CellPointerRefreshPlan,
  type CellChangeset,
  type CellChangesetMaterialization,
  type CellChangesetProjectionPlan,
  type ChangedCell,
  type ChangedFields,
  type IncrementalCellProjection,
  mergeChangesets,
  planCellPointerRefresh,
  planCellChangesetProjection,
  summarizeChangedFields,
} from "./cell-changeset";

// Execution projection
export {
  buildRuntimeExecutionSnapshot,
  collectExecutionOutputIds,
  collectOutputIds,
  executionFingerprint,
  extractOutputId,
  RuntimeExecutionProjector,
  type RuntimeExecutionProjection,
  type RuntimeExecutionSnapshot,
} from "./execution-projection";

// Runtime state
export {
  type CommDocEntry,
  DEFAULT_RUNTIME_STATE,
  type EnvState,
  type EnvProgressEnvType,
  type EnvProgressEvent,
  type EnvProgressPhase,
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
export { type CommBroadcast, isCommBroadcast, type KnownBroadcast } from "./broadcast-types";

// Env progress projection
export {
  EMPTY_ENV_PROGRESS,
  envProgressKey,
  type EnvProgressState,
  getEnvProgressStatusText,
  projectEnvProgress,
} from "./env-progress";

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
  deriveEnvironmentYml,
  deriveEnvManager,
  deriveEnvSyncState,
  deriveKernelInfo,
  derivePixiInfo,
  derivePyproject,
  deriveQueueState,
  deriveRuntimeKind,
  type EnvManager,
  type EnvManagerMetadataInputs,
  type EnvSyncDiff,
  type EnvSyncState,
  type EnvironmentYmlDeps,
  type EnvironmentYmlInfo,
  isKernelStatus,
  KERNEL_STATUS,
  type KernelInfo,
  type KernelStatus,
  lifecycleToLegacyStatus,
  type PixiInfo,
  type PyProjectDeps,
  type PyProjectInfo,
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
  DenoLaunchedConfig,
  EnvSource,
  GuardedNotebookProvenance,
  HistoryEntry,
  LaunchedEnvConfig,
  LaunchSpec,
  NotebookRequest,
  NotebookResponse,
  PackageManager,
  SaveErrorKind,
} from "./request-types";

// MIME priority
export { DEFAULT_MIME_PRIORITY } from "./mime-priority";

// Testing
export { DirectTransport } from "./direct-transport";
export type { ServerHandle } from "./direct-transport";
