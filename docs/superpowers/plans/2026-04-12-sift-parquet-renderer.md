# Sift Parquet Renderer Plugin Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Render `application/vnd.apache.parquet` outputs in notebook cells using `@nteract/sift` as an iframe renderer plugin.

**Architecture:** The `SiftTable` `url` prop gains content-type detection — fetches the URL, checks the `Content-Type` response header, and routes to either Arrow IPC streaming (existing) or WASM-based parquet loading. A new `setWasmUrl()` API lets the iframe configure where to load the WASM binary. A renderer plugin (`sift-renderer.tsx`) registers for `application/vnd.apache.parquet` and passes the blob URL to `SiftTable`. The WASM binary is served from the blob server's `/plugins/` route.

**Tech Stack:** TypeScript, React, nteract-predicate WASM, Vite Plus, Rust (blob server)

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `packages/sift/src/predicate.ts` | Modify | Add `setWasmUrl()` + pass configured URL to WASM init |
| `packages/sift/src/index.ts` | Modify | Export `setWasmUrl` |
| `packages/sift/src/react.tsx` | Modify | Content-type detection in `url` effect, parquet loading path |
| `src/isolated-renderer/sift-renderer.tsx` | Create | Renderer plugin for `application/vnd.apache.parquet` |
| `src/build/renderer-plugin-builder.ts` | Modify | Add sift to `RENDERER_PLUGINS` array |
| `src/components/isolated/iframe-libraries.ts` | Modify | Add parquet MIME → sift virtual module mapping |
| `crates/runtimed/src/embedded_plugins.rs` | Modify | Add `nteract-predicate.wasm` to embedded assets |
| `crates/runt-mcp/assets/plugins/nteract-predicate.wasm` | Create | WASM binary (Git LFS) |

---

### Task 1: Add `setWasmUrl()` to predicate.ts and export it

Configure where the WASM module loads from. Needed because the iframe's `blob:` origin can't resolve `import.meta.url` for the default WASM path.

**Files:**
- Modify: `packages/sift/src/predicate.ts:74-82`
- Modify: `packages/sift/src/index.ts`

- [ ] **Step 1: Add configuredWasmUrl and setWasmUrl to predicate.ts**

In `packages/sift/src/predicate.ts`, after line 72 (`};`) and before line 74 (`let mod`), add:

```typescript
let configuredWasmUrl: string | undefined;
```

After line 74 (`let mod: PredicateModule | null = null;`), add:

```typescript
/**
 * Configure an explicit URL for the WASM binary.
 * Must be called before the first WASM operation.
 * Used in iframe contexts where import.meta.url doesn't resolve.
 */
export function setWasmUrl(url: string): void {
  configuredWasmUrl = url;
}
```

Modify `ensureModule()` (line 76-82) to pass the configured URL:

```typescript
async function ensureModule(): Promise<PredicateModule> {
  if (mod) return mod;
  const wasm = await import("nteract-predicate/nteract_predicate.js");
  await wasm.default(configuredWasmUrl);
  mod = wasm as unknown as PredicateModule;
  return mod;
}
```

- [ ] **Step 2: Export setWasmUrl from index.ts**

In `packages/sift/src/index.ts`, add after the existing predicate-related exports (or at the end):

```typescript
export { setWasmUrl } from "./predicate";
```

- [ ] **Step 3: Build sift package to verify**

```bash
cd packages/sift && npx vp build --config vite.lib.config.ts 2>&1 | tail -10
```

Expected: clean build, `setWasmUrl` in output.

- [ ] **Step 4: Commit**

```bash
git add packages/sift/src/predicate.ts packages/sift/src/index.ts
git commit -m "feat(sift): add setWasmUrl() API for iframe WASM configuration"
```

---

### Task 2: Add content-type detection to SiftTable url prop

The `url` effect currently assumes Arrow IPC. Add a content-type check: if the response is `application/vnd.apache.parquet`, route to the WASM parquet loader. Otherwise fall back to the existing Arrow IPC streaming path.

**Files:**
- Modify: `packages/sift/src/react.tsx:158-240` (the `url` useEffect)

- [ ] **Step 1: Read the current url effect and understand it**

Read `packages/sift/src/react.tsx` lines 158-240 to understand the existing Arrow IPC streaming path.

