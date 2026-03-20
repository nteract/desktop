/**
 * Hook for daemon-owned kernel execution.
 *
 * State (kernel status, queue, env sync) is derived from the daemon's
 * RuntimeStateDoc via `useRuntimeState()`. Broadcasts are only used for
 * event callbacks (execution lifecycle, outputs, comms).
 */

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getBlobPort, refreshBlobPort, resetBlobPort } from "../lib/blob-port";
import {
  isKernelStatus,
  KERNEL_STATUS,
  type KernelStatus,
} from "../lib/kernel-status";
import { logger } from "../lib/logger";
import { subscribeBroadcast } from "../lib/notebook-frame-bus";
import { resetRuntimeState, useRuntimeState } from "../lib/runtime-state";
import type {
  DaemonBroadcast,
  DaemonNotebookResponse,
  JupyterMessage,
  JupyterOutput,
} from "../types";
import { resolveOutputString } from "./useManifestResolver";

/** Kernel status from daemon */
export type DaemonKernelStatus = KernelStatus;

/** Queue state from daemon */
export interface DaemonQueueState {
  executing: string | null;
  queued: string[];
}

interface UseDaemonKernelOptions {
  /** Called when an output is produced for a cell.
   * Optional — when omitted, Output broadcast processing (including blob
   * resolution) is skipped entirely. Sync delivers outputs via materializeCells.
   * Provide a callback for OutputWidget capture or low-latency streaming. */
  onOutput?: (cellId: string, output: JupyterOutput) => void;
  /** Called when execution count is set for a cell */
  onExecutionCount: (cellId: string, count: number) => void;
  /** Called when execution completes for a cell */
  onExecutionDone: (cellId: string) => void;
  /** Called when kernel status changes */
  onStatusChange?: (status: DaemonKernelStatus, cellId?: string) => void;
  /** Called when queue state changes */
  onQueueChange?: (state: DaemonQueueState) => void;
  /** Called on kernel error */
  onKernelError?: (error: string) => void;
  /** Called when a display_data output should be updated by display_id */
  onUpdateDisplayData?: (
    displayId: string,
    data: Record<string, unknown>,
    metadata: Record<string, unknown>,
  ) => void;
  /** Called when outputs are cleared for a cell (broadcast from another window) */
  onClearOutputs?: (cellId: string) => void;
  /** Called when a comm message is received (for widgets) */
  onCommMessage?: (msg: JupyterMessage) => void;
}

