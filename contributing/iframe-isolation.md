# Iframe Isolation for Untrusted Outputs

This document explains the security architecture for isolating untrusted notebook outputs (HTML, widgets, markdown, SVG) from Tauri APIs.

## Why Isolation Matters

When users open notebooks from untrusted sources, malicious JavaScript in cell outputs could:
- Access `window.__TAURI__` to invoke native commands
- Read/write files via Tauri's filesystem APIs
- Execute arbitrary shell commands
- Exfiltrate data from other cells or the kernel

Our isolation strategy prevents all of these attacks.

## Security Model

### Blob URLs Create Opaque Origins

We render untrusted content inside iframes with `blob:` URLs:

```tsx
const html = generateFrameHtml({ darkMode });
const blob = new Blob([html], { type: "text/html" });
const url = URL.createObjectURL(blob);

<iframe src={url} sandbox="..." />
```

Blob URLs have a unique **opaque origin** (displayed as `"null"`). Because the origin differs from the parent window, Tauri's IPC bridge is **not injected** into the iframe.

### Sandbox Restrictions

The iframe uses restricted sandbox attributes:

```tsx
// src/components/isolated/isolated-frame.tsx
const SANDBOX_ATTRS = [
  "allow-scripts",          // Required for widgets
  "allow-downloads",        // Allow file downloads
  "allow-forms",            // Allow form submissions
  "allow-pointer-lock",     // Allow pointer lock API
  "allow-popups",           // Allow window.open (for links)
  "allow-popups-to-escape-sandbox",
  "allow-modals",           // Allow alert/confirm dialogs
].join(" ");
```

### Critical: No `allow-same-origin`

**NEVER add `allow-same-origin` to the sandbox.**

If `allow-same-origin` were present, the iframe would share the parent's origin and gain access to:
- `window.__TAURI__` and all Tauri APIs
- Parent's localStorage and sessionStorage
- Parent's cookies
- Parent DOM via `window.parent.document`

This is the single most important security invariant. It's tested in CI:

```typescript
// src/components/isolated/__tests__/isolated-frame.test.ts
it("sandbox does NOT include allow-same-origin", () => {
  expect(EXPECTED_SANDBOX_ATTRS).not.toContain("allow-same-origin");
});
```

### Source Validation

The iframe's message handler validates that messages come from the parent window:

```javascript
// src/components/isolated/frame-html.ts
window.addEventListener('message', function(event) {
  if (event.source !== window.parent) {
    return;  // Reject messages from other windows
  }
  // ... handle message
});
```

This prevents other windows/iframes from injecting messages.

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                        PARENT WINDOW                             │
│                                                                  │
│  Kernel ←→ WidgetStore ←→ CommBridgeManager ←→ IsolatedFrame    │
│                                   │                              │
│                              postMessage                         │
└───────────────────────────────────┼──────────────────────────────┘
                                    │
                                    ▼
