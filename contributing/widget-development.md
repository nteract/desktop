# Widget Development Guide

This guide covers widget system internals for developers. For user-facing widget support info, see [docs/widgets.md](../docs/widgets.md).

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│ Parent Window                                                           │
│                                                                         │
│  ┌──────────────┐    comm_open/msg/close    ┌─────────────────────────┐│
│  │ Kernel       │◄────────────────────────►│ WidgetStore             ││
│  │ (via daemon) │                           │ (state management)      ││
│  └──────────────┘                           └───────────┬─────────────┘│
│                                                         │              │
│                                            ┌────────────┴────────────┐ │
│                                            │ CommBridgeManager       │ │
│                                            │ (routes messages)       │ │
│                                            └────────────┬────────────┘ │
│                                                         │ postMessage  │
└─────────────────────────────────────────────────────────┼──────────────┘
                                                          │
┌─────────────────────────────────────────────────────────┼──────────────┐
│ Isolated Iframe (sandboxed, no Tauri access)            ▼              │
│                                                                         │
│  ┌─────────────────┐         ┌──────────────────┐                      │
│  │ WidgetBridge    │◄───────►│ Widget Components │                      │
│  │ (JSON-RPC 2.0)  │         │ (React renders)   │                      │
│  └─────────────────┘         └──────────────────┘                      │
└─────────────────────────────────────────────────────────────────────────┘
```

Widgets run inside a security-isolated iframe. The parent window owns the
WidgetStore and proxies Jupyter comm messages through the CommBridgeManager.
Across the iframe boundary, widget traffic now uses JSON-RPC 2.0 over
`postMessage` via `jsonrpc-transport.ts` and `rpc-methods.ts`.

### Reserved comm namespace: `nteract.dx.*`

The `nteract.dx.*` target-name prefix is reserved for nteract's own
kernel-side protocols (dx uses `nteract.dx.blob`; future subsystems may use
`nteract.dx.query`, `nteract.dx.stream`). Comms in this namespace are
**filtered out of `RuntimeStateDoc::comms` by the runtime agent** — they are
not widget state, do not sync to the frontend, and do not reach the
WidgetStore. Do not register widget targets under this prefix. See
`docs/superpowers/specs/2026-04-13-nteract-dx-design.md` for the protocol.

## Key Files

| File | Role |
|------|------|
| `src/components/widgets/widget-store.ts` | Model state management (useSyncExternalStore pattern) |
| `src/components/widgets/widget-registry.ts` | Maps model names to React components |
| `src/components/widgets/controls/` | 54 built-in ipywidgets implementations |
| `src/components/widgets/controls/index.ts` | Registration of all built-in widgets |
| `src/components/widgets/anywidget-view.tsx` | AFM loader for anywidget ESM modules |
| `src/components/widgets/widget-view.tsx` | Renders widgets by looking up registry |
| `src/components/isolated/comm-bridge-manager.ts` | Routes comm messages between store and iframe |
| `src/components/isolated/frame-bridge.ts` | Legacy message types and guards for frame bootstrap/render events |
| `src/components/isolated/jsonrpc-transport.ts` | JSON-RPC 2.0 transport over `postMessage` |
| `src/components/isolated/rpc-methods.ts` | Shared widget bridge method names |
| `src/isolated-renderer/widget-bridge-client.ts` | Iframe-side JSON-RPC widget bridge |

## WidgetStore API

The store manages widget models using React's `useSyncExternalStore` pattern:

```typescript
interface WidgetStore {
  // For useSyncExternalStore
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

Usage in components via the `useWidgetModelValue` hook from `widget-store-context.tsx`:

```tsx
import { useWidgetModelValue } from "../widget-store-context";

// Inside a widget component
const value = useWidgetModelValue(modelId, "value");
```

Under the hood this calls `useSyncExternalStore` with `subscribeToKey` and `getModel`.

## Comm Bridge Protocol

Widget communication follows the Jupyter Comm protocol:

| Message | When | Jupyter Wire Format |
|---------|------|---------------------|
| `comm_open` | Widget created | `{ comm_id, target_name, data: { state } }` |
| `comm_msg` | State update | `{ comm_id, data: { method: "update", state } }` |
| `comm_msg` | Custom message | `{ comm_id, data: { method: "custom", content } }` |
| `comm_close` | Widget destroyed | `{ comm_id }` |

> **Note:** The table above shows the Jupyter wire protocol message shapes. The internal TypeScript types in `frame-bridge.ts` use camelCase (`commId`, `targetName`) and a flatter structure (e.g., `method` is a sibling of `data` in `CommMsgMessage`, not nested inside it). See `CommOpenMessage` and `CommMsgMessage` in `src/components/isolated/frame-bridge.ts` for the actual postMessage payload shapes.

The CommBridgeManager:
1. Subscribes to WidgetStore changes
2. Forwards model updates to the isolated iframe via JSON-RPC notifications
3. Receives iframe messages and routes them to kernel or store

## Adding a New Built-in Widget

1. **Create the component** in `src/components/widgets/controls/`:

```tsx
// src/components/widgets/controls/my-widget.tsx
import type { WidgetComponentProps } from "../widget-registry";
import { useWidgetModelValue, useWidgetStoreRequired } from "../widget-store-context";

export function MyWidget({ modelId }: WidgetComponentProps) {
  const { sendUpdate } = useWidgetStoreRequired();
  const value = useWidgetModelValue(modelId, "value");
  const description = useWidgetModelValue(modelId, "description");

  const handleChange = (newValue: number) => {
    // Send update back to kernel
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

2. **Register the widget** in `src/components/widgets/controls/index.ts`:

```typescript
import { MyWidget } from "./my-widget";
registerWidget("MyWidgetModel", MyWidget);
```

3. **Export the component** (optional, for direct use):

```typescript
export { MyWidget } from "./my-widget";
```

## Widget State Convention

Widgets receive state from ipywidgets with these common fields:

| Field | Type | Description |
|-------|------|-------------|
| `_model_name` | string | e.g., "IntSliderModel" |
| `_model_module` | string | e.g., "@jupyter-widgets/controls" |
| `value` | varies | Current widget value |
| `description` | string | Label text |
| `disabled` | boolean | Whether widget is interactive |
| `layout` | string | IPY_MODEL_ reference to LayoutModel |

## IPY_MODEL_ References

Container widgets reference children via `IPY_MODEL_<comm_id>` strings:

```typescript
import { isModelRef, parseModelRef } from "../widget-store";

// Check if value is a model reference
if (isModelRef(child)) {
  const childModelId = parseModelRef(child);
  // Render child widget by its ID
}
```

## Anywidget Support

Anywidgets use ESM modules loaded at runtime. The `anywidget-view.tsx` component:

1. Detects `_esm` field in model state
2. Dynamically imports the ESM module
3. Calls the module's `render` function with a model proxy

## Testing Widgets

**Unit tests:** `src/components/widgets/__tests__/`

```typescript
// widget-store.test.ts
describe("WidgetStore", () => {
  it("creates and updates models", () => {
    const store = createWidgetStore();
    store.createModel("test-id", { value: 42 });
    expect(store.getModel("test-id")?.state.value).toBe(42);
  });
});
```

**Manual testing:**

```python
# In a notebook
import ipywidgets as widgets
slider = widgets.IntSlider(value=50, min=0, max=100)
display(slider)
```

## Debugging

There is no `localStorage` widget-debug toggle in the current app. Use the
browser devtools console in development builds, where `logger.ts` calls
`attachConsole()`, and use daemon logs for comm-level tracing:

Watch for comm messages in the daemon logs:

```bash
runt daemon logs -f | grep -i comm
```
