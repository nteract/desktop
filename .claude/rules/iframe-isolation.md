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
allow-scripts, allow-downloads, allow-forms, allow-pointer-lock, allow-fullscreen
```

No `allow-same-origin`. Ever. No `allow-popups` or `allow-modals` — these were removed to reduce phishing surface.

### Content Security Policy

The iframe HTML includes a `<meta>` CSP as defense-in-depth. `script-src` and `style-src` allow `http://127.0.0.1:*` for anywidget ESM/CSS served from the daemon's blob store. This is safe because the sandbox lacks `allow-same-origin` — loaded scripts still can't access the parent DOM, Tauri APIs, or storage. See `contributing/iframe-isolation.md` § Content Security Policy for the full policy.

**Key file:** `src/components/isolated/frame-html.ts` — `generateFrameHtml()`.

### Source Validation

The iframe's message handler validates `event.source !== window.parent` to reject messages from other windows/iframes.

## Key Components

| Component | Location | Purpose |
|-----------|----------|---------|
| `IsolatedFrame` | `src/components/isolated/isolated-frame.tsx` | Manages blob URL lifecycle |
| `CommBridgeManager` | `src/components/isolated/comm-bridge-manager.ts` | Parent-side: syncs widget state to iframe |
| `jsonrpc-transport.ts` | `src/components/isolated/jsonrpc-transport.ts` | JSON-RPC 2.0 transport over `postMessage` |
| `rpc-methods.ts` | `src/components/isolated/rpc-methods.ts` | Shared widget bridge method constants |
| `WidgetBridgeClient` | `src/isolated-renderer/widget-bridge-client.ts` | Iframe-side JSON-RPC widget bridge |
| `frame-html.ts` | `src/components/isolated/frame-html.ts` | Generates bootstrap HTML for iframe |
| `frame-bridge.ts` | `src/components/isolated/frame-bridge.ts` | Legacy frame message definitions and guards |

The isolated renderer bundle is built inline via Vite plugin (`apps/notebook/vite-plugin-isolated-renderer.ts`) and provided via `IsolatedRendererProvider` context.

## Message Protocol

Two layers coexist:

1. **Frame bootstrap/render messages** in `frame-bridge.ts` handle events like
   `eval`, `render`, `theme`, `clear`, `resize`, `link_click`, and search flow.
2. **Widget sync traffic** uses JSON-RPC 2.0 over `postMessage`, implemented by
   `jsonrpc-transport.ts` and `rpc-methods.ts`.

The JSON-RPC widget methods include:
- Parent -> iframe: `nteract/bridgeReady`, `nteract/commOpen`, `nteract/commMsg`, `nteract/commClose`, `nteract/widgetSnapshot`
- Iframe -> parent: `nteract/widgetReady`, `nteract/widgetCommMsg`, `nteract/widgetCommClose`

### Widget Sync Flow

1. IsolatedFrame mounts
2. Iframe sends `ready`
3. Parent sends `eval` (React bundle)
4. Iframe sends `renderer_ready`
5. CommBridgeManager sends `nteract/bridgeReady`
6. Iframe sends `nteract/widgetReady`
7. CommBridgeManager sends `nteract/widgetSnapshot` (all existing models)
8. Iframe renders widgets
9. Bidirectional widget updates flow through JSON-RPC notifications

## Critical Code Paths for Review

1. **Sandbox configuration** -- `isolated-frame.tsx`, `SANDBOX_ATTRS`. Changes here can compromise security.
2. **Source validation** -- `frame-html.ts`, `event.source !== window.parent` check. Must remain intact.
3. **Custom message forwarding** -- `comm-bridge-manager.ts`, `subscribeToModelCustomMessages`. Required for anywidgets like quak.
4. **Type guard whitelist** -- `frame-bridge.ts`, `isIframeMessage`. New message types must be added here.

## Code Review Checklist

- No `allow-same-origin` added to sandbox attributes
- CSP not weakened — no new origins in `script-src`/`style-src` beyond `127.0.0.1:*` and `https:`
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
