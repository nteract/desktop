/**
 * anywidget ESM loader and AFM (AnyWidget Frontend Module) interface.
 *
 * This module handles dynamic loading of anywidget ESM code and provides
 * the AFM-compatible model interface that widget code expects.
 *
 * @see https://anywidget.dev/en/afm/
 */

import { useCallback, useEffect, useRef, useState } from "react";
import { getCrdtCommWriter } from "./crdt-comm-writer";
import {
  type SendMessage,
  useWidgetModel,
  useWidgetStoreRequired,
  type WidgetModel,
  type WidgetStore,
} from "./widget-store-context";

// === AFM Types ===

/**
 * The AFM (AnyWidget Frontend Module) model interface.
 * This is what widget ESM code expects to receive in render().
 */
export interface AnyWidgetModel {
  /** Get a value from the model state */
  get(key: string): unknown;
  /** Set a value in the model state (buffered until save_changes) */
  set(key: string, value: unknown): void;
  /**
   * Subscribe to model events.
   * - "change:key" callbacks receive no arguments (use model.get() to read values)
   * - "change" callbacks receive no arguments (fired alongside change:key events)
   * - "msg:custom" callbacks receive (content, buffers)
   */
  on(event: string, callback: (...args: unknown[]) => void): void;
  /** Unsubscribe from model events */
  off(event: string, callback?: (...args: unknown[]) => void): void;
  /** Send buffered changes to the kernel */
  save_changes(): void;
  /** Send a custom message to the kernel */
  send(
    content: Record<string, unknown>,
    callbacks?: Record<string, unknown>,
    buffers?: ArrayBuffer[],
  ): void;
  /** Access to other widget models */
  widget_manager: {
    get_model(modelId: string): Promise<AnyWidgetModel>;
  };
}

/**
 * Lifecycle methods that a widget definition provides.
 */
type WidgetLifecycle = {
  render?(context: {
    model: AnyWidgetModel;
    el: HTMLElement;
  }): void | (() => void) | Promise<undefined | (() => void)>;
  initialize?(context: { model: AnyWidgetModel }): void | Promise<void>;
};

/**
 * Factory function pattern - default export is a function that returns lifecycle methods.
 * Per AFM spec: "The default export can also be an async function returning this interface."
 */
type WidgetFactory = () => WidgetLifecycle | Promise<WidgetLifecycle>;

/**
 * The expected structure of an anywidget ESM module.
 * Supports both standard pattern (object with render) and factory pattern (function returning object).
 */
interface AnyWidgetModule {
  default?: WidgetLifecycle | WidgetFactory;
  render?(context: {
    model: AnyWidgetModel;
    el: HTMLElement;
  }): void | (() => void) | Promise<undefined | (() => void)>;
  initialize?(context: { model: AnyWidgetModel }): void | Promise<void>;
}

// === ESM Loading ===

/**
 * Load an ESM module from either a URL or inline code.
 *
 * Handles both:
 * - Remote URLs: `https://cdn.example.com/widget.js`
 * - Inline ESM: Actual JavaScript code as a string
 */
export async function loadESM(esm: string): Promise<AnyWidgetModule> {
  // Handle remote URLs directly
  if (esm.startsWith("http://") || esm.startsWith("https://")) {
    // Dynamic import with webpackIgnore comment for bundler compatibility
    return import(/* webpackIgnore: true */ /* @vite-ignore */ esm);
  }

  // Inline ESM - create a blob URL for dynamic import
  const blob = new Blob([esm], { type: "text/javascript" });
  const url = URL.createObjectURL(blob);
  try {
    return await import(/* webpackIgnore: true */ /* @vite-ignore */ url);
  } finally {
    // Clean up the blob URL after import completes
    URL.revokeObjectURL(url);
  }
}

// === CSS Injection ===

/**
 * Inject CSS into the document head for a widget.
 *
 * @returns Cleanup function to remove the style element
 */
export function injectCSS(modelId: string, css: string): () => void {
  const style = document.createElement("style");
  style.setAttribute("data-widget-id", modelId);
  style.textContent = css;
  document.head.appendChild(style);

  return () => {
    style.remove();
  };
}