┌─────────────────────────────────────────────────────────────────┐
│                     ISOLATED IFRAME (blob:)                      │
│                                                                  │
│  WidgetBridgeClient ←→ WidgetStore ←→ WidgetView/AnyWidget      │
│                                                                  │
│  ❌ window.__TAURI__ = undefined                                 │
│  ❌ window.parent.document → cross-origin error                  │
│  ❌ localStorage → cross-origin error                            │
└─────────────────────────────────────────────────────────────────┘
```

### Key Components

| Component | Location | Purpose |
|-----------|----------|---------|
| `IsolatedFrame` | `src/components/isolated/isolated-frame.tsx` | React component that manages blob URL lifecycle |
| `CommBridgeManager` | `src/components/isolated/comm-bridge-manager.ts` | Parent-side: syncs widget state to iframe |
| `jsonrpc-transport.ts` | `src/components/isolated/jsonrpc-transport.ts` | JSON-RPC 2.0 transport over `postMessage` |
| `rpc-methods.ts` | `src/components/isolated/rpc-methods.ts` | Shared widget bridge method constants |
| `WidgetBridgeClient` | `src/isolated-renderer/widget-bridge-client.ts` | Iframe-side JSON-RPC widget bridge (via `createWidgetBridgeClient`) |
| `frame-html.ts` | `src/components/isolated/frame-html.ts` | Generates bootstrap HTML for iframe |
| `frame-bridge.ts` | `src/components/isolated/frame-bridge.ts` | Legacy frame message definitions |

### Renderer Bundle

The isolated renderer code is built inline during the notebook app build via the Vite plugin (`apps/notebook/vite-plugin-isolated-renderer.ts`). The bundle is embedded as a virtual module and provided to `IsolatedFrame` via the `IsolatedRendererProvider` context (accessed internally through the `useIsolatedRenderer()` hook)—no separate build step or HTTP fetch required.

### Renderer Plugins

Heavy output renderers (markdown, plotly, vega, leaflet) are **not** bundled into the core IIFE. Instead, they are built as on-demand **renderer plugins** — CJS modules loaded via `frame.installRenderer()` only when their MIME types appear in cell outputs.

#### Architecture

```
┌────────────────────────────────┐     ┌──────────────────────────┐
│  Parent (iframe-libraries.ts)  │     │  Iframe (core renderer)  │
│                                │     │                          │
│  1. Scan outputs for MIME types│     │  RendererRegistry (Map)  │
│  2. loadPlugin("vega")         │     │  + pattern matchers      │
│     → import("virtual:        │     │                          │
│        renderer-plugin/vega")  │     │  installRendererPlugin() │
│  3. frame.installRenderer(     │────▶│    new Function(code)    │
│       code, css)               │     │    mod.exports.install({ │
│                                │     │      register,           │
│                                │     │      registerPattern     │
│                                │     │    })                    │
└────────────────────────────────┘     └──────────────────────────┘
```

**How it works:**

1. `OutputArea` scans cell outputs for MIME types via `getRequiredLibraries()`
2. Matching plugins are lazy-loaded from their own virtual module (`virtual:renderer-plugin/{name}`)
3. The parent sends an `nteract/installRenderer` message with the plugin code + CSS
4. The iframe loads the CJS module via `new Function("module", "exports", "require", code)` with a custom `require` shim that provides the shared React instance
5. The plugin's `install(ctx)` function registers components for its MIME types
6. When `OutputRenderer` encounters a MIME type, it checks the registry before the built-in switch

**React sharing:** Plugins declare `react` and `react/jsx-runtime` as external. The iframe's custom `require` shim maps them to the React instance already loaded in the core bundle — no globals, just dependency injection.

**Code splitting:** Each plugin has its own Vite virtual module, so they load independently. A notebook with only markdown outputs never fetches the plotly chunk (~4.8MB).

#### Current Plugins

| Plugin | MIME Type(s) | Virtual Module | Size |
|--------|-------------|----------------|------|
| Markdown | `text/markdown` | `virtual:renderer-plugin/markdown` | ~2.2MB |
| Vega | `application/vnd.vega*.v*` | `virtual:renderer-plugin/vega` | ~865KB |
| Plotly | `application/vnd.plotly.v1+json` | `virtual:renderer-plugin/plotly` | ~4.8MB |
| Leaflet | `application/geo+json` | `virtual:renderer-plugin/leaflet` | ~194KB |

#### Adding a New Renderer Plugin

1. **Create the plugin entry** at `src/isolated-renderer/{name}-renderer.tsx`:

```typescript
import { SomeLibrary } from "some-library";
import { useEffect, useRef } from "react";

interface RendererProps {
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}

function MyRenderer({ data }: RendererProps) {
  // Render using the library...
}

export function install(ctx: {
  register: (mimeTypes: string[], component: React.ComponentType<RendererProps>) => void;
  registerPattern: (test: (mime: string) => boolean, component: React.ComponentType<RendererProps>) => void;
}) {
  // Use register() for exact MIME types
  ctx.register(["application/x-my-type"], MyRenderer);
  // Or registerPattern() for versioned/regex MIME types
  ctx.registerPattern((mime) => /^application\/vnd\.mylib\.v\d/.test(mime), MyRenderer);
}
```

2. **Add the build step** in `apps/notebook/vite-plugin-isolated-renderer.ts`:
   - Add the entry path
   - Add variables for code/css
   - Add to `invalidateCache()`
   - Add to the `Promise.all([...buildRendererPlugin()])` call
   - Add a virtual module case in `load()`

3. **Add the virtual module type** in `apps/notebook/src/vite-env.d.ts`:

```typescript
declare module "virtual:renderer-plugin/my-renderer" {
  export const code: string;
  export const css: string;
}
```

4. **Register the MIME type** in `src/components/isolated/iframe-libraries.ts`:
   - Add to `MIME_PLUGINS` map (or handle in `pluginForMime()` for regex patterns)
   - Add the import case in `loadPlugin()`

5. **Remove from core bundle** in `src/isolated-renderer/index.tsx`:
   - Remove the component import
   - Remove the MIME type case from `OutputRenderer`

#### Key Files

| File | Role |
|------|------|
| `src/isolated-renderer/index.tsx` | Core renderer with plugin registry and CJS loader |
| `src/isolated-renderer/*-renderer.tsx` | Plugin entry points |
| `apps/notebook/vite-plugin-isolated-renderer.ts` | Builds core IIFE + all plugins |
| `src/components/isolated/iframe-libraries.ts` | MIME detection and on-demand plugin loading |
| `src/components/isolated/frame-bridge.ts` | `InstallRendererMessage` type |
| `src/components/isolated/rpc-methods.ts` | `NTERACT_INSTALL_RENDERER` constant |
| `src/components/isolated/isolated-frame.tsx` | `installRenderer()` on `IsolatedFrameHandle` |
| `apps/notebook/src/vite-env.d.ts` | Virtual module type declarations |

## Message Protocol

Two message layers coexist:

1. **Frame bootstrap/render messages** in `frame-bridge.ts` cover output rendering,
   theming, resize notifications, link clicks, and in-iframe search flow.
2. **Widget sync traffic** uses JSON-RPC 2.0 over `postMessage`, implemented by
   `jsonrpc-transport.ts` and `rpc-methods.ts`.

The JSON-RPC widget methods include:
- Parent → iframe: `nteract/bridgeReady`, `nteract/commOpen`, `nteract/commMsg`, `nteract/commClose`, `nteract/widgetSnapshot`
- Iframe → parent: `nteract/widgetReady`, `nteract/widgetCommMsg`, `nteract/widgetCommClose`

### Widget Sync Flow

```
1. IsolatedFrame mounts
2. Iframe sends: ready
3. Parent sends: `eval` (React bundle)
4. Iframe sends: `renderer_ready`
5. CommBridgeManager sends: `nteract/bridgeReady`
6. Iframe sends: `nteract/widgetReady`
7. CommBridgeManager sends: `nteract/widgetSnapshot` (all existing models)
8. Iframe renders widgets
9. Bidirectional widget updates flow through JSON-RPC notifications
```

## Critical Code Paths

These are security-sensitive and should be reviewed carefully:

### 1. Sandbox Configuration
**File:** `src/components/isolated/isolated-frame.tsx` — `SANDBOX_ATTRS`

The `SANDBOX_ATTRS` constant defines what the iframe can do. Changes here can compromise security.

### 2. Source Validation
**File:** `src/components/isolated/frame-html.ts` — `event.source` check

The `event.source !== window.parent` check prevents message spoofing. This must remain intact.

### 3. Custom Message Forwarding
**File:** `src/components/isolated/comm-bridge-manager.ts`

The `subscribeToModelCustomMessages` method was added to support anywidgets like quak that use custom messages. Without it, widgets would appear to load but not receive kernel data.

### 4. Type Guard Whitelist
**File:** `src/components/isolated/frame-bridge.ts` — `isIframeMessage`

The `isIframeMessage` function whitelists the legacy frame-bridge message types.
JSON-RPC widget methods are defined separately in `rpc-methods.ts`.

## Code Review Checklist

When reviewing changes to iframe isolation code:

- [ ] **No `allow-same-origin`** added to sandbox attributes
- [ ] **Source validation intact** (`event.source !== window.parent`)
- [ ] **Message whitelist updated** if new types added (frame-bridge.ts)
- [ ] **Tests updated** for any new message types
- [ ] **Unit tests pass** (`pnpm test:run`)

## Testing

### Unit Tests

Security-critical invariants are tested in CI:

```bash
pnpm test:run
```

Tests verify:
- Sandbox does NOT include `allow-same-origin`
- Message type guards validate correctly
- HTML includes source validation

### Manual Testing

Use the test notebook to verify isolation:

Open any notebook (e.g. `crates/notebook/fixtures/audit-test/1-vanilla.ipynb`), add a code cell with JavaScript output, and verify:
- `window.__TAURI__` is undefined in the iframe
- Parent DOM access throws a cross-origin error
- `localStorage` access throws a cross-origin error

### Dev Tools Toggle

Press `Cmd+Shift+I` in debug builds to open the isolation test panel.

## Troubleshooting

### Widget Not Rendering

1. Check console for errors in iframe (may need to inspect iframe in DevTools)
2. Verify `nteract/widgetSnapshot` was sent and the iframe answered with `nteract/widgetReady`
3. Check `jsonrpc-transport.ts` / `rpc-methods.ts` if the bridge handshake changed

### Widget Not Receiving Updates

1. Check if custom messages are being forwarded
2. Look for `subscribeToModelCustomMessages` being called
3. Verify kernel comm traffic is being translated into `nteract/commMsg` notifications with the correct `commId`

### Theme Not Syncing

1. Check `theme` message is being sent on mode change
2. Verify `color-scheme` CSS property is set on root element
3. Some widgets use `@media (prefers-color-scheme)` which requires this

## Future Work

- **E2E Security Tests**: Automated browser tests verifying `window.__TAURI__` is undefined (blocked by Tauri WebDriver macOS support)
- **Widget Compatibility Matrix**: Systematic testing of popular widgets

## References

- [HTML5 sandbox attribute](https://developer.mozilla.org/en-US/docs/Web/HTML/Element/iframe#sandbox)
- [Blob URLs and origins](https://developer.mozilla.org/en-US/docs/Web/API/URL/createObjectURL)
- [postMessage security](https://developer.mozilla.org/en-US/docs/Web/API/Window/postMessage#security_concerns)
