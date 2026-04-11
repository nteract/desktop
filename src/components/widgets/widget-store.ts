export interface WidgetModel {
  /** comm_id from the Jupyter protocol */
  id: string;
  /** Widget state (value, min, max, description, etc.) */
  state: Record<string, unknown>;
  /** Binary buffers for data-heavy widgets */
  buffers: ArrayBuffer[];
  /** Model class name, e.g., "IntSliderModel", "AnyModel" */
  modelName: string;
  /** Model module, e.g., "@jupyter-widgets/controls", "anywidget" */
  modelModule: string;
}

type Listener = () => void;
type KeyListener = (value: unknown) => void;
// Anywidgets expect DataView[] so they can access .buffer for the underlying ArrayBuffer
// This matches JupyterLab services which deserializes buffers as DataView[]
type CustomMessageCallback = (content: Record<string, unknown>, buffers?: DataView[]) => void;

export interface WidgetStore {
  /** Subscribe to all model changes (for useSyncExternalStore) */
  subscribe: (listener: Listener) => () => void;
  /** Get current models snapshot (for useSyncExternalStore) */
  getSnapshot: () => Map<string, WidgetModel>;
  /** Get a single model by ID */
  getModel: (modelId: string) => WidgetModel | undefined;
  /** Create a new model (on comm_open) */
  createModel: (commId: string, state: Record<string, unknown>, buffers?: ArrayBuffer[]) => void;
  /** Update a model's state (on comm_msg with method: "update") */
  updateModel: (
    commId: string,
    statePatch: Record<string, unknown>,
    buffers?: ArrayBuffer[],
  ) => void;
  /** Delete a model (on comm_close) */
  deleteModel: (commId: string) => void;
  /** Check if a model was explicitly closed (vs never existed) */
  wasModelClosed: (modelId: string) => boolean;
  /** Subscribe to a specific key on a specific model */
  subscribeToKey: (modelId: string, key: string, callback: KeyListener) => () => void;
  /** Emit a custom message to listeners for a model */
  emitCustomMessage: (
    commId: string,
    content: Record<string, unknown>,
    buffers?: ArrayBuffer[],
  ) => void;
  /** Subscribe to custom messages for a model */
  subscribeToCustomMessage: (commId: string, callback: CustomMessageCallback) => () => void;
}

// === IPY_MODEL_ Reference Resolution ===

const IPY_MODEL_PREFIX = "IPY_MODEL_";

/**
 * Check if a value is an IPY_MODEL_ reference string.
 */
export function isModelRef(value: unknown): value is string {
  return typeof value === "string" && value.startsWith(IPY_MODEL_PREFIX);
}

/**
 * Extract the model ID from an IPY_MODEL_ reference.
 */
export function parseModelRef(ref: string): string | null {
  if (ref.startsWith(IPY_MODEL_PREFIX)) {
    return ref.slice(IPY_MODEL_PREFIX.length);
  }
  return null;
}

/**
 * Resolve an IPY_MODEL_ reference to the actual model.
 * If the value is not a reference, returns it unchanged.
 */
export function resolveModelRef(
  value: unknown,
  getModel: (id: string) => WidgetModel | undefined,
): unknown {
  if (isModelRef(value)) {
    const refId = parseModelRef(value);
    return refId ? getModel(refId) : value;
  }
  return value;
}

// === Store Factory ===

/**
 * Create a new widget store instance.
 *
 * The store manages widget models and provides fine-grained subscriptions
 * for reactive updates. It's designed to work with React's useSyncExternalStore.
 */
