/**
 * Hook for daemon-owned kernel execution.
 *
 * This hook provides an interface to the daemon's kernel management,
 * enabling multi-window kernel sharing. The daemon owns the kernel lifecycle
 * and execution queue, broadcasting outputs to all connected windows.
 *
 * Note: This is separate from useKernel.ts which manages local kernels.
 * Use this when daemon execution is enabled; use useKernel for local execution.
 */

import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { useCallback, useEffect, useRef, useState } from "react";
import type {
  DaemonBroadcast,
  DaemonNotebookResponse,
  JupyterMessage,
  JupyterOutput,
} from "../types";

/** Kernel status from daemon */
export type DaemonKernelStatus =
  | "not_started"
  | "starting"
  | "idle"
  | "busy"
  | "error"
  | "shutdown";

/** Queue state from daemon */
export interface DaemonQueueState {
  executing: string | null;
  queued: string[];
}

interface UseDaemonKernelOptions {
  /** Called when an output is produced for a cell */
  onOutput: (cellId: string, output: JupyterOutput) => void;
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
  const [kernelStatus, setKernelStatus] =
    useState<DaemonKernelStatus>("not_started");
  const [queueState, setQueueState] = useState<DaemonQueueState>({
    executing: null,
    queued: [],
  });
  const [kernelInfo, setKernelInfo] = useState<{
    kernelType?: string;
    envSource?: string;
  }>({});

  // Store callbacks in refs to avoid effect re-runs
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

