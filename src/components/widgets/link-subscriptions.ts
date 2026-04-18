import { parseModelRef, type WidgetStore } from "./widget-store";

/**
 * Dispatch a widget state patch for a linked target.
 *
 * `createLinkManager` accepts this callback instead of writing to the
 * store directly (the pre-A2 behavior). Routing link-propagated
 * updates through the CRDT writer means the target's state stays in
 * sync with the kernel — the old store-only path left targets
 * diverged from the authoritative RuntimeStateDoc, which was fine
 * pre-A2 when the store was itself a dual-write optimistic mirror,
 * but wrong post-A2 (store is a projection; kernel needs the write).
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

    // Initial sync: read source value, write to target through the
    // CRDT writer so the kernel learns the target's new value. The
    // store will update when `projectLocalState` fires the commChanges$
    // emission for the target's write.
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

    // Initial sync: source → target via the CRDT writer.
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
 * Post-A2 semantics: link-propagated updates go through `writer`
 * (the CRDT commit path) rather than `store.updateModel` directly.
 * The store picks up the target's new value from the projectLocalState
 * emission that follows the write. Keeps the link's target state in
 * agreement with the kernel's view.
 *
 * Returns a cleanup function that tears down all active link subscriptions.
 *
 * Called automatically by WidgetStoreProvider. For non-React
 * integrations (e.g. iframe isolation), call this directly after
 * creating the store.
 *
 * @param store - The widget store instance
 * @param writer - CRDT writer for propagating updates to linked comms
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
