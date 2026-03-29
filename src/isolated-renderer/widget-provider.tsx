/**
 * Widget Provider for Isolated Iframe
 *
 * This module provides the same React context interface as the main app's
 * WidgetStoreProvider, but uses the CommBridgeClient to proxy communication
 * through postMessage to the parent window.
 *
 * This allows existing widget components to work unchanged inside the iframe.
 * We provide to both the main WidgetStoreContext (so WidgetView works) and
 * our own IframeWidgetStoreContext (for iframe-specific code if needed).
 */

import {
  createContext,
  type ReactNode,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useSyncExternalStore,
} from "react";
import { createCanvasManagerRouter } from "@/components/widgets/canvas-manager-subscriptions";
import { createLinkManager } from "@/components/widgets/link-subscriptions";
import {
  resolveModelRef,
  type WidgetModel,
  type WidgetStore,
} from "@/components/widgets/widget-store";
import { WidgetStoreContext } from "@/components/widgets/widget-store-context";
import {
  createWidgetBridgeClient,
  type WidgetBridgeClient,
} from "./widget-bridge-client";

// === Context Types ===

interface IframeWidgetStoreContextValue {
  store: WidgetStore;
  /** Send a state update to the parent (which forwards to kernel) */
  sendUpdate: (
    commId: string,
    state: Record<string, unknown>,
    buffers?: ArrayBuffer[],
  ) => void;
  /** Send a custom message to the parent (which forwards to kernel) */
  sendCustom: (
    commId: string,
    content: Record<string, unknown>,
    buffers?: ArrayBuffer[],
  ) => void;
  /** Close a comm channel via parent */
  closeComm: (commId: string) => void;
}

// === Context ===

const IframeWidgetStoreContext =
  createContext<IframeWidgetStoreContextValue | null>(null);

// === Provider ===

interface IframeWidgetStoreProviderProps {
  children: ReactNode;
}

/**
 * Provider component for widgets in the isolated iframe.
 *
 * Creates a CommBridgeClient that:
 * - Maintains a local WidgetStore for state
 * - Receives comm messages from parent via postMessage
 * - Sends state updates/custom messages back to parent
 *
 * Also provides to WidgetStoreContext so existing widget components work unchanged.
 */
export function IframeWidgetStoreProvider({
  children,
}: IframeWidgetStoreProviderProps) {
  // Create bridge client once
  const clientRef = useRef<WidgetBridgeClient | null>(null);
  if (!clientRef.current) {
    clientRef.current = createWidgetBridgeClient();
  }
  const client = clientRef.current;

  // Clean up on unmount
  useEffect(() => {
    return () => {
      client.dispose();
    };
  }, [client]);

  // Set up link subscriptions (jslink/jsdlink)
  useEffect(
    () => createLinkManager(client.store, client.sendUpdate),
    [client.store, client.sendUpdate],
  );

  // Set up canvas manager router (ipycanvas)
  useEffect(() => createCanvasManagerRouter(client.store), [client.store]);

  // Value for iframe-specific context
  const iframeValue = useMemo<IframeWidgetStoreContextValue>(
    () => ({
      store: client.store,
      sendUpdate: client.sendUpdate,
      sendCustom: client.sendCustom,
      closeComm: client.closeComm,
    }),
    [client],
  );

  // Value for main WidgetStoreContext (so existing widget components work)
  // sendMessage parses Jupyter protocol and routes to bridge functions
  const mainContextValue = useMemo(
    () => ({
      store: client.store,
      handleMessage: () => {
        // No-op: messages come through postMessage in iframe
      },
      sendMessage: (msg: {
        content: {
          comm_id?: string;
          data?: {
            method?: string;
            state?: Record<string, unknown>;
            content?: Record<string, unknown>;
          };
        };
        buffers?: ArrayBuffer[];
      }) => {
        // Route Jupyter protocol messages to bridge functions
        const commId = msg.content?.comm_id;
        const method = msg.content?.data?.method;
        const buffers = msg.buffers;

        if (!commId) return;

        if (method === "update" && msg.content?.data?.state) {
          client.sendUpdate(commId, msg.content.data.state, buffers);
        } else if (method === "custom" && msg.content?.data?.content) {
          client.sendCustom(commId, msg.content.data.content, buffers);
        }
      },
      sendUpdate: client.sendUpdate,
      sendCustom: client.sendCustom,
      closeComm: client.closeComm,
    }),
    [client],
  );

  return (
    <WidgetStoreContext.Provider value={mainContextValue}>
      <IframeWidgetStoreContext.Provider value={iframeValue}>
        {children}
      </IframeWidgetStoreContext.Provider>
    </WidgetStoreContext.Provider>
  );
}

// === Hooks ===

/**
 * Access the iframe widget store context.
 * Returns null if used outside of IframeWidgetStoreProvider.
 */
export function useIframeWidgetStore(): IframeWidgetStoreContextValue | null {
  return useContext(IframeWidgetStoreContext);
}

/**
 * Access the iframe widget store context, throwing if not available.
 */
export function useIframeWidgetStoreRequired(): IframeWidgetStoreContextValue {
  const ctx = useContext(IframeWidgetStoreContext);
  if (!ctx) {
    throw new Error(
      "useIframeWidgetStoreRequired must be used within IframeWidgetStoreProvider",
    );
  }
  return ctx;
}

/**
 * Subscribe to all widget models.
 */
export function useIframeWidgetModels(): Map<string, WidgetModel> {
  const { store } = useIframeWidgetStoreRequired();

  return useSyncExternalStore(
    store.subscribe,
    store.getSnapshot,
    store.getSnapshot,
  );
}

/**
 * Subscribe to a specific widget model.
 */
export function useIframeWidgetModel(modelId: string): WidgetModel | undefined {
  const { store } = useIframeWidgetStoreRequired();

  const subscribe = useCallback(
    (callback: () => void) => store.subscribe(callback),
    [store],
  );

  const getSnapshot = useCallback(
    () => store.getModel(modelId),
    [store, modelId],
  );

  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

/**
 * Subscribe to a specific key in a widget model's state.
 */
export function useIframeWidgetModelValue<T = unknown>(
  modelId: string,
  key: string,
): T | undefined {
  const { store } = useIframeWidgetStoreRequired();

  const subscribe = useCallback(
    (callback: () => void) => store.subscribeToKey(modelId, key, callback),
    [store, modelId, key],
  );

  const getSnapshot = useCallback(
    () => store.getModel(modelId)?.state[key] as T | undefined,
    [store, modelId, key],
  );

  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

/**
 * Get a value from a widget model, resolving IPY_MODEL_ references.
 */
export function useIframeResolvedModelValue<T = unknown>(
  modelId: string,
  key: string,
): T | WidgetModel | undefined {
  const { store } = useIframeWidgetStoreRequired();
  const value = useIframeWidgetModelValue(modelId, key);

  const resolved = resolveModelRef(value, (id) => store.getModel(id));

  return resolved as T | WidgetModel | undefined;
}

/**
 * Check if a widget model was explicitly closed.
 */
export function useIframeWasWidgetClosed(modelId: string): boolean {
  const { store } = useIframeWidgetStoreRequired();
  return store.wasModelClosed(modelId);
}
