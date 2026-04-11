import { parseModelRef, type WidgetStore } from "./widget-store";

/**
 * Parse a link source/target tuple from widget state.
 * The state arrives as: ["IPY_MODEL_<id>", "attribute_name"]
 * Returns [modelId, attrName] or null if malformed.
 */
function parseLinkTarget(tuple: unknown): [string, string] | null {
  if (
    !Array.isArray(tuple) ||
    tuple.length !== 2 ||
    typeof tuple[0] !== "string" ||
    typeof tuple[1] !== "string"
  ) {
    return null;
  }
  const modelId = parseModelRef(tuple[0]);
  if (!modelId) return null;
  return [modelId, tuple[1]];
}

/**
 * Set up a one-way property subscription (source → target).
 * Returns a cleanup function to tear down the subscription.
 */
function setupDirectionalLink(store: WidgetStore, linkModelId: string): () => void {
  let keyUnsub: (() => void) | undefined;
  let globalUnsub: (() => void) | undefined;
  let isSetUp = false;

  function trySetup() {
    if (isSetUp) return;

    const linkModel = store.getModel(linkModelId);
    if (!linkModel) return;

    const src = parseLinkTarget(linkModel.state.source);
    const tgt = parseLinkTarget(linkModel.state.target);
    if (!src || !tgt) return;

    const [sourceModelId, sourceAttr] = src;
    const [targetModelId, targetAttr] = tgt;

    if (!store.getModel(sourceModelId) || !store.getModel(targetModelId)) {
      return;
    }
    isSetUp = true;

    // Initial sync: read source value, write to target.
    // Uses store.updateModel directly — jslink/jsdlink are client-side only,
    // no CRDT write needed. The originating widget's sendUpdate handles persistence.
    const sourceModel = store.getModel(sourceModelId);
    if (sourceModel) {
      const currentValue = sourceModel.state[sourceAttr];
      if (currentValue !== undefined) {
        store.updateModel(targetModelId, { [targetAttr]: currentValue });
      }
    }

    // Subscribe to source changes, propagate to target
    keyUnsub = store.subscribeToKey(sourceModelId, sourceAttr, (newValue) => {
      const tgt = store.getModel(targetModelId);
      if (tgt && tgt.state[targetAttr] === newValue) return;
      store.updateModel(targetModelId, { [targetAttr]: newValue });
    });

    // Clean up global listener once setup is complete
    if (globalUnsub) {
      globalUnsub();
      globalUnsub = undefined;
    }
  }

  trySetup();

  // If source/target models aren't ready yet, wait for them
  if (!isSetUp) {
    globalUnsub = store.subscribe(() => trySetup());
  }

  return () => {
    globalUnsub?.();
    keyUnsub?.();
  };
}

/**
 * Set up a bidirectional property subscription (source ↔ target).
 * Uses a synchronous guard to prevent infinite loops since store
 * subscriptions fire synchronously during updateModel.
 * Returns a cleanup function to tear down the subscriptions.
 */
function setupBidirectionalLink(store: WidgetStore, linkModelId: string): () => void {
  const keyUnsubs: (() => void)[] = [];
  let globalUnsub: (() => void) | undefined;
  let isSetUp = false;
  let isSyncing = false;

  function trySetup() {
    if (isSetUp) return;

    const linkModel = store.getModel(linkModelId);
    if (!linkModel) return;

    const src = parseLinkTarget(linkModel.state.source);
    const tgt = parseLinkTarget(linkModel.state.target);
    if (!src || !tgt) return;

    const [sourceModelId, sourceAttr] = src;
    const [targetModelId, targetAttr] = tgt;

    if (!store.getModel(sourceModelId) || !store.getModel(targetModelId)) {
      return;
    }
    isSetUp = true;

    // Initial sync: source → target.
    // Local-only — jslink is client-side, no CRDT write needed.
    const sourceModel = store.getModel(sourceModelId);
    if (sourceModel) {
      const currentValue = sourceModel.state[sourceAttr];
      if (currentValue !== undefined) {
        isSyncing = true;
        store.updateModel(targetModelId, { [targetAttr]: currentValue });
        isSyncing = false;
      }
    }

    // Source → Target
    keyUnsubs.push(
      store.subscribeToKey(sourceModelId, sourceAttr, (newValue) => {
        if (isSyncing) return;
        const tgt = store.getModel(targetModelId);
        if (tgt && tgt.state[targetAttr] === newValue) return;
        isSyncing = true;
        store.updateModel(targetModelId, { [targetAttr]: newValue });
        isSyncing = false;
      }),
    );

    // Target → Source
    keyUnsubs.push(
      store.subscribeToKey(targetModelId, targetAttr, (newValue) => {
        if (isSyncing) return;
        const src = store.getModel(sourceModelId);
        if (src && src.state[sourceAttr] === newValue) return;
        isSyncing = true;
        store.updateModel(sourceModelId, { [sourceAttr]: newValue });
        isSyncing = false;
      }),
    );

    // Clean up global listener once setup is complete
    if (globalUnsub) {
      globalUnsub();
      globalUnsub = undefined;
    }
  }

  trySetup();

  // If source/target models aren't ready yet, wait for them
  if (!isSetUp) {
    globalUnsub = store.subscribe(() => trySetup());
  }

  return () => {
    globalUnsub?.();
    keyUnsubs.forEach((unsub) => unsub());
  };
}

/**
 * Create a link manager that monitors the store for LinkModel and
 * DirectionalLinkModel widgets and manages their property subscriptions.
 *
 * Returns a cleanup function that tears down all active link subscriptions.
 *
 * Called automatically by WidgetStoreProvider. For non-React integrations
 * (e.g. iframe isolation), call this directly after creating the store.
 *
 * @param store - The widget store instance
 */
export function createLinkManager(store: WidgetStore): () => void {
  const activeLinks = new Map<string, () => void>();
  let lastSize = -1;

  function scan() {
    const models = store.getSnapshot();

    // Only do a full scan when models are added or removed.
    // State updates (e.g. slider drags) don't change the map size.
    if (models.size === lastSize) return;
    lastSize = models.size;

    // Set up new links
    models.forEach((model, id) => {
      if (activeLinks.has(id)) return;

      if (model.modelName === "DirectionalLinkModel") {
        activeLinks.set(id, setupDirectionalLink(store, id));
      } else if (model.modelName === "LinkModel") {
        activeLinks.set(id, setupBidirectionalLink(store, id));
      }
    });

    // Clean up removed links
    for (const [id, cleanup] of activeLinks) {
      if (!models.has(id)) {
        cleanup();
        activeLinks.delete(id);
      }
    }
  }

  const unsubscribe = store.subscribe(scan);
  scan();

  return () => {
    unsubscribe();
    activeLinks.forEach((cleanup) => cleanup());
    activeLinks.clear();
  };
}
