# Widget Development Guide

This guide covers widget system internals for developers. For user-facing widget support info, see [docs/widgets.md](../docs/widgets.md).

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│ Daemon (kernel + blob store + RuntimeStateDoc)                          │
│   comm_open / comm_msg / binary buffers → blob-store + CRDT ContentRefs │
└──────────────────────────────────┬──────────────────────────────────────┘
                                   │ Automerge sync
┌──────────────────────────────────▼──────────────────────────────────────┐
│ Parent Window                                                           │
│                                                                         │
│  ┌──────────────────┐ commChanges$ ┌───────────────────────────────┐   │
│  │ SyncEngine       │─────────────►│ WidgetStore                   │   │
│  │ (resolve_comm +  │              │ (state + bufferPaths)         │   │
│  │  text-blob fetch)│              └───────────┬───────────────────┘   │
│  └──────────────────┘                          │                       │
│                                    ┌───────────┴──────────────────┐    │
│                                    │ CommBridgeManager            │    │
│                                    │ (forwards state+bufferPaths) │    │
│                                    └───────────┬──────────────────┘    │
│                                                │ postMessage / JSON-RPC│
└────────────────────────────────────────────────┼───────────────────────┘
                                                 │
┌────────────────────────────────────────────────┼───────────────────────┐
│ Isolated Iframe (sandboxed, no Tauri access)   ▼                       │
│                                                                         │
│  ┌─────────────────────┐    fetch blob URLs → install DataView         │
│  │ WidgetBridgeClient  │──► at each bufferPath, then store.createModel │
│  │ (resolveBlobUrls)   │                                                │
│  └─────────────────────┘                                                │
│            │                                                            │
│  ┌─────────▼───────────┐     ┌──────────────────┐                      │
│  │ Iframe WidgetStore  │────►│ Widget Components │                      │
│  │                     │     │ (React renders)   │                      │
│  └─────────────────────┘     └──────────────────┘                      │
└─────────────────────────────────────────────────────────────────────────┘
```

Widgets run inside a security-isolated iframe. State is **not** shipped as
Jupyter comm frames; it flows through the RuntimeStateDoc CRDT. SyncEngine
diffs the CRDT, runs the WASM resolver, emits typed `ResolvedComm` events,
and the CommBridgeManager forwards them (plus `bufferPaths`) to the iframe.
The iframe resolves blob URLs into `DataView`s at those paths before widget
code observes state. Cross-iframe traffic uses JSON-RPC 2.0 over
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
  createModel(commId: string, state: Record<string, unknown>, bufferPaths?: string[][]): void;
  updateModel(commId: string, statePatch: Record<string, unknown>, bufferPaths?: string[][]): void;
  deleteModel(commId: string): void;
  wasModelClosed(commId: string): boolean;

  // Fine-grained subscriptions
  subscribeToKey(modelId: string, key: string, callback: (value: unknown) => void): () => void;

  // Custom messages (e.g., anywidget model.send())
  emitCustomMessage(commId: string, content: Record<string, unknown>, buffers?: ArrayBuffer[]): void;
  subscribeToCustomMessage(commId: string, callback: CustomMessageCallback): () => void;
}
```

`bufferPaths` is the manifest of JSON paths in `state` whose values are
blob URL strings. The iframe fetches each URL and swaps in a `DataView`
before the anywidget model observes state. Parent-window code sees URL
strings; iframe-local code sees `DataView`s at those paths.

Custom `buffers` on `emitCustomMessage` / `subscribeToCustomMessage` are a
separate channel — transient event payloads (ipycanvas draw commands, quak
row batches) that don't belong in CRDT state.

Usage in components via the `useWidgetModelValue` hook from `widget-store-context.tsx`:

```tsx
import { useWidgetModelValue } from "../widget-store-context";

// Inside a widget component
const value = useWidgetModelValue(modelId, "value");
```

Under the hood this calls `useSyncExternalStore` with `subscribeToKey` and `getModel`.

## Comm Bridge Protocol

Inbound widget state flows from the CRDT, not from Jupyter comm frames.
SyncEngine emits `commChanges$` events; the CommBridgeManager forwards them
to the iframe via JSON-RPC notifications with the shapes below.

| Message | When | Iframe payload |
|---------|------|----------------|
| `comm_open` | Widget created (CRDT opened) | `{ commId, targetName, state, bufferPaths? }` |
| `comm_msg` method `update` | State delta (CRDT changed) | `{ commId, method: "update", data, bufferPaths? }` |
| `comm_msg` method `custom` | Ephemeral event | `{ commId, method: "custom", data, buffers? }` |
| `comm_close` | Widget destroyed | `{ commId }` |
| `widget_snapshot` | Iframe reconnect | `{ models: [{ commId, targetName, state, bufferPaths? }] }` |

- `bufferPaths` only applies to `open` / `update` — it tells the iframe which
  paths are blob URLs the resolver must fetch and install as `DataView`s
  before the anywidget model sees state.
- `buffers` only applies to `custom` — transient binary payloads alongside
  the event content. The two don't share a channel.

See `CommOpenMessage`, `CommMsgMessage`, and `WidgetSnapshotMessage` in
`src/components/isolated/frame-bridge.ts` for the exact payload types.

The CommBridgeManager:
1. Subscribes to WidgetStore changes.
2. Forwards model updates (state + `bufferPaths`) to the iframe via
   JSON-RPC notifications.
3. Receives iframe-originated `widget_comm_msg` / `widget_comm_close`
   messages and routes them to the kernel (via `sendUpdate` /
   `sendCustom` / `closeComm`) and the parent store.

### Inbound path is CRDT-driven, not Jupyter-message-synthesized

`useCommRouter` is **outbound-only**. Older versions synthesized
`JupyterCommMessage` inbound envelopes and ran `applyBufferPaths` to stitch
buffers back into state; that path was removed once
`SyncEngine.commChanges$` became authoritative. If you're tempted to re-add
an inbound `handleMessage`, reach for SyncEngine instead.

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

1. Detects `_esm` field in model state.
2. Dynamically imports the ESM module.
3. Calls the module's `render` function with an AFM-compatible model proxy.

### Binary traitlets (`traitlets.Bytes(sync=True)`)

When a widget defines a binary traitlet, `ipywidgets`' `_remove_buffers()`
extracts the raw bytes out of state before sending the comm message. The
daemon's kernel handler blob-stores each buffer with
`media_type: application/octet-stream` and replaces the state placeholder
with a ContentRef. The WASM resolver rewrites that ContentRef to a blob
URL and lists the path in `buffer_paths`. The iframe fetches the URL and
installs a `DataView` at that path, which is exactly what anywidget
consumers expect (`model.get("data").byteLength`, etc).

The anywidget-reserved keys `_esm` and `_css` are intentionally **not**
listed in `buffer_paths`. They stay as URL strings so `loadESM` can
`import(url)` and `injectCSS` can render a `<link rel="stylesheet">`.
Listing them would cause the iframe's resolver to swap in a `DataView` and
break both loaders.

ipywidgets `Image` / `Audio` / `Video` `value` traitlets also flow through
the binary path. `buildMediaSrc` in `buffer-utils.ts` accepts `DataView` in
addition to `ArrayBuffer` / `Uint8Array` / URL string, so the resolved
`DataView` turns into a valid `data:` URL for the `<img>` / `<audio>` /
`<video>` `src`.

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
