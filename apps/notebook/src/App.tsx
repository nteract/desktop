import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import {
  deriveEnvManager,
  deriveRuntimeKind,
  type DependencyGuard,
  type GuardedNotebookProvenance,
  NotebookClient,
  type SessionStatus,
} from "runtimed";
import { IsolationTest } from "@/components/isolated";
import { MediaProvider } from "@/components/outputs/media-provider";
import { getCrdtCommWriter, setCrdtCommWriter } from "@/components/widgets/crdt-comm-writer";
import {
  useWidgetStoreRequired,
  WidgetStoreProvider,
} from "@/components/widgets/widget-store-context";
import { WidgetUpdateManager } from "@/components/widgets/widget-update-manager";
import { WidgetView } from "@/components/widgets/widget-view";
import { useSyncedTheme } from "@/hooks/useSyncedSettings";
import { ErrorBoundary } from "@/lib/error-boundary";
import { CondaDependencyHeader } from "./components/CondaDependencyHeader";
import { type DaemonStatus, DaemonStatusBanner } from "./components/DaemonStatusBanner";
import { DebugBanner } from "./components/DebugBanner";
import { DenoDependencyHeader } from "./components/DenoDependencyHeader";
import { DependencyHeader } from "./components/DependencyHeader";
import { GlobalFindBar } from "./components/GlobalFindBar";
import { NotebookToolbar } from "./components/NotebookToolbar";
import { NotebookView } from "./components/NotebookView";
import { PixiDependencyHeader } from "./components/PixiDependencyHeader";
import { PoolErrorBanner } from "./components/PoolErrorBanner";
import { TrustDialog } from "./components/TrustDialog";
import {
  KernelLaunchErrorBanner,
  shouldShowKernelLaunchErrorBanner,
} from "./components/KernelLaunchErrorBanner";
import { UntrustedBanner } from "./components/UntrustedBanner";
import { PresenceProvider } from "./contexts/PresenceContext";
import { useAutomergeNotebook } from "./hooks/useAutomergeNotebook";
import { useCondaDependencies } from "./hooks/useCondaDependencies";
import { CrdtBridgeProvider } from "./hooks/useCrdtBridge";
import { useDaemonKernel } from "./hooks/useDaemonKernel";
import { useDenoConfig } from "./hooks/useDenoConfig";
import { type EnvSyncState, useDependencies } from "./hooks/useDependencies";
import { useEnvProgress } from "./hooks/useEnvProgress";
import { useDaemonInfo, useGitInfo } from "./hooks/useGitInfo";
import { useGlobalFind } from "./hooks/useGlobalFind";
import { resolveOutputValue } from "./hooks/useManifestResolver";
import { usePixiDetection } from "./hooks/usePixiDetection";
import { usePoolState } from "./hooks/usePoolState";
import { useTrust } from "./hooks/useTrust";
import { useUpdater } from "./hooks/useUpdater";
import { startAttributionDispatch } from "./lib/attribution-registry";
import { getBlobPort, useBlobPort } from "./lib/blob-port";
import { useRuntimeState } from "./lib/runtime-state";
import {
  flushCellUIState,
  setExecutingCellIds as storeSetExecutingCellIds,
  setFocusedCellId as storeSetFocusedCellId,
  setQueuedCellIds as storeSetQueuedCellIds,
  setSearchCurrentMatch as storeSetSearchCurrentMatch,
  setSearchQuery as storeSetSearchQuery,
} from "./lib/cell-ui-state";
import { startCursorDispatch } from "./lib/cursor-registry";
import { getTrustApprovalHandoffDisplayStatus, KERNEL_STATUS } from "./lib/kernel-status";
import { useObservable } from "./lib/use-observable";
import { logger } from "./lib/logger";
import { getNotebookCellsSnapshot } from "./lib/notebook-cells";
import { useDetectRuntime } from "./lib/notebook-metadata";
import { useNotebookHost } from "@nteract/notebook-host";
import { startWindowFocusHandler } from "./lib/window-focus";
import type { JupyterOutput } from "./types";

/** MIME bundle type for output data */
export type MimeBundle = Record<string, unknown>;

type PendingTrustAction =
  | {
      kind: "execute_cell";
      cellId: string;
      provenance: GuardedNotebookProvenance;
      dependencyFingerprint: string;
    }
  | {
      kind: "run_all";
      provenance: GuardedNotebookProvenance;
      dependencyFingerprint: string;
    }
  | { kind: "sync_deps"; provenance: DependencyGuard };

const EMPTY_DEPENDENCY_FINGERPRINT = "{}";

/**
 * Module-level reference for daemon comm sending.
 * Set by AppContent when daemon kernel is initialized.
 */
let daemonCommSender: ((message: unknown) => Promise<void>) | null = null;

/**
 * Update the daemon comm sender reference.
 * Called by AppContent when daemon kernel is initialized.
 */
export function setDaemonCommSender(sender: ((message: unknown) => Promise<void>) | null): void {
  daemonCommSender = sender;
}

/**
 * Send a message to the kernel's shell channel via daemon.
 * Used by the widget store for comm_msg/comm_open/comm_close.
 */
async function sendMessage(message: unknown): Promise<void> {
  try {
    if (daemonCommSender) {
      await daemonCommSender(message);
    } else {
      logger.debug("[widget] sendMessage called but daemon sender not ready");
    }
  } catch (e) {
    logger.error("[widget] send_comm_message failed:", e);
  }
}

// ── Output widget manifest resolution ─────────────────────────────────
// Generation counter per comm to discard stale async results.
const _outputResolveGen = new Map<string, number>();

/**
 * Resolve Output widget manifests and update the WidgetStore.
 *
 * When SyncEngine.commChanges$ emits a comm with `unresolvedOutputs`,
 * this function fetches + resolves the manifests asynchronously and
 * pushes the resolved outputs into the widget store.
 */
function resolveCommOutputs(
  commId: string,
  outputs: unknown[],
  store: import("@/components/widgets/widget-store").WidgetStore,
): void {
  const port = getBlobPort();
  if (port === null) return;

  const gen = (_outputResolveGen.get(commId) ?? 0) + 1;
  _outputResolveGen.set(commId, gen);

  void (async () => {
    const resolved = await Promise.all(outputs.map((o) => resolveOutputValue(o, port)));
    if (_outputResolveGen.get(commId) !== gen) return;

    const resolvedOutputs = resolved.filter((o): o is JupyterOutput => o !== null);
    if (resolvedOutputs.length > 0) {
      store.updateModel(commId, { outputs: resolvedOutputs });
    }
  })();
}

