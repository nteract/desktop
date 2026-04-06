/**
 * Comm Protocol Router Hook
 *
 * Handles routing of Jupyter comm messages between the kernel and widget store.
 * Provides both inbound message handling and outbound message construction.
 *
 * Message format follows the Jupyter messaging protocol specification and is
 * compatible with strongly-typed backends (Rust, Go) that require explicit fields.
 *
 * @see https://jupyter-widgets.readthedocs.io/en/latest/examples/Widget%20Low%20Level.html
 * @see https://jupyter-client.readthedocs.io/en/latest/messaging.html
 */

import { useCallback, useEffect, useRef } from "react";
import { applyBufferPaths } from "./buffer-utils";
import { getCrdtCommWriter } from "./crdt-comm-writer";
import type { WidgetUpdateManager } from "./widget-update-manager";
import type { WidgetStore } from "./widget-store";

// === Message Types ===

/**
 * Jupyter message header.
 * Required fields vary between incoming (may have fewer) and outgoing (should have all).
 */
export interface JupyterMessageHeader {
  msg_id: string;
  msg_type: string;
  username?: string;
  session?: string;
  date?: string;
  version?: string;
}

/**
 * Full header with all fields required (for outgoing messages).
 */
interface FullJupyterMessageHeader {
  msg_id: string;
  msg_type: string;
  username: string;
  session: string;
  date: string;
  version: string;
}

/**
 * Jupyter comm message.
 * Some fields are optional for incoming messages but should be set for outgoing.
 */
export interface JupyterCommMessage {
  header: JupyterMessageHeader;
  /** Should be null for outgoing messages (not undefined or empty object) */
  parent_header?: JupyterMessageHeader | null;
  metadata?: Record<string, unknown>;
  content: {
    comm_id?: string;
    target_name?: string;
    data?: {
      state?: Record<string, unknown>;
      method?: string;
      content?: Record<string, unknown>;
      buffer_paths?: string[][];
      [key: string]: unknown;
    };
  };
  buffers?: ArrayBuffer[];
  channel?: string | null;
}

/**
 * Outgoing message with all fields populated for protocol compliance.
 * Use this type for messages being sent to strongly-typed backends.
 */
interface OutgoingJupyterCommMessage {
  header: FullJupyterMessageHeader;
  parent_header: null;
  metadata: Record<string, unknown>;
  content: {
    comm_id: string;
    data?: {
      state?: Record<string, unknown>;
      method?: string;
      content?: Record<string, unknown>;
      buffer_paths: string[][];
    };
  };
  buffers: ArrayBuffer[];
  channel: string;
}

/**
 * Function type for sending messages to the kernel.
 */
export type SendMessage = (msg: JupyterCommMessage) => void;

// === Hook Types ===

export interface UseCommRouterOptions {
  /** Function to send messages to the kernel */
  sendMessage: SendMessage;
  /** Widget store instance */
  store: WidgetStore;
  /** Optional username for message headers (default: "frontend") */
  username?: string;
  /** Optional update manager for debounced CRDT writes + echo suppression. */
  updateManager?: WidgetUpdateManager;
}

export interface UseCommRouterReturn {
  /** Handle incoming Jupyter comm messages */
  handleMessage: (msg: JupyterCommMessage) => void;
  /** Send a state update to the kernel */
  sendUpdate: (
    commId: string,
    state: Record<string, unknown>,
    buffers?: ArrayBuffer[],
  ) => void;
  /** Send a custom message to the kernel */
  sendCustom: (
    commId: string,
    content: Record<string, unknown>,
    buffers?: ArrayBuffer[],
  ) => void;
  /** Close a comm channel */
  closeComm: (commId: string) => void;
}

// === Message Construction Helpers ===

// Session ID for this frontend instance (stable across messages)
const SESSION_ID = crypto.randomUUID();

/**
 * Create a complete Jupyter message header with all fields.
 * All fields are required for compatibility with strongly-typed backends.
 */
function createHeader(
  msgType: string,
  username: string,
): FullJupyterMessageHeader {
  return {
    msg_id: crypto.randomUUID(),
    msg_type: msgType,
    username,
    session: SESSION_ID,
    date: new Date().toISOString(),
    version: "5.3",
  };
}

/**
 * Create a comm_msg for state updates.
 * Includes all required fields for Jupyter protocol compliance.
 */
function createUpdateMessage(
  commId: string,
  state: Record<string, unknown>,
  buffers: ArrayBuffer[] | undefined,
  username: string,
): OutgoingJupyterCommMessage {
  return {
    header: createHeader("comm_msg", username),
    parent_header: null,
    metadata: {},
    content: {
      comm_id: commId,
      data: {
        method: "update",
        state,
        buffer_paths: [],
      },
    },
    buffers: buffers ?? [],
    channel: "shell",
  };
}

/**
 * Create a comm_msg for custom messages.
 * Includes all required fields for Jupyter protocol compliance.
 */
function createCustomMessage(
  commId: string,
  content: Record<string, unknown>,
  buffers: ArrayBuffer[] | undefined,
  username: string,
): OutgoingJupyterCommMessage {
  return {
    header: createHeader("comm_msg", username),
    parent_header: null,
    metadata: {},
    content: {
      comm_id: commId,
      data: {
        method: "custom",
        content,
        buffer_paths: [],
      },
    },
    buffers: buffers ?? [],
    channel: "shell",
  };
}

