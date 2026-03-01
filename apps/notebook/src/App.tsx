import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useCallback, useEffect, useRef, useState } from "react";
import { IsolationTest } from "@/components/isolated";
import { MediaProvider } from "@/components/outputs/media-provider";
import {
  useWidgetStoreRequired,
  WidgetStoreProvider,
} from "@/components/widgets/widget-store-context";
import { WidgetView } from "@/components/widgets/widget-view";
import { useSyncedSettings, useSyncedTheme } from "@/hooks/useSyncedSettings";
import { ErrorBoundary } from "@/lib/error-boundary";
import { CondaDependencyHeader } from "./components/CondaDependencyHeader";
import {
  type DaemonStatus,
  DaemonStatusBanner,
} from "./components/DaemonStatusBanner";
import { DebugBanner } from "./components/DebugBanner";
import { DenoDependencyHeader } from "./components/DenoDependencyHeader";
import { DependencyHeader } from "./components/DependencyHeader";
import { NotebookToolbar } from "./components/NotebookToolbar";
import { NotebookView } from "./components/NotebookView";
import { TrustDialog } from "./components/TrustDialog";
import { useCondaDependencies } from "./hooks/useCondaDependencies";
import { useDaemonKernel } from "./hooks/useDaemonKernel";
import { useDenoDependencies } from "./hooks/useDenoDependencies";
import { useDependencies } from "./hooks/useDependencies";
import { useEnvProgress } from "./hooks/useEnvProgress";
import { useDaemonInfo, useGitInfo } from "./hooks/useGitInfo";
import { useNotebook } from "./hooks/useNotebook";
import { useTrust } from "./hooks/useTrust";
import type { JupyterMessage } from "./types";

/** MIME bundle type for page payloads */
export type MimeBundle = Record<string, unknown>;

/** Page payload data for a cell */
export interface CellPagePayload {
  data: MimeBundle;
  start: number;
}

/**
 * Module-level reference for daemon comm sending.
 * Set by AppContent when daemon kernel is initialized.
 */
let daemonCommSender: ((message: unknown) => Promise<void>) | null = null;

/**
 * Update the daemon comm sender reference.
 * Called by AppContent when daemon kernel is initialized.
 */
export function setDaemonCommSender(
  sender: ((message: unknown) => Promise<void>) | null,
): void {
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
      console.warn("[widget] sendMessage called but daemon sender not ready");
    }
  } catch (e) {
    console.error("[widget] send_comm_message failed:", e);
  }
}

