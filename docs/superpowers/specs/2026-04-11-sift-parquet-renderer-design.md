# Sift Parquet Renderer — Frontend Integration

**Date:** 2026-04-11
**Issue:** #1453 (Phase 1 only)
**Status:** Draft

## Goal

Wire `@nteract/sift` into the notebook frontend as an iframe renderer plugin so that outputs with MIME type `application/vnd.apache.parquet` render as interactive, filterable, sortable tables. Must work in both the notebook app and the MCPB (Claude Desktop extension). No kernel integration, no python formatter, no blob upload — just the rendering path.

## MIME Type

`application/vnd.apache.parquet` — the registered IANA MIME type for Apache Parquet files.

## Rendering Approach: Iframe Plugin

Sift renders as an **iframe renderer plugin** — the same architecture as plotly, vega, leaflet, and markdown. This is required for MCPB support, where all rich output renders inside isolated iframes.

The plugin is:
- Built as a CJS module with React externalized
- Loaded on demand via `virtual:renderer-plugin/sift`
- Installed into the iframe via `installRendererPlugin()`
- Registered in the iframe's renderer registry for `application/vnd.apache.parquet`

## Data Flow

```
CRDT output (application/vnd.apache.parquet)
  → WASM resolves ContentRef → blob URL (http://127.0.0.1:<port>/blob/<hash>)
  → OutputArea pre-scans MIME types, sees needsPlugin() → true
  → injectPluginsForMimes() loads virtual:renderer-plugin/sift
  → iframe installRendererPlugin() executes CJS, registers SiftRenderer
  → iframe receives NTERACT_RENDER_OUTPUT with blob URL as data
  → SiftRenderer passes blob URL to <SiftTable url={blobUrl} />
  → sift fetches parquet bytes from blob URL
  → parquet-wasm decodes client-side
  → virtual-scrolled interactive table
  → iframe sends render_complete with height
```

`application/vnd.apache.parquet` is `application/*` with no text carve-out in `mime.rs`, so `is_binary_mime()` returns `true`. WASM resolves the `ContentRef` to a `Url` variant pointing at the blob server. The `data` field arriving at the renderer is already a blob URL string.

## Components

### 1. Sift renderer plugin

**Location:** `src/isolated-renderer/sift-renderer.tsx`

CJS plugin module following the same pattern as `plotly-renderer.tsx`:

```tsx
import { SiftTable } from "@nteract/sift";
import "@nteract/sift/style.css";

interface RendererProps {
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}

function SiftRenderer({ data }: RendererProps) {
  const url = String(data);
  return <SiftTable url={url} />;
}

export function install(ctx: {
  register: (mimeTypes: string[], component: React.ComponentType<RendererProps>) => void;
}) {
  ctx.register(["application/vnd.apache.parquet"], SiftRenderer);
}
```

### 2. Vite build plugin integration

**Location:** `apps/notebook/vite-plugin-isolated-renderer.ts`

- Add `sift-renderer.tsx` as a new plugin entry alongside markdown, vega, plotly, leaflet
- Build via `buildRendererPlugin(siftEntry, "sift-renderer", srcDir)`
- Register `virtual:renderer-plugin/sift` virtual module
- Add to HMR invalidation list

### 3. MIME → plugin mapping

**Location:** `src/components/isolated/iframe-libraries.ts`

Add to `PLUGIN_MIME_TYPES`:

```typescript
"application/vnd.apache.parquet": () => import("virtual:renderer-plugin/sift").then(normalize),
```

This makes `needsPlugin("application/vnd.apache.parquet")` return `true`, triggering plugin injection before render.

### 4. MIME priority

**Location:** `src/components/outputs/media-router.tsx` (`DEFAULT_PRIORITY`) and `packages/runtimed/src/mime-priority.ts` (`DEFAULT_MIME_PRIORITY`)

Add `application/vnd.apache.parquet` above `text/html` in both priority lists. When a kernel produces both parquet and HTML representations, sift wins.

## Files Changed

| File | Change |
|------|--------|
| `src/isolated-renderer/sift-renderer.tsx` | **New** — CJS plugin wrapping SiftTable |
| `apps/notebook/vite-plugin-isolated-renderer.ts` | Add sift to plugin build, virtual module, HMR |
| `src/components/isolated/iframe-libraries.ts` | Add MIME → plugin mapping |
| `src/components/outputs/media-router.tsx` | Add to `DEFAULT_PRIORITY` |
| `packages/runtimed/src/mime-priority.ts` | Add to `DEFAULT_MIME_PRIORITY` |

## Files NOT Changed

| File | Why |
|------|-----|
| `src/components/outputs/safe-mime-types.ts` | Not rendering in main DOM |
| `apps/notebook/src/App.tsx` | No MediaProvider registration needed — iframe handles it |
| `src/isolated-renderer/index.tsx` | Already has generic `installRendererPlugin()` infrastructure |
| `crates/notebook-doc/src/mime.rs` | Already classifies as binary correctly |

## WASM Dependencies

Sift depends on two WASM modules:
- **nteract-predicate** — filtering/summary compute kernels (already in Cargo workspace via #1709)
- **parquet-wasm** — parquet decoding (npm dependency of `@nteract/sift`)

Both are loaded at runtime by sift internally. Since sift runs inside the iframe, WASM must be loadable from the iframe's blob: origin. The iframe CSP already allows `'unsafe-eval'` (required by plotly), which covers WASM instantiation. The `.wasm` files need to be served as assets accessible from the iframe — verify they're included in the Vite build output or accessible via the blob server.

## Testing

Manual verification:
1. Place a `.parquet` file in the blob store with a known hash
2. Create a notebook output referencing it (via MCP tools or manual CRDT edit)
3. Confirm sift renders inside the iframe as an interactive table
4. Verify column detection, sorting, filtering, virtual scroll all work
5. Verify iframe height auto-sizing works (render_complete message)

## Out of Scope

- Kernel integration (python formatter, blob upload) — Phase 2 of #1453
- Dark/light theme integration — Phase 3
- `application/vnd.apache.arrow.stream` (Arrow IPC) — future work
- Streaming by row group — future work
