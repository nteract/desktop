import type { WidgetStore } from "@/components/widgets/widget-store";
import type {
  CommCloseMessage,
  CommMsgMessage,
  CommOpenMessage,
  WidgetSnapshotMessage,
  IframeToParentMessage,
} from "./frame-bridge";
import type { IsolatedFrameHandle } from "./isolated-frame";

// Type for sending messages to kernel
type SendUpdate = (commId: string, state: Record<string, unknown>, buffers?: ArrayBuffer[]) => void;

type SendCustom = (
  commId: string,
  content: Record<string, unknown>,
  buffers?: ArrayBuffer[],
) => void;

type CloseComm = (commId: string) => void;

interface CommBridgeManagerOptions {
  /** The isolated frame handle for sending messages */
  frame: IsolatedFrameHandle;
  /** The parent widget store */
  store: WidgetStore;
  /** Function to send state updates to kernel */
  sendUpdate: SendUpdate;
  /** Function to send custom messages to kernel */
  sendCustom: SendCustom;
  /** Function to close a comm with kernel */
  closeComm: CloseComm;
}

/**
 * Comm Bridge Manager for proxying widget communication to an isolated iframe.
 *
 * Usage:
 * 1. Create manager when IsolatedFrame is mounted
 * 2. Subscribe to widget store changes to forward to iframe
 * 3. Handle iframe messages via onMessage callback
 * 4. Dispose when iframe is unmounted
 */
export class CommBridgeManager {
  private frame: IsolatedFrameHandle;
  private store: WidgetStore;
  private sendUpdateToKernel: SendUpdate;
  private sendCustomToKernel: SendCustom;
  private closeCommWithKernel: CloseComm;

  private isWidgetReady = false;
  private messageBuffer: Array<CommOpenMessage | CommMsgMessage | CommCloseMessage> = [];
  private storeUnsubscribe: (() => void) | null = null;

  // Track which models have been sent to avoid duplicate sends
  private sentModels = new Set<string>();

  // Track previous state for each model to detect kernel updates
  private previousState = new Map<string, Record<string, unknown>>();

  // Flag to prevent echoing iframe updates back to iframe
  private isProcessingIframeUpdate = false;

  // Track custom message subscriptions for each model
  private customMessageUnsubscribers = new Map<string, () => void>();

  constructor(options: CommBridgeManagerOptions) {
    this.frame = options.frame;
    this.store = options.store;
    this.sendUpdateToKernel = options.sendUpdate;
    this.sendCustomToKernel = options.sendCustom;
    this.closeCommWithKernel = options.closeComm;

    // Subscribe to store changes to forward to iframe
    this.storeUnsubscribe = this.store.subscribe(() => {
      if (!this.isWidgetReady) return;
      // Skip if this change came from iframe (avoid echo)
      if (this.isProcessingIframeUpdate) return;
      this.syncModels();
    });

    // Signal to iframe that parent bridge is ready
    // Iframe will respond with widget_ready to trigger widget_snapshot
    this.frame.send({ type: "bridge_ready" });
  }

  /**
   * Handle a message from the iframe.
   * Call this from the IsolatedFrame's onMessage callback.
   */
  handleIframeMessage(message: IframeToParentMessage): void {
    switch (message.type) {
      case "widget_ready":
        this.handleWidgetReady();
        break;

      case "widget_comm_msg":
        this.handleWidgetCommMsg(message.payload);
        break;

      case "widget_comm_close":
        this.handleWidgetCommClose(message.payload);
        break;
    }
  }

  /**
   * Forward a comm_open to the iframe.
   * Called when a widget model is created by the kernel.
   */
  sendCommOpen(
    commId: string,
    targetName: string,
    state: Record<string, unknown>,
    buffers?: ArrayBuffer[],
  ): void {
    const msg: CommOpenMessage = {
      type: "comm_open",
      payload: {
        commId,
        targetName,
        state,
        buffers,
      },
    };

    if (this.isWidgetReady) {
      try {
        this.frame.send(msg);
        this.sentModels.add(commId);
      } catch (e) {
        console.warn(`[CommBridge] Skipping non-cloneable comm_open for ${commId}:`, e);
      }
    } else {
      this.messageBuffer.push(msg);
    }
  }

