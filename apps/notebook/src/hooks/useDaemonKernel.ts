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
  type CommDiffState,
  type DaemonQueueState,
  deriveEnvSyncState,
  deriveKernelInfo,
  deriveQueueState,
  detectUnresolvedOutputs,
  diffComms,
  isKernelStatus,
  KERNEL_STATUS,
  type KernelStatus,
  type NotebookClient,
  type NotebookResponse,
} from "runtimed";
import {
  getBlobPort,
  refreshBlobPort,
  resetBlobPort,
  useBlobPort,
} from "../lib/blob-port";
import { replaceSentinelsWithBlobUrls } from "../lib/blob-sentinel";
import { logger } from "../lib/logger";
import { subscribeBroadcast } from "../lib/notebook-frame-bus";
import {
  diffExecutions,
  type ExecutionState,
  type RuntimeState,
  resetRuntimeState,
  setRuntimeState,
  useRuntimeState,
} from "../lib/runtime-state";
import type { DaemonBroadcast, JupyterMessage, JupyterOutput } from "../types";
import { resolveOutputValue } from "./useManifestResolver";

// ── Output widget manifest resolution ───────────────────────────────

/**
 * Resolve Output widget manifest hashes to JupyterOutput objects.
 *
 * Uses a generation counter per comm_id to discard stale async completions
 * when a newer CRDT update arrives before the old fetch finishes.
 */
const _outputResolveGen = new Map<string, number>();

function resolveCommOutputHashes(
  commId: string,
  state: Record<string, unknown>,
  callbacksRef: {
    readonly current: { onCommMessage?: (msg: JupyterMessage) => void };
  },
): void {
  const detected = detectUnresolvedOutputs(state);
  if (!detected) {
    // Bump generation so any in-flight fetch is discarded (for OutputModel with empty outputs)
    if (state._model_name === "OutputModel") {
      _outputResolveGen.set(commId, (_outputResolveGen.get(commId) ?? 0) + 1);
    }
    return;
  }

  const blobPort = getBlobPort();
  if (blobPort === null) return; // Will retry on next CRDT update

  const gen = (_outputResolveGen.get(commId) ?? 0) + 1;
  _outputResolveGen.set(commId, gen);

  void (async () => {
    const resolved = await Promise.all(
      detected.outputs.map((h) => resolveOutputValue(h, blobPort)),
    );
    if (_outputResolveGen.get(commId) !== gen) return;

    const resolvedOutputs = resolved.filter(
      (o): o is JupyterOutput => o !== null,
    );
    const cb = callbacksRef.current?.onCommMessage;
    if (cb) {
      cb({
        header: {
          msg_id: crypto.randomUUID(),
          msg_type: "comm_msg",
          session: "",
          username: "kernel",
          date: new Date().toISOString(),
          version: "5.3",
        },
        metadata: {},
        content: {
          comm_id: commId,
          data: {
            method: "update",
            state: { outputs: resolvedOutputs },
          },
        },
        buffers: [],
      });
    }
  })();
}

// ── Hook types ──────────────────────────────────────────────────────

/** Re-export for backward compatibility */
export type DaemonKernelStatus = KernelStatus;
export type { DaemonQueueState } from "runtimed";

interface UseDaemonKernelOptions {
  /** NotebookClient for sending kernel commands via transport. */
  client: NotebookClient;
  /** Called when an output is produced for a cell (optional — for OutputWidget capture). */
  onOutput?: (cellId: string, output: JupyterOutput) => void;
  /** Called when execution count is set for a cell */
  onExecutionCount: (cellId: string, count: number) => void;
  /** Called when execution completes for a cell */
  onExecutionDone: (cellId: string) => void;
  /** Called when kernel status changes */
  onStatusChange?: (status: KernelStatus, cellId?: string) => void;
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
  client,
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
  const blobPort = useBlobPort();

  const kernelInfo = useMemo(
    () => deriveKernelInfo(runtimeState),
    [runtimeState],
  );

  const queueState = useMemo(
    () => deriveQueueState(runtimeState),
    [runtimeState],
  );

  const envSyncState = useMemo(
    () => deriveEnvSyncState(runtimeState),
    [runtimeState],
  );

  // ── Busy throttle ────────────────────────────────────────────────
  const rawStatus = runtimeState.kernel.status;
  const [throttledStatus, setThrottledStatus] = useState<KernelStatus>(
    isKernelStatus(rawStatus) ? rawStatus : KERNEL_STATUS.NOT_STARTED,
  );
  const busyTimerRef = useRef<number | null>(null);
  const prevRawStatusRef = useRef(rawStatus);

