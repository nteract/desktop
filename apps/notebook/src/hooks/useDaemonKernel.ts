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
import { replaceSentinelsWithBlobUrls } from "../lib/blob-sentinel";
import {
  isKernelStatus,
  KERNEL_STATUS,
  type KernelStatus,
} from "../lib/kernel-status";
import { logger } from "../lib/logger";
import { isManifestHash } from "../lib/manifest-resolution";
import { subscribeBroadcast } from "../lib/notebook-frame-bus";
import {
  type CommDocEntry,
  diffExecutions,
  type ExecutionState,
  type QueueEntry,
  type RuntimeState,
  resetRuntimeState,
  setRuntimeState,
  useRuntimeState,
} from "../lib/runtime-state";
import type {
  DaemonBroadcast,
  DaemonNotebookResponse,
  JupyterMessage,
  JupyterOutput,
} from "../types";
import { resolveOutputString } from "./useManifestResolver";

/**
 * If an OutputModel's `state.outputs` contains manifest hash strings, resolve
 * them asynchronously to JupyterOutput objects and deliver a follow-up
 * comm_msg(update) with the resolved outputs. This mirrors how cell outputs
 * are resolved from the CRDT.
 *
 * Only triggers for OutputModel widgets (`_model_name === "OutputModel"`).
 * Other widgets may have an `outputs` field with different semantics.
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
  // Only resolve for OutputModel widgets
  if (state._model_name !== "OutputModel") return;

  const outputs = state.outputs;
  if (!Array.isArray(outputs) || outputs.length === 0) {
    // Bump generation so any in-flight fetch is discarded
    _outputResolveGen.set(commId, (_outputResolveGen.get(commId) ?? 0) + 1);
    return;
  }

  // Verify all entries are manifest hashes (64-char hex strings).
  // If not, this is unexpected — log and skip.
  const allHashes = outputs.every(
    (o) => typeof o === "string" && isManifestHash(o),
  );
  if (!allHashes) {
    if (outputs.some((o) => typeof o !== "string")) {
      // Already resolved objects — skip silently
      return;
    }
    logger.warn(
      `[comm-sync] OutputModel ${commId}: state.outputs contains unexpected format, skipping resolution`,
    );
    return;
  }

  const blobPort = getBlobPort();
  if (blobPort === null) return; // Will retry on next CRDT update

  // Bump generation — any older in-flight fetch will check and bail
  const gen = (_outputResolveGen.get(commId) ?? 0) + 1;
  _outputResolveGen.set(commId, gen);

  void (async () => {
    const resolved = await Promise.all(
      (outputs as string[]).map((h) => resolveOutputString(h, blobPort)),
    );

    // Discard if a newer CRDT update superseded us
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

/** Kernel status from daemon */
export type DaemonKernelStatus = KernelStatus;

/** Queue state from daemon */
export interface DaemonQueueState {
  executing: QueueEntry | null;
  queued: QueueEntry[];
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