// === Message Headers ===

/**
 * Session ID for all outgoing messages.
 * Must be stable across messages for the kernel to track the session.
 */
const SESSION_ID = crypto.randomUUID();

/**
 * Create a complete Jupyter message header with all fields.
 * All fields are required for compatibility with strongly-typed backends (Rust, Go).
 */
function createHeader(msgType: string, username: string = "frontend") {
  return {
    msg_id: crypto.randomUUID(),
    msg_type: msgType,
    username,
    session: SESSION_ID,
    date: new Date().toISOString(),
    version: "5.3",
  };
}

// === AFM Model Proxy ===

type EventCallback = (...args: unknown[]) => void;

/**
 * Create an AFM-compatible model proxy that wraps the widget store.
 *
 * The proxy buffers local changes until save_changes() is called,
 * at which point it sends a comm_msg to the kernel.
 */
export function createAFMModelProxy(
  model: WidgetModel,
  store: WidgetStore,
  sendMessage: SendMessage,
  getCurrentState: () => Record<string, unknown>,
): AnyWidgetModel {
  // Buffer for local changes (set but not yet saved)
  const pendingChanges: Record<string, unknown> = {};

  // Event listeners: event name -> Set of callbacks
  const listeners = new Map<string, Set<EventCallback>>();

  // Store unsubscribe functions for key listeners
  const keyUnsubscribers = new Map<string, () => void>();

  // Custom message subscription (only one needed per model)
  let customMessageUnsubscriber: (() => void) | null = null;

  return {
    get(key: string): unknown {
      // Pending changes are caller-owned (just set via model.set()),
      // so return them directly without cloning.
      if (key in pendingChanges) {
        return pendingChanges[key];
      }
      const value = getCurrentState()[key];
      // Deep clone objects/arrays so widget code can mutate them freely.
      // State objects originating from WASM (serde_wasm_bindgen) can have
      // readonly properties in WebKit, causing "Attempted to assign to
      // readonly property" errors when widgets like Plotly mutate in-place.
      if (value !== null && typeof value === "object") {
        try {
          return structuredClone(value);
        } catch {
          return value;
        }
      }
      return value;
    },

    set(key: string, value: unknown): void {
      // Buffer the change locally
      pendingChanges[key] = value;
    },

    save_changes(): void {
      if (Object.keys(pendingChanges).length === 0) return;

      const patch = { ...pendingChanges };

      // Try CRDT path first (writes directly to RuntimeStateDoc,
      // no SendComm round-trip). Falls back to SendComm if CRDT
      // writer isn't available yet.
      const writer = getCrdtCommWriter();
      if (writer) {
        writer(model.id, patch);
        // Optimistically update the WidgetStore for instant feedback
        store.updateModel(model.id, patch);
      } else {
        // Fallback: Send comm_msg with update method to kernel
        sendMessage({
          header: createHeader("comm_msg"),
          parent_header: null,
          metadata: {},
          content: {
            comm_id: model.id,
            data: {
              method: "update",
              state: patch,
              buffer_paths: [],
            },
          },
          buffers: [],
          channel: "shell",
        });
      }

      // Clear pending changes after sending
      for (const key of Object.keys(pendingChanges)) {
        delete pendingChanges[key];
      }
    },

    on(event: string, callback: EventCallback): void {
      // Get or create listener set for this event
      if (!listeners.has(event)) {
        listeners.set(event, new Set());
      }
      listeners.get(event)?.add(callback);

      // Handle change:* events by subscribing to the store
      if (event.startsWith("change:")) {
        const key = event.slice(7); // Remove "change:" prefix

        // Only subscribe once per key
        if (!keyUnsubscribers.has(key)) {
          const unsubscribe = store.subscribeToKey(model.id, key, () => {
            // Notify all listeners for this specific key (no args per AFM spec)
            const keyEvent = `change:${key}`;
            const keyListeners = listeners.get(keyEvent);
            if (keyListeners) {
              keyListeners.forEach((cb) => cb());
            }

            // Also notify generic "change" listeners
            const changeListeners = listeners.get("change");
            if (changeListeners) {
              changeListeners.forEach((cb) => cb());
            }
          });
          keyUnsubscribers.set(key, unsubscribe);
        }
      }

      // Handle msg:custom event by subscribing to store custom messages
      if (event === "msg:custom" && !customMessageUnsubscriber) {
        customMessageUnsubscriber = store.subscribeToCustomMessage(
          model.id,
          (content, buffers) => {
            // Notify all msg:custom listeners
            const msgListeners = listeners.get("msg:custom");
            if (msgListeners) {
              msgListeners.forEach((cb) => cb(content, buffers));
            }
          },
        );
      }
    },

    off(event: string, callback?: EventCallback): void {
      if (!listeners.has(event)) return;

      if (callback) {
        // Remove specific callback
        listeners.get(event)?.delete(callback);
      } else {
        // Remove all callbacks for this event
        listeners.delete(event);
      }

      // Clean up store subscription if no listeners remain for a key
      if (event.startsWith("change:")) {
        const key = event.slice(7);
        const keyEvent = `change:${key}`;

        if (!listeners.has(keyEvent) || listeners.get(keyEvent)?.size === 0) {
          const unsubscribe = keyUnsubscribers.get(key);
          if (unsubscribe) {
            unsubscribe();
            keyUnsubscribers.delete(key);
          }
        }
      }

      // Clean up custom message subscription if no listeners remain
      if (event === "msg:custom") {
        if (
          !listeners.has("msg:custom") ||
          listeners.get("msg:custom")?.size === 0
        ) {
          if (customMessageUnsubscriber) {
            customMessageUnsubscriber();
            customMessageUnsubscriber = null;
          }
        }
      }
    },

    send(
      content: Record<string, unknown>,
      _callbacks?: Record<string, unknown>,
      buffers?: ArrayBuffer[],
    ): void {
      // Send custom message to kernel
      // Full Jupyter protocol message format for strongly-typed backends
      // Note: ipywidgets expects content to be nested in data.content, not spread
      sendMessage({
        header: createHeader("comm_msg"),
        parent_header: null,
        metadata: {},
        content: {
          comm_id: model.id,
          data: {
            method: "custom",
            content: content, // Wrap content properly for ipywidgets protocol
          },
        },
        buffers: buffers ?? [],
        channel: "shell",
      });
    },

    widget_manager: {
      async get_model(modelId: string): Promise<AnyWidgetModel> {
        const refModel = store.getModel(modelId);
        if (!refModel) {
          throw new Error(`Model not found: ${modelId}`);
        }
        // Create a proxy for the referenced model
        return createAFMModelProxy(
          refModel,
          store,
          sendMessage,
          () => store.getModel(modelId)?.state ?? {},
        );
      },
    },
  };
}