function AppContent() {
  const host = useNotebookHost();
  const gitInfo = useGitInfo();
  const daemonInfo = useDaemonInfo();

  // Apply theme to this window
  useSyncedTheme();

  // Stable peer ID for presence (generated once per window lifetime)
  const peerIdRef = useRef(crypto.randomUUID());

  // OS username for presence labels (injected by Tauri initialization_script)
  const peerLabel = (window as unknown as Record<string, string>).__NTERACT_USERNAME__ ?? "";

  // Start dispatching presence events to CodeMirror EditorViews
  useEffect(() => {
    return startCursorDispatch(peerIdRef.current);
  }, []);

  // Start dispatching text attribution events to CodeMirror EditorViews
  useEffect(() => {
    return startAttributionDispatch();
  }, []);

  // Re-establish CodeMirror input context on window reactivation.
  // Without this, WKWebView may drop the first few keystrokes after Cmd+Tab.
  useEffect(() => {
    return startWindowFocusHandler(host);
  }, [host]);

  const {
    cellIds,
    isLoading,
    loadError,
    focusedCellId,
    setFocusedCellId,
    addCell,
    moveCell,
    deleteCell,
    save,
    openNotebook,
    cloneNotebook,

    applyExecutionCountFromDaemon,
    setCellSourceHidden,
    setCellOutputsHidden,
    flushSync,
    getHandle,
    getEngine,
    sessionStatus$,
    triggerSync,
    localActor,
  } = useAutomergeNotebook();

  // Daemon sync status. Drives the kernel-action gate: until the daemon
  // has confirmed the first RuntimeStateSync round-trip (`runtime_state ==
  // "ready"`), trust state is unknown and kernel-modifying actions must
  // fail-closed. `sessionStatus$` is a ReplaySubject(1) on the engine;
  // `useObservable` seeds with `null` until the engine emits.
  const sessionStatus = useObservable<SessionStatus | null>(sessionStatus$, null);
  const sessionReady = sessionStatus?.runtime_state === "ready";

  // Global find (Cmd+F)
  const globalFind = useGlobalFind(cellIds);

  const [dependencyHeaderOpen, setDependencyHeaderOpen] = useState(false);
  const [showIsolationTest, setShowIsolationTest] = useState(false);
  const [trustDialogOpen, setTrustDialogOpen] = useState(false);
  const [pendingTrustAction, setPendingTrustAction] = useState<PendingTrustAction | null>(null);
  const pendingTrustActionRef = useRef<PendingTrustAction | null>(null);
  const [trustActionNotice, setTrustActionNotice] = useState<string | null>(null);
  const [trustApprovalHandoffPending, setTrustApprovalHandoffPending] = useState(false);
  const [clearingDeps, setClearingDeps] = useState(false);
  // Track when sync/restart just completed for success feedback
  const [justSynced, setJustSynced] = useState(false);
  // Per-error-instance dismissal for the kernel-launch error banner.
  // Stores the `errorDetails` string the user dismissed; cleared
  // whenever the kernel transitions out of Error (so the next failure
  // shows the banner fresh) or a different details string arrives.
  const [dismissedLaunchError, setDismissedLaunchError] = useState<string | null>(null);

  // Daemon startup status (installing, starting, failed, etc.)
  const [daemonStatus, setDaemonStatus] = useState<DaemonStatus>(null);
  // Track ready timeout so we can cancel it if status changes
  const readyTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Pool state - prewarm pool errors from daemon (typo'd default packages, etc.)
  const {
    uvError: poolUvError,
    condaError: poolCondaError,
    dismissUvError: dismissPoolUvError,
    dismissCondaError: dismissPoolCondaError,
  } = usePoolState();

  // Trust verification for notebook dependencies
  const {
    trustInfo,
    typosquatWarnings,
    loading: trustLoading,
    approvalError,
    needsApproval,
    checkTrust,
    approveTrust,
  } = useTrust();

  // Track pending kernel start that was blocked by trust dialog
  const pendingKernelStartRef = useRef(false);

  // Guard against concurrent Run All / Restart & Run All operations (#982)
  const runAllInFlightRef = useRef(false);

  // Guard against duplicate per-cell execute requests (rapid Shift+Enter)
  const executingCellsRef = useRef(new Set<string>());

  // Full runtime state from the daemon. Feeds the env-manager + runtime
  // derivers below and the path read further down. Single subscription
  // point; the derivers are pure and don't add re-renders.
  const runtimeState = useRuntimeState();

  // Notebook runtime type — reactive read from WASM Automerge doc.
  // Re-renders automatically when metadata changes (bootstrap, sync, writes).
  const detectedRuntime = useDetectRuntime();
  // Runtime hint from daemon:ready payload — available before metadata syncs,
  // prevents momentary flicker of wrong runtime UI (e.g. Python for Deno notebooks).
  const [runtimeHint, setRuntimeHint] = useState<string | null>(null);
  const runtime = deriveRuntimeKind(runtimeState, detectedRuntime, runtimeHint);

  // `true` when the room is in-memory only (untitled); reported by the daemon
  // via `daemon:ready`. Drives the always-dirty titlebar asterisk. Null until
  // the first ready event lands — treated conservatively as "unknown, assume
  // persisted" so we don't flash an asterisk on open-from-disk notebooks.
  const [ephemeral, setEphemeral] = useState<boolean | null>(null);

  // Canonical window-title base. Bootstrapped from the host on mount (Rust
  // sets it to the filename or "Untitled.ipynb" at window creation), then
  // updated on `path_changed` broadcasts. Used to compute the title asterisk
  // without a getTitle-then-setTitle round-trip that would race with the
  // concurrent Rust-side title update from `applyPathChanged`.
  const [titleBase, setTitleBase] = useState<string | null>(null);

  // Auto-clear justSynced after 3 seconds
  useEffect(() => {
    if (!justSynced) return;
    const timer = setTimeout(() => setJustSynced(false), 3000);
    return () => clearTimeout(timer);
  }, [justSynced]);

  // UV Dependency management
  const {
    dependencies,
    hasDependencies: hasUvDependencies,
    isUvConfigured,
    loading: depsLoading,
    addDependency,
    removeDependency,
    clearAllDependencies: clearAllUvDeps,
    pyprojectInfo,
    pyprojectDeps,
    importFromPyproject,
  } = useDependencies();

  // Conda Dependency management
  const {
    dependencies: condaDependencies,
    hasDependencies: hasCondaDependencies,
    isCondaConfigured,
    loading: condaDepsLoading,
    addDependency: addCondaDependency,
    removeDependency: removeCondaDependency,
    clearAllDependencies: clearAllCondaDeps,
    setChannels: setCondaChannels,
    setPython: setCondaPython,
    environmentYmlInfo,
    environmentYmlDeps,
  } = useCondaDependencies();

  // Pixi project detection
  const { pixiInfo } = usePixiDetection();

  // Deno config detection and settings
  const { denoConfigInfo, flexibleNpmImports, setFlexibleNpmImports } = useDenoConfig();

  // Get widget store for CRDT → WidgetStore projection.
  // Set the module-level ref so the updateManager can access it.
  const { store: widgetStore } = useWidgetStoreRequired();
  _widgetStoreRef = widgetStore;

  const handleExecutionCount = useCallback(
    (cellId: string, count: number) => {
      applyExecutionCountFromDaemon(cellId, count);
    },
    [applyExecutionCountFromDaemon],
  );

  // Execution completion is handled by the daemon queue via broadcast events
  const handleExecutionDone = useCallback((_cellId: string) => {
    // Daemon queue handles execution tracking via broadcasts
  }, []);

  // NotebookClient for sending kernel commands via transport. The host's
  // transport is the single instance shared with the SyncEngine in
  // useAutomergeNotebook — no more separate connection per consumer.
  const notebookClient = useMemo(() => new NotebookClient({ transport: host.transport }), [host]);

  // Daemon-owned kernel execution
  const {
    kernelStatus,
    statusKey,
    lifecycle,
    errorReason,
    errorDetails,
    kernelInfo,
    queueState,
    envSyncState,
    launchKernel,
    executeCell,
    executeCellGuarded,
    clearOutputs,
    interruptKernel,
    shutdownKernel,
    syncEnvironment,
    runAllCells: daemonRunAllCells,
    runAllCellsGuarded,
    sendCommMessage,
  } = useDaemonKernel({
    client: notebookClient,
    onExecutionCount: handleExecutionCount,
    onExecutionDone: handleExecutionDone,
  });

  // Derive values from daemon kernel
  const envSource = kernelInfo.envSource ?? null;

  useEffect(() => {
    if (!trustApprovalHandoffPending) return;
    if (
      kernelStatus !== KERNEL_STATUS.AWAITING_TRUST &&
      kernelStatus !== KERNEL_STATUS.NOT_STARTED
    ) {
      setTrustApprovalHandoffPending(false);
      return;
    }

    const timeout = setTimeout(() => setTrustApprovalHandoffPending(false), 10_000);
    return () => clearTimeout(timeout);
  }, [kernelStatus, trustApprovalHandoffPending]);

  const { kernelStatus: displayKernelStatus, statusKey: displayStatusKey } =
    getTrustApprovalHandoffDisplayStatus({
      pending: trustApprovalHandoffPending,
      kernelStatus,
      statusKey,
    });

  // Clear banner dismissal whenever the kernel leaves Error or the
  // details text changes, so the next failure re-presents the banner
  // even if the user dismissed an earlier identical-looking one.
  useEffect(() => {
    if (lifecycle.lifecycle !== "Error") {
      setDismissedLaunchError(null);
    }
  }, [lifecycle.lifecycle]);

  // Set up daemon comm sender for widget messages
  useEffect(() => {
    setDaemonCommSender(async (message: unknown) => {
      const msg = message as {
        header: { msg_type: string };
        content: Record<string, unknown>;
        buffers?: ArrayBuffer[];
      };
      await sendCommMessage(msg);
    });

    return () => {
      setDaemonCommSender(null);
    };
  }, [sendCommMessage]);

  // Set up CRDT comm writer for widget state updates.
  // Writes directly to RuntimeStateDoc via WASM — no SendComm round-trip.
  useEffect(() => {
    setCrdtCommWriter((commId: string, patch: Record<string, unknown>) => {
      const handle = getHandle();
      if (!handle) return;
      handle.set_comm_state_batch(commId, JSON.stringify(patch));
      triggerSync();
    });
    return () => {
      setCrdtCommWriter(null);
    };
  }, [getHandle, triggerSync]);

  // E2E-only bridge for driving widget updates through the real pipeline
  // (WidgetUpdateManager → debounced CRDT write → daemon → kernel) from
  // a WebDriver spec, without reaching into the security-isolated iframe.
  // Gated on `VITE_E2E` — `cargo xtask e2e build` sets it, production
  // builds don't, so these globals aren't exposed to end users.
  useEffect(() => {
    if (!import.meta.env.VITE_E2E) return;
    const w = window as unknown as Record<string, unknown>;
    w.__nteractWidgetUpdate = (commId: string, patch: Record<string, unknown>) => {
      updateManager.updateAndPersist(commId, patch);
    };
    w.__nteractWidgetStore = widgetStore;
    return () => {
      delete w.__nteractWidgetUpdate;
      delete w.__nteractWidgetStore;
    };
  }, [widgetStore]);

  // ── CRDT → WidgetStore projection via SyncEngine.commChanges$ ──────
  // Replaces the old Jupyter message synthesis path. The SyncEngine diffs
  // RuntimeStateDoc.comms, resolves ContentRefs via WASM, and emits
  // opened/updated/closed events. We drive the WidgetStore directly.
  useEffect(() => {
    const engine = getEngine();
    if (!engine) return;

    const commSub = engine.commChanges$.subscribe((changes) => {
      for (const comm of changes.opened) {
        widgetStore.createModel(comm.commId, comm.state);
        if (comm.unresolvedOutputs) {
          resolveCommOutputs(comm.commId, comm.unresolvedOutputs, widgetStore);
        }
      }
      for (const comm of changes.updated) {
        // Suppress CRDT echoes for keys with pending optimistic values
        // (e.g. slider being dragged — don't clobber with stale echo).
        const filtered = updateManager.shouldSuppressEcho(comm.commId, comm.state);
        if (filtered) {
          widgetStore.updateModel(comm.commId, filtered);
        }
        if (comm.unresolvedOutputs) {
          resolveCommOutputs(comm.commId, comm.unresolvedOutputs, widgetStore);
        }
      }
      for (const commId of changes.closed) {
        updateManager.clearComm(commId);
        widgetStore.deleteModel(commId);
      }
    });

    // Custom comm messages (buttons, model.send()) are ephemeral events
    // delivered via broadcast, not CRDT state. Route to WidgetStore.
    const customSub = engine.commBroadcasts$.subscribe((broadcast) => {
      const content = broadcast.content as Record<string, unknown> | undefined;
      const data = content?.data as Record<string, unknown> | undefined;
      if (data?.method === "custom") {
        const commId = content?.comm_id as string;
        const inner = (data?.content as Record<string, unknown>) ?? {};
        const buffers = (broadcast as { buffers?: number[][] }).buffers;
        const arrayBuffers = buffers?.map((arr: number[]) => new Uint8Array(arr).buffer);
        widgetStore.emitCustomMessage(commId, inner, arrayBuffers);
      }
    });

    return () => {
      commSub.unsubscribe();
      customSub.unsubscribe();
    };
  }, [getEngine, widgetStore]);

  // Reset the update manager when kernel restarts so fresh echoes
  // from the new session aren't suppressed by stale optimistic state.
  useEffect(() => {
    if (
      kernelStatus === KERNEL_STATUS.NOT_STARTED ||
      kernelStatus === KERNEL_STATUS.AWAITING_TRUST
    ) {
      updateManager.reset();
    }
  }, [kernelStatus]);

  // Re-project comms when blob_port changes (deferred comms retry).
  const blobPort = useBlobPort();
  useEffect(() => {
    if (blobPort !== null) {
      getEngine()?.reProjectComms();
    }
  }, [blobPort, getEngine]);

  // Split queue state into executing (currently running) and queued (waiting).
  const executingCellIds = new Set(queueState.executing ? [queueState.executing.cell_id] : []);
  const queuedCellIds = new Set(queueState.queued.map((e) => e.cell_id));

  // ── Sync transient UI state into the cell-ui-state store ────────────
  // Two-phase update for StrictMode safety:
  //
  // Phase 1 (render): Assign module-level variables so child
  // useSyncExternalStore snapshots return current values. Equality
  // guards make this idempotent — same inputs produce no mutation.
  //
  // Phase 2 (commit): useLayoutEffect calls flushCellUIState() to
  // notify subscribers. Discarded renders never trigger notifications.
  storeSetFocusedCellId(focusedCellId);
  storeSetExecutingCellIds(executingCellIds);
  storeSetQueuedCellIds(queuedCellIds);
  storeSetSearchQuery(globalFind.query);
  storeSetSearchCurrentMatch(globalFind.currentMatch);

  useLayoutEffect(() => {
    flushCellUIState();
  });

  // Env manager (uv / conda / pixi) — drives the toolbar badge and
  // which dep header renders. Derived from RuntimeState + WASM-metadata
  // signals in priority order: running kernel's env_source, inline
  // deps, detected project file. See `deriveEnvManager` in runtimed.
  const envType = deriveEnvManager(runtimeState, {
    isUvConfigured,
    isCondaConfigured,
    environmentYmlHasDeps: Boolean(environmentYmlInfo?.has_dependencies),
    pixiHasDeps: Boolean(pixiInfo?.has_dependencies || pixiInfo?.has_pypi_dependencies),
  });

  // Pre-start hint for the env badge: same answer as envType when the
  // daemon hasn't yet reported env_source (kernel still launching).
  // Once envSource is set, toolbar renders the real label off it.
  const envTypeHint = envSource ? null : envType;

  // Auto-updater
  const {
    status: updateStatus,
    version: updateVersion,
    checkForUpdate,
    restartToUpdate,
  } = useUpdater();

  // Environment preparation progress
  const envProgress = useEnvProgress();

  // Reset progress error when dependencies change (allows retry after fixing issues)
  const progressError = envProgress.error;
  const progressReset = envProgress.reset;
  useEffect(() => {
    if (envSyncState && !envSyncState.inSync && progressError) {
      progressReset();
    }
  }, [envSyncState, progressError, progressReset]);

  // Derive sync state from daemon's envSyncState for inline environments
  // Also shows for prewarmed kernels when user adds inline deps (prewarmed->inline drift)
  const uvDerivedSyncState: EnvSyncState | null = useMemo(() => {
    // Show for uv:inline or uv:prewarmed (when user adds deps to prewarmed kernel)
    const isUvEnv = envSource === "uv:inline" || envSource === "uv:prewarmed" || !envSource;
    if (!isUvEnv || !envSyncState) return null;
    // Only show dirty state for prewarmed if there's actually a diff with UV deps
    if (envSource === "uv:prewarmed" && !envSyncState.diff?.added?.length) return null;
    if (envSyncState.inSync) return { status: "synced" };
    return {
      status: "dirty",
      added: envSyncState.diff?.added ?? [],
      removed: envSyncState.diff?.removed ?? [],
    };
  }, [envSource, envSyncState]);

  const condaDerivedSyncState: EnvSyncState | null = useMemo(() => {
    // Show for conda:inline or conda:prewarmed (when user adds deps to prewarmed kernel)
    const isCondaEnv = envSource === "conda:inline" || envSource === "conda:prewarmed";
    if (!isCondaEnv || !envSyncState) return null;
    // Only show dirty state for prewarmed if there's actually a diff with conda deps
    if (envSource === "conda:prewarmed" && !envSyncState.diff?.added?.length) return null;
    if (envSyncState.inSync) return { status: "synced" };
    return {
      status: "dirty",
      added: envSyncState.diff?.added ?? [],
      removed: envSyncState.diff?.removed ?? [],
    };
  }, [envSource, envSyncState]);

  const pixiDerivedSyncState: EnvSyncState | null = useMemo(() => {
    const isPixiEnv = envSource?.startsWith("pixi:");
    if (!isPixiEnv || !envSyncState) return null;
    if (envSource === "pixi:prewarmed" && !envSyncState.diff?.added?.length) return null;
    if (envSyncState.inSync) return { status: "synced" };
    return {
      status: "dirty",
      added: envSyncState.diff?.added ?? [],
      removed: envSyncState.diff?.removed ?? [],
    };
  }, [envSource, envSyncState]);

  // Derive sync state for Deno kernels
  const denoDerivedSyncState: {
    status: "synced" | "dirty";
  } | null = useMemo(() => {
    // Only show for Deno kernels (env_source is "deno")
    if (envSource !== "deno" || !envSyncState) return null;
    // Check if deno config has drifted
    if (envSyncState.inSync) return { status: "synced" };
    if (envSyncState.diff?.denoChanged) return { status: "dirty" };
    return null;
  }, [envSource, envSyncState]);

  const setBlockedTrustAction = useCallback((action: PendingTrustAction | null) => {
    pendingTrustActionRef.current = action;
    setPendingTrustAction(action);
  }, []);

  const captureNotebookProvenance = useCallback((): GuardedNotebookProvenance | null => {
    const observedHeads = getHandle()?.get_heads_hex() ?? [];
    if (observedHeads.length === 0) return null;
    return { observed_heads: observedHeads };
  }, [getHandle]);

  const captureDependencyFingerprint = useCallback((): string => {
    return getHandle()?.get_dependency_fingerprint() ?? EMPTY_DEPENDENCY_FINGERPRINT;
  }, [getHandle]);

  const captureExecuteTrustAction = useCallback(
    (cellId: string): PendingTrustAction | null => {
      const cell = getNotebookCellsSnapshot().find((c) => c.id === cellId);
      if (!cell || cell.cell_type !== "code") return null;
      const provenance = captureNotebookProvenance();
      if (!provenance) return null;
      return {
        kind: "execute_cell",
        cellId: cell.id,
        provenance,
        dependencyFingerprint: captureDependencyFingerprint(),
      };
    },
    [captureDependencyFingerprint, captureNotebookProvenance],
  );

  const captureRunAllTrustAction = useCallback((): PendingTrustAction | null => {
    const provenance = captureNotebookProvenance();
    if (!provenance) return null;
    return {
      kind: "run_all",
      provenance,
      dependencyFingerprint: captureDependencyFingerprint(),
    };
  }, [captureDependencyFingerprint, captureNotebookProvenance]);

  const captureSyncTrustAction = useCallback((): PendingTrustAction | null => {
    const provenance = captureNotebookProvenance();
    if (!provenance) return null;
    return {
      kind: "sync_deps",
      provenance: {
        observed_heads: provenance.observed_heads,
        dependency_fingerprint: captureDependencyFingerprint(),
      },
    };
  }, [captureDependencyFingerprint, captureNotebookProvenance]);

  const pendingActionDependencyFingerprint = useCallback(
    (action: PendingTrustAction | null): string | undefined => {
      if (!action) return undefined;
      return action.kind === "sync_deps"
        ? action.provenance.dependency_fingerprint
        : action.dependencyFingerprint;
    },
    [],
  );

  // Check trust and start kernel if trusted, otherwise show dialog.
  // Returns true if kernel was started, false if trust dialog opened or error.
  const tryStartKernel = useCallback(
    async (blockedAction: PendingTrustAction | null = null): Promise<boolean> => {
      // Fail-closed until the daemon has confirmed first RuntimeStateSync.
      // Before that, `runtimeState.trust.status` is the default and cannot
      // gate an install. Buttons are disabled in this state, but keyboard
      // shortcuts and menu routes still call in — so guard here too.
      if (!sessionReady) {
        logger.debug("[App] tryStartKernel: session not ready, skipping");
        return false;
      }

      // Re-check trust status (may have changed)
      const info = await checkTrust();
      if (!info) return false;

      if (info.status === "trusted" || info.status === "no_dependencies") {
        setBlockedTrustAction(null);
        // Trusted - launch kernel via daemon
        // Both kernel_type and env_source use "auto" - daemon detects from Automerge doc
        const response = await launchKernel("auto", "auto");
        if (response.result === "error") {
          logger.error("[App] tryStartKernel: daemon error", response.error);
          return false;
        }
        if (response.result === "guard_rejected") {
          setTrustActionNotice(response.reason);
          return false;
        }
        return true;
      }
      // Untrusted - show dialog and mark pending start
      pendingKernelStartRef.current = true;
      setBlockedTrustAction(blockedAction);
      setTrustDialogOpen(true);
      return false;
    },
    [sessionReady, checkTrust, launchKernel, setBlockedTrustAction, setTrustActionNotice],
  );

  const performTrustedSyncDeps = useCallback(
    async (guard?: DependencyGuard): Promise<boolean> => {
      // For UV or Conda inline deps with only additions, try hot-sync first
      const isUvInline = envSource === "uv:inline";
      const isCondaInline = envSource === "conda:inline";
      const hasOnlyAdditions =
        envSyncState?.diff?.added?.length && !envSyncState?.diff?.removed?.length;

      if ((isUvInline || isCondaInline) && hasOnlyAdditions) {
        logger.debug("[App] Trying hot-sync for additions");
        const response = await syncEnvironment(guard);

        if (response.result === "sync_environment_complete") {
          logger.debug("[App] Hot-sync succeeded:", response.synced_packages);
          envProgress.reset();
          setJustSynced(true);
          return true;
        }

        if (response.result === "guard_rejected") {
          setTrustActionNotice(response.reason);
          envProgress.reset();
          return false;
        }

        if (response.result === "sync_environment_failed" && !response.needs_restart) {
          // Error but doesn't need restart (e.g., install failed)
          logger.error("[App] Hot-sync failed:", {
            error: response.error,
            envSource,
            packages: envSyncState?.diff?.added,
          });
          envProgress.reset();
          return false;
        }

        // needs_restart or other error - fall through to restart flow
        logger.debug("[App] Hot-sync requires restart, falling back");
      }

      // Restart flow - deps are already trusted from check above
      await shutdownKernel();
      const started = await tryStartKernel();
      if (started) {
        envProgress.reset();
        setJustSynced(true);
      }
      return started;
    },
    [envSource, envSyncState, envProgress, syncEnvironment, shutdownKernel, tryStartKernel],
  );

  // Handler to sync deps - tries hot-sync for UV additions, falls back to restart
  // Always checks trust before any operation that installs packages
  const handleSyncDeps = useCallback(async (): Promise<boolean> => {
    // Fail-closed until the daemon has pushed initial RuntimeStateSync.
    // Hot-sync and restart both install packages; can't run either if we
    // don't have authoritative trust state yet.
    if (!sessionReady) {
      logger.debug("[App] handleSyncDeps: session not ready, skipping");
      return false;
    }

    // Reset any previous error state before attempting
    envProgress.reset();

    await flushSync();
    const blockedAction = captureSyncTrustAction();

    // Check trust first - required before any package installation (hot-sync or restart)
    const info = await checkTrust();
    if (!info) return false;

    if (info.status !== "trusted" && info.status !== "no_dependencies") {
      // Untrusted - show dialog, let user approve before any installation
      pendingKernelStartRef.current = true;
      setBlockedTrustAction(blockedAction);
      setTrustDialogOpen(true);
      return false;
    }

    // Trusted - proceed with sync/restart
    return performTrustedSyncDeps();
  }, [
    sessionReady,
    captureSyncTrustAction,
    checkTrust,
    envProgress,
    flushSync,
    performTrustedSyncDeps,
    setBlockedTrustAction,
  ]);

  // Restart and run all cells
  const restartAndRunAll = useCallback(async () => {
    // Same fail-closed reasoning as handleRestartKernel: don't shut
    // down before first RuntimeStateSync.
    if (!sessionReady) {
      logger.debug("[App] restartAndRunAll: session not ready, skipping");
      return;
    }
    if (runAllInFlightRef.current) {
      logger.debug("[App] restartAndRunAll: already in flight, skipping");
      return;
    }
    runAllInFlightRef.current = true;
    try {
      // Flush pending source sync so daemon has latest code
      await flushSync();

      // Shutdown existing kernel
      await shutdownKernel();

      // Start kernel - returns false if not started (e.g., trust dialog)
      const kernelStarted = await tryStartKernel(captureRunAllTrustAction());
      if (!kernelStarted) {
        logger.debug("[App] restartAndRunAll: kernel not started, skipping");
        return;
      }

      // Daemon reads cell sources from Automerge doc and queues them
      const response = await daemonRunAllCells();
      if (response.result === "error") {
        logger.error("[App] restartAndRunAll: daemon error", response.error);
      } else if (response.result === "no_kernel") {
        logger.warn("[App] restartAndRunAll: no kernel available");
      }
    } finally {
      runAllInFlightRef.current = false;
    }
  }, [
    sessionReady,
    flushSync,
    shutdownKernel,
    tryStartKernel,
    captureRunAllTrustAction,
    daemonRunAllCells,
  ]);

  const runTrustApprovedAction = useCallback(
    async (action: PendingTrustAction | null) => {
      if (!action) return;

      if (action.kind === "execute_cell") {
        const response = await executeCellGuarded(action.cellId, action.provenance);
        if (response.result === "guard_rejected") {
          setTrustApprovalHandoffPending(false);
          setTrustActionNotice(response.reason);
        } else if (response.result === "error") {
          logger.error("[App] guarded execute after trust approval failed:", response.error);
          setTrustApprovalHandoffPending(false);
          setTrustActionNotice(response.error);
        } else if (response.result === "no_kernel") {
          setTrustApprovalHandoffPending(false);
          setTrustActionNotice("Kernel was not ready. Run the cell again when startup finishes.");
        }
        return;
      }

      if (action.kind === "sync_deps") {
        await performTrustedSyncDeps(action.provenance);
        return;
      }

      const response = await runAllCellsGuarded(action.provenance);
      if (response.result === "guard_rejected") {
        setTrustApprovalHandoffPending(false);
        setTrustActionNotice(response.reason);
      } else if (response.result === "error") {
        logger.error("[App] guarded Run All after trust approval failed:", response.error);
        setTrustApprovalHandoffPending(false);
        setTrustActionNotice(response.error);
      } else if (response.result === "no_kernel") {
        setTrustApprovalHandoffPending(false);
        setTrustActionNotice("Kernel was not ready. Run all cells again when startup finishes.");
      }
    },
    [executeCellGuarded, performTrustedSyncDeps, runAllCellsGuarded],
  );

  const handleTrustApprovedLaunch = useCallback(
    async (action: PendingTrustAction | null) => {
      if (!sessionReady) {
        logger.debug("[App] handleTrustApprovedLaunch: session not ready, skipping");
        return;
      }
      try {
        const response = await launchKernel("auto", "auto");
        if (response.result === "error") {
          logger.error("[App] kernel launch after trust approval failed:", response.error);
          setTrustApprovalHandoffPending(false);
          setTrustActionNotice(response.error);
          return;
        }
        if (response.result === "guard_rejected") {
          setTrustApprovalHandoffPending(false);
          setTrustActionNotice(response.reason);
          return;
        }
        await runTrustApprovedAction(action);
      } catch (e) {
        logger.error("[App] kernel launch after trust approval failed:", e);
        setTrustApprovalHandoffPending(false);
        setTrustActionNotice(e instanceof Error ? e.message : String(e));
      }
    },
    [sessionReady, launchKernel, runTrustApprovedAction, setTrustActionNotice],
  );

  // Handle trust approval from dialog
  const handleTrustApprove = useCallback(async () => {
    const action = pendingTrustActionRef.current;
    const dependencyFingerprint = pendingActionDependencyFingerprint(action);
    const success = await approveTrust(
      dependencyFingerprint ? { dependencyFingerprint } : undefined,
    );
    if (success && pendingKernelStartRef.current) {
      pendingKernelStartRef.current = false;
      setTrustApprovalHandoffPending(true);
      setBlockedTrustAction(null);
      if (action?.kind === "sync_deps") {
        void runTrustApprovedAction(action);
      } else {
        void handleTrustApprovedLaunch(action);
      }
    }
    return success;
  }, [
    approveTrust,
    handleTrustApprovedLaunch,
    pendingActionDependencyFingerprint,
    runTrustApprovedAction,
    setBlockedTrustAction,
  ]);

  const handleTrustApproveOnly = useCallback(async () => {
    const action = pendingTrustActionRef.current;
    const dependencyFingerprint = pendingActionDependencyFingerprint(action);
    const success = await approveTrust(
      dependencyFingerprint ? { dependencyFingerprint } : undefined,
    );
    if (success && pendingKernelStartRef.current) {
      pendingKernelStartRef.current = false;
      setTrustApprovalHandoffPending(true);
      setBlockedTrustAction(null);
      if (action?.kind !== "sync_deps") {
        void handleTrustApprovedLaunch(null);
      }
    }
    return success;
  }, [
    approveTrust,
    handleTrustApprovedLaunch,
    pendingActionDependencyFingerprint,
    setBlockedTrustAction,
  ]);

  // Handle trust decline from dialog
  const handleTrustDecline = useCallback(() => {
    pendingKernelStartRef.current = false;
    setTrustApprovalHandoffPending(false);
    setBlockedTrustAction(null);
    // User declined - don't start kernel, just close dialog
  }, [setBlockedTrustAction]);

  const handleTrustDialogOpenChange = useCallback(
    (open: boolean) => {
      setTrustDialogOpen(open);
      if (!open) {
        pendingKernelStartRef.current = false;
        setBlockedTrustAction(null);
      }
    },
    [setBlockedTrustAction],
  );

  // Start kernel explicitly with pyproject.toml (user action from DependencyHeader)
  const handleStartKernelWithPyproject = useCallback(async () => {
    if (!sessionReady) {
      logger.debug("[App] handleStartKernelWithPyproject: session not ready, skipping");
      return;
    }
    const response = await launchKernel("python", "uv:pyproject");
    if (response.result === "error") {
      logger.error("[App] handleStartKernelWithPyproject: daemon error", response.error);
    } else if (response.result === "guard_rejected") {
      setTrustActionNotice(response.reason);
    }
  }, [sessionReady, launchKernel, setTrustActionNotice]);

  const handleExecuteCell = useCallback(
    async (cellId: string) => {
      // Fail-closed until the daemon has confirmed first RuntimeStateSync.
      // If a runtime agent is already alive from a prior session,
      // `execute_cell` would otherwise queue into RuntimeStateDoc before
      // we've verified trust (see crates/runtimed/src/requests/execute_cell.rs).
      if (!sessionReady) {
        logger.debug("[App] handleExecuteCell: session not ready, skipping");
        return;
      }

      // Resolve cell up front before awaiting sync operations.
      const cell = getNotebookCellsSnapshot().find((c) => c.id === cellId);
      if (!cell || cell.cell_type !== "code") return;

      // Dedup guard: skip if this cell already has an execute in flight.
      if (executingCellsRef.current.has(cellId)) {
        logger.debug("[App] handleExecuteCell: already in flight for", cellId);
        return;
      }
      executingCellsRef.current.add(cellId);

      try {
        // Flush pending source sync so daemon has latest code before executing.
        // flushAndWait() guarantees any in-flight debounced flush has landed,
        // then sends remaining changes and awaits delivery.
        await flushSync();

        // No explicit ClearOutputs IPC needed — the daemon clears outputs
        // on execute_input and the SyncEngine injects a clear changeset
        // when the RuntimeStateDoc reports execution started.

        // Start kernel via daemon if not running or awaiting trust, then queue cell.
        if (
          kernelStatus === KERNEL_STATUS.NOT_STARTED ||
          kernelStatus === KERNEL_STATUS.AWAITING_TRUST
        ) {
          const started = await tryStartKernel(captureExecuteTrustAction(cellId));
          // Only block execution when trust approval is pending.
          // For startup races (e.g. daemon already auto-starting), still try execute.
          if (!started && pendingKernelStartRef.current) return;
        }
        const response = await executeCell(cellId);
        if (response.result === "error") {
          logger.error("[App] handleExecuteCell: daemon error", response.error);
        } else if (response.result === "no_kernel") {
          // Kernel died — try to restart and retry once.
          logger.warn("[App] handleExecuteCell: no kernel, attempting restart");
          const restarted = await tryStartKernel();
          if (restarted) {
            const retry = await executeCell(cellId);
            if (retry.result === "error") {
              logger.error("[App] handleExecuteCell: daemon error after restart", retry.error);
            } else if (retry.result === "no_kernel") {
              logger.error("[App] handleExecuteCell: still no kernel after restart");
            }
          }
        }
      } finally {
        // Brief guard to absorb accidental double-taps. The daemon
        // queues correctly either way, so this only needs to catch
        // the sub-150ms "same keypress fired twice" case.
        setTimeout(() => {
          executingCellsRef.current.delete(cellId);
        }, 150);
      }
    },
    [sessionReady, flushSync, kernelStatus, tryStartKernel, captureExecuteTrustAction, executeCell],
  );

  const handleAddCell = useCallback(
    (type: "code" | "markdown" | "raw", afterCellId?: string | null) => {
      addCell(type, afterCellId);
    },
    [addCell],
  );

  // Wrapper for toolbar's start kernel - uses trust check before starting
  const handleStartKernel = useCallback(
    async (_name: string) => {
      await tryStartKernel();
    },
    [tryStartKernel],
  );

  // Restart kernel (shutdown then start)
  const handleRestartKernel = useCallback(async () => {
    // Fail-closed until first RuntimeStateSync. Shutdown writes
    // RuntimeLifecycle::Shutdown via the runtime-agent request path, so
    // firing it against a still-syncing session would mutate kernel
    // state before we've seen the authoritative snapshot — and the
    // follow-up tryStartKernel would then no-op, leaving the kernel
    // stopped.
    if (!sessionReady) {
      logger.debug("[App] handleRestartKernel: session not ready, skipping");
      return;
    }
    await shutdownKernel();
    await tryStartKernel();
  }, [sessionReady, shutdownKernel, tryStartKernel]);

  const handleRunAllCells = useCallback(async () => {
    // Fail-closed until first RuntimeStateSync. Same reasoning as
    // handleExecuteCell: if a runtime agent is already alive, the daemon
    // would queue into RuntimeStateDoc before we've verified trust.
    if (!sessionReady) {
      logger.debug("[App] handleRunAllCells: session not ready, skipping");
      return;
    }

    if (runAllInFlightRef.current) {
      logger.debug("[App] handleRunAllCells: already in flight, skipping");
      return;
    }
    runAllInFlightRef.current = true;
    try {
      // Flush pending source sync so daemon has latest code
      await flushSync();

      // Start kernel via daemon if not running or awaiting trust
      if (
        kernelStatus === KERNEL_STATUS.NOT_STARTED ||
        kernelStatus === KERNEL_STATUS.AWAITING_TRUST
      ) {
        const started = await tryStartKernel(captureRunAllTrustAction());
        if (!started) {
          logger.debug("[App] handleRunAllCells: kernel not started, skipping");
          return;
        }
      }

      // Daemon reads cell sources from Automerge doc and queues them
      const response = await daemonRunAllCells();
      if (response.result === "error") {
        logger.error("[App] handleRunAllCells: daemon error", response.error);
      } else if (response.result === "no_kernel") {
        logger.warn("[App] handleRunAllCells: no kernel available");
      }
    } finally {
      runAllInFlightRef.current = false;
    }
  }, [
    sessionReady,
    kernelStatus,
    tryStartKernel,
    captureRunAllTrustAction,
    flushSync,
    daemonRunAllCells,
  ]);

  const handleRestartAndRunAll = useCallback(async () => {
    // Backend clears outputs and emits cells:outputs_cleared before queuing,
    // then ensureKernelStarted restarts the kernel
    await restartAndRunAll();
  }, [restartAndRunAll]);

  // Cmd+S keyboard shortcut. The native menu item is routed through
  // host.commands.run("notebook.save") by the Tauri menu bridge.
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "s") {
        e.preventDefault();
        save();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [save]);

  // Derive a filename + ephemeral flag from a path (or its absence). Shared
  // between the mount-time `getReadyInfo` pull, the `daemon:ready` event, and
  // the `path_changed` broadcast. Keeping all three paths identical prevents
  // "one of them forgot to update titleBase" bugs.
  const applyNotebookPath = useCallback((path: string | null | undefined) => {
    if (path) {
      const parts = path.split(/[\\/]/);
      setTitleBase(parts[parts.length - 1] || "Untitled.ipynb");
      setEphemeral(false);
    } else {
      setTitleBase("Untitled.ipynb");
      setEphemeral(true);
    }
  }, []);

  // Path transitions are driven by `RuntimeStateDoc.path` (frame 0x05).
  // A non-null value means the room is file-backed; a null value (only
  // really at first mount before sync catches up) keeps the room in
  // untitled state. The runtime state hook returns the same default
  // until the daemon's first state sync arrives, so this effect is a
  // straight projection.
  const runtimePath = runtimeState.path;
  useEffect(() => {
    applyNotebookPath(runtimePath);
  }, [applyNotebookPath, runtimePath]);

  // Title is purely a function of `ephemeral`. Untitled notebooks get
  // the `*` prefix; saved notebooks render their filename. Autosave
  // (2s quiet, 10s max) keeps the file current within seconds of any
  // edit, so the file-backed case never shows an unsaved-changes dot.
  useEffect(() => {
    if (titleBase == null) return;
    const next = ephemeral === true ? `* ${titleBase}` : titleBase;
    host.window.setTitle(next).catch(() => {
      // Window may have been closed mid-render.
    });
  }, [host, ephemeral, titleBase]);

  // Cmd+F to open global find
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "f") {
        e.preventDefault();
        globalFind.open();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [globalFind.open]);

  // Cmd+O keyboard shortcut. Menu item routes through
  // host.commands.run("notebook.open") via the Tauri menu bridge.
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "o") {
        e.preventDefault();
        openNotebook();
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, [openNotebook]);

  // Route all notebook-level commands to their latest implementations via
  // a single ref. The ref is updated every render; the host-level registration
  // below runs only once per host, so a native menu event that lands during
  // a state-driven re-render never finds the slot empty — the previous
  // design re-registered every command on focusedCellId change, which
  // opened a "no handler" window any menu click could fall into.
  const commandHandlersRef = useRef({
    save,
    openNotebook,
    cloneNotebook,
    handleAddCell,
    focusedCellId,
    clearOutputs,
    handleRunAllCells,
    handleRestartAndRunAll,
    checkForUpdate,
  });
  commandHandlersRef.current = {
    save,
    openNotebook,
    cloneNotebook,
    handleAddCell,
    focusedCellId,
    clearOutputs,
    handleRunAllCells,
    handleRestartAndRunAll,
    checkForUpdate,
  };

  useEffect(() => {
    const disposables = [
      host.commands.register("notebook.save", () => {
        commandHandlersRef.current.save();
      }),
      host.commands.register("notebook.open", () => {
        commandHandlersRef.current.openNotebook();
      }),
      host.commands.register("notebook.clone", () => {
        commandHandlersRef.current.cloneNotebook();
      }),
      host.commands.register("notebook.insertCell", ({ type }) => {
        const h = commandHandlersRef.current;
        h.handleAddCell(type, h.focusedCellId);
      }),
      host.commands.register("notebook.clearOutputs", async () => {
        const h = commandHandlersRef.current;
        if (!h.focusedCellId) return;
        const cell = getNotebookCellsSnapshot().find((c) => c.id === h.focusedCellId);
        if (!cell || cell.cell_type !== "code") return;
        await h.clearOutputs(h.focusedCellId);
      }),
      host.commands.register("notebook.clearAllOutputs", async () => {
        const h = commandHandlersRef.current;
        const codeCells = getNotebookCellsSnapshot().filter((c) => c.cell_type === "code");
        await Promise.all(codeCells.map((cell) => h.clearOutputs(cell.id)));
      }),
      host.commands.register("notebook.runAll", () => {
        commandHandlersRef.current.handleRunAllCells();
      }),
      host.commands.register("notebook.restartAndRunAll", () => {
        commandHandlersRef.current.handleRestartAndRunAll();
      }),
      host.commands.register("updater.check", () => {
        commandHandlersRef.current.checkForUpdate();
      }),
    ];
    return () => disposables.forEach((d) => d());
  }, [host]);

  // Listen for daemon startup progress events
  useEffect(() => {
    // Helper to cancel any pending ready timeout
    const cancelReadyTimeout = () => {
      if (readyTimeoutRef.current) {
        clearTimeout(readyTimeoutRef.current);
        readyTimeoutRef.current = null;
      }
    };

    const unlistenProgress = host.daemonEvents.onProgress((payload) => {
      const status = payload as DaemonStatus;

      // Cancel any pending ready timeout before setting new status
      cancelReadyTimeout();
      setDaemonStatus(status);

      // Clear status after a short delay when daemon is ready
      if (status?.status === "ready") {
        readyTimeoutRef.current = setTimeout(() => {
          // Only clear if still in ready state (use functional update)
          setDaemonStatus((prev) => (prev?.status === "ready" ? null : prev));
          readyTimeoutRef.current = null;
        }, 1000);
      }
    });

    // Listen for daemon disconnection (mid-session)
    const unlistenDisconnect = host.daemonEvents.onDisconnected(() => {
      cancelReadyTimeout();
      setDaemonStatus({
        status: "failed",
        error: "Runtime disconnected. Attempting to reconnect...",
      });
    });

    // Listen for daemon unavailable (startup failure, fires after sync timeout)
    const unlistenUnavailable = host.daemonEvents.onUnavailable((payload) => {
      cancelReadyTimeout();
      setDaemonStatus({
        status: "failed",
        error: `${payload.message} ${payload.guidance}`,
      });
    });

    // Shared handler for both the live event and the cached backfill below.
    // Factored out so the two paths can never drift.
    const handleReady = (
      payload:
        | {
            runtime?: string;
            ephemeral?: boolean;
            notebook_path?: string | null;
          }
        | null
        | undefined,
    ) => {
      cancelReadyTimeout();
      setDaemonStatus(null);
      // Set or clear the runtime hint — clearing prevents stale hints
      // when a window is reused to open a different notebook (Open path
      // sends runtime: null).
      setRuntimeHint(payload?.runtime ?? null);
      // Sync titlebar: derive filename + ephemeral from the path carried
      // on the ready payload.
      if (payload) {
        if (typeof payload.ephemeral === "boolean") {
          applyNotebookPath(payload.ephemeral ? null : (payload.notebook_path ?? null));
        } else if (payload.notebook_path !== undefined) {
          applyNotebookPath(payload.notebook_path);
        }
      }
    };

    // Listen for daemon ready (reconnection success, Finder-reuse of an
    // untitled window into a file-backed one, etc.). `onReady` internally
    // also backfills from the Rust-side cache, so a `daemon:ready` that
    // fired before this subscription still hydrates the state.
    const unlistenReady = host.daemonEvents.onReady(handleReady);

    // Check daemon status on mount (in case events fired before React was ready)
    // Small delay to let initial events settle
    const checkTimeout = setTimeout(() => {
      host.daemon.isConnected().then((connected) => {
        if (!connected) {
          setDaemonStatus((prev) => {
            // Only set if no status is already shown
            if (!prev) {
              return {
                status: "failed",
                error: "Runtime daemon not available. Click Retry to connect.",
              };
            }
            return prev;
          });
        }
      });
    }, 500);

    return () => {
      clearTimeout(checkTimeout);
      cancelReadyTimeout();
      unlistenProgress();
      unlistenDisconnect();
      unlistenUnavailable();
      unlistenReady();
    };
  }, [host, applyNotebookPath]);

  // Cmd+Shift+I to toggle isolation test panel (dev only)
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.shiftKey && e.key === "i") {
        e.preventDefault();
        setShowIsolationTest((prev) => !prev);
      }
    };
    window.addEventListener("keydown", handleKeyDown);
    return () => window.removeEventListener("keydown", handleKeyDown);
  }, []);

  const trustApproveLabel =
    pendingTrustAction?.kind === "execute_cell"
      ? "Trust and Run Cell"
      : pendingTrustAction?.kind === "run_all"
        ? "Trust and Run All"
        : pendingTrustAction?.kind === "sync_deps"
          ? "Trust and Sync"
          : undefined;
  const trustApproveOnlyLabel =
    pendingTrustAction?.kind === "sync_deps" ? "Trust Notebook" : "Trust & Start";
  const trustDialogDescription = pendingTrustAction
    ? "This notebook wants to install packages. Approve them before running code."
    : undefined;

  return (
    <PresenceProvider peerId={peerIdRef.current} peerLabel={peerLabel} actorLabel={localActor}>
      <div className="flex h-full flex-col bg-background overflow-hidden">
        {gitInfo && (
          <DebugBanner
            branch={gitInfo.branch}
            commit={gitInfo.commit}
            description={gitInfo.description}
            daemonVersion={daemonInfo?.version}
            isDevMode={daemonInfo?.is_dev_mode}
          />
        )}
        <DaemonStatusBanner
          status={daemonStatus}
          onDismiss={() => setDaemonStatus(null)}
          onRetry={() => {
            setDaemonStatus({ status: "checking" });
            host.daemon
              .reconnect()
              .then(() => {
                // Success - daemon:ready event will clear the banner
              })
              .catch((e) => {
                setDaemonStatus({
                  status: "failed",
                  error: `Reconnection failed: ${e}`,
                });
              });
          }}
        />
        <PoolErrorBanner
          uvError={poolUvError}
          condaError={poolCondaError}
          onDismissUv={dismissPoolUvError}
          onDismissConda={dismissPoolCondaError}
        />
        {needsApproval &&
          !trustApprovalHandoffPending &&
          (kernelStatus === KERNEL_STATUS.NOT_STARTED ||
            kernelStatus === KERNEL_STATUS.AWAITING_TRUST) && (
            <UntrustedBanner
              onReviewClick={() => {
                pendingKernelStartRef.current = true;
                setBlockedTrustAction(null);
                setTrustDialogOpen(true);
              }}
            />
          )}
        {trustActionNotice && (
          <div className="flex items-center justify-between gap-3 border-b border-amber-200 bg-amber-50 px-4 py-2 text-sm text-amber-900 dark:border-amber-800 dark:bg-amber-950/40 dark:text-amber-100">
            <span>{trustActionNotice}</span>
            <button
              type="button"
              className="text-xs font-medium text-amber-800 hover:text-amber-950 dark:text-amber-200 dark:hover:text-amber-50"
              onClick={() => setTrustActionNotice(null)}
            >
              Dismiss
            </button>
          </div>
        )}
        {shouldShowKernelLaunchErrorBanner({
          lifecycle,
          errorDetails,
          errorReason,
          runtime,
        }) &&
          dismissedLaunchError !== errorDetails && (
            <KernelLaunchErrorBanner
              errorDetails={errorDetails as string}
              onRetry={() => {
                setDismissedLaunchError(null);
                tryStartKernel();
              }}
              onDismiss={() => setDismissedLaunchError(errorDetails)}
            />
          )}
        <NotebookToolbar
          kernelStatus={displayKernelStatus}
          statusKey={displayStatusKey}
          lifecycle={lifecycle}
          errorReason={errorReason}
          kernelErrorMessage={errorDetails}
          envSource={envSource}
          envTypeHint={envTypeHint}
          envProgress={envProgress.isActive || envProgress.error ? envProgress : null}
          runtime={runtime}
          onStartKernel={handleStartKernel}
          onInterruptKernel={interruptKernel}
          onRestartKernel={handleRestartKernel}
          onRunAllCells={handleRunAllCells}
          onRestartAndRunAll={handleRestartAndRunAll}
          focusedCellId={focusedCellId}
          lastCellId={cellIds.length > 0 ? cellIds[cellIds.length - 1] : null}
          onAddCell={handleAddCell}
          onToggleDependencies={() => setDependencyHeaderOpen((prev) => !prev)}
          isDepsOpen={dependencyHeaderOpen}
          depsOutOfSync={envSyncState ? !envSyncState.inSync : false}
          updateStatus={updateStatus}
          updateVersion={updateVersion}
          onRestartToUpdate={restartToUpdate}
        />
        {/* Dual-dependency choice: both UV and conda deps exist, let user pick */}
        {dependencyHeaderOpen &&
          runtime === "python" &&
          hasUvDependencies &&
          hasCondaDependencies && (
            <div className="border-b bg-amber-50/50 dark:bg-amber-950/20 px-3 py-2">
              <div className="flex items-center gap-2 text-xs text-amber-700 dark:text-amber-400">
                <span className="shrink-0">&#9888;</span>
                <span className="font-medium">
                  This notebook has both uv and conda dependencies.
                </span>
                <div className="flex gap-1.5 ml-auto shrink-0">
                  <button
                    disabled={clearingDeps}
                    onClick={async () => {
                      setClearingDeps(true);
                      try {
                        await clearAllCondaDeps();
                      } finally {
                        setClearingDeps(false);
                      }
                    }}
                    className="px-2 py-0.5 text-xs font-medium rounded bg-fuchsia-100 dark:bg-fuchsia-900/40 hover:bg-fuchsia-200 dark:hover:bg-fuchsia-800/50 text-fuchsia-800 dark:text-fuchsia-300 border border-fuchsia-300 dark:border-fuchsia-700 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    Use uv ({dependencies?.dependencies?.length ?? 0}{" "}
                    {(dependencies?.dependencies?.length ?? 0) === 1 ? "package" : "packages"})
                  </button>
                  <button
                    disabled={clearingDeps}
                    onClick={async () => {
                      setClearingDeps(true);
                      try {
                        await clearAllUvDeps();
                      } finally {
                        setClearingDeps(false);
                      }
                    }}
                    className="px-2 py-0.5 text-xs font-medium rounded bg-emerald-100 dark:bg-emerald-900/40 hover:bg-emerald-200 dark:hover:bg-emerald-800/50 text-emerald-800 dark:text-emerald-300 border border-emerald-300 dark:border-emerald-700 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                  >
                    Use conda ({condaDependencies?.dependencies?.length ?? 0}{" "}
                    {(condaDependencies?.dependencies?.length ?? 0) === 1 ? "package" : "packages"})
                  </button>
                </div>
              </div>
            </div>
          )}
        {dependencyHeaderOpen && runtime === "deno" && (
          <DenoDependencyHeader
            denoConfigInfo={denoConfigInfo}
            flexibleNpmImports={flexibleNpmImports}
            onSetFlexibleNpmImports={setFlexibleNpmImports}
            syncState={denoDerivedSyncState}
            syncing={kernelStatus === KERNEL_STATUS.STARTING}
            onSyncNow={handleSyncDeps}
            justSynced={justSynced}
          />
        )}
        {dependencyHeaderOpen && runtime === "python" && envType === "conda" && (
          <CondaDependencyHeader
            dependencies={condaDependencies?.dependencies ?? []}
            channels={condaDependencies?.channels ?? []}
            python={condaDependencies?.python ?? null}
            loading={condaDepsLoading}
            syncState={condaDerivedSyncState}
            onAdd={addCondaDependency}
            onRemove={removeCondaDependency}
            onSetChannels={setCondaChannels}
            onSetPython={setCondaPython}
            onSyncNow={handleSyncDeps}
            onRetryLaunch={tryStartKernel}
            envProgress={envProgress.envType === "conda" ? envProgress : null}
            onResetProgress={envProgress.reset}
            environmentYmlInfo={environmentYmlInfo}
            environmentYmlDeps={environmentYmlDeps}
            justSynced={justSynced}
          />
        )}
        {dependencyHeaderOpen && runtime === "python" && envType === "pixi" && (
          <PixiDependencyHeader
            pixiInfo={pixiInfo}
            envSource={envSource}
            syncState={pixiDerivedSyncState}
            onSyncNow={handleSyncDeps}
            justSynced={justSynced}
          />
        )}
        {dependencyHeaderOpen &&
          runtime === "python" &&
          envType !== "conda" &&
          envType !== "pixi" && (
            <DependencyHeader
              dependencies={dependencies?.dependencies ?? []}
              requiresPython={dependencies?.requires_python ?? null}
              loading={depsLoading}
              onAdd={addDependency}
              onRemove={removeDependency}
              syncState={uvDerivedSyncState}
              onSyncNow={handleSyncDeps}
              pyprojectInfo={pyprojectInfo}
              pyprojectDeps={pyprojectDeps}
              onImportFromPyproject={importFromPyproject}
              onUseProjectEnv={handleStartKernelWithPyproject}
              isUsingProjectEnv={envSource === "uv:pyproject"}
              justSynced={justSynced}
            />
          )}
        {globalFind.isOpen && (
          <GlobalFindBar
            query={globalFind.query}
            matchCount={globalFind.matches.length}
            currentMatchIndex={globalFind.currentMatchIndex}
            onQueryChange={globalFind.setQuery}
            onNextMatch={globalFind.nextMatch}
            onPrevMatch={globalFind.prevMatch}
            onClose={globalFind.close}
          />
        )}
        {showIsolationTest && <IsolationTest />}
        <TrustDialog
          open={trustDialogOpen}
          onOpenChange={handleTrustDialogOpenChange}
          trustInfo={trustInfo}
          typosquatWarnings={typosquatWarnings}
          onApprove={handleTrustApprove}
          onApproveOnly={pendingTrustAction ? handleTrustApproveOnly : undefined}
          onDecline={handleTrustDecline}
          loading={trustLoading}
          daemonMode={true}
          approveLabel={trustApproveLabel}
          approveOnlyLabel={trustApproveOnlyLabel}
          description={trustDialogDescription}
          approvalError={approvalError}
        />
        <CrdtBridgeProvider
          getHandle={getHandle}
          onSyncNeeded={triggerSync}
          localActor={localActor}
        >
          <NotebookView
            cellIds={cellIds}
            isLoading={isLoading}
            loadError={loadError}
            runtime={runtime}
            onFocusCell={setFocusedCellId}
            onExecuteCell={handleExecuteCell}
            onInterruptKernel={interruptKernel}
            onDeleteCell={deleteCell}
            onAddCell={handleAddCell}
            onMoveCell={moveCell}
            onReportOutputMatchCount={globalFind.reportOutputMatchCount}
            onSetCellSourceHidden={setCellSourceHidden}
            onSetCellOutputsHidden={setCellOutputsHidden}
          />
        </CrdtBridgeProvider>
      </div>
    </PresenceProvider>
  );
}