/**
 * Create a comm_close message.
 * Includes all required fields for Jupyter protocol compliance.
 */
function createCloseMessage(
  commId: string,
  username: string,
): OutgoingJupyterCommMessage {
  return {
    header: createHeader("comm_close", username),
    parent_header: null,
    metadata: {},
    content: {
      comm_id: commId,
    },
    buffers: [],
    channel: "shell",
  };
}

// === Hook Implementation ===

/**
 * Hook for routing Jupyter comm protocol messages.
 *
 * Handles:
 * - Inbound: comm_open, comm_msg (update/custom), comm_close
 * - Outbound: sendUpdate, sendCustom, closeComm
 *
 * @example
 * const { handleMessage, sendUpdate, sendCustom, closeComm } = useCommRouter({
 *   sendMessage: (msg) => kernel.send(msg),
 *   store: widgetStore,
 * });
 *
 * // Route incoming messages
 * kernel.onMessage((msg) => handleMessage(msg));
 *
 * // Send updates back to kernel
 * sendUpdate(commId, { value: 42 });
 */
export function useCommRouter({
  sendMessage,
  store,
  username = "frontend",
  updateManager,
}: UseCommRouterOptions): UseCommRouterReturn {
  // Refs for values that need to be fresh but shouldn't cause function identity changes.
  // This ensures sendUpdate, sendCustom, etc. are stable across renders, preventing
  // unnecessary effect re-runs in consumer widgets (see PR #137 pattern).
  const sendMessageRef = useRef(sendMessage);
  const storeRef = useRef(store);
  const usernameRef = useRef(username);
  const managerRef = useRef(updateManager);

  // Keep refs up-to-date without changing function identities
  useEffect(() => {
    sendMessageRef.current = sendMessage;
    storeRef.current = store;
    usernameRef.current = username;
    managerRef.current = updateManager;
  });

  /**
   * Handle incoming Jupyter comm messages.
   * Routes to appropriate store methods based on message type.
   */
  const handleMessage = useCallback((msg: JupyterCommMessage) => {
    const msgType = msg.header.msg_type;
    const commId = msg.content.comm_id;

    if (!commId) return;

    switch (msgType) {
      case "comm_open": {
        // Get state and apply buffer paths if present
        let state = msg.content.data?.state || {};
        const bufferPaths = msg.content.data?.buffer_paths;

        if (bufferPaths && msg.buffers?.length) {
          state = { ...state };
          applyBufferPaths(state, bufferPaths, msg.buffers);
        }

        storeRef.current.createModel(commId, state, msg.buffers);
        break;
      }

      case "comm_msg": {
        const data = msg.content.data;
        const method = data?.method;

        if (method === "update" && data?.state) {
          // Apply buffer paths to state update
          let state = data.state;
          const bufferPaths = data.buffer_paths;

          if (bufferPaths && msg.buffers?.length) {
            state = { ...state };
            applyBufferPaths(state, bufferPaths, msg.buffers);
          }

          storeRef.current.updateModel(commId, state, msg.buffers);
        } else if (method === "custom") {
          // Dispatch custom message to widget handlers
          const content = (data?.content as Record<string, unknown>) || {};
          storeRef.current.emitCustomMessage(commId, content, msg.buffers);
        }
        break;
      }

      case "comm_close": {
        storeRef.current.deleteModel(commId);
        break;
      }
    }
  }, []);

  /**
   * Send a state update to the kernel.
   * Also applies optimistic update to local store for immediate UI response.
   */
  const sendUpdate = useCallback(
    (
      commId: string,
      state: Record<string, unknown>,
      buffers?: ArrayBuffer[],
    ) => {
      // When a manager is available, delegate to it for debounced CRDT
      // writes + echo suppression. Otherwise fall back to direct write
      // (used in iframe context where no manager exists).
      const manager = managerRef.current;
      if (manager) {
        manager.updateAndPersist(commId, state, buffers);
        return;
      }
      // Fallback: immediate optimistic update + direct CRDT write
      storeRef.current.updateModel(commId, state, buffers);
      const writer = getCrdtCommWriter();
      if (writer && !buffers?.length) {
        writer(commId, state);
      } else {
        sendMessageRef.current(
          createUpdateMessage(commId, state, buffers, usernameRef.current),
        );
      }
    },
    [],
  );

  /**
   * Send a custom message to the kernel.
   */
  const sendCustom = useCallback(
    (
      commId: string,
      content: Record<string, unknown>,
      buffers?: ArrayBuffer[],
    ) => {
      sendMessageRef.current(
        createCustomMessage(commId, content, buffers, usernameRef.current),
      );
    },
    [],
  );

  /**
   * Close a comm channel.
   * Sends comm_close to kernel and removes model from store.
   */
  const closeComm = useCallback((commId: string) => {
    sendMessageRef.current(createCloseMessage(commId, usernameRef.current));
    storeRef.current.deleteModel(commId);
  }, []);

  return {
    handleMessage,
    sendUpdate,
    sendCustom,
    closeComm,
  };
}