  // Cache for resolved output manifests (shared with Output widget CRDT path)

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
    [runtimeState.queue],
  );

  // Derive env sync state from the doc
  const envSyncState = useMemo(() => {
    // Before any kernel launch, env state is default (in_sync: true, empty lists).
    // Return null to indicate "unknown" to consumers, matching prior behavior.
    if (
      (runtimeState.kernel.status === "not_started" &&
        !runtimeState.kernel.env_source) ||
      runtimeState.kernel.status === "shutdown" ||
      runtimeState.kernel.status === "error"
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

  // ── Execution lifecycle transitions (from CRDT, not broadcasts) ───

  const prevExecutionsRef = useRef<Record<string, ExecutionState>>({});
  useEffect(() => {
    const prev = prevExecutionsRef.current;
    const curr = runtimeState.executions;
    prevExecutionsRef.current = curr;

    // Skip the initial empty→populated transition (slow joiner catch-up)
    if (Object.keys(prev).length === 0 && Object.keys(curr).length > 0) {
      return;
    }

    const transitions = diffExecutions(prev, curr);
    for (const t of transitions) {
      if (t.kind === "started") {
        callbacksRef.current.onExecutionCount(
          t.cell_id,
          t.execution_count ?? 0,
        );
      } else {
        // "done" or "error"
        callbacksRef.current.onExecutionDone(t.cell_id);
      }
    }
  }, [runtimeState.executions]);

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

        case "outputs_cleared": {
          callbacksRef.current.onClearOutputs?.(broadcast.cell_id);
          break;
        }

        case "comm": {
          // Custom comm messages only (buttons, model.send()).
          // State updates and lifecycle (open/close) flow through CRDT.
          const { onCommMessage } = callbacksRef.current;
          if (onCommMessage) {
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
            onCommMessage(msg);
          }
          break;
        }

        case "env_progress":
          // Handled by useEnvProgress hook's own frame bus subscriber
          break;

        // State broadcasts — redundant with RuntimeStateDoc.
        // The daemon still sends these for backward compatibility;
        // we silently ignore them. Will be removed from daemon in
        // a future cleanup.
        case "execution_started":
        case "execution_done":
        case "kernel_status":
        case "queue_changed":
        case "env_sync_state":
          break;

        case "runtime_state_snapshot": {
          // Eager snapshot from connection setup — apply immediately so the
          // client has kernel status before the Automerge sync handshake completes.
          setRuntimeState(broadcast.state as RuntimeState);
          break;
        }

        case "kernel_error": {
          // Still consume this broadcast for the detailed error message.
          // Status is derived from RuntimeStateDoc, but the error string
          // only arrives via broadcast.
          callbacksRef.current.onKernelError?.(broadcast.error);
          break;
        }

        default: {
          const event = (broadcast as { event?: string }).event;
          // Internal SyncEngine broadcasts (e.g. text_attribution) use
          // "type" not "event" — skip logging for those.
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

  // ── Sync comms from RuntimeStateDoc → WidgetStore ─────────────────
  //
  // Full lifecycle: the CRDT is the source of truth for widget state.
  // Detect new comms (comm_open), state changes (comm_msg update),
  // and removed comms (comm_close) by diffing against previous state.
  // Blob sentinels in state are resolved to HTTP URLs before delivery.
  // New comms are sorted by seq for correct widget dependency order.
  const prevCommsRef = useRef<Record<string, CommDocEntry>>({});
  const prevCommsJsonRef = useRef<Record<string, string>>({});
  useEffect(() => {
    const { onCommMessage } = callbacksRef.current;
    if (!onCommMessage) return;

    const docComms = runtimeState.comms ?? {};
    const prevComms = prevCommsRef.current;
    const prevJson = prevCommsJsonRef.current;
    const nextComms: Record<string, CommDocEntry> = {};
    const nextJson: Record<string, string> = {};

    // Blob port is needed to resolve {"$blob":"hash"} sentinels in state.
    // Without it, comm_open and state updates would deliver unresolved sentinels
    // which cause DataCloneError in the widget iframe. Skip delivery and leave
    // those comms out of prevCommsRef so the next effect run retries them.
    const hasBlobPort = getBlobPort() !== null;

    // New comms — synthesize comm_open (sorted by seq for dependency order)
    const newEntries = Object.entries(docComms)
      .filter(([commId]) => !(commId in prevComms))
      .sort(([, a], [, b]) => (a.seq ?? 0) - (b.seq ?? 0));

    for (const [commId, entry] of newEntries) {
      if (!hasBlobPort) continue; // Retry on next CRDT update once port is ready
      const { state: resolvedState, bufferPaths } =
        replaceSentinelsWithBlobUrls(entry.state as Record<string, unknown>);
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
      };
      onCommMessage(msg);
      nextComms[commId] = entry;
      nextJson[commId] = JSON.stringify(entry.state);

      // Resolve Output widget manifest hashes in state.outputs to JupyterOutput objects.
      // The daemon writes manifest hashes (same format as execution outputs);
      // we resolve them here so the iframe receives ready-to-render outputs.
      resolveCommOutputHashes(commId, entry.state, callbacksRef);
    }

    // State changes — synthesize comm_msg(update)
    for (const [commId, entry] of Object.entries(docComms)) {
      const stateStr = JSON.stringify(entry.state);
      if (commId in prevComms) {
        nextComms[commId] = entry;
        nextJson[commId] = stateStr;
        if (prevJson[commId] !== stateStr) {
          const { state: resolvedState, bufferPaths } =
            replaceSentinelsWithBlobUrls(
              entry.state as Record<string, unknown>,
            );
          const msg: JupyterMessage = {
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
          };
          onCommMessage(msg);

          // Resolve Output widget manifest hashes in state.outputs
          resolveCommOutputHashes(commId, entry.state, callbacksRef);
        }
      }
    }

    // Removed comms — synthesize comm_close
    for (const commId of Object.keys(prevComms)) {
      if (!docComms[commId]) {
        const msg: JupyterMessage = {
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
        };
        onCommMessage(msg);
      }
    }

    // Only track comms that were successfully delivered.
    // Comms skipped due to missing blob port will be retried on next update.
    prevCommsRef.current = nextComms;
    prevCommsJsonRef.current = nextJson;
  }, [runtimeState.comms]);

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
    /** Sub-phase detail when status is "starting" */
    startingPhase: runtimeState.kernel.starting_phase,
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
    /** Run all code cells (daemon reads from synced doc) */
    runAllCells,
    /** Send a comm message to the kernel (for widget interactions) */
    sendCommMessage,
    /** Check if a cell is currently executing */
    isCellExecuting: (cellId: string) =>
      queueState.executing?.cell_id === cellId,
    /** Check if a cell is in the queue */
    isCellQueued: (cellId: string) =>
      queueState.executing?.cell_id === cellId ||
      queueState.queued.some((entry) => entry.cell_id === cellId),
  };
}
