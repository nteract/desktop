/**
 * React context and hooks for the widget model store.
 *
 * Provides:
 * - WidgetStoreProvider: Wrap your app to enable widget support
 * - useWidgetStore: Access the store context (nullable)
 * - useWidgetModels: Subscribe to all models
 * - useWidgetModel: Subscribe to a single model
 * - useWidgetModelValue: Subscribe to a single key (finest granularity)
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
import { createCanvasManagerRouter } from "./canvas-manager-subscriptions";
import { createLinkManager } from "./link-subscriptions";
import {
  type JupyterCommMessage,
  type SendMessage,
  useCommRouter,
} from "./use-comm-router";
import {
  createWidgetStore,
  resolveModelRef,
  type WidgetModel,
  type WidgetStore,
} from "./widget-store";

// === Context Types ===

interface WidgetStoreContextValue {
  store: WidgetStore;
  /** Handle incoming Jupyter comm messages */
  handleMessage: (msg: JupyterCommMessage) => void;
  /** Raw send function (prefer sendUpdate/sendCustom for specific cases) */
  sendMessage: SendMessage;
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

// === Context ===

// Export context for use in isolated iframe widget provider
export const WidgetStoreContext = createContext<WidgetStoreContextValue | null>(
  null,
);

// === Provider ===

interface WidgetStoreProviderProps {
  children: ReactNode;
  /** Function to send messages back to the kernel (for widget interactions) */
  sendMessage?: SendMessage;
  /** Optional update manager for debounced CRDT writes + echo suppression. */
  updateManager?: import("./widget-update-manager").WidgetUpdateManager;
}

/**
 * Provider component for the widget store.
 *
 * Wrap your app (or the part that needs widgets) with this provider.
 * Pass a sendMessage function to enable widget interactions back to the kernel.
 */
export function WidgetStoreProvider({
  children,
  sendMessage = () => {},
  updateManager,
}: WidgetStoreProviderProps) {
  // Create store once and keep it stable across renders
  const storeRef = useRef<WidgetStore | null>(null);
  if (!storeRef.current) {
    storeRef.current = createWidgetStore();
  }
  const store = storeRef.current;

  // Use the comm router hook for message handling
  const { handleMessage, sendUpdate, sendCustom, closeComm } = useCommRouter({
    sendMessage,
    store,
    updateManager,
  });

  // Manage link subscriptions (jslink/jsdlink) at the store level.
  // Headless widgets like LinkModel have _view_name: null and won't be
  // in any container's children, so they need store-level subscriptions.
  // Links use store.updateModel directly (client-side only, no CRDT write).
  useEffect(() => createLinkManager(store), [store]);
  useEffect(() => createCanvasManagerRouter(store), [store]);

  const value = useMemo(
    () => ({
      store,
      handleMessage,
      sendMessage,
      sendUpdate,
      sendCustom,
      closeComm,
    }),
    [store, handleMessage, sendMessage, sendUpdate, sendCustom, closeComm],
  );

  return (
    <WidgetStoreContext.Provider value={value}>
      {children}
    </WidgetStoreContext.Provider>
  );
}

// === Hooks ===

/**
 * Access the widget store context.
 * Returns null if used outside of WidgetStoreProvider.
 */
export function useWidgetStore(): WidgetStoreContextValue | null {
  return useContext(WidgetStoreContext);
}

/**
 * Access the widget store context, throwing if not available.
 * Use this when you know you're inside a WidgetStoreProvider.
 */
export function useWidgetStoreRequired(): WidgetStoreContextValue {
  const ctx = useContext(WidgetStoreContext);
  if (!ctx) {
    throw new Error(
      "useWidgetStoreRequired must be used within WidgetStoreProvider",
    );
  }
  return ctx;
}

/**
 * Subscribe to all widget models.
 * Re-renders when any model is added, updated, or removed.
 */
export function useWidgetModels(): Map<string, WidgetModel> {
  const { store } = useWidgetStoreRequired();

  return useSyncExternalStore(
    store.subscribe,
    store.getSnapshot,
    store.getSnapshot, // SSR snapshot (same as client)
  );
}

/**
 * Subscribe to a specific widget model.
 * Re-renders when that model's state changes.
 */
export function useWidgetModel(modelId: string): WidgetModel | undefined {
  const { store } = useWidgetStoreRequired();

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
 * This is the finest granularity - only re-renders when that specific key changes.
 *
 * @example
 * const value = useWidgetModelValue<number>(modelId, 'value');
 * const description = useWidgetModelValue<string>(modelId, 'description');
 */
export function useWidgetModelValue<T = unknown>(
  modelId: string,
  key: string,
): T | undefined {
  const { store } = useWidgetStoreRequired();

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
 * If the value is an IPY_MODEL_<id> reference, returns the referenced model.
 *
 * @example
 * // If state.layout is "IPY_MODEL_abc123", this returns the LayoutModel
 * const layout = useResolvedModelValue(modelId, 'layout');
 */
export function useResolvedModelValue<T = unknown>(
  modelId: string,
  key: string,
): T | WidgetModel | undefined {
  const { store } = useWidgetStoreRequired();
  const value = useWidgetModelValue(modelId, key);

  // Resolve IPY_MODEL_ reference if applicable
  const resolved = resolveModelRef(value, (id) => store.getModel(id));

  return resolved as T | WidgetModel | undefined;
}

/**
 * Check if a widget model was explicitly closed (e.g., tqdm with leave=False).
 * Returns true if the model was closed, false if it never existed or is active.
 *
 * Note: This hook is not reactive on its own. It relies on being used alongside
 * useWidgetModel, which subscribes to store changes and triggers re-renders.
 */
export function useWasWidgetClosed(modelId: string): boolean {
  const { store } = useWidgetStoreRequired();
  return store.wasModelClosed(modelId);
}

// Re-export store-level managers for non-React integrations (e.g. iframe isolation)
export { createCanvasManagerRouter } from "./canvas-manager-subscriptions";
export { createLinkManager } from "./link-subscriptions";
export type {
  JupyterCommMessage,
  JupyterMessageHeader,
  SendMessage,
} from "./use-comm-router";
// Re-export types from use-comm-router
export { useCommRouter } from "./use-comm-router";
export type { WidgetModel, WidgetStore } from "./widget-store";
// Re-export types and utilities from widget-store
export { isModelRef, parseModelRef, resolveModelRef } from "./widget-store";