export function useDaemonKernel({
  onOutput,
  onExecutionCount,
  onExecutionDone,
  onStatusChange,
  onQueueChange,
  onKernelError,
  onUpdateDisplayData,
  onClearOutputs,
  onCommMessage,
}: UseDaemonKernelOptions) {
  // ── State from RuntimeStateDoc (daemon-authoritative) ─────────────
  const runtimeState = useRuntimeState();

  // Derive kernel info from the doc
  const kernelInfo = useMemo(
    () => ({
      kernelType: runtimeState.kernel.language || undefined,
      envSource: runtimeState.kernel.env_source || undefined,
    }),
    [runtimeState.kernel.language, runtimeState.kernel.env_source],
  );

  // Derive queue state from the doc
  const queueState: DaemonQueueState = useMemo(
    () => ({
      executing: runtimeState.queue.executing,
      queued: runtimeState.queue.queued,
    }),
    [runtimeState.queue.executing, runtimeState.queue.queued],
  );

  // Derive env sync state from the doc
  const envSyncState = useMemo(() => {
    // Before any kernel launch, env state is default (in_sync: true, empty lists).
    // Return null to indicate "unknown" to consumers, matching prior behavior.
    if (
      runtimeState.kernel.status === "not_started" &&
      !runtimeState.kernel.env_source
    ) {
      return null;
    }
    return {
      inSync: runtimeState.env.in_sync,
      diff: runtimeState.env.in_sync
        ? undefined
        : {
            added: runtimeState.env.added,
            removed: runtimeState.env.removed,
            channelsChanged: runtimeState.env.channels_changed,
            denoChanged: runtimeState.env.deno_changed,
          },
    };
  }, [
    runtimeState.kernel.status,
    runtimeState.kernel.env_source,
    runtimeState.env.in_sync,
    runtimeState.env.added,
    runtimeState.env.removed,
    runtimeState.env.channels_changed,
    runtimeState.env.deno_changed,
  ]);

  // ── Busy throttle ────────────────────────────────────────────────
  //
  // The RuntimeStateDoc faithfully records every busy→idle transition
  // from the kernel, including sub-60ms blips from tab completions.
  // We apply the same throttle the broadcast path used: only show "busy"
  // if it persists past a 60ms threshold.

  const rawStatus = runtimeState.kernel.status;
  const [throttledStatus, setThrottledStatus] = useState<DaemonKernelStatus>(
    isKernelStatus(rawStatus) ? rawStatus : KERNEL_STATUS.NOT_STARTED,
  );
  const busyTimerRef = useRef<number | null>(null);
  const prevRawStatusRef = useRef(rawStatus);

  useEffect(() => {
    const prev = prevRawStatusRef.current;
    prevRawStatusRef.current = rawStatus;

    // Skip if status didn't actually change
    if (rawStatus === prev) return;

    if (!isKernelStatus(rawStatus)) return;
    const status: DaemonKernelStatus = rawStatus;

    if (status === KERNEL_STATUS.BUSY) {
      // Throttle busy: only show if it persists past threshold
      if (busyTimerRef.current === null) {
        busyTimerRef.current = window.setTimeout(() => {
          busyTimerRef.current = null;
          setThrottledStatus(KERNEL_STATUS.BUSY);
        }, 60);
      }
    } else if (status === KERNEL_STATUS.IDLE) {
      // Cancel pending busy transition if idle arrives quickly
      if (busyTimerRef.current !== null) {
        clearTimeout(busyTimerRef.current);
        busyTimerRef.current = null;
        // Don't update — stay at current status (probably idle already)
      } else {
        setThrottledStatus(status);
      }
    } else {
      // Other statuses (starting, error, shutdown, not_started) shown immediately
      if (busyTimerRef.current !== null) {
        clearTimeout(busyTimerRef.current);
        busyTimerRef.current = null;
      }
      setThrottledStatus(status);
    }

    return () => {
      if (busyTimerRef.current !== null) {
        clearTimeout(busyTimerRef.current);
        busyTimerRef.current = null;
      }
    };
  }, [rawStatus]);

  // The externally visible status uses the throttled value
  const kernelStatus = throttledStatus;

  // ── Callbacks in refs (avoid effect re-runs) ──────────────────────

  const callbacksRef = useRef({
    onOutput,
    onExecutionCount,
    onExecutionDone,
    onStatusChange,
    onQueueChange,
    onKernelError,
    onUpdateDisplayData,
    onClearOutputs,
    onCommMessage,
  });
  callbacksRef.current = {
    onOutput,
    onExecutionCount,
    onExecutionDone,
    onStatusChange,
    onQueueChange,
    onKernelError,
    onUpdateDisplayData,
    onClearOutputs,
    onCommMessage,
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
    if (
      prev.executing !== queueState.executing ||
      prev.queued !== queueState.queued
    ) {
      callbacksRef.current.onQueueChange?.(queueState);
    }
  }, [queueState]);

  // Fire onKernelError when status transitions to error
  useEffect(() => {
    if (kernelStatus === KERNEL_STATUS.ERROR) {
      callbacksRef.current.onKernelError?.("Kernel error");
    }
  }, [kernelStatus]);

  // ── Broadcast listener (events only — no state) ──────────────────

  useEffect(() => {
    let cancelled = false;
    const webview = getCurrentWebview();

    // Ensure blob port is fresh on mount
    refreshBlobPort();

    const unsubscribeBroadcast = subscribeBroadcast((payload) => {
      if (cancelled) return;

      const broadcast = payload as DaemonBroadcast;

      switch (broadcast.event) {
        // ── Events that stay as broadcasts ────────────────────────

        case "execution_started": {
          callbacksRef.current.onExecutionCount(
            broadcast.cell_id,
            broadcast.execution_count,
          );
          break;
        }

        case "output": {
          // Skip blob resolution entirely when no onOutput callback is
          // registered. Sync delivers outputs via materializeCells; the
          // broadcast path is only needed for OutputWidget capture.
          if (!callbacksRef.current.onOutput) break;

          const cellId = broadcast.cell_id;
          const outputJson = broadcast.output_json;

          const resolveWithRetry = async (retried = false) => {
            let port = getBlobPort();
            if (!port) {
              port = await refreshBlobPort();
            }
            if (!port) {
              logger.error(
                "[daemon-kernel] Blob port unavailable, cannot resolve output",
              );
              return;
            }
            const output = await resolveOutputString(outputJson, port);
            if (cancelled) return;
            if (output) {
              callbacksRef.current.onOutput?.(cellId, output);
            } else if (!retried) {
              logger.debug(
                "[daemon-kernel] Output resolution failed, refreshing port",
              );
              resetBlobPort();
              await resolveWithRetry(true);
            } else {
              logger.error(
                "[daemon-kernel] Failed to resolve output for cell:",
                cellId,
              );
            }
          };

          resolveWithRetry().catch((e) => {
            logger.error("[daemon-kernel] Failed to resolve output:", e);
          });
          break;
        }

        case "display_update": {
          const { onUpdateDisplayData: cb } = callbacksRef.current;
          if (cb) {
            cb(broadcast.display_id, broadcast.data, broadcast.metadata);
          }
          break;
        }

        case "execution_done": {
          callbacksRef.current.onExecutionDone(broadcast.cell_id);
          break;
        }

        case "outputs_cleared": {
          callbacksRef.current.onClearOutputs?.(broadcast.cell_id);
          break;
        }

        case "comm": {
          // Comm message from kernel (for widgets)
          const { onCommMessage } = callbacksRef.current;
          if (onCommMessage) {
            // Convert daemon broadcast to JupyterMessage format expected by widget store
            const msg: JupyterMessage = {
              header: {
                msg_id: crypto.randomUUID(),
                msg_type: broadcast.msg_type,
                session: "",
                username: "kernel",
                date: new Date().toISOString(),
                version: "5.3",
              },
              metadata: {},
              content: broadcast.content,
              // Convert number[][] back to ArrayBuffer[] for widgets
              buffers: broadcast.buffers.map(
                (arr) => new Uint8Array(arr).buffer,
              ),
            };
            onCommMessage(msg);
          }
          break;
        }

        case "comm_sync": {
          // Initial comm state sync from daemon for multi-window widget reconstruction
          // Replay all comms as comm_open messages to the widget store
          const { onCommMessage } = callbacksRef.current;
          if (onCommMessage && broadcast.comms) {
            logger.debug(
              `[daemon-kernel] comm_sync: replaying ${broadcast.comms.length} comms`,
            );
            for (const comm of broadcast.comms) {
              // Synthesize a comm_open message for each active comm
              const msg: JupyterMessage = {
                header: {
                  msg_id: crypto.randomUUID(),
                  msg_type: "comm_open",
                  session: "",
                  username: "kernel",
                  date: new Date().toISOString(),
                  version: "5.3",
                },
                metadata: {},
                content: {
                  comm_id: comm.comm_id,
                  target_name: comm.target_name,
                  data: {
                    state: comm.state,
                    buffer_paths: [],
                  },
                },
                // Convert buffers if present
                buffers: comm.buffers
                  ? comm.buffers.map((arr) => new Uint8Array(arr).buffer)
                  : [],
              };
              onCommMessage(msg);
            }
          } else if (!onCommMessage) {
            logger.debug(
              "[daemon-kernel] comm_sync received but onCommMessage not set",
            );
          }
          break;
        }

        case "env_progress":
          // Handled by useEnvProgress hook's own frame bus subscriber
          break;

        // ── State broadcasts — now read from RuntimeStateDoc ─────
        // Keep cases to avoid "unknown broadcast" log spam, but don't
        // set state — the RuntimeStateDoc is the source of truth.

        case "kernel_status":
        case "queue_changed":
        case "kernel_error":
        case "env_sync_state":
          break;

        default: {
          logger.debug(
            `[daemon-kernel] Unknown broadcast event: ${(broadcast as { event: string }).event}`,
          );
        }
      }
    });

    // Listen for daemon disconnection
    const unlistenDisconnect = webview.listen(
      "daemon:disconnected",
      async () => {
        if (cancelled) return;
        logger.warn("[daemon-kernel] Daemon disconnected, resetting state");
        // Reset RuntimeStateDoc store — next sync will repopulate
        resetRuntimeState();
        resetBlobPort();

        try {
          await invoke("reconnect_to_daemon");
          logger.debug("[daemon-kernel] Reconnected to daemon");
          refreshBlobPort();
        } catch (e) {
          logger.error("[daemon-kernel] Failed to reconnect:", e);
        }
      },
    );

    // Listen for daemon ready signal
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

  // ── Actions ───────────────────────────────────────────────────────

  /** Launch a kernel via the daemon */
  const launchKernel = useCallback(
    async (
      kernelType: string,
      envSource: string,
      notebookPath?: string,
    ): Promise<DaemonNotebookResponse> => {
      logger.debug("[daemon-kernel] Launching kernel:", kernelType, envSource);
      // Don't set status manually — the RuntimeStateDoc will update
      // via sync when the daemon processes the launch.

      try {
        const response = await invoke<DaemonNotebookResponse>(
          "launch_kernel_via_daemon",
          { kernelType, envSource, notebookPath },
        );
        return response;
      } catch (e) {
        logger.error("[daemon-kernel] Launch failed:", e);
        throw e;
      }
    },
    [],
  );

  /** Execute a cell via the daemon (reads source from synced document) */
  const executeCell = useCallback(
    async (cellId: string): Promise<DaemonNotebookResponse> => {
      logger.debug("[daemon-kernel] Executing cell:", cellId);
      try {
        const response = await invoke<DaemonNotebookResponse>(
          "execute_cell_via_daemon",
          { cellId },
        );
        return response;
      } catch (e) {
        logger.error("[daemon-kernel] Execute failed:", e);
        throw e;
      }
    },
    [],
  );

  /** Clear outputs for a cell via the daemon */
  const clearOutputs = useCallback(
    async (cellId: string): Promise<DaemonNotebookResponse> => {
      try {
        const response = await invoke<DaemonNotebookResponse>(
          "clear_outputs_via_daemon",
          { cellId },
        );
        return response;
      } catch (e) {
        logger.error("[daemon-kernel] Clear outputs failed:", e);
        throw e;
      }
    },
    [],
  );

  /** Interrupt kernel execution */
  const interruptKernel = useCallback(async () => {
    try {
      const response = await invoke<DaemonNotebookResponse>(
        "interrupt_via_daemon",
      );
      return response;
    } catch (e) {
      logger.error("[daemon-kernel] Interrupt failed:", e);
      throw e;
    }
  }, []);

  /** Shutdown the kernel */
  const shutdownKernel = useCallback(async () => {
    try {
      const response = await invoke<DaemonNotebookResponse>(
        "shutdown_kernel_via_daemon",
      );
      // Don't set status manually — RuntimeStateDoc will update via sync
      return response;
    } catch (e) {
      logger.error("[daemon-kernel] Shutdown failed:", e);
      throw e;
    }
  }, []);

  /** Hot-sync environment - install new packages without restart (UV only) */
  const syncEnvironment = useCallback(async () => {
    try {
      const response = await invoke<DaemonNotebookResponse>(
        "sync_environment_via_daemon",
      );
      if (response.result === "error") {
        logger.error("[daemon-kernel] Sync env failed:", response.error);
      }
      return response;
    } catch (e) {
      logger.error("[daemon-kernel] Sync environment failed:", e);
      throw e;
    }
  }, []);

  /** Refresh queue state from daemon */
  const refreshQueueState = useCallback(async () => {
    try {
      const response = await invoke<DaemonNotebookResponse>(
        "get_daemon_queue_state",
      );
      if (response.result === "queue_state") {
        // Queue state is now read from RuntimeStateDoc — this is a no-op
        // for state but kept for backward compatibility.
        return {
          executing: response.executing ?? null,
          queued: response.queued,
        };
      }
    } catch (e) {
      logger.error("[daemon-kernel] Refresh queue failed:", e);
    }
    return queueState;
  }, [queueState]);

  /** Run all code cells (daemon reads from synced doc) */
  const runAllCells = useCallback(async (): Promise<DaemonNotebookResponse> => {
    logger.debug("[daemon-kernel] Running all cells");
    try {
      return await invoke<DaemonNotebookResponse>("run_all_cells_via_daemon");
    } catch (e) {
      logger.error("[daemon-kernel] Run all cells failed:", e);
      throw e;
    }
  }, []);

  /** Send a comm message to the kernel (for widget interactions) */
  const sendCommMessage = useCallback(
    async (message: {
      header: Record<string, unknown>;
      parent_header?: Record<string, unknown> | null;
      metadata?: Record<string, unknown>;
      content: Record<string, unknown>;
      buffers?: ArrayBuffer[];
      channel?: string;
    }): Promise<void> => {
      const msgType = message.header.msg_type as string;
      logger.debug("[daemon-kernel] Sending comm message:", msgType);
      try {
        // Convert ArrayBuffer[] to number[][] for JSON serialization
        const buffers: number[][] = (message.buffers ?? []).map((buf) =>
          Array.from(new Uint8Array(buf)),
        );

        const fullMessage = {
          header: message.header,
          parent_header: message.parent_header ?? null,
          metadata: message.metadata ?? {},
          content: message.content,
          buffers,
          channel: message.channel ?? "shell",
        };

        const response = await invoke<DaemonNotebookResponse>(
          "send_comm_via_daemon",
          { message: fullMessage },
        );

        if (response.result === "error") {
          logger.error("[daemon-kernel] Send comm failed:", response.error);
        } else if (response.result === "no_kernel") {
          logger.error("[daemon-kernel] Send comm failed: no kernel running");
        }
      } catch (e) {
        logger.error("[daemon-kernel] Send comm failed:", e);
        throw e;
      }
    },
    [],
  );

  return {
    /** Current kernel status (with busy throttle applied) */
    kernelStatus,
    /** Current execution queue state */
    queueState,
    /** Kernel type and environment source */
    kernelInfo,
    /** Environment sync state - null if unknown, has inSync and diff if known */
    envSyncState,
    /** Launch a kernel via the daemon */
    launchKernel,
    /** Execute a cell (reads source from synced document) */
    executeCell,
    /** Clear outputs for a cell */
    clearOutputs,
    /** Interrupt kernel execution */
    interruptKernel,
    /** Shutdown the kernel */
    shutdownKernel,
    /** Hot-sync environment - install new packages without restart (UV only) */
    syncEnvironment,
    /** Refresh queue state from daemon */
    refreshQueueState,
    /** Run all code cells (daemon reads from synced doc) */
    runAllCells,
    /** Send a comm message to the kernel (for widget interactions) */
    sendCommMessage,
    /** Check if a cell is currently executing */
    isCellExecuting: (cellId: string) => queueState.executing === cellId,
    /** Check if a cell is in the queue */
    isCellQueued: (cellId: string) =>
      queueState.executing === cellId || queueState.queued.includes(cellId),
  };
}