- [ ] **Step 2: Add parquet imports and the updateWasmSummaries helper**

At the top of `react.tsx`, add imports for the WASM parquet path. After the existing imports (around line 35), add:

```typescript
import { ensureModule, getModuleSync } from "./predicate";
import { createWasmTableData } from "./wasm-table-data";
```

Note: `ensureModule` is not currently exported. You'll need to export it from `predicate.ts`:

In `packages/sift/src/predicate.ts`, change `ensureModule` from a plain function to an exported one:
```typescript
export async function ensureModule(): Promise<PredicateModule> {
```

Add the `updateWasmSummaries` helper function inside `react.tsx`, before the `SiftTable` component (around line 55). This is extracted from `main.ts:449-555`:

```typescript
function updateWasmSummaries(
  mod: ReturnType<typeof getModuleSync>,
  handle: number,
  tableData: TableData,
  columns: Column[],
) {
  const numRows = mod.num_rows(handle);
  const BIN_COUNT = 25;

  tableData.rowCount = numRows;
  tableData.columnSummaries = columns.map((col, c) => {
    switch (col.columnType) {
      case "categorical": {
        const counts = mod.store_value_counts(handle, c) as {
          label: string;
          count: number;
        }[];
        const allCategories = counts.map(({ label, count }) => ({
          label,
          count,
          pct: Math.round((count / numRows) * 1000) / 10,
        }));
        const topCategories = allCategories.slice(0, 3);
        const othersCount = counts.slice(3).reduce((s, e) => s + e.count, 0);
        const othersPct = Math.round((othersCount / numRows) * 1000) / 10;
        const lengths = counts.map(({ label }) => label.length).sort((a, b) => a - b);
        const medianTextLength = lengths.length > 0 ? lengths[Math.floor(lengths.length / 2)] : 0;
        return {
          kind: "categorical" as const,
          uniqueCount: counts.length,
          topCategories,
          othersCount,
          othersPct,
          allCategories,
          medianTextLength,
        };
      }
      case "boolean": {
        const [trueCount, falseCount, nullCount] = mod.store_bool_counts(handle, c);
        return {
          kind: "boolean" as const,
          trueCount,
          falseCount,
          nullCount,
          total: numRows,
        };
      }
      case "timestamp": {
        const bins = mod.store_temporal_histogram(handle, c) as {
          x0: number;
          x1: number;
          count: number;
        }[];
        if (bins.length === 0) return null;
        return {
          kind: "timestamp" as const,
          min: bins[0].x0,
          max: bins[bins.length - 1].x1,
          bins,
        };
      }
      case "numeric": {
        const bins = mod.store_histogram(handle, c, BIN_COUNT) as {
          x0: number;
          x1: number;
          count: number;
        }[];
        if (bins.length === 0) return null;
        return {
          kind: "numeric" as const,
          min: bins[0].x0,
          max: bins[bins.length - 1].x1,
          bins,
        };
      }
    }
  });
}
```

- [ ] **Step 3: Refactor the url effect to detect content type**

Replace the `url` useEffect (lines ~158-240) with a version that checks the `Content-Type` header. The structure:

1. Fetch the URL
2. Check `Content-Type` header
3. If `application/vnd.apache.parquet` → parquet WASM path
4. Otherwise → existing Arrow IPC streaming path

The parquet path:
- Awaits `ensureModule()` to initialize WASM
- Reads the full response as `Uint8Array`
- Gets metadata via `mod.parquet_metadata()` to know row group count
- Loads the first row group → mounts the table immediately
- Streams remaining row groups with `setTimeout(r, 0)` yielding
- Frees the WASM handle on cleanup

