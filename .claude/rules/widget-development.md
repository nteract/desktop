---
paths:
  - src/components/widgets/**
  - src/components/isolated/**
---

# Widget Development

## Architecture

Widgets run inside a security-isolated iframe. The parent window owns the WidgetStore and proxies Jupyter comm messages through the CommBridgeManager via postMessage. The iframe has no access to `window.__TAURI__` or parent DOM.

**Parent window:** Kernel (via daemon) <-> WidgetStore (state management) <-> CommBridgeManager (routes messages) <-> postMessage boundary

**Isolated iframe:** WidgetBridgeClient (receives msgs) <-> Widget Components (React renders)

### Current State Architecture

Widget state lives **outside** the Automerge doc in parallel in-memory stores:
- **Daemon:** `CommState` in `comm_state.rs` -- tracks active Jupyter comm channels, maintains output capture routing for Output widgets
- **Frontend:** `WidgetStore` in `widget-store.ts` -- per-model subscriptions, IPY_MODEL_ reference resolution, custom message buffering

New clients receive a `CommSync` broadcast (snapshot of all active widgets) on connect. Widget messages flow as `NotebookBroadcast::Comm` events, not document mutations.

**Planned:** Move widget state into `doc.comms/` in the Automerge document (#761). This eliminates `CommSync`, simplifies Output widget routing, and means new clients get widget state via normal CRDT sync. Implementation phases: #808-#811.

## Key Files

| File | Role |
|------|------|
| `src/components/widgets/widget-store.ts` | Model state management (useSyncExternalStore) |
| `src/components/widgets/widget-registry.ts` | Maps model names to React components |
| `src/components/widgets/controls/` | 54 built-in ipywidgets implementations |
| `src/components/widgets/controls/index.ts` | Registration of all built-in widgets |
| `src/components/widgets/anywidget-view.tsx` | AFM loader for anywidget ESM modules |
| `src/components/widgets/widget-view.tsx` | Renders widgets by looking up registry |
| `src/components/isolated/comm-bridge-manager.ts` | Routes comm messages between store and iframe |
| `src/components/isolated/frame-bridge.ts` | Message protocol types and guards |
| `src/isolated-renderer/widget-bridge-client.ts` | Iframe-side message handler |

## WidgetStore API

```typescript
interface WidgetStore {
  // useSyncExternalStore pattern
  subscribe(listener: () => void): () => void;
  getSnapshot(): Map<string, WidgetModel>;

  // Model operations
  getModel(modelId: string): WidgetModel | undefined;
  createModel(commId: string, state: Record<string, unknown>, buffers?: ArrayBuffer[]): void;
  updateModel(commId: string, statePatch: Record<string, unknown>, buffers?: ArrayBuffer[]): void;
  deleteModel(commId: string): void;
  wasModelClosed(commId: string): boolean;

  // Fine-grained subscriptions
  subscribeToKey(modelId: string, key: string, callback: (value: unknown) => void): () => void;

  // Custom messages (e.g., anywidget)
  emitCustomMessage(commId: string, content: Record<string, unknown>, buffers?: ArrayBuffer[]): void;
  subscribeToCustomMessage(commId: string, callback: CustomMessageCallback): () => void;
}
```

Use `useWidgetModelValue(modelId, "key")` from `widget-store-context.tsx` in components. Under the hood this calls `useSyncExternalStore` with `subscribeToKey` and `getModel`.

## Comm Bridge Protocol

| Message | When | Jupyter Wire Format |
|---------|------|---------------------|
| `comm_open` | Widget created | `{ comm_id, target_name, data: { state } }` |
| `comm_msg` | State update | `{ comm_id, data: { method: "update", state } }` |
| `comm_msg` | Custom message | `{ comm_id, data: { method: "custom", content } }` |
| `comm_close` | Widget destroyed | `{ comm_id }` |

Internal TypeScript types in `frame-bridge.ts` use camelCase (`commId`, `targetName`) and a flatter structure than the wire format. See `CommOpenMessage` and `CommMsgMessage` for actual postMessage payload shapes.

## Adding a New Built-in Widget

1. Create component in `src/components/widgets/controls/`:

```tsx
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function MyWidget({ modelId }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();
  const value = useWidgetModelValue(modelId, "value");
  const description = useWidgetModelValue(modelId, "description");

  const handleChange = (newValue: number) => {
    sendUpdate(modelId, { value: newValue });
  };

  return (
    <div>
      <label>{description}</label>
      <input value={value} onChange={(e) => handleChange(Number(e.target.value))} />
    </div>
  );
}
```

2. Register in `src/components/widgets/controls/index.ts`:

```typescript
import { MyWidget } from "./my-widget";
registerWidget("MyWidgetModel", MyWidget);
```

## Widget State Convention

Common ipywidgets fields:

| Field | Type | Description |
|-------|------|-------------|
| `_model_name` | string | e.g., "IntSliderModel" |
| `_model_module` | string | e.g., "@jupyter-widgets/controls" |
| `value` | varies | Current widget value |
| `description` | string | Label text |
| `disabled` | boolean | Whether widget is interactive |
| `layout` | string | IPY_MODEL_ reference to LayoutModel |

## IPY_MODEL_ References

Container widgets reference children via `IPY_MODEL_<comm_id>` strings. Use `isModelRef(child)` and `parseModelRef(child)` from `widget-store.ts` to resolve.

## Anywidget Support

Anywidgets use ESM modules loaded at runtime. `anywidget-view.tsx` detects `_esm` field in model state, dynamically imports the ESM module, and calls its `render` function with a model proxy.

## Testing and Debugging

**Unit tests:** `src/components/widgets/__tests__/`

**Enable debug logging:**
```javascript
// Browser console
localStorage.setItem("runt:debug", "true");
// Reload the page
```

**Daemon comm logs:**
```bash
runt daemon logs -f | grep -i comm
```

**Troubleshooting:**
- Widget not rendering: Check iframe console, verify `comm_sync` was sent (look for `[CommBridge]` logs), check if type is in `ISOLATED_MIME_TYPES`
- Widget not receiving updates: Check custom message forwarding, verify `subscribeToModelCustomMessages` is called
