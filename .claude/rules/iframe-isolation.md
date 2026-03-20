---
paths:
  - src/components/isolated/**
  - src/isolated-renderer/**
---

# Iframe Isolation for Untrusted Outputs

## SECURITY CRITICAL: Never Add `allow-same-origin`

**NEVER add `allow-same-origin` to the iframe sandbox attributes.** This is the single most important security invariant. If present, the iframe would share the parent's origin and gain access to `window.__TAURI__`, all Tauri APIs, parent DOM, localStorage, and cookies. This is tested in CI.

## Why Isolation Matters

Malicious JavaScript in cell outputs from untrusted notebooks could:
- Access `window.__TAURI__` to invoke native commands
- Read/write files via Tauri's filesystem APIs
- Execute arbitrary shell commands
- Exfiltrate data from other cells or the kernel

## Security Model

### Blob URLs Create Opaque Origins

Untrusted content renders inside iframes with `blob:` URLs. Blob URLs have a unique opaque origin (displayed as `"null"`). Because the origin differs from the parent window, Tauri's IPC bridge is not injected into the iframe.

### Sandbox Attributes

```
allow-scripts, allow-downloads, allow-forms, allow-pointer-lock,
allow-popups, allow-popups-to-escape-sandbox, allow-modals
```

No `allow-same-origin`. Ever.

### Source Validation

The iframe's message handler validates `event.source !== window.parent` to reject messages from other windows/iframes.

## Key Components

| Component | Location | Purpose |
|-----------|----------|---------|
| `IsolatedFrame` | `src/components/isolated/isolated-frame.tsx` | Manages blob URL lifecycle |
| `CommBridgeManager` | `src/components/isolated/comm-bridge-manager.ts` | Parent-side: syncs widget state to iframe |
| `WidgetBridgeClient` | `src/isolated-renderer/widget-bridge-client.ts` | Iframe-side: receives comm messages |
| `frame-html.ts` | `src/components/isolated/frame-html.ts` | Generates bootstrap HTML for iframe |
| `frame-bridge.ts` | `src/components/isolated/frame-bridge.ts` | Message type definitions and guards |

The isolated renderer bundle is built inline via Vite plugin (`apps/notebook/vite-plugin-isolated-renderer.ts`) and provided via `IsolatedRendererProvider` context.

## Message Protocol

### Parent -> Iframe

| Message | Purpose |
|---------|---------|
| `eval` | Bootstrap: inject React renderer bundle |
| `render` | Render output content (HTML, markdown, etc.) |
| `theme` | Sync dark/light mode |
| `clear` | Clear all outputs |
| `comm_open` | Forward widget creation from kernel |
| `comm_msg` | Forward state update or custom message |
| `comm_close` | Forward widget destruction |
| `comm_sync` | Bulk sync all existing models on ready |
| `bridge_ready` | Signal parent bridge is initialized |
| `ping` | Liveness check |
| `search` | Trigger in-iframe text search |

### Iframe -> Parent

| Message | Purpose |
|---------|---------|
| `ready` | Bootstrap HTML loaded |
| `renderer_ready` | React bundle initialized |
| `widget_ready` | Widget system ready for comm_sync |
| `resize` | Content height changed |
| `error` | JavaScript error occurred |
| `link_click` | User clicked a link |
| `widget_comm_msg` | Widget state update (forward to kernel) |
| `widget_comm_close` | Widget close request |
| `pong` | Response to ping |
| `render_complete` | Content finished rendering |
| `dblclick` | Double-click event (for cell editing) |
| `search_results` | Search match count/position info |

### Widget Sync Flow

1. IsolatedFrame mounts
2. Iframe sends `ready`
3. Parent sends `eval` (React bundle)
4. Iframe sends `renderer_ready`
5. CommBridgeManager sends `bridge_ready`
6. Iframe sends `widget_ready`
7. CommBridgeManager sends `comm_sync` (all existing models)
8. Iframe renders widgets
9. Bidirectional updates via `comm_msg` / `widget_comm_msg`

## Critical Code Paths for Review

1. **Sandbox configuration** -- `isolated-frame.tsx`, `SANDBOX_ATTRS`. Changes here can compromise security.
2. **Source validation** -- `frame-html.ts`, `event.source !== window.parent` check. Must remain intact.
3. **Custom message forwarding** -- `comm-bridge-manager.ts`, `subscribeToModelCustomMessages`. Required for anywidgets like quak.
4. **Type guard whitelist** -- `frame-bridge.ts`, `isIframeMessage`. New message types must be added here.

## Code Review Checklist

- No `allow-same-origin` added to sandbox attributes
- Source validation intact (`event.source !== window.parent`)
- Message whitelist updated if new types added (`frame-bridge.ts`)
- Tests updated for new message types
- Unit tests pass (`pnpm test:run`)

## Testing

**Unit tests** verify: sandbox does NOT include `allow-same-origin`, message type guards validate correctly, HTML includes source validation.

```bash
pnpm test:run
```

**Manual testing:** Open a notebook, add a code cell with JavaScript output, verify `window.__TAURI__` is undefined in the iframe, parent DOM access throws cross-origin error, `localStorage` throws cross-origin error.