```typescript
  // Stream from URL when `url` prop is provided
  useEffect(() => {
    if (!url || !containerRef.current) return;

    let cancelled = false;
    const container = containerRef.current;
    let wasmHandle: number | null = null;

    async function loadFromUrl() {
      setStatus("loading");
      setError(null);

      const response = await fetch(url!);
      if (!response.ok) {
        throw new Error(`Failed to fetch: ${response.status} ${response.statusText}`);
      }

      const contentType = response.headers.get("Content-Type") ?? "";
      const isParquet = contentType.includes("parquet");

      if (isParquet) {
        await loadParquet(response);
      } else {
        await loadArrowIpc(response);
      }
    }

    async function loadParquet(response: Response) {
      await ensureModule();
      const mod = getModuleSync();
      const parquetBytes = new Uint8Array(await response.arrayBuffer());
      if (cancelled) return;

      const meta = mod.parquet_metadata(parquetBytes);
      const numRowGroups = meta[0];

      // Load first row group → mount table immediately
      const handle = mod.load_parquet_row_group(parquetBytes, 0, 0);
      wasmHandle = handle;

      const { tableData, columns, prefetchViewport } = createWasmTableData(handle, columnOverrides);
      tableData.prefetchViewport = prefetchViewport;
      tableData.recomputeSummaries = () => updateWasmSummaries(mod, handle, tableData, columns);
      updateWasmSummaries(mod, handle, tableData, columns);

      if (cancelled) return;

      // Clean up previous engine
      if (engineRef.current) {
        engineRef.current.destroy();
        engineRef.current = null;
      }
      container.innerHTML = "";
      engineRef.current = createTable(container, tableData, { onChange: stableOnChange ?? undefined });
      setStatus("ready");

      // Stream remaining row groups progressively
      for (let g = 1; g < numRowGroups; g++) {
        if (cancelled) return;
        await new Promise((r) => setTimeout(r, 0));
        if (cancelled) return;
        mod.load_parquet_row_group(parquetBytes, g, handle);
        tableData.rowCount = mod.num_rows(handle);
        updateWasmSummaries(mod, handle, tableData, columns);
        engineRef.current!.onBatchAppended();
      }

      engineRef.current!.setStreamingDone();
    }

    async function loadArrowIpc(response: Response) {
      // --- existing Arrow IPC streaming code (lines ~175-240) ---
      // Move the existing code into this function body unchanged
    }

    loadFromUrl().catch((err) => {
      if (!cancelled) {
        setError(String(err));
        setStatus("error");
      }
    });

    return () => {
      cancelled = true;
      if (wasmHandle !== null) {
        try { getModuleSync().free(wasmHandle); } catch { /* module may not be loaded */ }
      }
    };
  }, [url, stableOnChange]);
```

IMPORTANT: The existing Arrow IPC code (the body of the current `loadFromUrl` function from line 170 onward) should be moved into `loadArrowIpc()` unchanged. Do not rewrite it — just wrap it.

- [ ] **Step 4: Run sift tests**

```bash
cd packages/sift && npx vitest run 2>&1 | tail -10
```

Expected: tests pass (existing tests use `data` prop, not `url`).

- [ ] **Step 5: Commit**

```bash
git add packages/sift/src/react.tsx packages/sift/src/predicate.ts
git commit -m "feat(sift): add content-type detection to url prop for parquet support

SiftTable's url prop now checks the Content-Type response header.
Parquet responses route to the WASM loader with progressive row group
streaming. Arrow IPC responses continue using the existing streaming
path."
```

---

### Task 3: Create sift renderer plugin

Create the iframe renderer plugin that registers for `application/vnd.apache.parquet` and renders `SiftTable`.

**Files:**
- Create: `src/isolated-renderer/sift-renderer.tsx`

- [ ] **Step 1: Create the renderer plugin**

Create `src/isolated-renderer/sift-renderer.tsx`:

```tsx
/**
 * Sift Renderer Plugin
 *
 * On-demand renderer plugin for application/vnd.apache.parquet outputs.
 * Loaded into the isolated iframe via the renderer plugin API.
 *
 * Data flow: kernel outputs parquet bytes → daemon stores in blob server →
 * frontend gets blob URL → iframe loads sift plugin → SiftTable fetches
 * parquet from blob URL → WASM decodes → table renders.
 */

import { setWasmUrl, SiftTable } from "@nteract/sift";
import "@nteract/sift/style.css";

// --- Types ---

interface RendererProps {
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}

// --- WASM configuration ---

let wasmConfigured = false;

/**
 * Extract the blob server origin from a blob URL and configure WASM
 * to load from the same server's /plugins/ route.
 */
function configureWasm(blobUrl: string): void {
  if (wasmConfigured) return;
  try {
    const parsed = new URL(blobUrl);
    const wasmUrl = `${parsed.protocol}//${parsed.host}/plugins/nteract-predicate.wasm`;
    setWasmUrl(wasmUrl);
    wasmConfigured = true;
  } catch {
    // Fall back to default WASM resolution
  }
}