export function createWidgetStore(): WidgetStore {
  // Internal state - using Map for O(1) lookups
  let models = new Map<string, WidgetModel>();

  // Track explicitly closed models (for distinguishing from "never existed")
  const closedModels = new Set<string>();

  // Global listeners (for useSyncExternalStore)
  const listeners = new Set<Listener>();

  // Key-specific listeners for fine-grained subscriptions
  // Structure: modelId -> key -> Set<callback>
  const keyListeners = new Map<string, Map<string, Set<KeyListener>>>();

  // Custom message listeners
  // Structure: modelId -> Set<callback>
  const customListeners = new Map<string, Set<CustomMessageCallback>>();

  // Buffered custom messages for comm_ids with no listeners yet
  // Structure: commId -> Array<{ content, buffers }>
  const customMessageBuffer = new Map<
    string,
    Array<{ content: Record<string, unknown>; buffers?: DataView[] }>
  >();
  const MAX_BUFFERED_MESSAGES = 1000;

  // Notify all global listeners that something changed
  function emitChange() {
    listeners.forEach((listener) => listener());
  }

  // Notify key-specific listeners for changed keys
  function emitKeyChanges(modelId: string, changedKeys: string[]) {
    const modelListeners = keyListeners.get(modelId);
    if (!modelListeners) return;

    const model = models.get(modelId);
    for (const key of changedKeys) {
      const keyCallbacks = modelListeners.get(key);
      if (keyCallbacks) {
        const value = model?.state[key];
        keyCallbacks.forEach((cb) => cb(value));
      }
    }
  }

  return {
    subscribe(listener: Listener): () => void {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },

    getSnapshot(): Map<string, WidgetModel> {
      return models;
    },

    getModel(modelId: string): WidgetModel | undefined {
      return models.get(modelId);
    },

    createModel(commId: string, state: Record<string, unknown>, buffers: ArrayBuffer[] = []): void {
      // Handle re-open: remove from closed set if re-opening
      closedModels.delete(commId);

      // Extract model metadata from state
      const modelName = (state._model_name as string) || "UnknownModel";
      const modelModule = (state._model_module as string) || "";

      const model: WidgetModel = {
        id: commId,
        state,
        buffers,
        modelName,
        modelModule,
      };

      // Create new Map to trigger useSyncExternalStore update
      models = new Map(models).set(commId, model);
      emitChange();
    },

    updateModel(
      commId: string,
      statePatch: Record<string, unknown>,
      buffers?: ArrayBuffer[],
    ): void {
      const existing = models.get(commId);
      if (!existing) {
        // Model doesn't exist yet - this can happen if messages arrive out of order
        // In practice, comm_open should always come first
        return;
      }

      // Merge state patch into existing state
      const newState = { ...existing.state, ...statePatch };
      const newModel: WidgetModel = {
        ...existing,
        state: newState,
        buffers: buffers ?? existing.buffers,
      };

      // Create new Map to trigger useSyncExternalStore update
      models = new Map(models).set(commId, newModel);

      // Emit changes
      emitChange();
      emitKeyChanges(commId, Object.keys(statePatch));
    },

    deleteModel(commId: string): void {
      if (!models.has(commId)) return;

      // Track that this model was explicitly closed
      closedModels.add(commId);

      // Create new Map without the deleted model
      models = new Map(models);
      models.delete(commId);

      // Clean up listeners and buffered messages for this model
      keyListeners.delete(commId);
      customListeners.delete(commId);
      customMessageBuffer.delete(commId);

      emitChange();
    },

    subscribeToKey(modelId: string, key: string, callback: KeyListener): () => void {
      // Ensure model entry exists
      if (!keyListeners.has(modelId)) {
        keyListeners.set(modelId, new Map());
      }
      const modelMap = keyListeners.get(modelId)!;

      // Ensure key entry exists
      if (!modelMap.has(key)) {
        modelMap.set(key, new Set());
      }
      modelMap.get(key)?.add(callback);

      // Return unsubscribe function
      return () => {
        modelMap.get(key)?.delete(callback);

        // Clean up empty sets
        if (modelMap.get(key)?.size === 0) {
          modelMap.delete(key);
        }
        if (modelMap.size === 0) {
          keyListeners.delete(modelId);
        }
      };
    },

    emitCustomMessage(
      commId: string,
      content: Record<string, unknown>,
      buffers?: ArrayBuffer[],
    ): void {
      // Convert ArrayBuffer[] to DataView[] for anywidget compatibility
      // Anywidgets access the underlying buffer via .buffer property
      const dataViewBuffers = buffers?.map((b) => (b instanceof DataView ? b : new DataView(b)));

      // Always buffer so future subscribers get the history (e.g. a second
      // CanvasWidget subscribing to the same CanvasManagerModel after the
      // first canvas is already mounted and receiving messages).
      if (!customMessageBuffer.has(commId)) {
        customMessageBuffer.set(commId, []);
      }
      const buffer = customMessageBuffer.get(commId)!;
      buffer.push({ content, buffers: dataViewBuffers });
      // Evict oldest messages if over limit
      if (buffer.length > MAX_BUFFERED_MESSAGES) {
        buffer.splice(0, buffer.length - MAX_BUFFERED_MESSAGES);
      }

      // Also deliver to existing subscribers
      const callbacks = customListeners.get(commId);
      if (callbacks && callbacks.size > 0) {
        callbacks.forEach((cb) => cb(content, dataViewBuffers));
      }
    },

    subscribeToCustomMessage(commId: string, callback: CustomMessageCallback): () => void {
      // Ensure entry exists
      if (!customListeners.has(commId)) {
        customListeners.set(commId, new Set());
      }
      customListeners.get(commId)?.add(callback);

      // Flush any buffered messages to this new subscriber.
      // Keep the buffer (don't delete) so other subscribers to the same
      // comm_id also receive these messages — e.g. multiple CanvasWidgets
      // subscribing to one CanvasManagerModel.
      const buffered = customMessageBuffer.get(commId);
      if (buffered && buffered.length > 0) {
        for (const msg of buffered) {
          callback(msg.content, msg.buffers);
        }
      }

      // Return unsubscribe function
      return () => {
        customListeners.get(commId)?.delete(callback);

        // Clean up empty sets
        if (customListeners.get(commId)?.size === 0) {
          customListeners.delete(commId);
        }
      };
    },

    wasModelClosed(modelId: string): boolean {
      return closedModels.has(modelId);
    },
  };
}
