/**
 * Widget Bridge Client - Iframe Side
 *
 * This module runs inside the isolated iframe and manages widget communication
 * with the parent window via JSON-RPC 2.0 notifications through a shared
 * JsonRpcTransport instance.
 *
 * It:
 * - Creates a local WidgetStore for widget state management
 * - Registers notification handlers on the transport for comm messages from parent
 * - Provides methods to send state updates and custom messages back to parent
 * - Sends `nteract/widgetReady` when initialized
 *
 * Security: This code runs in a sandboxed iframe with an opaque origin.
 * It cannot access Tauri APIs, the parent DOM, or localStorage.
 */

import type { JsonRpcTransport } from "@/components/isolated/jsonrpc-transport";
import {
  NTERACT_BRIDGE_READY,
  NTERACT_COMM_CLOSE,
  NTERACT_COMM_MSG,
  NTERACT_COMM_OPEN,
  NTERACT_COMM_SYNC,
  NTERACT_WIDGET_COMM_CLOSE,
  NTERACT_WIDGET_COMM_MSG,
  NTERACT_WIDGET_READY,
} from "@/components/isolated/rpc-methods";
import {
  createWidgetStore,
  type WidgetStore,
} from "@/components/widgets/widget-store";

/**
 * Interface for the widget bridge client.
 * Provides access to the local store and methods to communicate with parent.
 */
export interface WidgetBridgeClient {
  /** The local widget store for this iframe */
  store: WidgetStore;

  /**
   * Send a state update to the parent (to be forwarded to kernel).
   * Called when a widget's state changes due to user interaction.
   */
  sendUpdate: (
    commId: string,
    state: Record<string, unknown>,
    buffers?: ArrayBuffer[],
  ) => void;

  /**
   * Send a custom message to the parent (to be forwarded to kernel).
   * Used for widget-specific protocols (e.g., ipycanvas draw commands).
   */
  sendCustom: (
    commId: string,
    content: Record<string, unknown>,
    buffers?: ArrayBuffer[],
  ) => void;

  /**
   * Request to close a comm (to be forwarded to kernel).
   */
  closeComm: (commId: string) => void;

  /**
   * Clean up the bridge.
   */
  dispose: () => void;
}

/**
 * Create a widget bridge client for the iframe.
 * This sets up:
 * - A local WidgetStore instance
 * - Notification handlers on the transport for parent → iframe comm messages
 * - Methods to send iframe → parent messages via the transport
 *
 * @param transport - The shared JsonRpcTransport (created in index.tsx init())
 */
export function createWidgetBridgeClient(
  transport: JsonRpcTransport,
): WidgetBridgeClient {
  const store = createWidgetStore();

  function sendWidgetReady() {
    transport.notify(NTERACT_WIDGET_READY);
  }

  // Register handlers for parent → iframe comm messages
  transport.onNotification(NTERACT_BRIDGE_READY, () => {
    sendWidgetReady();
  });

  transport.onNotification(NTERACT_COMM_OPEN, (params) => {
    const { commId, state, buffers } = params as {
      commId: string;
      state: Record<string, unknown>;
      buffers?: ArrayBuffer[];
    };
    store.createModel(commId, state, buffers);
  });

  transport.onNotification(NTERACT_COMM_MSG, (params) => {
    const { commId, method, data, buffers } = params as {
      commId: string;
      method: "update" | "custom";
      data: Record<string, unknown>;
      buffers?: ArrayBuffer[];
    };
    if (method === "update") {
      store.updateModel(commId, data, buffers);
    } else if (method === "custom") {
      store.emitCustomMessage(commId, data, buffers);
    }
  });

  transport.onNotification(NTERACT_COMM_CLOSE, (params) => {
    const { commId } = params as { commId: string };
    store.deleteModel(commId);
  });

  transport.onNotification(NTERACT_COMM_SYNC, (params) => {
    const { models } = params as {
      models: Array<{
        commId: string;
        state: Record<string, unknown>;
        buffers?: ArrayBuffer[];
      }>;
    };
    for (const model of models) {
      store.createModel(model.commId, model.state, model.buffers);
    }
  });

  // Send initial widget_ready
  // (Parent may not be listening yet; it will send bridgeReady when ready,
  // and we'll re-send via the handler above)
  sendWidgetReady();

  return {
    store,

    sendUpdate(
      commId: string,
      state: Record<string, unknown>,
      buffers?: ArrayBuffer[],
    ) {
      // Update local store immediately for responsive UI (optimistic update)
      store.updateModel(commId, state, buffers);
      transport.notify(NTERACT_WIDGET_COMM_MSG, {
        commId,
        method: "update",
        data: state,
        buffers,
      });
    },

    sendCustom(
      commId: string,
      content: Record<string, unknown>,
      buffers?: ArrayBuffer[],
    ) {
      transport.notify(NTERACT_WIDGET_COMM_MSG, {
        commId,
        method: "custom",
        data: content,
        buffers,
      });
    },

    closeComm(commId: string) {
      transport.notify(NTERACT_WIDGET_COMM_CLOSE, { commId });
    },

    dispose() {
      // Transport lifecycle is managed by index.tsx
    },
  };
}
