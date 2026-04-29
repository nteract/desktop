/**
 * Hook for daemon-owned kernel execution.
 *
 * Thin React wrapper around transport-agnostic logic from the `runtimed`
 * package. State (kernel status, queue, env sync) is derived from the
 * daemon's RuntimeStateDoc. Broadcasts are only used for event callbacks.
 */

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  type DaemonQueueState,
  type DependencyGuard,
  deriveEnvSyncState,
  deriveKernelInfo,
  deriveQueueState,
  type GuardedNotebookProvenance,
  type KernelStatus,
  type NotebookClient,
  type NotebookResponse,
  RUNTIME_STATUS,
  isEnvProgressBroadcast,
  runtimeStatusKey,
  type RuntimeStatusKey,
  statusKeyToLegacyStatus,
} from "runtimed";
import { refreshBlobPort, resetBlobPort } from "../lib/blob-port";
import { logger } from "../lib/logger";
import { subscribeBroadcast } from "../lib/notebook-frame-bus";
import {
  diffExecutions,
  type ExecutionState,
  resetRuntimeState,
  useRuntimeState,
} from "../lib/runtime-state";

// ── Hook types ──────────────────────────────────────────────────────

/** Re-export for backward compatibility */
export type DaemonKernelStatus = KernelStatus;
export type { DaemonQueueState } from "runtimed";

interface UseDaemonKernelOptions {
  /** NotebookClient for sending kernel commands via transport. */
  client: NotebookClient;
  /** Called when execution count is set for a cell */
  onExecutionCount: (cellId: string, count: number) => void;
  /** Called when execution completes for a cell */
  onExecutionDone: (cellId: string) => void;
  /** Called when kernel status changes */
  onStatusChange?: (status: KernelStatus, cellId?: string) => void;
  /** Called when queue state changes */
  onQueueChange?: (state: DaemonQueueState) => void;
}

