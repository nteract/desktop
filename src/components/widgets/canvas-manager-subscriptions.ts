import type { WidgetStore } from "./widget-store";

// ipycanvas drawing command names indexed by protocol number.
// Duplicated from ipycanvas-commands.ts to keep the router self-contained
// within the widget-store package. Must match the Python-side enum in ipycanvas.
const COMMANDS = [
  "fillRect",
  "strokeRect",
  "fillRects",
  "strokeRects",
  "clearRect",
  "fillArc",
  "fillCircle",
  "strokeArc",
  "strokeCircle",
  "fillArcs",
  "strokeArcs",
  "fillCircles",
  "strokeCircles",
  "strokeLine",
  "beginPath",
  "closePath",
  "stroke",
  "strokePath",
  "fillPath",
  "fill",
  "moveTo",
  "lineTo",
  "rect",
  "arc",
  "ellipse",
  "arcTo",
  "quadraticCurveTo",
  "bezierCurveTo",
  "fillText",
  "strokeText",
  "setLineDash",
  "drawImage",
  "putImageData",
  "clip",
  "save",
  "restore",
  "translate",
  "rotate",
  "scale",
  "transform",
  "setTransform",
  "resetTransform",
  "set",
  "clear",
  "sleep",
  "fillPolygon",
  "strokePolygon",
  "strokeLines",
  "fillPolygons",
  "strokePolygons",
  "strokeLineSegments",
  "fillStyledRects",
  "strokeStyledRects",
  "fillStyledCircles",
  "strokeStyledCircles",
  "fillStyledArcs",
  "strokeStyledArcs",
  "fillStyledPolygons",
  "strokeStyledPolygons",
  "strokeStyledLineSegments",
  "switchCanvas",
] as const;

/**
 * Convert a DataView to a TypedArray based on dtype metadata.
 * Inlined here to avoid depending on ipycanvas-commands.ts,
 * keeping the router self-contained within the widget-store package.
 */
function getTypedArray(dataview: DataView, metadata: { dtype: string }): ArrayBufferView {
  const buffer = dataview.buffer;
  switch (metadata.dtype) {
    case "int8":
      return new Int8Array(buffer);
    case "uint8":
      return new Uint8Array(buffer);
    case "int16":
      return new Int16Array(buffer);
    case "uint16":
      return new Uint16Array(buffer);
    case "int32":
      return new Int32Array(buffer);
    case "uint32":
      return new Uint32Array(buffer);
    case "float32":
      return new Float32Array(buffer);
    case "float64":
      return new Float64Array(buffer);
    default:
      return new Uint8Array(buffer);
  }
}

/**
 * Walk a command structure and collect switchCanvas target IDs.
 * Calls setTarget as a side effect so subsequent messages without
 * switchCanvas route to the last known target.
 */
function collectSwitchCanvasTargets(
  commands: unknown,
  targets: Set<string>,
  setTarget: (id: string) => void,
): void {
  if (!Array.isArray(commands) || commands.length === 0) return;

  if (Array.isArray(commands[0])) {
    for (const sub of commands) {
      collectSwitchCanvasTargets(sub, targets, setTarget);
    }
  } else {
    const cmdIndex = commands[0] as number;
    if (COMMANDS[cmdIndex] === "switchCanvas") {
      const args = commands[1] as string[] | undefined;
      const ref = args?.[0] ?? "";
      const targetId = ref.startsWith("IPY_MODEL_") ? ref.slice(10) : ref;
      setTarget(targetId);
      targets.add(targetId);
    }
  }
}

/**
 * Set up message routing for a single CanvasManagerModel.
 * Returns a cleanup function to tear down the subscription.
 */
function setupManagerRouting(store: WidgetStore, managerId: string): () => void {
  let currentTarget: string | null = null;

  return store.subscribeToCustomMessage(managerId, (content, buffers) => {
    if (!buffers || buffers.length === 0) return;

    try {
      const metadata = content as { dtype: string };
      const typedArray = getTypedArray(buffers[0], metadata);
      const jsonStr = new TextDecoder("utf-8").decode(typedArray);
      const commands = JSON.parse(jsonStr);

      const targets = new Set<string>();
      collectSwitchCanvasTargets(commands, targets, (id) => {
        currentTarget = id;
      });

      if (targets.size === 0 && currentTarget) {
        targets.add(currentTarget);
      }

      const rawBuffers = buffers.map((dv) => dv.buffer as ArrayBuffer);
      for (const targetId of targets) {
        store.emitCustomMessage(targetId, content, rawBuffers);
      }
    } catch (err) {
      console.warn("[ipycanvas] Manager routing error:", err);
    }
  });
}

/**
 * Create a canvas manager router that monitors the store for
 * CanvasManagerModel widgets and routes their messages to target canvases.
 *
 * Returns a cleanup function that tears down all active routing.
 *
 * Called automatically by WidgetStoreProvider. For non-React integrations
 * (e.g. iframe isolation), call this directly after creating the store.
 */
export function createCanvasManagerRouter(store: WidgetStore): () => void {
  const activeRoutes = new Map<string, () => void>();
  let lastSize = -1;

  function scan() {
    const models = store.getSnapshot();

    if (models.size === lastSize) return;
    lastSize = models.size;

    // Find CanvasModel widgets and extract their _canvas_manager reference.
    // CanvasManagerModel is a headless singleton that isn't added to the store,
    // but its ID is referenced by each CanvasModel via _canvas_manager.
    models.forEach((model, _id) => {
      if (model.modelName !== "CanvasModel") return;

      const managerRef = model.state?._canvas_manager as string | undefined;
      if (!managerRef) return;

      // Extract manager ID from "IPY_MODEL_xxx" reference
      const managerId = managerRef.startsWith("IPY_MODEL_") ? managerRef.slice(10) : managerRef;

      if (activeRoutes.has(managerId)) return;

      activeRoutes.set(managerId, setupManagerRouting(store, managerId));
    });

    // Clean up routes for managers no longer referenced by any canvas.
    const referencedManagers = new Set<string>();
    models.forEach((model) => {
      if (model.modelName === "CanvasModel") {
        const managerRef = model.state?._canvas_manager as string | undefined;
        if (managerRef) {
          const managerId = managerRef.startsWith("IPY_MODEL_") ? managerRef.slice(10) : managerRef;
          referencedManagers.add(managerId);
        }
      }
    });

    for (const [managerId, cleanup] of activeRoutes) {
      if (!referencedManagers.has(managerId)) {
        cleanup();
        activeRoutes.delete(managerId);
      }
    }
  }

  const unsubscribe = store.subscribe(scan);
  scan();

  return () => {
    unsubscribe();
    activeRoutes.forEach((cleanup) => cleanup());
    activeRoutes.clear();
  };
}