function AppContent() {
  const gitInfo = useGitInfo();
  const daemonInfo = useDaemonInfo();

  const { theme, setTheme } = useSyncedTheme();
  const {
    defaultRuntime,
    setDefaultRuntime,
    defaultPythonEnv,
    setDefaultPythonEnv,
    defaultUvPackages,
    setDefaultUvPackages,
    defaultCondaPackages,
    setDefaultCondaPackages,
  } = useSyncedSettings();

  const {
    cells,
    focusedCellId,
    setFocusedCellId,
    updateCellSource,
    addCell,
    deleteCell,
    save,
    openNotebook,
    cloneNotebook,
    dirty,
    updateOutputByDisplayId,
    setExecutionCount,
    clearCellOutputs,
    formatCell,
  } = useNotebook();

  const [dependencyHeaderOpen, setDependencyHeaderOpen] = useState(false);
  const [showIsolationTest, setShowIsolationTest] = useState(false);
  const [trustDialogOpen, setTrustDialogOpen] = useState(false);
  const [clearingDeps, setClearingDeps] = useState(false);

  // Daemon startup status (installing, starting, failed, etc.)
  const [daemonStatus, setDaemonStatus] = useState<DaemonStatus>(null);
  // Track ready timeout so we can cancel it if status changes
  const readyTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  // Trust verification for notebook dependencies
  const {
    trustInfo,
    typosquatWarnings,
    loading: trustLoading,
    checkTrust,
    approveTrust,
  } = useTrust();

  // Track pending kernel start that was blocked by trust dialog
  const pendingKernelStartRef = useRef(false);

  // Notebook runtime type (python or deno)
  const [runtime, setRuntime] = useState<"python" | "deno">("python");

  // Load runtime from notebook metadata on mount
  useEffect(() => {
    invoke<string>("get_notebook_runtime").then((r) => {
      setRuntime(r as "python" | "deno");
    });
  }, []);

  // Page payload state: maps cell_id -> payload (transient, not saved)
  const [pagePayloads, setPagePayloads] = useState<
    Map<string, CellPagePayload>
  >(new Map());

  // UV Dependency management
  const {
    dependencies,
    uvAvailable,
    hasDependencies: hasUvDependencies,
    isUvConfigured,
    loading: depsLoading,
    syncedWhileRunning,
    needsKernelRestart,
    addDependency,
    removeDependency,
    clearAllDependencies: clearAllUvDeps,
    syncState,
    syncNow,
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
    syncing: condaSyncing,
    syncState: condaSyncState,
    syncedWhileRunning: condaSyncedWhileRunning,
    needsKernelRestart: condaNeedsKernelRestart,
    addDependency: addCondaDependency,
    removeDependency: removeCondaDependency,
    clearAllDependencies: clearAllCondaDeps,
    setChannels: setCondaChannels,
    setPython: setCondaPython,
    environmentYmlInfo,
    environmentYmlDeps,
    syncNow: syncCondaNow,
    pixiInfo,
    importFromPixi,
  } = useCondaDependencies();

  // Deno config detection and settings
  const {
    denoAvailable,
    denoConfigInfo,
    flexibleNpmImports,
    setFlexibleNpmImports,
  } = useDenoDependencies();

  // Combine hasDependencies for toolbar badge
  // For Deno, show badge if deno.json is found with imports
  const hasDependencies =
    runtime === "deno"
      ? (denoConfigInfo?.has_imports ?? false)
      : hasUvDependencies ||
        hasCondaDependencies ||
        (environmentYmlInfo?.has_dependencies ?? false);

  // Get widget store handler for routing comm messages
  const { handleMessage: handleWidgetMessage } = useWidgetStoreRequired();

  const handleExecutionCount = useCallback(
    (cellId: string, count: number) => {
      setExecutionCount(cellId, count);
    },
    [setExecutionCount],
  );

  // Execution completion is handled by the daemon queue via broadcast events
  const handleExecutionDone = useCallback((_cellId: string) => {
    // Daemon queue handles execution tracking via broadcasts
  }, []);

  const handleCommMessage = useCallback(
    (msg: JupyterMessage) => {
      // Forward comm messages to the widget store's comm router
      handleWidgetMessage(msg as Parameters<typeof handleWidgetMessage>[0]);
    },
    [handleWidgetMessage],
  );

  // Clear page payload for a cell (e.g., when dismissed or re-executed)
  const clearPagePayload = useCallback((cellId: string) => {
    setPagePayloads((prev) => {
      const next = new Map(prev);
      next.delete(cellId);
      return next;
    });
  }, []);

  // Daemon-owned kernel execution
  const {
    kernelStatus,
    kernelInfo,
    queueState,
    launchKernel,
    executeCell,
    clearOutputs,
    interruptKernel,
    shutdownKernel,
    runAllCells: daemonRunAllCells,
    sendCommMessage,
  } = useDaemonKernel({
    // Daemon execution: Automerge is the source of truth for outputs.
    // The daemon writes outputs to Automerge, then broadcasts for immediate UI.
    // We skip broadcast handling to avoid race conditions - Automerge sync
    // arrives shortly after and provides the canonical state.
    onOutput: () => {},
    onExecutionCount: handleExecutionCount,
    onExecutionDone: handleExecutionDone,
    onUpdateDisplayData: updateOutputByDisplayId,
    onClearOutputs: clearCellOutputs, // Handle broadcast from other windows
    onCommMessage: handleCommMessage, // Route comm messages to widget store
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

  // Get executing cell IDs from daemon queue state (as Set for NotebookView)
  const executingCellIds = new Set(
    queueState.executing
      ? [queueState.executing, ...queueState.queued]
      : queueState.queued,
  );

  // When kernel is running and we know the env source, use it to determine panel type.
  // This handles: both-deps (backend picks based on preference), pixi (auto-detected, no metadata).
  // Fall back to metadata-based detection when kernel hasn't started yet.
  const envType = envSource?.startsWith("conda:")
    ? "conda"
    : envSource?.startsWith("uv:")
      ? "uv"
      : isUvConfigured && uvAvailable !== false
        ? "uv"
        : isCondaConfigured ||
            environmentYmlInfo?.has_dependencies ||
            uvAvailable === false
          ? "conda"
          : null;

  // Pre-start hint for the env badge (more specific than envType: distinguishes pixi)
  const envTypeHint = envSource
    ? null // backend has spoken, no hint needed
    : pixiInfo?.has_dependencies
      ? ("pixi" as const)
      : envType === "conda"
        ? ("conda" as const)
        : envType === "uv"
          ? ("uv" as const)
          : null;

  // Environment preparation progress
  const envProgress = useEnvProgress();

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
        console.error("[App] tryStartKernel: daemon error", response.error);
        return false;
      }
      return true;
    }
    // Untrusted - show dialog and mark pending start
    pendingKernelStartRef.current = true;
    setTrustDialogOpen(true);
    return false;
  }, [checkTrust, launchKernel]);

  // Restart and run all cells
  const restartAndRunAll = useCallback(async () => {
    const codeCells = cells.filter((c) => c.cell_type === "code");

    // Clear all outputs locally (immediate feedback)
    for (const cell of codeCells) {
      clearCellOutputs(cell.id);
    }

    // Clear outputs via daemon for cross-window sync
    await Promise.all(codeCells.map((cell) => clearOutputs(cell.id)));

    // Shutdown existing kernel
    await shutdownKernel();

    // Start kernel - returns false if not started (e.g., trust dialog)
    const kernelStarted = await tryStartKernel();
    if (!kernelStarted) {
      console.log(
        "[App] restartAndRunAll: kernel not started, skipping run all",
      );
      return;
    }

    // Daemon reads cell sources from Automerge doc and queues them
    const response = await daemonRunAllCells();
    if (response.result === "error") {
      console.error("[App] restartAndRunAll: daemon error", response.error);
    } else if (response.result === "no_kernel") {
      console.warn("[App] restartAndRunAll: no kernel available");
    }
  }, [
    cells,
    clearCellOutputs,
    clearOutputs,
    shutdownKernel,
    tryStartKernel,
    daemonRunAllCells,
  ]);

  // Handle trust approval from dialog
  const handleTrustApprove = useCallback(async () => {
    const success = await approveTrust();
    if (success && pendingKernelStartRef.current) {
      pendingKernelStartRef.current = false;
      // Now start the kernel since trust was approved
      // Use "auto" for both - daemon detects from Automerge doc
      await launchKernel("auto", "auto");
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
      console.error(
        "[App] handleStartKernelWithPyproject: daemon error",
        response.error,
      );
    }
  }, [launchKernel]);

  const handleExecuteCell = useCallback(
    async (cellId: string) => {
      // Resolve cell up front before awaiting sync operations.
      const cell = cells.find((c) => c.id === cellId);
      if (!cell || cell.cell_type !== "code") return;

      // Clear outputs immediately so user sees feedback
      clearCellOutputs(cellId);

      // Broadcast clear to other windows
      await clearOutputs(cellId);

      // Start kernel via daemon if not running, then queue cell.
      if (kernelStatus === "not_started") {
        const started = await tryStartKernel();
        // Only block execution when trust approval is pending.
        // For startup races (e.g. daemon already auto-starting), still try execute.
        if (!started && pendingKernelStartRef.current) return;
      }
      const response = await executeCell(cellId);
      if (response.result === "error") {
        console.error("[App] handleExecuteCell: daemon error", response.error);
      } else if (response.result === "no_kernel") {
        console.warn("[App] handleExecuteCell: no kernel available");
      }
    },
    [
      clearCellOutputs,
      clearOutputs,
      cells,
      kernelStatus,
      tryStartKernel,
      executeCell,
    ],
  );

  const handleAddCell = useCallback(
    (type: "code" | "markdown", afterCellId?: string | null) => {
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
    // Daemon reads cells from synced Automerge doc
    const codeCells = cells.filter((c) => c.cell_type === "code");
    if (codeCells.length === 0) return;

    // Clear all outputs first (local for immediate feedback)
    for (const cell of codeCells) {
      clearCellOutputs(cell.id);
    }

    // Await all daemon clears to ensure ordering before queueing
    await Promise.all(codeCells.map((cell) => clearOutputs(cell.id)));

    // Start kernel via daemon if not running
    if (kernelStatus === "not_started") {
      const started = await tryStartKernel();
      if (!started) {
        console.log(
          "[App] handleRunAllCells: kernel not started, skipping run all",
        );
        return;
      }
    }

    // Daemon reads cell sources from Automerge doc and queues them
    const response = await daemonRunAllCells();
    if (response.result === "error") {
      console.error("[App] handleRunAllCells: daemon error", response.error);
    } else if (response.result === "no_kernel") {
      console.warn("[App] handleRunAllCells: no kernel available");
    }
  }, [
    kernelStatus,
    tryStartKernel,
    cells,
    clearCellOutputs,
    clearOutputs,
    daemonRunAllCells,
  ]);

  const handleRestartAndRunAll = useCallback(async () => {
    // Backend clears outputs and emits cells:outputs_cleared before queuing,
    // then ensureKernelStarted restarts the kernel
    await restartAndRunAll();
  }, [restartAndRunAll]);

  // Cmd+S to save (keyboard and native menu)
  useEffect(() => {
    const webview = getCurrentWebview();
    // Listen for native menu save event
    const unlistenPromise = webview.listen("menu:save", () => {
      save();
    });

    // Keep keyboard shortcut as fallback
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "s") {
        e.preventDefault();
        save();
      }
    };
    window.addEventListener("keydown", handleKeyDown);

    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [save]);

  // Cmd+O to open (keyboard and native menu)
  useEffect(() => {
    const webview = getCurrentWebview();
    // Listen for native menu open event
    const unlistenPromise = webview.listen("menu:open", () => {
      openNotebook();
    });

    // Keep keyboard shortcut as fallback
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key === "o") {
        e.preventDefault();
        openNotebook();
      }
    };
    window.addEventListener("keydown", handleKeyDown);

    return () => {
      window.removeEventListener("keydown", handleKeyDown);
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [openNotebook]);

  // Clone notebook via native menu
  useEffect(() => {
    const webview = getCurrentWebview();
    const unlistenPromise = webview.listen("menu:clone", () => {
      cloneNotebook();
    });

    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [cloneNotebook]);

  // Kernel menu: Run All Cells
  useEffect(() => {
    const webview = getCurrentWebview();
    const unlistenPromise = webview.listen("menu:run-all", () => {
      handleRunAllCells();
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [handleRunAllCells]);

  // Kernel menu: Restart & Run All Cells
  useEffect(() => {
    const webview = getCurrentWebview();
    const unlistenPromise = webview.listen("menu:restart-and-run-all", () => {
      handleRestartAndRunAll();
    });
    return () => {
      unlistenPromise.then((unlisten) => unlisten());
    };
  }, [handleRestartAndRunAll]);

  // Zoom controls via native menu
  useEffect(() => {
    const webview = getCurrentWebview();
    let currentZoom = 1.0;

    const handleZoomIn = () => {
      currentZoom = Math.min(3.0, currentZoom + 0.1);
      webview.setZoom(currentZoom);
    };

    const handleZoomOut = () => {
      currentZoom = Math.max(0.5, currentZoom - 0.1);
      webview.setZoom(currentZoom);
    };

    const handleZoomReset = () => {
      currentZoom = 1.0;
      webview.setZoom(1.0);
    };

    const unlistenIn = webview.listen("menu:zoom-in", handleZoomIn);
    const unlistenOut = webview.listen("menu:zoom-out", handleZoomOut);
    const unlistenReset = webview.listen("menu:zoom-reset", handleZoomReset);

    return () => {
      unlistenIn.then((u) => u());
      unlistenOut.then((u) => u());
      unlistenReset.then((u) => u());
    };
  }, []);

  // Listen for daemon startup progress events
  useEffect(() => {
    const webview = getCurrentWebview();
    // Helper to cancel any pending ready timeout
    const cancelReadyTimeout = () => {
      if (readyTimeoutRef.current) {
        clearTimeout(readyTimeoutRef.current);
        readyTimeoutRef.current = null;
      }
    };

    const unlistenProgress = listen<DaemonStatus>(
      "daemon:progress",
      (event) => {
        const status = event.payload;

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
      },
    );

    // Listen for daemon disconnection (mid-session)
    const unlistenDisconnect = webview.listen("daemon:disconnected", () => {
      cancelReadyTimeout();
      setDaemonStatus({
        status: "failed",
        error: "Runtime disconnected. Attempting to reconnect...",
      });
    });

    // Listen for daemon unavailable (startup failure, fires after sync timeout)
    const unlistenUnavailable = listen<{
      reason: string;
      message: string;
      guidance: string;
    }>("daemon:unavailable", (event) => {
      cancelReadyTimeout();
      setDaemonStatus({
        status: "failed",
        error: `${event.payload.message} ${event.payload.guidance}`,
      });
    });

    // Listen for daemon ready (reconnection success)
    const unlistenReady = webview.listen("daemon:ready", () => {
      // Clear any status banner when daemon reconnects (failed, checking, etc.)
      cancelReadyTimeout();
      setDaemonStatus(null);
    });

    // Check daemon status on mount (in case events fired before React was ready)
    // Small delay to let initial events settle
    const checkTimeout = setTimeout(() => {
      invoke<boolean>("is_daemon_connected").then((connected) => {
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
      unlistenProgress.then((unlisten) => unlisten()).catch(() => {});
      unlistenDisconnect.then((unlisten) => unlisten()).catch(() => {});
      unlistenUnavailable.then((unlisten) => unlisten()).catch(() => {});
      unlistenReady.then((unlisten) => unlisten()).catch(() => {});
    };
  }, []);

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
    <div className="flex h-full flex-col bg-background overflow-hidden">
      {gitInfo && (
        <DebugBanner
          branch={gitInfo.branch}
          commit={gitInfo.commit}
          description={gitInfo.description}
          daemonVersion={daemonInfo?.version}
          socketPath={daemonInfo?.socket_path}
          isDevMode={daemonInfo?.is_dev_mode}
        />
      )}
      <DaemonStatusBanner
        status={daemonStatus}
        onDismiss={() => setDaemonStatus(null)}
        onRetry={() => {
          setDaemonStatus({ status: "checking" });
          invoke("reconnect_to_daemon")
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
      <NotebookToolbar
        kernelStatus={kernelStatus}
        envSource={envSource}
        envTypeHint={envTypeHint}
        dirty={dirty}
        hasDependencies={hasDependencies}
        theme={theme}
        envProgress={
          envProgress.isActive || envProgress.error ? envProgress : null
        }
        runtime={runtime}
        onThemeChange={setTheme}
        defaultRuntime={defaultRuntime}
        onDefaultRuntimeChange={setDefaultRuntime}
        defaultPythonEnv={defaultPythonEnv}
        onDefaultPythonEnvChange={setDefaultPythonEnv}
        defaultUvPackages={defaultUvPackages}
        onDefaultUvPackagesChange={setDefaultUvPackages}
        defaultCondaPackages={defaultCondaPackages}
        onDefaultCondaPackagesChange={setDefaultCondaPackages}
        onSave={save}
        onStartKernel={handleStartKernel}
        onInterruptKernel={interruptKernel}
        onRestartKernel={handleRestartKernel}
        onRunAllCells={handleRunAllCells}
        onRestartAndRunAll={handleRestartAndRunAll}
        onAddCell={handleAddCell}
        onToggleDependencies={() => setDependencyHeaderOpen((prev) => !prev)}
        isDepsOpen={dependencyHeaderOpen}
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
                  {(dependencies?.dependencies?.length ?? 0) === 1
                    ? "package"
                    : "packages"}
                  )
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
                  {(condaDependencies?.dependencies?.length ?? 0) === 1
                    ? "package"
                    : "packages"}
                  )
                </button>
              </div>
            </div>
          </div>
        )}
      {dependencyHeaderOpen && runtime === "deno" && (
        <DenoDependencyHeader
          denoAvailable={denoAvailable}
          denoConfigInfo={denoConfigInfo}
          flexibleNpmImports={flexibleNpmImports}
          onSetFlexibleNpmImports={setFlexibleNpmImports}
        />
      )}
      {dependencyHeaderOpen && runtime === "python" && envType === "conda" && (
        <CondaDependencyHeader
          dependencies={condaDependencies?.dependencies ?? []}
          channels={condaDependencies?.channels ?? []}
          python={condaDependencies?.python ?? null}
          loading={condaDepsLoading}
          syncing={condaSyncing}
          syncState={condaSyncState}
          syncedWhileRunning={condaSyncedWhileRunning}
          needsKernelRestart={condaNeedsKernelRestart}
          onAdd={addCondaDependency}
          onRemove={removeCondaDependency}
          onSetChannels={setCondaChannels}
          onSetPython={setCondaPython}
          onSyncNow={syncCondaNow}
          envProgress={envProgress.envType === "conda" ? envProgress : null}
          onResetProgress={envProgress.reset}
          environmentYmlInfo={environmentYmlInfo}
          environmentYmlDeps={environmentYmlDeps}
          pixiInfo={pixiInfo}
          onImportFromPixi={importFromPixi}
        />
      )}
      {dependencyHeaderOpen && runtime === "python" && envType !== "conda" && (
        <DependencyHeader
          dependencies={dependencies?.dependencies ?? []}
          requiresPython={dependencies?.requires_python ?? null}
          uvAvailable={uvAvailable}
          loading={depsLoading}
          syncedWhileRunning={syncedWhileRunning}
          needsKernelRestart={needsKernelRestart}
          onAdd={addDependency}
          onRemove={removeDependency}
          syncState={syncState}
          onSyncNow={syncNow}
          pyprojectInfo={pyprojectInfo}
          pyprojectDeps={pyprojectDeps}
          onImportFromPyproject={importFromPyproject}
          onUseProjectEnv={handleStartKernelWithPyproject}
          isUsingProjectEnv={envSource === "uv:pyproject"}
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
      <NotebookView
        cells={cells}
        focusedCellId={focusedCellId}
        executingCellIds={executingCellIds}
        pagePayloads={pagePayloads}
        runtime={runtime}
        onFocusCell={setFocusedCellId}
        onUpdateCellSource={updateCellSource}
        onExecuteCell={handleExecuteCell}
        onInterruptKernel={interruptKernel}
        onDeleteCell={deleteCell}
        onAddCell={handleAddCell}
        onClearPagePayload={clearPagePayload}
        onFormatCell={formatCell}
      />
    </div>
  );
}

function AppErrorFallback(_error: Error, resetErrorBoundary: () => void) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-4 bg-background p-8">
      <div className="text-center">
        <h1 className="text-lg font-semibold text-foreground">
          Something went wrong
        </h1>
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

export default function App() {
  return (
    <ErrorBoundary fallback={AppErrorFallback}>
      <WidgetStoreProvider sendMessage={sendMessage}>
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
