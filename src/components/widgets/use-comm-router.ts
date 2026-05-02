/**
 * Outbound comm protocol helpers.
 *
 * Widget interactions (slider drags, button clicks, `model.send()` calls)
 * travel frontend → kernel through two channels:
 *
 * - State updates (method: "update") prefer the CRDT path — the store
 *   writes directly into `RuntimeStateDoc.comms[commId].state` and the
 *   daemon forwards to the kernel.
 * - Custom messages (method: "custom") and fallback updates still use the
 *   daemon shell channel, wrapped in a Jupyter comm_msg frame.
 *
 * Inbound state arrives via `SyncEngine.commChanges$` (see `App.tsx`);
 * there's no Jupyter-protocol inbound path in this file.
 *
 * @see https://jupyter-widgets.readthedocs.io/en/latest/examples/Widget%20Low%20Level.html
 * @see https://jupyter-client.readthedocs.io/en/latest/messaging.html
 */

import { useCallback, useEffect, useRef } from "react";
import { getCrdtCommWriter } from "./crdt-comm-writer";
import type { WidgetStore } from "./widget-store";
import type { WidgetUpdateManager } from "./widget-update-manager";

/**
 * Jupyter message header.
 */
export interface JupyterMessageHeader {
  msg_id: string;
  msg_type: string;
  username: string;
  session: string;
  date: string;
  version: string;
}

/**
 * Outgoing comm message. All fields populated for strongly-typed backends.
 */
interface OutgoingJupyterCommMessage {
  header: JupyterMessageHeader;
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
export type SendMessage = (msg: OutgoingJupyterCommMessage) => void;

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
  /** Send a state update to the kernel */
  sendUpdate: (commId: string, state: Record<string, unknown>, buffers?: ArrayBuffer[]) => void;
  /** Send a custom message to the kernel */
  sendCustom: (commId: string, content: Record<string, unknown>, buffers?: ArrayBuffer[]) => void;
  /** Close a comm channel */
  closeComm: (commId: string) => void;
}

// Session ID for this frontend instance (stable across messages)
const SESSION_ID = crypto.randomUUID();

function createHeader(msgType: string, username: string): JupyterMessageHeader {
  return {
    msg_id: crypto.randomUUID(),
    msg_type: msgType,
    username,
    session: SESSION_ID,
    date: new Date().toISOString(),
    version: "5.3",
  };
}

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

function createCloseMessage(commId: string, username: string): OutgoingJupyterCommMessage {
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

/**
 * Hook exposing outbound comm helpers.
 *
 * `sendUpdate` prefers the CRDT writer when no binary buffers are involved;
 * the fallback path is the daemon shell channel. `sendCustom` and
 * `closeComm` always go through the shell channel.
 */
export function useCommRouter({
  sendMessage,
  store,
  username = "frontend",
  updateManager,
}: UseCommRouterOptions): UseCommRouterReturn {
  const sendMessageRef = useRef(sendMessage);
  const storeRef = useRef(store);
  const usernameRef = useRef(username);
  const managerRef = useRef(updateManager);

  useEffect(() => {
    sendMessageRef.current = sendMessage;
    storeRef.current = store;
    usernameRef.current = username;
    managerRef.current = updateManager;
  });

  const sendUpdate = useCallback(
    (commId: string, state: Record<string, unknown>, buffers?: ArrayBuffer[]) => {
      const manager = managerRef.current;
      if (manager) {
        manager.updateAndPersist(commId, state, buffers);
        return;
      }
      // Fallback for contexts without a manager (iframe outbound bridge).
      storeRef.current.updateModel(commId, state);
      const writer = getCrdtCommWriter();
      if (writer && !buffers?.length) {
        writer(commId, state);
      } else {
        sendMessageRef.current(createUpdateMessage(commId, state, buffers, usernameRef.current));
      }
    },
    [],
  );

  const sendCustom = useCallback(
    (commId: string, content: Record<string, unknown>, buffers?: ArrayBuffer[]) => {
      sendMessageRef.current(createCustomMessage(commId, content, buffers, usernameRef.current));
    },
    [],
  );

  const closeComm = useCallback((commId: string) => {
    sendMessageRef.current(createCloseMessage(commId, usernameRef.current));
    storeRef.current.deleteModel(commId);
  }, []);

  return {
    sendUpdate,
    sendCustom,
    closeComm,
  };
}