  /**
   * Forward a comm_msg (state update or custom message) to the iframe.
   * Called when the kernel sends a state update or custom message.
   */
  sendCommMsg(
    commId: string,
    method: "update" | "custom",
    data: Record<string, unknown>,
    buffers?: ArrayBuffer[],
  ): void {
    const msg: CommMsgMessage = {
      type: "comm_msg",
      payload: {
        commId,
        method,
        data,
        buffers,
      },
    };

    if (this.isWidgetReady) {
      this.frame.send(msg);
    } else {
      this.messageBuffer.push(msg);
    }
  }

  /**
   * Forward a comm_close to the iframe.
   * Called when the kernel closes a widget.
   */
  sendCommClose(commId: string): void {
    const msg: CommCloseMessage = {
      type: "comm_close",
      payload: { commId },
    };

    if (this.isWidgetReady) {
      this.frame.send(msg);
      this.sentModels.delete(commId);
    } else {
      this.messageBuffer.push(msg);
    }
  }

  /**
   * Clean up subscriptions and state.
   */
  dispose(): void {
    if (this.storeUnsubscribe) {
      this.storeUnsubscribe();
      this.storeUnsubscribe = null;
    }
    // Unsubscribe from all custom message subscriptions
    for (const unsubscribe of this.customMessageUnsubscribers.values()) {
      unsubscribe();
    }
    this.customMessageUnsubscribers.clear();
    this.messageBuffer = [];
    this.sentModels.clear();
    this.previousState.clear();
    this.isWidgetReady = false;
  }

  // --- Private Methods ---

  private handleWidgetReady(): void {
    this.isWidgetReady = true;

    // Send widget_snapshot with all existing models
    const models = this.store.getSnapshot();
    const modelArray: WidgetSnapshotMessage["payload"]["models"] = [];

    for (const [commId, model] of models) {
      modelArray.push({
        commId,
        targetName: model.modelModule || "jupyter.widget",
        state: model.state,
        buffers: model.buffers,
      });
      this.sentModels.add(commId);
      // Store initial state for change detection
      this.previousState.set(commId, this.cloneStateSnapshot(model.state));
      // Subscribe to custom messages for this model
      this.subscribeToModelCustomMessages(commId);
    }

    if (modelArray.length > 0) {
      const syncMsg: WidgetSnapshotMessage = {
        type: "widget_snapshot",
        payload: { models: modelArray },
      };
      try {
        this.frame.send(syncMsg);
      } catch (e) {
        // Batch send failed (likely a non-cloneable value in one model).
        // Fall back to sending models individually so one bad model
        // doesn't prevent all widgets from loading.
        console.warn(
          `[CommBridge] Batch widget_snapshot failed, sending ${modelArray.length} models individually:`,
          e,
        );
        for (const model of modelArray) {
          try {
            this.frame.send({
              type: "widget_snapshot",
              payload: { models: [model] },
            } as WidgetSnapshotMessage);
          } catch (perModelError) {
            console.warn(
              `[CommBridge] Skipping non-cloneable model ${model.commId}:`,
              perModelError,
            );
          }
        }
      }
    }

    // Flush buffered messages
    for (const msg of this.messageBuffer) {
      this.frame.send(msg);
      if (msg.type === "comm_open") {
        this.sentModels.add(msg.payload.commId);
      } else if (msg.type === "comm_close") {
        this.sentModels.delete(msg.payload.commId);
      }
    }
    this.messageBuffer = [];
  }

  private handleWidgetCommMsg(payload: {
    commId: string;
    method: "update" | "custom";
    data: Record<string, unknown>;
    bufferPaths?: string[][];
    buffers?: ArrayBuffer[];
  }): void {
    const { commId, method, data, buffers } = payload;

    if (method === "update") {
      // Set flag to prevent echoing this update back to iframe
      this.isProcessingIframeUpdate = true;
      try {
        // Update parent store first (so UI stays in sync)
        this.store.updateModel(commId, data, buffers);
        // Update our tracked state
        const current = this.previousState.get(commId) ?? {};
        this.previousState.set(commId, this.cloneStateSnapshot({ ...current, ...data }));
        // Then forward to kernel
        this.sendUpdateToKernel(commId, data, buffers);
      } finally {
        this.isProcessingIframeUpdate = false;
      }
    } else if (method === "custom") {
      // Custom messages go directly to kernel (no store update)
      this.sendCustomToKernel(commId, data, buffers);
    }
  }

