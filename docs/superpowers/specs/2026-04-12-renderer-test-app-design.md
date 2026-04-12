# Renderer Plugin Test App

**Date:** 2026-04-12
**Status:** Design approved

## Problem

The iframe renderer plugin pipeline (build → load → render) can only be tested
inside the full notebook+Tauri+daemon stack. This makes plugin development slow
and debugging difficult — especially for LLM agents that can't interact with a
GUI. PR #1712 exposed build issues (alias resolution, code splitting, CSS
conflicts) that would have been caught earlier with a standalone test harness.

## Solution

A standalone Vite app at `apps/renderer-test/` that exercises the full iframe
renderer plugin pipeline without Tauri, the daemon, or the notebook app.
Fixture-driven with Playwright for automated headless verification.

## Architecture

The app reuses the existing isolated renderer infrastructure from `src/`:

- `IsolatedRendererProvider` with the same `loader` pattern as the notebook
- `IsolatedFrame` for the sandboxed iframe (blob URL, opaque origin, no
  `allow-same-origin`)
- `isolatedRendererPlugin()` Vite plugin to build the IIFE bundle + all
  renderer plugins from source at dev/build time
- The shared `renderer-plugin-builder.ts` for plugin CJS bundles

The entire isolated renderer stack has zero Tauri dependencies. The only thing
the test app provides is a React host page and fixture data.

## Fixtures

A static TypeScript array of `{ label, mimeType, data }` objects. Each fixture
gets its own `IsolatedFrame` — the same component the notebook uses. Initial
fixtures:

| MIME Type | Plugin | Data |
|-----------|--------|------|
| `text/plain` | built-in | Short text string |
| `text/html` | built-in | Small HTML snippet |
| `application/json` | built-in | JSON object |
| `image/svg+xml` | built-in | Inline SVG |
| `text/markdown` | markdown | Markdown with headings, code, list |
| `application/vnd.plotly.v1+json` | plotly | Minimal scatter plot |

Additional fixtures (parquet, vega, leaflet) can be added as those plugins
mature. The fixture list is the single place to extend when adding new test
cases.

## What This Tests

1. **Plugin build pipeline** — `renderer-plugin-builder.ts` builds all plugins
   from source (markdown, plotly, vega, leaflet) using the same Vite config as
   the notebook
2. **IIFE renderer build** — `isolatedRendererPlugin()` builds the core iframe
   bundle that bootstraps React and the plugin loader
3. **Plugin injection** — `iframe-libraries.ts` loads the correct plugin for
   each MIME type on demand
4. **Iframe sandbox** — same security model as notebook (opaque origin, no
   `allow-same-origin`)
5. **Message protocol** — parent sends render via JSON-RPC, iframe processes
   and renders
6. **CSS isolation** — plugin CSS doesn't leak between fixtures

## What This Does Not Test

- Tauri APIs (intentionally excluded — the iframe never has access)
- Daemon/kernel integration (no running daemon needed)
- Widget/comm bridge (could add later if needed)
- WASM-based renderers like sift parquet (added when sift plugin ships)

## File Structure

```
apps/renderer-test/
  index.html              — entry HTML, mounts #root
  src/
    main.tsx              — React app, renders fixtures in IsolatedFrames
    fixtures.ts           — test fixture definitions (MIME + data pairs)
  vite.config.ts          — uses isolatedRendererPlugin(), tailwindcss
  tsconfig.json           — extends root tsconfig
  package.json            — react, vite-plus, playwright
  playwright.config.ts    — headless Chromium, dev server on port 5176
  e2e/
    render.spec.ts        — verify each fixture renders without errors
```

## Playwright Tests

Each fixture gets a basic smoke test:

1. Navigate to the page
2. Wait for all iframes to appear (one per fixture)
3. For each iframe, wait for the `data-renderer-ready` attribute (set by the
   host after receiving `renderer_ready` from the iframe)
4. Verify no error elements visible inside the iframe
5. Verify the iframe has non-zero content height

Assertions will evolve as we learn what's practical to check per MIME type.
The initial bar is: every fixture renders without crashing.

## Developer Workflow

```bash
cd apps/renderer-test
pnpm install
npx vp dev           # dev server with hot reload on port 5176
npx playwright test  # headless fixture verification
```

For agents:

```bash
cd apps/renderer-test && npx playwright test --reporter=line
```

## Dependencies

The test app imports from the shared `src/` tree (same as `apps/notebook/` and
`apps/mcp-app/`). Key dependencies:

- `src/components/isolated/isolated-frame.tsx` — the iframe component
- `src/components/isolated/isolated-renderer-context.tsx` — bundle provider
- `src/components/isolated/iframe-libraries.ts` — MIME → plugin mapping
- `src/isolated-renderer/index.tsx` — the IIFE entry point
- `src/build/renderer-plugin-builder.ts` — shared plugin build logic
- `apps/notebook/vite-plugin-isolated-renderer.ts` — Vite plugin that builds
  the IIFE and plugins (reused by the test app's vite.config.ts)