export function useDaemonKernel({
  client,
  onExecutionCount,
  onExecutionDone,
  onStatusChange,
  onQueueChange,
}: UseDaemonKernelOptions) {
  // ── State from RuntimeStateDoc (daemon-authoritative) ─────────────
  const runtimeState = useRuntimeState();

  const kernelInfo = useMemo(() => deriveKernelInfo(runtimeState), [runtimeState]);

  const queueState = useMemo(() => deriveQueueState(runtimeState), [runtimeState]);

  const envSyncState = useMemo(() => deriveEnvSyncState(runtimeState), [runtimeState]);

  // ── Busy throttle ────────────────────────────────────────────────
  // Project the typed lifecycle into the flat `RuntimeStatusKey`
  // vocabulary and throttle the `RUNNING_BUSY` ↔ `RUNNING_IDLE`
  // transition so quick execute/idle cycles don't flash "busy" at the
  // user. Non-Running keys (starting sub-phases, error, shutdown) pass
  // through untouched — they carry richer sub-state the toolbar wants
  // to render verbatim.
  const rawStatusKey = runtimeStatusKey(runtimeState.kernel.lifecycle);
  const [throttledStatusKey, setThrottledStatusKey] = useState<RuntimeStatusKey>(rawStatusKey);
  const busyTimerRef = useRef<number | null>(null);
  const prevRawStatusKeyRef = useRef(rawStatusKey);

  useEffect(() => {
    const prev = prevRawStatusKeyRef.current;
    prevRawStatusKeyRef.current = rawStatusKey;
    if (rawStatusKey === prev) return;

    if (rawStatusKey === RUNTIME_STATUS.RUNNING_BUSY) {
      // Delay committing BUSY by 60ms — if IDLE arrives first, the
      // pending commit is cancelled below and the user never sees a
      // busy flash.
      if (busyTimerRef.current === null) {
        busyTimerRef.current = window.setTimeout(() => {
          busyTimerRef.current = null;
          setThrottledStatusKey(RUNTIME_STATUS.RUNNING_BUSY);
        }, 60);
      }
    } else if (
      rawStatusKey === RUNTIME_STATUS.RUNNING_IDLE ||
      rawStatusKey === RUNTIME_STATUS.RUNNING_UNKNOWN
    ) {
      if (busyTimerRef.current !== null) {
        clearTimeout(busyTimerRef.current);
        busyTimerRef.current = null;
      } else {
        setThrottledStatusKey(rawStatusKey);
      }
    } else {
      if (busyTimerRef.current !== null) {
        clearTimeout(busyTimerRef.current);
        busyTimerRef.current = null;
      }
      setThrottledStatusKey(rawStatusKey);
    }

    return () => {
      if (busyTimerRef.current !== null) {
        clearTimeout(busyTimerRef.current);
        busyTimerRef.current = null;
      }
    };
  }, [rawStatusKey]);

  const statusKey = throttledStatusKey;
  const kernelStatus: KernelStatus = statusKeyToLegacyStatus(statusKey);

  // ── Callbacks in refs (avoid effect re-runs) ──────────────────────
  const callbacksRef = useRef({
    onExecutionCount,
    onExecutionDone,
    onStatusChange,
    onQueueChange,
  });
  callbacksRef.current = {
    onExecutionCount,
    onExecutionDone,
    onStatusChange,
    onQueueChange,
  };

  // ── Fire callbacks when derived state changes ─────────────────────
  const prevThrottledStatusRef = useRef(kernelStatus);
  useEffect(() => {
    const prev = prevThrottledStatusRef.current;
    prevThrottledStatusRef.current = kernelStatus;
    if (kernelStatus !== prev) {
      callbacksRef.current.onStatusChange?.(kernelStatus);
    }
  }, [kernelStatus]);

  const prevQueueRef = useRef(queueState);
  useEffect(() => {
    const prev = prevQueueRef.current;
    prevQueueRef.current = queueState;
    const executingChanged = prev.executing?.cell_id !== queueState.executing?.cell_id;
    let queuedChanged = prev.queued.length !== queueState.queued.length;
    if (!queuedChanged) {
      for (let i = 0; i < prev.queued.length; i++) {
        if (prev.queued[i]?.cell_id !== queueState.queued[i]?.cell_id) {
          queuedChanged = true;
          break;
        }
      }
    }
    if (executingChanged || queuedChanged) {
      callbacksRef.current.onQueueChange?.(queueState);
    }
  }, [queueState]);

  // ── Execution lifecycle transitions (from CRDT) ───────────────────
  const prevExecutionsRef = useRef<Record<string, ExecutionState>>({});
  useEffect(() => {
    const prev = prevExecutionsRef.current;
    const curr = runtimeState.executions;
    prevExecutionsRef.current = curr;

    if (Object.keys(prev).length === 0 && Object.keys(curr).length > 0) {
      return;
    }

    const transitions = diffExecutions(prev, curr);
    for (const t of transitions) {
      if (t.kind === "started") {
        // Only forward when the kernel has actually reported the count
        // (arrives via execute_input, after the queued→running transition).
        // Materialization reads execution_count from RuntimeState directly,
        // so skipping null here avoids a brief flash of "0".
        if (t.execution_count != null) {
          callbacksRef.current.onExecutionCount(t.cell_id, t.execution_count);
        }
      } else {
        callbacksRef.current.onExecutionDone(t.cell_id);
      }
    }
  }, [runtimeState.executions]);

  // ── Broadcast listener (events only — no state) ──────────────────
  useEffect(() => {
    let cancelled = false;
    const webview = getCurrentWebview();
    refreshBlobPort();

    const unsubscribeBroadcast = subscribeBroadcast((payload) => {
      if (cancelled) return;

      // Custom comm messages (buttons, model.send()) are now handled
      // by the SyncEngine.commBroadcasts$ subscriber in App.tsx. Env
      // progress state is daemon-authored in RuntimeStateDoc; the
      // broadcast remains only as a compatibility event for older
      // consumers.
      if (isEnvProgressBroadcast(payload)) {
        return;
      }

      if (
        typeof payload === "object" &&
        payload !== null &&
        "event" in payload &&
        typeof (payload as { event: unknown }).event === "string"
      ) {
        logger.debug(
          `[daemon-kernel] Unknown broadcast event: ${(payload as { event: string }).event}`,
        );
      }
    });

    // Listen for daemon disconnection
    const unlistenDisconnect = webview.listen("daemon:disconnected", async () => {
      if (cancelled) return;
      logger.warn("[daemon-kernel] Daemon disconnected, resetting state");
      resetRuntimeState();
      resetBlobPort();
      try {
        await invoke("reconnect_to_daemon");
        logger.debug("[daemon-kernel] Reconnected to daemon");
        refreshBlobPort();
      } catch (e) {
        logger.error("[daemon-kernel] Failed to reconnect:", e);
      }
    });

    const unlistenReady = webview.listen("daemon:ready", () => {
      if (cancelled) return;
      logger.debug("[daemon-kernel] Daemon ready");
      refreshBlobPort();
    });

    return () => {
      cancelled = true;
      if (busyTimerRef.current !== null) {
        clearTimeout(busyTimerRef.current);
        busyTimerRef.current = null;
      }
      unsubscribeBroadcast();
      unlistenDisconnect.then((fn) => fn()).catch(() => {});
      unlistenReady.then((fn) => fn()).catch(() => {});
    };
  }, []);

  // Comm state projection is now handled by SyncEngine.commChanges$
  // (subscribed in App.tsx). No Jupyter message synthesis needed.

  // ── Actions (via NotebookClient) ──────────────────────────────────

  const launchKernel = useCallback(
    (kernelType: string, envSource: string, notebookPath?: string) =>
      client.launchKernel(kernelType, envSource, notebookPath) as Promise<NotebookResponse>,
    [client],
  );

  const executeCell = useCallback(
    (cellId: string) => client.executeCell(cellId) as Promise<NotebookResponse>,
    [client],
  );

  const executeCellGuarded = useCallback(
    (cellId: string, provenance: GuardedNotebookProvenance) =>
      client.executeCellGuarded(cellId, provenance) as Promise<NotebookResponse>,
    [client],
  );

  const clearOutputs = useCallback(
    (cellId: string) => client.clearOutputs(cellId) as Promise<NotebookResponse>,
    [client],
  );

  const interruptKernel = useCallback(
    () => client.interruptKernel() as Promise<NotebookResponse>,
    [client],
  );

  const shutdownKernel = useCallback(
    () => client.shutdownKernel() as Promise<NotebookResponse>,
    [client],
  );

  const syncEnvironment = useCallback(
    (guard?: DependencyGuard) => client.syncEnvironment(guard) as Promise<NotebookResponse>,
    [client],
  );

  const approveProjectEnvironment = useCallback(
    (projectFilePath?: string) =>
      client.approveProjectEnvironment(projectFilePath) as Promise<NotebookResponse>,
    [client],
  );

  const runAllCells = useCallback(
    () => client.runAllCells() as Promise<NotebookResponse>,
    [client],
  );

  const runAllCellsGuarded = useCallback(
    (provenance: GuardedNotebookProvenance) =>
      client.runAllCellsGuarded(provenance) as Promise<NotebookResponse>,
    [client],
  );

  const sendCommMessage = useCallback(
    (message: {
      header: Record<string, unknown>;
      parent_header?: Record<string, unknown> | null;
      metadata?: Record<string, unknown>;
      content: Record<string, unknown>;
      buffers?: ArrayBuffer[];
      channel?: string;
    }) => client.sendComm(message),
    [client],
  );

  return {
    kernelStatus,
    statusKey,
    lifecycle: runtimeState.kernel.lifecycle,
    errorReason: runtimeState.kernel.error_reason,
    errorDetails: runtimeState.kernel.error_details,
    queueState,
    kernelInfo,
    envSyncState,
    launchKernel,
    executeCell,
    executeCellGuarded,
    clearOutputs,
    interruptKernel,
    shutdownKernel,
    syncEnvironment,
    approveProjectEnvironment,
    runAllCells,
    runAllCellsGuarded,
    sendCommMessage,
    isCellExecuting: (cellId: string) => queueState.executing?.cell_id === cellId,
    isCellQueued: (cellId: string) =>
      queueState.executing?.cell_id === cellId ||
      queueState.queued.some((entry) => entry.cell_id === cellId),
  };
}