// --- SiftRenderer component ---

function SiftRenderer({ data }: RendererProps) {
  const url = String(data);
  configureWasm(url);

  return (
    <div style={{ height: 600, width: "100%" }}>
      <SiftTable url={url} />
    </div>
  );
}

// --- Plugin install ---

export function install(ctx: {
  register: (mimeTypes: string[], component: React.ComponentType<RendererProps>) => void;
}) {
  ctx.register(["application/vnd.apache.parquet"], SiftRenderer);
}
```

- [ ] **Step 2: Commit**

```bash
git add src/isolated-renderer/sift-renderer.tsx
git commit -m "feat(sift): create iframe renderer plugin for parquet MIME type"
```

---

### Task 4: Wire sift into the build system and MIME registry

Add sift to the shared renderer plugin builder and register the MIME type.

**Files:**
- Modify: `src/build/renderer-plugin-builder.ts:51-56`
- Modify: `src/components/isolated/iframe-libraries.ts:35-40`

- [ ] **Step 1: Add sift to RENDERER_PLUGINS**

In `src/build/renderer-plugin-builder.ts`, add sift to the `RENDERER_PLUGINS` array (line 51-56). The sift plugin needs an alias for `@nteract/sift` since the build uses its own Vite `build()` that doesn't inherit pnpm workspace resolution. Also needs `nteract-predicate` aliased to either the real WASM JS glue or a mock.

First, add sift to the array:

```typescript
export const RENDERER_PLUGINS: RendererPluginDef[] = [
  { name: "markdown", entry: path.resolve(srcDir, "isolated-renderer/markdown-renderer.tsx") },
  { name: "plotly", entry: path.resolve(srcDir, "isolated-renderer/plotly-renderer.tsx") },
  { name: "vega", entry: path.resolve(srcDir, "isolated-renderer/vega-renderer.tsx") },
  { name: "leaflet", entry: path.resolve(srcDir, "isolated-renderer/leaflet-renderer.tsx") },
  { name: "sift", entry: path.resolve(srcDir, "isolated-renderer/sift-renderer.tsx") },
];
```

Then, in `buildRendererPlugin()` (around line 101), add resolve aliases for the sift package and the WASM glue. Add these to the `resolve.alias` object:

```typescript
    resolve: {
      alias: {
        "@/": `${srcDir}/`,
        "@nteract/sift": path.resolve(srcDir, "../packages/sift/src/index.ts"),
        "@nteract/sift/style.css": path.resolve(srcDir, "../packages/sift/src/style.css"),
        "nteract-predicate/nteract_predicate.js": resolveWasmGlue(),
      },
    },
```

Add the `resolveWasmGlue()` helper before `buildRendererPlugin`:

```typescript
/**
 * Resolve the WASM JS glue for nteract-predicate.
 * Falls back to a stub if the WASM crate hasn't been built.
 */
function resolveWasmGlue(): string {
  const realPath = path.resolve(srcDir, "../crates/nteract-predicate/pkg/nteract_predicate.js");
  const mockPath = path.resolve(srcDir, "../packages/sift/src/__mocks__/nteract-predicate/nteract_predicate.js");
  try {
    require("node:fs").accessSync(realPath);
    return realPath;
  } catch {
    return mockPath;
  }
}
```

Also create the mock file if it doesn't exist. Create `packages/sift/src/__mocks__/nteract-predicate/nteract_predicate.js`:

```javascript
// Stub for when nteract-predicate WASM hasn't been built.
// The sift plugin will fall back gracefully (isAvailable() returns false).
export default function init() {
  throw new Error("nteract-predicate WASM not built — run: cargo xtask wasm sift");
}
```

- [ ] **Step 2: Add parquet MIME to iframe-libraries.ts**

In `src/components/isolated/iframe-libraries.ts`, add to `PLUGIN_MIME_TYPES` (around line 35-40):

```typescript
const PLUGIN_MIME_TYPES: Record<string, () => Promise<PluginModule>> = {
  "text/markdown": () => import("virtual:renderer-plugin/markdown").then(normalize),
  "text/latex": () => import("virtual:renderer-plugin/markdown").then(normalize),
  "application/vnd.plotly.v1+json": () => import("virtual:renderer-plugin/plotly").then(normalize),
  "application/geo+json": () => import("virtual:renderer-plugin/leaflet").then(normalize),
  "application/vnd.apache.parquet": () => import("virtual:renderer-plugin/sift").then(normalize),
};
```

- [ ] **Step 3: Add the virtual module type declaration**

In `apps/notebook/src/vite-env.d.ts`, add:

```typescript
declare module "virtual:renderer-plugin/sift" {
  export const code: string;
  export const css: string;
}
```

- [ ] **Step 4: Build to verify**

```bash
cd apps/notebook && npx vp build 2>&1 | tail -20
```

Expected: build succeeds, sift plugin appears in output.

- [ ] **Step 5: Commit**

```bash
git add src/build/renderer-plugin-builder.ts src/components/isolated/iframe-libraries.ts apps/notebook/src/vite-env.d.ts packages/sift/src/__mocks__/
git commit -m "feat(sift): wire sift renderer plugin into build system and MIME registry