  useEffect(() => {
    const prev = prevRawStatusRef.current;
    prevRawStatusRef.current = rawStatus;
    if (rawStatus === prev) return;
    if (!isKernelStatus(rawStatus)) return;
    const status: KernelStatus = rawStatus;

    if (status === KERNEL_STATUS.BUSY) {
      if (busyTimerRef.current === null) {
        busyTimerRef.current = window.setTimeout(() => {
          busyTimerRef.current = null;
          setThrottledStatus(KERNEL_STATUS.BUSY);
        }, 60);
      }
    } else if (status === KERNEL_STATUS.IDLE) {
      if (busyTimerRef.current !== null) {
        clearTimeout(busyTimerRef.current);
        busyTimerRef.current = null;
      } else {
        setThrottledStatus(status);
      }
    } else {
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
    const executingChanged =
      prev.executing?.cell_id !== queueState.executing?.cell_id;
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
      const broadcast = payload as DaemonBroadcast;

      switch (broadcast.event) {
        case "output": {
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
            // Parse the broadcast JSON string into a manifest/output object
            let parsedOutput: unknown;
            try {
              parsedOutput = JSON.parse(outputJson);
            } catch {
              logger.warn(
                "[daemon-kernel] Failed to parse output_json from broadcast",
              );
              return;
            }
            const output = await resolveOutputValue(parsedOutput, port);
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
          callbacksRef.current.onUpdateDisplayData?.(
            broadcast.display_id,
            broadcast.data,
            broadcast.metadata,
          );
          break;
        }

        case "outputs_cleared": {
          callbacksRef.current.onClearOutputs?.(broadcast.cell_id);
          break;
        }

        case "comm": {
          const { onCommMessage: cb } = callbacksRef.current;
          if (cb) {
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
              buffers: broadcast.buffers?.length
                ? broadcast.buffers.map(
                    (arr: number[]) => new Uint8Array(arr).buffer,
                  )
                : [],
            };
            cb(msg);
          }
          break;
        }

        case "env_progress":
          break;

        case "runtime_state_snapshot": {
          setRuntimeState(broadcast.state as RuntimeState);
          break;
        }

        case "kernel_error": {
          callbacksRef.current.onKernelError?.(broadcast.error);
          break;
        }

        default: {
          const event = (broadcast as { event?: string }).event;
          if (event !== undefined) {
            logger.debug(`[daemon-kernel] Unknown broadcast event: ${event}`);
          }
        }
      }
    });

    // Listen for daemon disconnection
    const unlistenDisconnect = webview.listen(
      "daemon:disconnected",
      async () => {
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
      },
    );

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

  // ── Comm state diffing → WidgetStore ──────────────────────────────
  const commDiffStateRef = useRef<CommDiffState>({
    comms: {},
    json: {},
  });

  useEffect(() => {
    const { onCommMessage: commCb } = callbacksRef.current;
    if (!commCb) return;

    const docComms = runtimeState.comms ?? {};
    const hasBlobPort = blobPort !== null;

    const { result, next } = diffComms(commDiffStateRef.current, docComms);

    // Process opened comms — synthesize comm_open
    for (const { commId, entry } of result.opened) {
      if (!hasBlobPort) {
        // Remove from next state so it retries on next CRDT update
        delete next.comms[commId];
        delete next.json[commId];
        continue;
      }
      const { state: resolvedState, bufferPaths } =
        replaceSentinelsWithBlobUrls(entry.state as Record<string, unknown>);
      commCb({
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
          comm_id: commId,
          target_name: entry.target_name,
          data: {
            state: {
              ...resolvedState,
              _model_module: entry.model_module || undefined,
              _model_name: entry.model_name || undefined,
            },
            buffer_paths: bufferPaths,
          },
        },
        buffers: [],
      });
      resolveCommOutputHashes(commId, entry.state, callbacksRef);
    }

    // Process updated comms — synthesize comm_msg(update)
    for (const { commId, entry } of result.updated) {
      const { state: resolvedState, bufferPaths } =
        replaceSentinelsWithBlobUrls(entry.state as Record<string, unknown>);
      commCb({
        header: {
          msg_id: crypto.randomUUID(),
          msg_type: "comm_msg",
          session: "",
          username: "kernel",
          date: new Date().toISOString(),
          version: "5.3",
        },
        metadata: {},
        content: {
          comm_id: commId,
          data: {
            method: "update",
            state: resolvedState,
            buffer_paths: bufferPaths,
          },
        },
        buffers: [],
      });
      resolveCommOutputHashes(commId, entry.state, callbacksRef);
    }

    // Process closed comms — synthesize comm_close
    for (const commId of result.closed) {
      commCb({
        header: {
          msg_id: crypto.randomUUID(),
          msg_type: "comm_close",
          session: "",
          username: "kernel",
          date: new Date().toISOString(),
          version: "5.3",
        },
        metadata: {},
        content: { comm_id: commId },
        buffers: [],
      });
    }

    commDiffStateRef.current = next;
    // biome-ignore lint/correctness/useExhaustiveDependencies: blobPort triggers retry for comms deferred due to missing blob server
  }, [runtimeState.comms, blobPort]);

  // ── Actions (via NotebookClient) ──────────────────────────────────

  const launchKernel = useCallback(
    (kernelType: string, envSource: string, notebookPath?: string) =>
      client.launchKernel(
        kernelType,
        envSource,
        notebookPath,
      ) as Promise<NotebookResponse>,
    [client],
  );

  const executeCell = useCallback(
    (cellId: string) => client.executeCell(cellId) as Promise<NotebookResponse>,
    [client],
  );

  const clearOutputs = useCallback(
    (cellId: string) =>
      client.clearOutputs(cellId) as Promise<NotebookResponse>,
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
    () => client.syncEnvironment() as Promise<NotebookResponse>,
    [client],
  );

  const runAllCells = useCallback(
    () => client.runAllCells() as Promise<NotebookResponse>,
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
    startingPhase: runtimeState.kernel.starting_phase,
    queueState,
    kernelInfo,
    envSyncState,
    launchKernel,
    executeCell,
    clearOutputs,
    interruptKernel,
    shutdownKernel,
    syncEnvironment,
    runAllCells,
    sendCommMessage,
    isCellExecuting: (cellId: string) =>
      queueState.executing?.cell_id === cellId,
    isCellQueued: (cellId: string) =>
      queueState.executing?.cell_id === cellId ||
      queueState.queued.some((entry) => entry.cell_id === cellId),
  };
}
