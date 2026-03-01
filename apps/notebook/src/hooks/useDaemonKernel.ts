/**
 * Hook for daemon-owned kernel execution.
 *
 * This hook provides an interface to the daemon's kernel management,
 * enabling multi-window kernel sharing. The daemon owns the kernel lifecycle
 * and execution queue, broadcasting outputs to all connected windows.
 */

import { invoke } from "@tauri-apps/api/core";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { useCallback, useEffect, useRef, useState } from "react";
import type {
  DaemonBroadcast,
  DaemonNotebookResponse,
  JupyterMessage,
  JupyterOutput,
} from "../types";
import {
  fetchBlobPortWithRetry,
  resolveOutputString,
} from "./useManifestResolver";

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

  // Store blob port in ref for use in event handlers
  const blobPortRef = useRef<number>(0);

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
    const webview = getCurrentWebview();

    // Helper to refresh blob port (called on mount, reconnect, and daemon:ready)
    const refreshBlobPort = () => {
      fetchBlobPortWithRetry().then((port) => {
        if (port && !cancelled) {
          blobPortRef.current = port;
        }
      });
    };

    // Fetch blob port for manifest resolution
    refreshBlobPort();

    const unlistenBroadcast = webview.listen<DaemonBroadcast>(
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
            // Resolve output (may be blob hash or raw JSON)
            const cellId = broadcast.cell_id;
            const outputJson = broadcast.output_json;

            // Helper to resolve with retry if port is unavailable or stale
            const resolveWithRetry = async (retried = false) => {
              let port = blobPortRef.current;
              // If port not yet available, try to fetch it
              if (!port) {
                const freshPort = await fetchBlobPortWithRetry();
                if (freshPort) {
                  blobPortRef.current = freshPort;
                  port = freshPort;
                }
              }
              if (!port) {
                console.error(
                  "[daemon-kernel] Blob port unavailable, cannot resolve output",
                );
                return;
              }
              const output = await resolveOutputString(outputJson, port);
              if (cancelled) return;
              if (output) {
                callbacksRef.current.onOutput(cellId, output);
              } else if (!retried) {
                // Resolution failed - port may be stale, refresh and retry once
                console.warn(
                  "[daemon-kernel] Output resolution failed, refreshing port and retrying",
                );
                blobPortRef.current = 0;
                await resolveWithRetry(true);
              } else {
                console.error(
                  "[daemon-kernel] Failed to resolve output for cell:",
                  cellId,
                );
              }
            };

            resolveWithRetry().catch((e) => {
              console.error("[daemon-kernel] Failed to resolve output:", e);
            });
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

          case "comm_sync": {
            // Initial comm state sync from daemon for multi-window widget reconstruction
            // Replay all comms as comm_open messages to the widget store
            const { onCommMessage } = callbacksRef.current;
            console.log(
              `[daemon-kernel] Received comm_sync event with ${broadcast.comms?.length ?? 0} comms, handler=${!!onCommMessage}`,
            );
            if (onCommMessage && broadcast.comms) {
              console.log(
                `[daemon-kernel] Processing comm_sync: replaying ${broadcast.comms.length} comms`,
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
              console.warn(
                "[daemon-kernel] comm_sync received but onCommMessage not set!",
              );
            }
            break;
          }

          case "env_progress":
            // Handled by useEnvProgress hook's own daemon:broadcast listener
            break;

          default: {
            // Log unknown events to help debug unexpected broadcast types
            console.log(
              `[daemon-kernel] Unknown broadcast event: ${(broadcast as { event: string }).event}`,
            );
          }
        }
      },
    );

    // Helper to fetch kernel info with retry for "not_started" status
    // (kernel may still be auto-launching when daemon:ready fires)
    const fetchKernelInfo = (retryCount = 0) => {
      invoke<DaemonNotebookResponse>("get_daemon_kernel_info")
        .then((response) => {
          if (cancelled) return;
          if (response.result === "kernel_info") {
            console.log(
              "[daemon-kernel] Got kernel info:",
              response.status,
              response.kernel_type,
              `(retry ${retryCount})`,
            );

            // If kernel is not started and we haven't retried too many times,
            // wait a bit and try again (kernel may be auto-launching)
            if (response.status === "not_started" && retryCount < 5) {
              setTimeout(() => {
                if (!cancelled) fetchKernelInfo(retryCount + 1);
              }, 500);
              return;
            }

            setKernelInfo({
              kernelType: response.kernel_type,
              envSource: response.env_source,
            });
            setKernelStatus(response.status as DaemonKernelStatus);
          }
        })
        .catch(() => {
          // Expected to fail if daemon isn't ready - daemon:ready listener will retry
        });
    };

    // Listen for daemon disconnection (e.g., daemon restarted)
    const unlistenDisconnect = webview.listen(
      "daemon:disconnected",
      async () => {
        if (cancelled) return;
        console.warn(
          "[daemon-kernel] Daemon disconnected, resetting kernel state",
        );
        setKernelStatus("not_started");
        setKernelInfo({});
        setQueueState({ executing: null, queued: [] });
        // Reset blob port so next output triggers fresh fetch
        blobPortRef.current = 0;

        // Attempt to reconnect to the daemon
        console.log("[daemon-kernel] Attempting to reconnect to daemon...");
        try {
          await invoke("reconnect_to_daemon");
          console.log(
            "[daemon-kernel] Reconnected to daemon, fetching kernel info",
          );
          // After reconnecting, refresh blob port (daemon may have new port) and kernel info
          refreshBlobPort();
          fetchKernelInfo();
        } catch (e) {
          console.error("[daemon-kernel] Failed to reconnect:", e);
        }
      },
    );

    // Listen for daemon ready signal
    const unlistenReady = webview.listen("daemon:ready", () => {
      if (cancelled) return;
      console.log(
        "[daemon-kernel] Daemon ready, refreshing blob port and kernel info",
      );
      refreshBlobPort();
      fetchKernelInfo();
    });

    // Also try immediately in case daemon is already ready
    // (handles page reload when daemon is already connected)
    fetchKernelInfo();

    return () => {
      cancelled = true;
      unlistenBroadcast.then((fn) => fn()).catch(() => {});
      unlistenDisconnect.then((fn) => fn()).catch(() => {});
      unlistenReady.then((fn) => fn()).catch(() => {});
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

  /** Execute a cell via the daemon (reads source from synced document) */
  const executeCell = useCallback(
    async (cellId: string): Promise<DaemonNotebookResponse> => {
      console.log("[daemon-kernel] executing cell:", cellId);
      try {
        return await invoke<DaemonNotebookResponse>("execute_cell_via_daemon", {
          cellId,
        });
      } catch (e) {
        console.error("[daemon-kernel] execute failed:", e);
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
    /** Execute a cell (reads source from synced document) */
    executeCell,
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
