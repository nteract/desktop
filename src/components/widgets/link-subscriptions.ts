import { parseModelRef, type WidgetStore } from "./widget-store";

/**
 * Dispatch a widget state patch for a linked target.
 *
 * `createLinkManager` accepts this callback so the app can choose the
 * scope of the propagation. In practice this is a store-only write:
 * `widgets.jslink` / `widgets.jsdlink` are the ipywidgets
 * frontend-only link primitives — they mirror a source property into
 * a target property in the browser without involving the kernel. The
 * kernel-side equivalent (`widgets.link`) uses Python traitlet
 * `observe`/`notify`, which flows through the normal CRDT round-trip
 * without touching this code.
 */
export type LinkWriter = (commId: string, patch: Record<string, unknown>) => void;

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
function setupDirectionalLink(
  store: WidgetStore,
  linkModelId: string,
  writer: LinkWriter,
): () => void {
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

    // Initial sync: read source value, propagate to the target.
    const sourceModel = store.getModel(sourceModelId);
    if (sourceModel) {
      const currentValue = sourceModel.state[sourceAttr];
      if (currentValue !== undefined) {
        writer(targetModelId, { [targetAttr]: currentValue });
      }
    }

    // Subscribe to source changes, propagate to target.
    keyUnsub = store.subscribeToKey(sourceModelId, sourceAttr, (newValue) => {
      const tgt = store.getModel(targetModelId);
      if (tgt && tgt.state[targetAttr] === newValue) return;
      writer(targetModelId, { [targetAttr]: newValue });
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
 *
 * Bidirectional link loops would normally go infinite: source change
 * → target write → target echo → source write → ... . The equality
 * check (`tgt.state[targetAttr] === newValue`) is the primary
 * termination guard — once both sides converge on a value, the
 * callback no-ops. Because CRDT writes flow through an async
 * projection hop, we also keep a synchronous `isSyncing` flag to
 * suppress the immediately reciprocal emission that lands inside the
 * same callback tree.
 *
 * Returns a cleanup function to tear down the subscriptions.
 */
function setupBidirectionalLink(
  store: WidgetStore,
  linkModelId: string,
  writer: LinkWriter,
): () => void {
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

    // Initial sync: source → target via the injected writer.
    const sourceModel = store.getModel(sourceModelId);
    if (sourceModel) {
      const currentValue = sourceModel.state[sourceAttr];
      if (currentValue !== undefined) {
        isSyncing = true;
        writer(targetModelId, { [targetAttr]: currentValue });
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
        writer(targetModelId, { [targetAttr]: newValue });
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
        writer(sourceModelId, { [sourceAttr]: newValue });
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
 * Link-propagated updates go through the injected `writer` callback.
 * The normal wiring is a store-only write — jslink/jsdlink are
 * frontend-only by design — but the callback is abstract so iframe
 * isolation and test harnesses can route it however they need.
 *
 * Returns a cleanup function that tears down all active link subscriptions.
 *
 * Called automatically by WidgetStoreProvider. For non-React
 * integrations (e.g. iframe isolation), call this directly after
 * creating the store.
 *
 * @param store - The widget store instance
 * @param writer - Dispatch callback for propagating updates to linked comms
 */
export function createLinkManager(store: WidgetStore, writer: LinkWriter): () => void {
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
        activeLinks.set(id, setupDirectionalLink(store, id, writer));
      } else if (model.modelName === "LinkModel") {
        activeLinks.set(id, setupBidirectionalLink(store, id, writer));
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
