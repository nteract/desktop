# Investigation: Stale Iframe Renderer Plugin Assets

## Problem

After rebuilding a renderer plugin (e.g. adding `text/latex` to the markdown plugin), the iframe continues serving old plugin code. A manual `window.location.reload()` in WebKit console was required. Vite's HMR full-reload signal didn't bust the cached plugin chunks.

## Findings

### Q1: Do production builds get cache-busted filenames?

**Yes — production is fine.** The Vite config in `apps/notebook/vite.config.ts` uses content-hashed output names:

```
entryFileNames: "assets/[name]-[hash].js",
chunkFileNames: "assets/[name]-[hash].js",
assetFileNames: "assets/[name]-[hash].[ext]",
```

The flow is: plugin sub-build (in-memory, unhashed) → string constant embedded in virtual module → Rollup chunk → `assets/[name]-[hash].js`. If plugin source changes, the embedded string changes, the chunk hash changes. Cache busting works correctly in production.

### Q2: Does Tauri's WebView persist its cache across app upgrades?

**Yes — the WebView cache survives upgrades.** No cache invalidation exists in the upgrade path:

- No `WKWebsiteDataStore` clearing anywhere in the codebase
- `core:webview:allow-clear-all-browsing-data` permission exists in Tauri's ACL schema but is **not granted**
- The upgrade flow (`run_upgrade` in `lib.rs`) does: download → save state → stop runtimes → upgrade daemon sidecar → `relaunch()` — no WebView cache step
- `tauri://` asset protocol uses default WKWebView caching with no `Cache-Control` overrides

However, this is **mitigated by content-hashed filenames**. When the new app bundle loads, `index.html` references new `assets/[name]-[hash].js` URLs. The old cached chunks have different hashes and are never requested. The only risk is if `index.html` itself is cached by WKWebView — but since it's served via `tauri://` protocol (not HTTP), standard HTTP caching semantics may not apply. Worth verifying empirically but likely not the active bug.

### Q3: Does `pluginCache` survive Vite HMR full-reload?

**This is the bug.** Here's the chain of events during dev:

1. Developer edits `src/isolated-renderer/markdown-renderer.tsx`
2. `handleHotUpdate` fires → calls `invalidateCache()` → rebuilds all plugins → invalidates `\0virtual:isolated-renderer` in the module graph → sends `full-reload`
3. **But:** only the core virtual module (`\0virtual:isolated-renderer`) is explicitly invalidated. The individual `\0virtual:renderer-plugin/*` modules are **not invalidated** in Vite's module graph.
4. `full-reload` tells the browser to reload the page. On reload, the main app modules are re-evaluated from scratch, including `iframe-libraries.ts` — so the module-level `pluginCache` Map starts empty. ✅
5. **However,** when the re-evaluated code calls `import("virtual:renderer-plugin/markdown")`, Vite's dev server checks its module graph. The `\0virtual:renderer-plugin/markdown` module was **never invalidated**, so Vite serves the **stale cached transform** from before the rebuild.

The `pluginCache` Map itself is a red herring — it resets on full-reload. The real problem is that the Vite plugin only invalidates the core virtual module but not the per-plugin virtual modules.

### Q4: Parent page modules cached by WebKit

This compounds Q3. Even if Vite invalidates correctly, the dev server sends `full-reload` with `path: "*"`. The browser reloads the page, but Vite's module-level cache in the dev server process is the authoritative source for virtual modules. The browser's HTTP cache is less relevant here because Vite's dev server serves transforms with appropriate cache headers. The root cause is server-side: Vite's module graph retains stale plugin transforms.

## Root Cause

In `vite-plugin-isolated-renderer.ts`, the `handleHotUpdate` handler:

```typescript
// Only invalidates the core module:
const mod = devServer.moduleGraph.getModuleById(RESOLVED_VIRTUAL_MODULE_ID);
if (mod) {
  devServer.moduleGraph.invalidateModule(mod);
  devServer.ws.send({ type: "full-reload", path: "*" });
}
```

It never invalidates the four plugin modules:
- `\0virtual:renderer-plugin/markdown`
- `\0virtual:renderer-plugin/vega`
- `\0virtual:renderer-plugin/plotly`
- `\0virtual:renderer-plugin/leaflet`

After `invalidateCache()` zeroes the in-memory strings and `buildRenderer()` repopulates them, the Vite module graph still holds the old `load()` results for the plugin modules. When the reloaded page re-imports them, Vite serves the cached (stale) transforms.

## Proposed Fix

### Fix 1: Invalidate all plugin virtual modules on HMR (the actual bug)

In `handleHotUpdate`, after invalidating the core module, also invalidate every plugin module:

```typescript
// Invalidate core
const mod = devServer.moduleGraph.getModuleById(RESOLVED_VIRTUAL_MODULE_ID);
if (mod) {
  devServer.moduleGraph.invalidateModule(mod);
}

// Invalidate all plugin virtual modules
const pluginNames = ["markdown", "vega", "plotly", "leaflet"];
for (const name of pluginNames) {
  const pluginMod = devServer.moduleGraph.getModuleById(
    `${RESOLVED_PLUGIN_PREFIX}${name}`,
  );
  if (pluginMod) {
    devServer.moduleGraph.invalidateModule(pluginMod);
  }
}

devServer.ws.send({ type: "full-reload", path: "*" });
```

This is the minimal correct fix. When the reloaded page re-imports `virtual:renderer-plugin/markdown`, Vite sees the module is invalidated, calls `load()` again, and gets the freshly rebuilt string constants.

### Fix 2 (defense-in-depth): Clear WebView cache on app upgrade

Grant `core:webview:allow-clear-all-browsing-data` in `crates/notebook/capabilities/default.json` and call `webview.clear_all_browsing_data()` during the upgrade sequence in `run_upgrade`. This prevents any possible stale-asset scenario across version boundaries, even if content hashing has an edge case.

This is a separate PR — it's not needed to fix the immediate dev-mode bug.

## Files to Change

| File | Change |
|------|--------|
| `apps/notebook/vite-plugin-isolated-renderer.ts` | Invalidate plugin virtual modules in `handleHotUpdate` |