  private handleWidgetCommClose(payload: { commId: string }): void {
    const { commId } = payload;

    // Update parent store
    this.store.deleteModel(commId);
    // Forward to kernel
    this.closeCommWithKernel(commId);
    // Clean up tracking
    this.sentModels.delete(commId);
  }

  /**
   * Sync models with iframe: new models, deleted models, and state changes.
   * Called when store changes after widget_ready.
   */
  private syncModels(): void {
    const models = this.store.getSnapshot();

    for (const [commId, model] of models) {
      if (!this.sentModels.has(commId)) {
        // New model - send comm_open
        this.sendCommOpen(
          commId,
          model.modelModule || "jupyter.widget",
          model.state,
          model.buffers,
        );
        // Store initial state for change detection
        this.previousState.set(commId, this.cloneStateSnapshot(model.state));
        // Subscribe to custom messages for this model
        this.subscribeToModelCustomMessages(commId);
      } else {
        // Existing model - check for state changes
        const previous = this.previousState.get(commId);
        if (previous) {
          const changedKeys = this.getChangedKeys(previous, model.state);
          if (changedKeys.length > 0) {
            // Build delta with only changed keys
            const delta: Record<string, unknown> = {};
            for (const key of changedKeys) {
              delta[key] = model.state[key];
            }
            // Forward state update to iframe
            this.sendCommMsg(commId, "update", delta, model.buffers);
            // Update tracked state
            this.previousState.set(commId, this.cloneStateSnapshot(model.state));
          }
        }
      }
    }

    // Check for deleted models
    for (const commId of this.sentModels) {
      if (!models.has(commId)) {
        this.sendCommClose(commId);
        this.previousState.delete(commId);
        // Unsubscribe from custom messages
        this.unsubscribeFromModelCustomMessages(commId);
      }
    }
  }

  /**
   * Subscribe to custom messages for a model and forward them to iframe.
   * This is critical for anywidgets like quak that use custom messages for data.
   */
  private subscribeToModelCustomMessages(commId: string): void {
    // Don't double-subscribe
    if (this.customMessageUnsubscribers.has(commId)) return;

    const unsubscribe = this.store.subscribeToCustomMessage(commId, (content, buffers) => {
      // Convert DataView[] to ArrayBuffer[] for postMessage
      const arrayBuffers = buffers?.map((dv) => dv.buffer as ArrayBuffer);
      // Forward custom message to iframe
      this.sendCommMsg(commId, "custom", content, arrayBuffers);
    });

    this.customMessageUnsubscribers.set(commId, unsubscribe);
  }

  /**
   * Unsubscribe from custom messages for a model.
   */
  private unsubscribeFromModelCustomMessages(commId: string): void {
    const unsubscribe = this.customMessageUnsubscribers.get(commId);
    if (unsubscribe) {
      unsubscribe();
      this.customMessageUnsubscribers.delete(commId);
    }
  }

  /**
   * Get keys that have changed between two state objects.
   * Uses shallow comparison for performance.
   */
  private getChangedKeys(
    previous: Record<string, unknown>,
    current: Record<string, unknown>,
  ): string[] {
    const changed: string[] = [];
    const allKeys = new Set([...Object.keys(previous), ...Object.keys(current)]);
    for (const key of allKeys) {
      if (this.valuesAreDifferent(previous[key], current[key])) {
        changed.push(key);
      }
    }
    return changed;
  }

  /**
   * Create a deep snapshot of model state so future in-place mutations are detectable.
   */
  private cloneStateSnapshot(state: Record<string, unknown>): Record<string, unknown> {
    try {
      return structuredClone(state);
    } catch {
      return { ...state };
    }
  }

  /**
   * Compare two state values.
   * For object/array values, use JSON content comparison so deep snapshots
   * can detect in-place mutations from live state objects.
   */
  private valuesAreDifferent(previous: unknown, current: unknown): boolean {
    if (
      typeof previous !== "object" ||
      previous === null ||
      typeof current !== "object" ||
      current === null
    ) {
      return previous !== current;
    }

    try {
      return JSON.stringify(previous) !== JSON.stringify(current);
    } catch {
      // If value can't be serialized consistently, err on sending an update.
      return true;
    }
  }
}

/**
 * Create a comm bridge manager for an isolated frame.
 */
export function createCommBridgeManager(options: CommBridgeManagerOptions): CommBridgeManager {
  return new CommBridgeManager(options);
}
