# Sift Parquet Renderer — Frontend Integration

**Date:** 2026-04-11
**Issue:** #1453 (Phase 1 only)
**Status:** Draft

## Goal

Wire `@nteract/sift` into the notebook frontend so that outputs with MIME type `application/vnd.apache.parquet` render as interactive, filterable, sortable tables. No kernel integration, no python formatter, no blob upload — just the rendering path.

## MIME Type

`application/vnd.apache.parquet` — the registered IANA MIME type for Apache Parquet files.

## Rendering Approach: Main DOM (not iframe plugin)

Sift is pure DOM — no script execution risk from user-supplied specs (unlike plotly/vega which eval user JSON). It renders in the **main DOM** via a custom renderer registered in `MediaProvider`, the same pattern used for `application/vnd.jupyter.widget-view+json`.

This means:
- No `sift-renderer.tsx` CJS plugin
- No iframe `installRendererPlugin` path
- No changes to `vite-plugin-isolated-renderer.ts` or `iframe-libraries.ts`

## Data Flow

```
CRDT output (application/vnd.apache.parquet)
  → WASM resolves ContentRef → blob URL (http://127.0.0.1:<port>/blob/<hash>)
  → MediaRouter selects MIME type
  → custom renderer in MediaProvider
  → DataFrameOutput component
  → <SiftTable url={blobUrl} />
  → sift fetches parquet bytes from blob URL
  → parquet-wasm decodes client-side
  → virtual-scrolled interactive table
```

`application/vnd.apache.parquet` is `application/*` with no text carve-out in `mime.rs`, so `is_binary_mime()` returns `true`. WASM resolves the `ContentRef` to a `Url` variant pointing at the blob server. The `data` field arriving at the renderer is already a blob URL string.

## Components

### 1. `DataFrameOutput` component

**Location:** `src/components/outputs/dataframe-output.tsx`

A thin wrapper that takes the blob URL from the output data and renders `SiftTable`:

```tsx
import { SiftTable } from "@nteract/sift";

interface DataFrameOutputProps {
  data: unknown;
}

export function DataFrameOutput({ data }: DataFrameOutputProps) {
  const url = String(data);
  return <SiftTable url={url} />;
}
```

The `data` prop is the resolved blob URL string (e.g. `http://127.0.0.1:8765/blob/abc123`). Sift handles everything else — fetching, parquet decoding via WASM, column type detection, virtual scrolling.

### 2. MediaProvider registration

**Location:** `apps/notebook/src/App.tsx` (existing `MediaProvider` block)

Add `application/vnd.apache.parquet` to the `renderers` map alongside the existing widget-view renderer:

```tsx
<MediaProvider
  renderers={{
    "application/vnd.jupyter.widget-view+json": ({ data }) => {
      const { model_id } = data as { model_id: string };
      return <WidgetView modelId={model_id} />;
    },
    "application/vnd.apache.parquet": ({ data }) => {
      return <DataFrameOutput data={data} />;
    },
  }}
>
```

### 3. MIME priority

**Location:** `src/components/outputs/media-router.tsx` (`DEFAULT_PRIORITY`) and `packages/runtimed/src/mime-priority.ts` (`DEFAULT_MIME_PRIORITY`)

Add `application/vnd.apache.parquet` above `text/html` in both priority lists. When a kernel produces both parquet and HTML representations, sift wins.

### 4. Main DOM safe types

**Location:** `src/components/outputs/safe-mime-types.ts`

Add `application/vnd.apache.parquet` to `MAIN_DOM_SAFE_TYPES`. Sift is pure DOM — no eval, no script injection surface.

### 5. Sift CSS

Sift ships its own stylesheet (`@nteract/sift/style.css`). Import it in `DataFrameOutput` or at the app level. Since this renders in the main DOM (not iframe), normal CSS imports work.

## Files Changed

| File | Change |
|------|--------|
| `src/components/outputs/dataframe-output.tsx` | **New** — `DataFrameOutput` component |
| `apps/notebook/src/App.tsx` | Add parquet renderer to `MediaProvider` |
| `src/components/outputs/media-router.tsx` | Add to `DEFAULT_PRIORITY` |
| `packages/runtimed/src/mime-priority.ts` | Add to `DEFAULT_MIME_PRIORITY` |
| `src/components/outputs/safe-mime-types.ts` | Add to `MAIN_DOM_SAFE_TYPES` |

## Files NOT Changed

| File | Why |
|------|-----|
| `vite-plugin-isolated-renderer.ts` | Not an iframe plugin |
| `iframe-libraries.ts` | Not an iframe plugin |
| `isolated-renderer/index.tsx` | Not an iframe plugin |
| `crates/notebook-doc/src/mime.rs` | Already classifies as binary correctly |

## WASM Dependencies

Sift depends on two WASM modules:
- **nteract-predicate** — filtering/summary compute kernels (already in Cargo workspace via #1709)
- **parquet-wasm** — parquet decoding (npm dependency of `@nteract/sift`)

Both are loaded at runtime by sift internally. The Vite build needs to handle `.wasm` files as assets. Verify that sift's WASM initialization works in the Tauri webview context (file:// or custom protocol origins may need CSP adjustments for WASM).

## Testing

Manual verification:
1. Place a `.parquet` file in the blob store with a known hash
2. Create a notebook output referencing it (via MCP tools or manual CRDT edit)
3. Confirm sift renders the interactive table in the main DOM
4. Verify column detection, sorting, filtering, virtual scroll all work

## Out of Scope

- Kernel integration (python formatter, blob upload) — Phase 2 of #1453
- iframe fallback for ipywidgets Output contexts — Phase 3
- Dark/light theme integration — Phase 3
- `application/vnd.apache.arrow.stream` (Arrow IPC) — future work
- Streaming by row group — future work
