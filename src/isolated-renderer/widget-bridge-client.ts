/**
 * Widget Bridge Client - Iframe Side
 *
 * This module runs inside the isolated iframe and manages widget communication
 * with the parent window. It:
 * - Creates a local WidgetStore for widget state management
 * - Listens for comm_open/comm_msg/comm_close from parent via postMessage
 * - Provides methods to send state updates and custom messages back to parent
 * - Sends `widget_ready` when initialized
 *
 * Security: This code runs in a sandboxed iframe with an opaque origin.
 * It cannot access Tauri APIs, the parent DOM, or localStorage.
 */

import type {
  CommCloseMessage,
  CommMsgMessage,
  CommOpenMessage,
  CommSyncMessage,
  WidgetCommCloseMessage,
  WidgetCommMsgMessage,
} from "@/components/isolated/frame-bridge";
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

// Type for method parameter in comm messages
type CommMethod = "update" | "custom";

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
   * Clean up the bridge (remove event listeners).
   */
  dispose: () => void;
}

/**
 * Create a widget bridge client for the iframe.
 * This sets up:
 * - A local WidgetStore instance
 * - Message listener for parent → iframe comm messages (JSON-RPC + legacy)
 * - Methods to send iframe → parent messages
 *
 * @param transport - Optional JsonRpcTransport for JSON-RPC communication.
 *   When provided, the bridge registers handlers on the transport and sends
 *   outbound messages via JSON-RPC. Legacy postMessage listener is kept as
 *   fallback for messages from the bootstrap HTML.
 */
export function createWidgetBridgeClient(
  transport?: JsonRpcTransport | null,
): WidgetBridgeClient {
  // Create local widget store
  const store = createWidgetStore();

  // --- Shared handler logic ---

  function sendWidgetReady() {
    if (transport) {
      transport.notify(NTERACT_WIDGET_READY, {});
    } else {
      window.parent.postMessage({ type: "widget_ready" }, "*");
    }
  }

  function handleCommOpenPayload(payload: CommOpenMessage["payload"]) {
    const { commId, state, buffers } = payload;
    store.createModel(commId, state, buffers);
  }

  function handleCommMsgPayload(payload: CommMsgMessage["payload"]) {
    const { commId, method, data, buffers } = payload;
    if (method === "update") {
      store.updateModel(commId, data, buffers);
    } else if (method === "custom") {
      store.emitCustomMessage(commId, data, buffers);
    }
  }

  function handleCommClosePayload(payload: CommCloseMessage["payload"]) {
    const { commId } = payload;
    store.deleteModel(commId);
  }

  function handleCommSyncPayload(payload: CommSyncMessage["payload"]) {
    const { models } = payload;
    for (const model of models) {
      store.createModel(model.commId, model.state, model.buffers);
    }
  }

  // --- JSON-RPC transport handlers ---
  if (transport) {
    transport.onNotification(NTERACT_BRIDGE_READY, () => {
      sendWidgetReady();
    });

    transport.onNotification(NTERACT_COMM_OPEN, (params) => {
      handleCommOpenPayload(params as CommOpenMessage["payload"]);
    });

    transport.onNotification(NTERACT_COMM_MSG, (params) => {
      handleCommMsgPayload(params as CommMsgMessage["payload"]);
    });

    transport.onNotification(NTERACT_COMM_CLOSE, (params) => {
      handleCommClosePayload(params as CommCloseMessage["payload"]);
    });

    transport.onNotification(NTERACT_COMM_SYNC, (params) => {
      handleCommSyncPayload(params as CommSyncMessage["payload"]);
    });
  }

  // --- Legacy postMessage handler (fallback) ---
  function handleMessage(event: MessageEvent) {
    if (event.source !== window.parent) return;

    const message = event.data;
    // Skip JSON-RPC messages — the transport handles them
    if (
      typeof message === "object" &&
      message !== null &&
      message.jsonrpc === "2.0"
    ) {
      return;
    }
    if (!message || typeof message.type !== "string") return;

    switch (message.type) {
      case "bridge_ready":
        sendWidgetReady();
        break;
      case "comm_open":
        handleCommOpenPayload((message as CommOpenMessage).payload);
        break;
      case "comm_msg":
        handleCommMsgPayload((message as CommMsgMessage).payload);
        break;
      case "comm_close":
        handleCommClosePayload((message as CommCloseMessage).payload);
        break;
      case "comm_sync":
        handleCommSyncPayload((message as CommSyncMessage).payload);
        break;
    }
  }

  window.addEventListener("message", handleMessage);

  // Send initial widget_ready to parent
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

      if (transport) {
        transport.notify(NTERACT_WIDGET_COMM_MSG, {
          commId,
          method: "update" as CommMethod,
          data: state,
          buffers,
        });
      } else {
        const msg: WidgetCommMsgMessage = {
          type: "widget_comm_msg",
          payload: {
            commId,
            method: "update" as CommMethod,
            data: state,
            buffers,
          },
        };
        window.parent.postMessage(msg, "*", buffers ?? []);
      }
    },

    sendCustom(
      commId: string,
      content: Record<string, unknown>,
      buffers?: ArrayBuffer[],
    ) {
      if (transport) {
        transport.notify(NTERACT_WIDGET_COMM_MSG, {
          commId,
          method: "custom" as CommMethod,
          data: content,
          buffers,
        });
      } else {
        const msg: WidgetCommMsgMessage = {
          type: "widget_comm_msg",
          payload: {
            commId,
            method: "custom" as CommMethod,
            data: content,
            buffers,
          },
        };
        window.parent.postMessage(msg, "*", buffers ?? []);
      }
    },

    closeComm(commId: string) {
      if (transport) {
        transport.notify(NTERACT_WIDGET_COMM_CLOSE, { commId });
      } else {
        const msg: WidgetCommCloseMessage = {
          type: "widget_comm_close",
          payload: { commId },
        };
        window.parent.postMessage(msg, "*");
      }
    },

    dispose() {
      window.removeEventListener("message", handleMessage);
    },
  };
}
