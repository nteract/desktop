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
 * - Message listener for parent → iframe comm messages
 * - Methods to send iframe → parent messages
 */
export function createWidgetBridgeClient(): WidgetBridgeClient {
  // Create local widget store
  const store = createWidgetStore();

  // Message handler for parent → iframe messages
  function handleMessage(event: MessageEvent) {
    // Only accept messages from parent
    if (event.source !== window.parent) return;

    const message = event.data;
    if (!message || typeof message.type !== "string") return;

    switch (message.type) {
      case "bridge_ready":
        // Parent's comm bridge is ready, re-send widget_ready to trigger sync
        sendWidgetReady();
        break;
      case "comm_open":
        handleCommOpen(message as CommOpenMessage);
        break;
      case "comm_msg":
        handleCommMsg(message as CommMsgMessage);
        break;
      case "comm_close":
        handleCommClose(message as CommCloseMessage);
        break;
      case "comm_sync":
        handleCommSync(message as CommSyncMessage);
        break;
    }
  }

  function sendWidgetReady() {
    window.parent.postMessage({ type: "widget_ready" }, "*");
  }

  function handleCommOpen(msg: CommOpenMessage) {
    const { commId, state, buffers } = msg.payload;
    store.createModel(commId, state, buffers);
  }

  function handleCommMsg(msg: CommMsgMessage) {
    const { commId, method, data, buffers } = msg.payload;

    if (method === "update") {
      // State update from kernel
      store.updateModel(commId, data, buffers);
    } else if (method === "custom") {
      // Custom message from kernel (e.g., ipycanvas commands)
      store.emitCustomMessage(commId, data, buffers);
    }
  }

  function handleCommClose(msg: CommCloseMessage) {
    const { commId } = msg.payload;
    store.deleteModel(commId);
  }

  function handleCommSync(msg: CommSyncMessage) {
    const { models } = msg.payload;

    for (const model of models) {
      store.createModel(model.commId, model.state, model.buffers);
    }
  }

  // Set up message listener
  window.addEventListener("message", handleMessage);

  // Send initial widget_ready to parent
  // (Parent may not be listening yet; it will send bridge_ready when ready, and we'll re-send)
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

      const msg: WidgetCommMsgMessage = {
        type: "widget_comm_msg",
        payload: {
          commId,
          method: "update" as CommMethod,
          data: state,
          buffers,
        },
      };
      // Transfer buffers for efficiency (note: buffers are consumed by transfer)
      window.parent.postMessage(msg, "*", buffers ?? []);
    },

    sendCustom(
      commId: string,
      content: Record<string, unknown>,
      buffers?: ArrayBuffer[],
    ) {
      const msg: WidgetCommMsgMessage = {
        type: "widget_comm_msg",
        payload: {
          commId,
          method: "custom" as CommMethod,
          data: content,
          buffers,
        },
      };
      // Transfer buffers for efficiency
      window.parent.postMessage(msg, "*", buffers ?? []);
    },

    closeComm(commId: string) {
      const msg: WidgetCommCloseMessage = {
        type: "widget_comm_close",
        payload: { commId },
      };
      window.parent.postMessage(msg, "*");
    },

    dispose() {
      window.removeEventListener("message", handleMessage);
    },
  };
}
