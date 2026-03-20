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
// src/components/isolated/isolated-frame.tsx:137
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
// src/components/isolated/frame-html.ts:161
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
│  CommBridgeClient ←→ IframeWidgetStore ←→ WidgetView/AnyWidget  │
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
| `CommBridgeClient` | `src/isolated-renderer/widget-bridge-client.ts` | Iframe-side: receives comm messages |
| `frame-html.ts` | `src/components/isolated/frame-html.ts` | Generates bootstrap HTML for iframe |
| `frame-bridge.ts` | `src/components/isolated/frame-bridge.ts` | Message type definitions |

### Renderer Bundle

The isolated renderer code is built inline during the notebook app build via the Vite plugin (`apps/notebook/vite-plugin-isolated-renderer.ts`). The bundle is embedded as a virtual module and passed to `IsolatedFrame` via `rendererCode` and `rendererCss` props—no separate build step or HTTP fetch required.

## Message Protocol

All communication uses structured `postMessage` calls.

### Parent → Iframe

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

### Iframe → Parent

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

### Widget Sync Flow

```
1. IsolatedFrame mounts
2. Iframe sends: ready
3. Parent sends: eval (React bundle)
4. Iframe sends: renderer_ready
5. CommBridgeManager sends: bridge_ready
6. Iframe sends: widget_ready
7. CommBridgeManager sends: comm_sync (all existing models)
8. Iframe renders widgets
9. Bidirectional updates via comm_msg / widget_comm_msg
```

## Critical Code Paths

These are security-sensitive and should be reviewed carefully:

### 1. Sandbox Configuration
**File:** `src/components/isolated/isolated-frame.tsx:137`

The `SANDBOX_ATTRS` constant defines what the iframe can do. Changes here can compromise security.

### 2. Source Validation
**File:** `src/components/isolated/frame-html.ts:161`

The `event.source !== window.parent` check prevents message spoofing. This must remain intact.

### 3. Custom Message Forwarding
**File:** `src/components/isolated/comm-bridge-manager.ts`

The `subscribeToModelCustomMessages` method was added to support anywidgets like quak that use custom messages. Without it, widgets would appear to load but not receive kernel data.

### 4. Type Guard Whitelist
**File:** `src/components/isolated/frame-bridge.ts:368`

The `isIframeMessage` function whitelists valid message types. New message types must be added here.

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

```bash
# Open notebooks/test-isolation.ipynb
# Run all cells to verify:
# - window.__TAURI__ is undefined
# - Parent DOM access throws error
# - localStorage access throws error
```

### Dev Tools Toggle

Press `Cmd+Shift+I` in debug builds to open the isolation test panel.

## Troubleshooting

### Widget Not Rendering

1. Check console for errors in iframe (may need to inspect iframe in DevTools)
2. Verify `comm_sync` was sent (look for `[CommBridge]` logs)
3. Check if widget type is in `ISOLATED_MIME_TYPES`

### Widget Not Receiving Updates

1. Check if custom messages are being forwarded
2. Look for `subscribeToModelCustomMessages` being called
3. Verify kernel is sending `comm_msg` with correct comm_id

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
