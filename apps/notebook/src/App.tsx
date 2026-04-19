import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { NotebookClient } from "runtimed";
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
import { UntrustedBanner } from "./components/UntrustedBanner";
import { PresenceProvider } from "./contexts/PresenceContext";
import { useAutomergeNotebook } from "./hooks/useAutomergeNotebook";
import { useCondaDependencies } from "./hooks/useCondaDependencies";
import { CrdtBridgeProvider } from "./hooks/useCrdtBridge";
import { useDaemonKernel } from "./hooks/useDaemonKernel";
import { useDenoDependencies } from "./hooks/useDenoDependencies";
import { type EnvSyncState, useDependencies } from "./hooks/useDependencies";
import { useEnvProgress } from "./hooks/useEnvProgress";
import { useDaemonInfo, useGitInfo } from "./hooks/useGitInfo";
import { useGlobalFind } from "./hooks/useGlobalFind";
import { resolveOutputValue } from "./hooks/useManifestResolver";
import { usePixiDependencies } from "./hooks/usePixiDependencies";
import { usePoolState } from "./hooks/usePoolState";
import { useTrust } from "./hooks/useTrust";
import { useUpdater } from "./hooks/useUpdater";
import { startAttributionDispatch } from "./lib/attribution-registry";
import { getBlobPort, useBlobPort } from "./lib/blob-port";
import { subscribeBroadcast } from "./lib/notebook-frame-bus";
import {
  flushCellUIState,
  setExecutingCellIds as storeSetExecutingCellIds,
  setFocusedCellId as storeSetFocusedCellId,
  setQueuedCellIds as storeSetQueuedCellIds,
  setSearchCurrentMatch as storeSetSearchCurrentMatch,
  setSearchQuery as storeSetSearchQuery,
} from "./lib/cell-ui-state";
import { startCursorDispatch } from "./lib/cursor-registry";
import { KERNEL_STATUS } from "./lib/kernel-status";
import { logger } from "./lib/logger";
import { getNotebookCellsSnapshot } from "./lib/notebook-cells";
import { useDetectRuntime } from "./lib/notebook-metadata";
import { useNotebookHost } from "@nteract/notebook-host";
import { startWindowFocusHandler } from "./lib/window-focus";
import type { JupyterOutput } from "./types";