  // Listen for daemon broadcasts
  useEffect(() => {
    let cancelled = false;

    const unlistenBroadcast = listen<DaemonBroadcast>(
      "daemon:broadcast",
      (event) => {
        if (cancelled) return;

        const broadcast = event.payload;

        switch (broadcast.event) {
          case "kernel_status": {
            const status = broadcast.status as DaemonKernelStatus;
            setKernelStatus(status);
            callbacksRef.current.onStatusChange?.(status, broadcast.cell_id);
            break;
          }

          case "execution_started": {
            callbacksRef.current.onExecutionCount(
              broadcast.cell_id,
              broadcast.execution_count,
            );
            break;
          }

          case "output": {
            try {
              // Parse output - daemon now sends nbformat shape directly
              const output = JSON.parse(broadcast.output_json) as JupyterOutput;
              callbacksRef.current.onOutput(broadcast.cell_id, output);
            } catch (e) {
              console.error("[daemon-kernel] Failed to parse output:", e);
            }
            break;
          }

          case "display_update": {
            // Update an existing output by display_id (e.g., progress bars)
            const { onUpdateDisplayData } = callbacksRef.current;
            if (onUpdateDisplayData) {
              onUpdateDisplayData(
                broadcast.display_id,
                broadcast.data,
                broadcast.metadata,
              );
            }
            break;
          }

          case "execution_done": {
            callbacksRef.current.onExecutionDone(broadcast.cell_id);
            break;
          }

          case "queue_changed": {
            const newState: DaemonQueueState = {
              executing: broadcast.executing ?? null,
              queued: broadcast.queued,
            };
            setQueueState(newState);
            callbacksRef.current.onQueueChange?.(newState);
            break;
          }

          case "kernel_error": {
            setKernelStatus("error");
            callbacksRef.current.onKernelError?.(broadcast.error);
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
        }
      },
    );

    // Listen for daemon disconnection (e.g., daemon restarted)
    const unlistenDisconnect = listen("daemon:disconnected", async () => {
      if (cancelled) return;
      console.warn(
        "[daemon-kernel] Daemon disconnected, resetting kernel state",
      );
      setKernelStatus("not_started");
      setKernelInfo({});
      setQueueState({ executing: null, queued: [] });

      // Attempt to reconnect to the daemon
      console.log("[daemon-kernel] Attempting to reconnect to daemon...");
      try {
        await invoke("reconnect_to_daemon");
        console.log("[daemon-kernel] Reconnected to daemon");
      } catch (e) {
        console.error("[daemon-kernel] Failed to reconnect:", e);
      }
    });

    // Get initial kernel info from daemon
    invoke<DaemonNotebookResponse>("get_daemon_kernel_info")
      .then((response) => {
        if (cancelled) return;
        if (response.result === "kernel_info") {
          setKernelInfo({
            kernelType: response.kernel_type,
            envSource: response.env_source,
          });
          setKernelStatus(response.status as DaemonKernelStatus);
        }
      })
      .catch((e) => {
        console.error("[daemon-kernel] Failed to get kernel info:", e);
      });

    return () => {
      cancelled = true;
      unlistenBroadcast.then((fn) => fn());
      unlistenDisconnect.then((fn) => fn());
    };
  }, []);

  /** Launch a kernel via the daemon */
  const launchKernel = useCallback(
    async (
      kernelType: string,
      envSource: string,
      notebookPath?: string,
    ): Promise<DaemonNotebookResponse> => {
      console.log(
        "[daemon-kernel] launching kernel:",
        kernelType,
        envSource,
        notebookPath,
      );
      setKernelStatus("starting");

      try {
        const response = await invoke<DaemonNotebookResponse>(
          "launch_kernel_via_daemon",
          { kernelType, envSource, notebookPath },
        );

        if (
          response.result === "kernel_launched" ||
          response.result === "kernel_already_running"
        ) {
          setKernelInfo({
            kernelType: response.kernel_type,
            envSource: response.env_source,
          });
          setKernelStatus("idle");
        } else if (response.result === "error") {
          setKernelStatus("error");
        }

        return response;
      } catch (e) {
        console.error("[daemon-kernel] launch failed:", e);
        setKernelStatus("error");
        throw e;
      }
    },
    [],
  );

  /** Queue a cell for execution via the daemon */
  const queueCell = useCallback(
    async (cellId: string, code: string): Promise<DaemonNotebookResponse> => {
      console.log("[daemon-kernel] queueing cell:", cellId);
      try {
        return await invoke<DaemonNotebookResponse>("queue_cell_via_daemon", {
          cellId,
          code,
        });
      } catch (e) {
        console.error("[daemon-kernel] queue failed:", e);
        throw e;
      }
    },
    [],
  );

  /** Clear outputs for a cell via the daemon */
  const clearOutputs = useCallback(
    async (cellId: string): Promise<DaemonNotebookResponse> => {
      console.log("[daemon-kernel] clearing outputs:", cellId);
      try {
        return await invoke<DaemonNotebookResponse>(
          "clear_outputs_via_daemon",
          {
            cellId,
          },
        );
      } catch (e) {
        console.error("[daemon-kernel] clear outputs failed:", e);
        throw e;
      }
    },
    [],
  );

  /** Interrupt kernel execution via the daemon */
  const interruptKernel =
    useCallback(async (): Promise<DaemonNotebookResponse> => {
      console.log("[daemon-kernel] interrupting kernel");
      try {
        return await invoke<DaemonNotebookResponse>("interrupt_via_daemon");
      } catch (e) {
        console.error("[daemon-kernel] interrupt failed:", e);
        throw e;
      }
    }, []);

  /** Shutdown the kernel via the daemon */
  const shutdownKernel =
    useCallback(async (): Promise<DaemonNotebookResponse> => {
      console.log("[daemon-kernel] shutting down kernel");
      try {
        const response = await invoke<DaemonNotebookResponse>(
          "shutdown_kernel_via_daemon",
        );
        setKernelStatus("not_started");
        setKernelInfo({});
        return response;
      } catch (e) {
        console.error("[daemon-kernel] shutdown failed:", e);
        throw e;
      }
    }, []);

  /** Get current queue state from daemon */
  const refreshQueueState = useCallback(async () => {
    try {
      const response = await invoke<DaemonNotebookResponse>(
        "get_daemon_queue_state",
      );
      if (response.result === "queue_state") {
        setQueueState({
          executing: response.executing ?? null,
          queued: response.queued,
        });
      }
    } catch (e) {
      console.error("[daemon-kernel] get queue state failed:", e);
    }
  }, []);

  /** Run all code cells via the daemon (reads from synced doc) */
  const runAllCells = useCallback(async (): Promise<DaemonNotebookResponse> => {
    console.log("[daemon-kernel] running all cells");
    try {
      return await invoke<DaemonNotebookResponse>("run_all_cells_via_daemon");
    } catch (e) {
      console.error("[daemon-kernel] run all cells failed:", e);
      throw e;
    }
  }, []);

  /** Send a comm message to the kernel via the daemon (for widget interactions) */
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
      console.log("[daemon-kernel] sending comm message:", msgType);
      try {
        // Convert ArrayBuffer[] to number[][] for JSON serialization
        const buffers: number[][] = (message.buffers ?? []).map((buf) =>
          Array.from(new Uint8Array(buf)),
        );

        // Send the full message envelope to preserve header/session
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
          console.error("[daemon-kernel] send comm failed:", response.error);
        } else if (response.result === "no_kernel") {
          console.error("[daemon-kernel] send comm failed: no kernel running");
        }
      } catch (e) {
        console.error("[daemon-kernel] send comm failed:", e);
        throw e;
      }
    },
    [],
  );

  return {
    /** Current kernel status */
    kernelStatus,
    /** Current execution queue state */
    queueState,
    /** Kernel type and environment source */
    kernelInfo,
    /** Launch a kernel via the daemon */
    launchKernel,
    /** Queue a cell for execution */
    queueCell,
    /** Clear outputs for a cell */
    clearOutputs,
    /** Interrupt kernel execution */
    interruptKernel,
    /** Shutdown the kernel */
    shutdownKernel,
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