Add sift to RENDERER_PLUGINS with workspace alias resolution and WASM
glue fallback. Register application/vnd.apache.parquet in the iframe
MIME type mapping."
```

---

### Task 5: Serve WASM from blob server

Embed the `nteract-predicate.wasm` binary in the blob server and serve it at `/plugins/nteract-predicate.wasm`.

**Files:**
- Create: `crates/runt-mcp/assets/plugins/nteract-predicate.wasm` (Git LFS)
- Modify: `crates/runtimed/src/embedded_plugins.rs`

- [ ] **Step 1: Copy the WASM binary**

The WASM binary should exist at `crates/nteract-predicate/pkg/nteract_predicate_bg.wasm` if the crate has been built. Copy it:

```bash
cp crates/nteract-predicate/pkg/nteract_predicate_bg.wasm crates/runt-mcp/assets/plugins/nteract-predicate.wasm
```

If the file doesn't exist, build it first:
```bash
cargo xtask wasm sift
cp crates/nteract-predicate/pkg/nteract_predicate_bg.wasm crates/runt-mcp/assets/plugins/nteract-predicate.wasm
```

Track it with Git LFS:
```bash
git lfs track "crates/runt-mcp/assets/plugins/nteract-predicate.wasm"
```

- [ ] **Step 2: Add to embedded_plugins.rs**

In `crates/runtimed/src/embedded_plugins.rs`, add to the LFS guard (after line 24):

```rust
    const WASM: &[u8] = include_bytes!("../../runt-mcp/assets/plugins/nteract-predicate.wasm");
    assert!(
        WASM.len() > 1024,
        "nteract-predicate.wasm appears to be a Git LFS pointer — run `git lfs pull`"
    );
```

Add to the `get()` match (after the leaflet entries):

```rust
        "nteract-predicate.wasm" => Some((
            include_bytes!("../../runt-mcp/assets/plugins/nteract-predicate.wasm"),
            "application/wasm",
        )),
```

- [ ] **Step 3: Build daemon to verify**

```bash
cargo build -p runtimed 2>&1 | tail -5
```

Expected: clean build with WASM embedded.

- [ ] **Step 4: Commit**

```bash
git add .gitattributes crates/runt-mcp/assets/plugins/nteract-predicate.wasm crates/runtimed/src/embedded_plugins.rs
git commit -m "feat(sift): serve nteract-predicate WASM from blob server /plugins/ route"
```

---

### Task 6: Lint, build, and verify

Full verification pass.

**Files:** None (verification only)

- [ ] **Step 1: Lint**

```bash
cargo xtask lint --fix 2>&1 | tail -15
```

- [ ] **Step 2: Full frontend build**

```bash
cd apps/notebook && npx vp build 2>&1 | tail -20
```

- [ ] **Step 3: Full Rust build**

```bash
cargo build --workspace --exclude runtimed-py 2>&1 | tail -5
```

- [ ] **Step 4: Run sift tests**

```bash
cd packages/sift && npx vitest run 2>&1 | tail -10
```

- [ ] **Step 5: Run notebook tests**

```bash
pnpm test:run 2>&1 | tail -10
```

- [ ] **Step 6: Commit any lint fixes**

```bash
git add -A
git commit -m "style: lint fixes" || echo "nothing to commit"
```