/** MIME bundle type for output data */
export type MimeBundle = Record<string, unknown>;

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
    focusedCellId,
    setFocusedCellId,
    addCell,
    moveCell,
    deleteCell,
    save,
    openNotebook,
    cloneNotebook,
    dirty,
    setDirty,

    updateOutputByDisplayId,
    applyExecutionCountFromDaemon,
    clearOutputsFromDaemon,
    setCellSourceHidden,
    setCellOutputsHidden,
    flushSync,
    getHandle,
    getEngine,
    triggerSync,
    localActor,
  } = useAutomergeNotebook();

  // Global find (Cmd+F)
  const globalFind = useGlobalFind(cellIds);

  const [dependencyHeaderOpen, setDependencyHeaderOpen] = useState(false);
  const [showIsolationTest, setShowIsolationTest] = useState(false);
  const [trustDialogOpen, setTrustDialogOpen] = useState(false);
  const [clearingDeps, setClearingDeps] = useState(false);
  // Track when sync/restart just completed for success feedback
  const [justSynced, setJustSynced] = useState(false);

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

  // Notebook runtime type — reactive read from WASM Automerge doc.
  // Re-renders automatically when metadata changes (bootstrap, sync, writes).
  const detectedRuntime = useDetectRuntime();
  // Runtime hint from daemon:ready payload — available before metadata syncs,
  // prevents momentary flicker of wrong runtime UI (e.g. Python for Deno notebooks).
  const [runtimeHint, setRuntimeHint] = useState<string | null>(null);
  const runtime = detectedRuntime ?? runtimeHint;

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
  const { pixiInfo } = usePixiDependencies();

  // Deno config detection and settings
  const { denoConfigInfo, flexibleNpmImports, setFlexibleNpmImports } = useDenoDependencies();

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
    startingPhase,
    kernelInfo,
    queueState,
    envSyncState,
    launchKernel,
    executeCell,
    clearOutputs,
    interruptKernel,
    shutdownKernel,
    syncEnvironment,
    runAllCells: daemonRunAllCells,
    sendCommMessage,
  } = useDaemonKernel({
    client: notebookClient,
    onExecutionCount: handleExecutionCount,
    onExecutionDone: handleExecutionDone,
    onUpdateDisplayData: updateOutputByDisplayId,
    onClearOutputs: clearOutputsFromDaemon,
  });

  // Derive values from daemon kernel
  const envSource = kernelInfo.envSource ?? null;

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

  // When kernel is running and we know the env source, use it to determine panel type.
  // This handles: both-deps (backend picks based on preference), pixi (auto-detected, no metadata).
  // Fall back to metadata-based detection when kernel hasn't started yet.
  const envType = envSource?.startsWith("conda:")
    ? "conda"
    : envSource?.startsWith("uv:")
      ? "uv"
      : envSource?.startsWith("pixi:")
        ? "pixi"
        : isUvConfigured
          ? "uv"
          : isCondaConfigured || environmentYmlInfo?.has_dependencies
            ? "conda"
            : pixiInfo?.has_dependencies || pixiInfo?.has_pypi_dependencies
              ? "pixi"
              : null;

  // Pre-start hint for the env badge (more specific than envType: distinguishes pixi)
  const envTypeHint = envSource
    ? null // backend has spoken, no hint needed
    : pixiInfo?.has_dependencies || pixiInfo?.has_pypi_dependencies
      ? ("pixi" as const)
      : envType === "conda"
        ? ("conda" as const)
        : envType === "uv"
          ? ("uv" as const)
          : null;

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

  // Check trust and start kernel if trusted, otherwise show dialog.
  // Returns true if kernel was started, false if trust dialog opened or error.
  const tryStartKernel = useCallback(async (): Promise<boolean> => {
    // Re-check trust status (may have changed)
    const info = await checkTrust();
    if (!info) return false;

    if (info.status === "trusted" || info.status === "no_dependencies") {
      // Trusted - launch kernel via daemon
      // Both kernel_type and env_source use "auto" - daemon detects from Automerge doc
      const response = await launchKernel("auto", "auto");
      if (response.result === "error") {
        logger.error("[App] tryStartKernel: daemon error", response.error);
        return false;
      }
      return true;
    }
    // Untrusted - show dialog and mark pending start
    pendingKernelStartRef.current = true;
    setTrustDialogOpen(true);
    return false;
  }, [checkTrust, launchKernel]);

  // Handler to sync deps - tries hot-sync for UV additions, falls back to restart
  // Always checks trust before any operation that installs packages
  const handleSyncDeps = useCallback(async (): Promise<boolean> => {
    // Reset any previous error state before attempting
    envProgress.reset();

    // Check trust first - required before any package installation (hot-sync or restart)
    const info = await checkTrust();
    if (!info) return false;

    if (info.status !== "trusted" && info.status !== "no_dependencies") {
      // Untrusted - show dialog, let user approve before any installation
      pendingKernelStartRef.current = true;
      setTrustDialogOpen(true);
      return false;
    }

    // Trusted - proceed with sync/restart
    // For UV or Conda inline deps with only additions, try hot-sync first
    const isUvInline = envSource === "uv:inline";
    const isCondaInline = envSource === "conda:inline";
    const hasOnlyAdditions =
      envSyncState?.diff?.added?.length && !envSyncState?.diff?.removed?.length;

    if ((isUvInline || isCondaInline) && hasOnlyAdditions) {
      logger.debug("[App] Trying hot-sync for additions");
      const response = await syncEnvironment();

      if (response.result === "sync_environment_complete") {
        logger.debug("[App] Hot-sync succeeded:", response.synced_packages);
        envProgress.reset();
        setJustSynced(true);
        return true;
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
  }, [
    envSource,
    envSyncState,
    envProgress,
    syncEnvironment,
    checkTrust,
    shutdownKernel,
    tryStartKernel,
  ]);

  // Restart and run all cells
  const restartAndRunAll = useCallback(async () => {
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
      const kernelStarted = await tryStartKernel();
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
  }, [flushSync, shutdownKernel, tryStartKernel, daemonRunAllCells]);

  // Handle trust approval from dialog
  const handleTrustApprove = useCallback(async () => {
    const success = await approveTrust();
    if (success && pendingKernelStartRef.current) {
      pendingKernelStartRef.current = false;
      // Fire and forget - dialog closes immediately, kernel starts in background
      // Use "auto" for both - daemon detects from Automerge doc
      launchKernel("auto", "auto").catch((e) => {
        logger.error("[App] kernel launch after trust approval failed:", e);
      });
    }
    return success;
  }, [approveTrust, launchKernel]);

  // Handle trust decline from dialog
  const handleTrustDecline = useCallback(() => {
    pendingKernelStartRef.current = false;
    // User declined - don't start kernel, just close dialog
  }, []);

  // Start kernel explicitly with pyproject.toml (user action from DependencyHeader)
  const handleStartKernelWithPyproject = useCallback(async () => {
    const response = await launchKernel("python", "uv:pyproject");
    if (response.result === "error") {
      logger.error("[App] handleStartKernelWithPyproject: daemon error", response.error);
    }
  }, [launchKernel]);

  const handleExecuteCell = useCallback(
    async (cellId: string) => {
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
          const started = await tryStartKernel();
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
    [flushSync, kernelStatus, tryStartKernel, executeCell],
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
    await shutdownKernel();
    await tryStartKernel();
  }, [shutdownKernel, tryStartKernel]);

  const handleRunAllCells = useCallback(async () => {
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
        const started = await tryStartKernel();
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
  }, [kernelStatus, tryStartKernel, flushSync, daemonRunAllCells]);

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

  // Path transitions. `path_changed` with a non-null path means the room is
  // now file-backed — flip ephemeral false and update the title base. A null
  // path puts the room back into untitled state.
  useEffect(() => {
    return subscribeBroadcast((payload) => {
      const b = payload as { event?: string; path?: string | null };
      if (b.event !== "path_changed") return;
      applyNotebookPath(b.path);
    });
  }, [applyNotebookPath]);

  // Render the asterisk purely from frontend state. `titleBase` is the
  // filename, `dirty || ephemeral` adds the prefix. One setTitle per change;
  // no read-then-write race against `applyPathChanged`.
  useEffect(() => {
    if (titleBase == null) return;
    const showDirty = dirty || ephemeral === true;
    const next = showDirty ? `* ${titleBase}` : titleBase;
    host.window.setTitle(next).catch(() => {
      // Window may have been closed between rapid dirty toggles
    });
  }, [host, dirty, ephemeral, titleBase]);

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
          (kernelStatus === KERNEL_STATUS.NOT_STARTED ||
            kernelStatus === KERNEL_STATUS.AWAITING_TRUST) && (
            <UntrustedBanner
              onReviewClick={() => {
                pendingKernelStartRef.current = true;
                setTrustDialogOpen(true);
              }}
            />
          )}
        <NotebookToolbar
          kernelStatus={kernelStatus}
          startingPhase={startingPhase}
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
          onOpenChange={setTrustDialogOpen}
          trustInfo={trustInfo}
          typosquatWarnings={typosquatWarnings}
          onApprove={handleTrustApprove}
          onDecline={handleTrustDecline}
          loading={trustLoading}
          daemonMode={true}
        />
        <CrdtBridgeProvider
          getHandle={getHandle}
          onSyncNeeded={triggerSync}
          setDirty={setDirty}
          localActor={localActor}
        >
          <NotebookView
            cellIds={cellIds}
            isLoading={isLoading}
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

export default function App() {
  return (
    <ErrorBoundary fallback={AppErrorFallback}>
      <WidgetStoreProvider sendMessage={sendMessage} updateManager={updateManager}>
        <MediaProvider
          renderers={{
            "application/vnd.jupyter.widget-view+json": ({ data }) => {
              const { model_id } = data as { model_id: string };
              return <WidgetView modelId={model_id} />;
            },
          }}
        >
          <AppContent />
        </MediaProvider>
      </WidgetStoreProvider>
    </ErrorBoundary>
  );
}