// === AnyWidgetView Component ===

interface AnyWidgetViewProps {
  /** The model ID (comm_id) of the widget to render */
  modelId: string;
  /** Optional className for the container element */
  className?: string;
}

/**
 * React component that renders an anywidget.
 *
 * Handles:
 * - Loading ESM code from _esm state
 * - Injecting CSS from _css state
 * - Creating the AFM model proxy
 * - Mounting the widget to a DOM element
 * - Cleanup on unmount
 */
export function AnyWidgetView({ modelId, className }: AnyWidgetViewProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const { store, sendMessage } = useWidgetStoreRequired();
  const [error, setError] = useState<Error | null>(null);

  // Refs for values that need to be fresh but shouldn't trigger effect re-runs
  const storeRef = useRef(store);
  const sendMessageRef = useRef(sendMessage);

  // Keep refs up to date without triggering the main effect
  useEffect(() => {
    storeRef.current = store;
    sendMessageRef.current = sendMessage;
  });

  // Use reactive model hook - triggers re-render when model changes
  const model = useWidgetModel(modelId);

  // Track the _esm value separately to trigger re-mount when it arrives
  // (anywidgets may send _esm in a comm_msg after the initial comm_open)
  const esm = model?.state._esm as string | undefined;
  const css = model?.state._css as string | undefined;

  // Only changes when widget identity changes, not on state updates
  const stableModelId = model?.id;

  // Track cleanup functions and mount state
  const cleanupRef = useRef<{
    css?: () => void;
    widget?: () => void;
  }>({});
  const hasMountedRef = useRef(false);

  // Get current state for the proxy - use ref to always get fresh store
  const getCurrentState = useCallback(
    () => storeRef.current.getModel(modelId)?.state ?? {},
    [modelId],
  );

  useEffect(() => {
    // Wait for container, model, and _esm to be ready
    // Note: _esm may arrive in a comm_msg after the initial comm_open
    if (!containerRef.current || !stableModelId || !esm) {
      // Don't set error - just wait for _esm to arrive via comm_msg
      return;
    }

    // Prevent double-mount
    if (hasMountedRef.current) return;

    // Clear any previous error when we have _esm
    setError(null);

    // Capture esm value for use in async function (TypeScript narrowing)
    const esmCode = esm;

    let isCancelled = false;
    hasMountedRef.current = true;

    async function mount() {
      try {
        // Clear any existing content
        if (containerRef.current) {
          containerRef.current.innerHTML = "";
        }

        // Inject CSS if provided
        if (css) {
          cleanupRef.current.css = injectCSS(modelId, css);
        }

        // Load the ESM module
        const module = await loadESM(esmCode);

        // Check if cancelled after async load
        if (isCancelled) return;

        // Create the AFM model proxy using refs for stable references
        const modelProxy = createAFMModelProxy(
          {
            id: stableModelId,
            state: {},
            buffers: [],
            modelName: "",
            modelModule: "",
          } as WidgetModel,
          storeRef.current,
          sendMessageRef.current,
          getCurrentState,
        );

        // Resolve widget definition - handles both standard and factory patterns
        // Standard: export default { render, initialize }
        // Factory: export default () => ({ render, initialize })
        let widgetDef: WidgetLifecycle | undefined;

        if (typeof module.default === "function") {
          // Factory pattern - call function to get widget definition
          widgetDef = await (module.default as WidgetFactory)();
        } else if (module.default) {
          // Standard object pattern
          widgetDef = module.default as WidgetLifecycle;
        }

        // Get lifecycle methods (from resolved default or top-level exports)
        const render = widgetDef?.render ?? module.render;
        const initialize = widgetDef?.initialize ?? module.initialize;

        if (!render) {
          throw new Error("ESM module has no render function");
        }

        // Call initialize if available
        if (initialize) {
          await initialize({ model: modelProxy });
        }

        // Call render
        const result = await render({
          model: modelProxy,
          el: containerRef.current!,
        });

        // Store cleanup if returned
        if (typeof result === "function") {
          cleanupRef.current.widget = result;
        }
      } catch (err) {
        if (!isCancelled) {
          setError(err instanceof Error ? err : new Error(String(err)));
        }
      }
    }

    mount();

    return () => {
      isCancelled = true;
      // Run cleanup functions
      cleanupRef.current.widget?.();
      cleanupRef.current.css?.();
      cleanupRef.current = {};
      // Clear container on cleanup
      if (containerRef.current) {
        containerRef.current.innerHTML = "";
      }
      hasMountedRef.current = false;
    };
    // Only values that should trigger a full remount:
    // - stableModelId: different widget instance
    // - esm: different widget code
    // - css: different widget styles
    // getCurrentState and modelId are stable unless modelId changes (safe to include)
  }, [stableModelId, esm, css, getCurrentState, modelId]);

  // Model not ready yet
  const modelExists = model !== undefined;

  if (error) {
    return (
      <div
        className={className}
        data-widget-id={modelId}
        data-widget-error="true"
      >
        <div style={{ color: "red", padding: "8px" }}>
          Widget error: {error.message}
        </div>
      </div>
    );
  }

  if (!modelExists) {
    return (
      <div
        className={className}
        data-widget-id={modelId}
        data-widget-loading="true"
      >
        Loading widget...
      </div>
    );
  }

  return (
    <div ref={containerRef} className={className} data-widget-id={modelId} />
  );
}

// === Utility Hook ===

/**
 * Check if a model is an anywidget (has _esm field).
 */
export function isAnyWidget(model: WidgetModel): boolean {
  return (
    typeof model.state._esm === "string" || model.modelModule === "anywidget"
  );
}