function AppErrorFallback(_error: Error, resetErrorBoundary: () => void) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-4 bg-background p-8">
      <div className="text-center">
        <h1 className="text-lg font-semibold text-foreground">Something went wrong</h1>
        <p className="mt-1 text-sm text-muted-foreground">
          The notebook encountered an unexpected error.
        </p>
      </div>
      <div className="flex gap-2">
        <button
          type="button"
          onClick={resetErrorBoundary}
          className="rounded-md border border-border bg-background px-4 py-2 text-sm font-medium text-foreground hover:bg-muted transition-colors"
        >
          Try again
        </button>
        <button
          type="button"
          onClick={() => window.location.reload()}
          className="rounded-md bg-foreground px-4 py-2 text-sm font-medium text-background hover:opacity-90 transition-opacity"
        >
          Reload
        </button>
      </div>
    </div>
  );
}

// Module-level ref for the widget store (set by AppContent, read by updateManager).
// This avoids a chicken-and-egg: the manager is created before the store exists.
let _widgetStoreRef: import("@/components/widgets/widget-store").WidgetStore | null = null;

const updateManager = new WidgetUpdateManager({
  getStore: () => _widgetStoreRef,
  getCrdtWriter: getCrdtCommWriter,
});

function WidgetViewRenderer({ data }: { data: unknown }) {
  const { model_id } = data as { model_id: string };
  return <WidgetView modelId={model_id} />;
}

const MEDIA_RENDERERS = {
  "application/vnd.jupyter.widget-view+json": WidgetViewRenderer,
};

export default function App() {
  return (
    <ErrorBoundary fallback={AppErrorFallback}>
      <WidgetStoreProvider sendMessage={sendMessage} updateManager={updateManager}>
        <MediaProvider renderers={MEDIA_RENDERERS}>
          <AppContent />
        </MediaProvider>
      </WidgetStoreProvider>
    </ErrorBoundary>
  );
}
