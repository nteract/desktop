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
  NTERACT_WIDGET_SNAPSHOT,
  NTERACT_WIDGET_COMM_CLOSE,
  NTERACT_WIDGET_COMM_MSG,
  NTERACT_WIDGET_READY,
} from "@/components/isolated/rpc-methods";
import {
  createWidgetStore,
  type WidgetStore,
} from "@/components/widgets/widget-store";

/** Blob URL pattern: http://127.0.0.1:{port}/blob/{hash} */
const BLOB_URL_RE = /^https?:\/\/127\.0\.0\.1:\d+\/blob\/[a-f0-9]+$/;

/**
 * Resolve blob URLs in state at buffer_paths positions to ArrayBuffers.
 *
 * Only converts blob URLs to DataView/ArrayBuffer at explicitly listed
 * `bufferPaths` positions (binary widget data like NumPy arrays).
 * When `bufferPaths` is absent or empty, returns [] — blob URL strings
 * (e.g. `_esm`, `_css`) stay as strings so `loadESM`/CSS injection
 * can load them natively.
 */
async function resolveBlobUrls(
  state: Record<string, unknown>,
  bufferPaths?: string[][],
): Promise<ArrayBuffer[]> {
  if (!bufferPaths || bufferPaths.length === 0) return [];
  return resolveAtPaths(state, bufferPaths);
}

/** Resolve blob URLs at known paths to ArrayBuffers. */
async function resolveAtPaths(
  state: Record<string, unknown>,
  paths: string[][],
): Promise<ArrayBuffer[]> {
  const resolved = await Promise.all(
    paths.map(async (path) => {
      let current: unknown = state;
      for (const segment of path) {
        if (typeof current !== "object" || current === null) return null;
        current = (current as Record<string, unknown>)[segment];
      }
      if (typeof current === "string" && BLOB_URL_RE.test(current)) {
        try {
          const resp = await fetch(current);
          if (!resp.ok) return null;
          const buffer = await resp.arrayBuffer();
          // Replace the URL in state with the buffer (DataView for widget protocol)
          let parent: Record<string, unknown> = state;
          for (let i = 0; i < path.length - 1; i++) {
            parent = parent[path[i]] as Record<string, unknown>;
          }
          parent[path[path.length - 1]] = new DataView(buffer);
          return buffer;
        } catch {
          return null;
        }
      }
      return null;
    }),
  );
  return resolved.filter((b): b is ArrayBuffer => b !== null);
}

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

  transport.onNotification(NTERACT_COMM_OPEN, async (params) => {
    const { commId, state, buffers, bufferPaths } = params as {
      commId: string;
      state: Record<string, unknown>;
      buffers?: ArrayBuffer[];
      bufferPaths?: string[][];
    };
    // Resolve blob URLs at buffer_paths positions to ArrayBuffers.
    const resolvedBuffers = await resolveBlobUrls(state, bufferPaths);
    store.createModel(
      commId,
      state,
      resolvedBuffers.length > 0 ? resolvedBuffers : buffers,
    );
  });

  transport.onNotification(NTERACT_COMM_MSG, async (params) => {
    const { commId, method, data, buffers, bufferPaths } = params as {
      commId: string;
      method: "update" | "custom";
      data: Record<string, unknown>;
      buffers?: ArrayBuffer[];
      bufferPaths?: string[][];
    };
    if (method === "update") {
      const resolvedBuffers = await resolveBlobUrls(data, bufferPaths);
      store.updateModel(
        commId,
        data,
        resolvedBuffers.length > 0 ? resolvedBuffers : buffers,
      );
    } else if (method === "custom") {
      store.emitCustomMessage(commId, data, buffers);
    }
  });

  transport.onNotification(NTERACT_COMM_CLOSE, (params) => {
    const { commId } = params as { commId: string };
    store.deleteModel(commId);
  });

  transport.onNotification(NTERACT_WIDGET_SNAPSHOT, (params) => {
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
