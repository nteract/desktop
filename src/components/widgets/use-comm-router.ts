/**
 * Outbound comm protocol helpers.
 *
 * Widget interactions (slider drags, button clicks, `model.send()` calls)
 * travel frontend → kernel through two channels:
 *
 * - State updates (method: "update") go to `WidgetUpdateManager`, which
 *   writes a debounced delta into `RuntimeStateDoc.comms[commId].state`;
 *   the daemon diffs and forwards to the kernel. If the CRDT writer
 *   isn't installed yet, the manager re-queues the flush — no shell-
 *   channel fallback.
 * - Custom messages (method: "custom") and `comm_close` still use the
 *   daemon shell channel, wrapped in a Jupyter comm_msg frame, because
 *   they're ephemeral events rather than CRDT state.
 *
 * Inbound state arrives via `SyncEngine.commChanges$` (see `App.tsx`);
 * there's no Jupyter-protocol inbound path in this file.
 *
 * @see https://jupyter-widgets.readthedocs.io/en/latest/examples/Widget%20Low%20Level.html
 * @see https://jupyter-client.readthedocs.io/en/latest/messaging.html
 */

import { useCallback, useEffect, useRef } from "react";
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
  /** Function to send messages to the kernel (used for custom messages and comm_close). */
  sendMessage: SendMessage;
  /** Widget store instance */
  store: WidgetStore;
  /** Optional username for message headers (default: "frontend") */
  username?: string;
  /** Debounced CRDT writer for outbound state updates. */
  updateManager: WidgetUpdateManager;
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
 * `sendUpdate` always routes through `WidgetUpdateManager` — the manager
 * applies the patch to the store immediately for optimistic UI and then
 * debounces the CRDT write. `sendCustom` and `closeComm` go through the
 * shell channel because they're ephemeral events, not CRDT state.
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
      managerRef.current.updateAndPersist(commId, state, buffers);
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
