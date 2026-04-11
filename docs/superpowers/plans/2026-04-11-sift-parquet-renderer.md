# Sift Parquet Renderer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire `@nteract/sift` into the notebook iframe renderer plugin system so `application/vnd.apache.parquet` outputs render as interactive tables.

**Architecture:** Sift becomes an iframe renderer plugin (CJS module, code-split, on-demand loaded) — same pattern as plotly/vega/leaflet. The plugin fetches parquet bytes from the blob URL, loads them into nteract-predicate WASM, builds a `WasmTableData`, and renders via the imperative `createTable` engine. A new `parquetUrl` prop on `SiftTable` encapsulates this loading path.

**Tech Stack:** `@nteract/sift`, `nteract-predicate` WASM, Vite plugin virtual modules, iframe renderer plugin API

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `packages/sift/src/react.tsx` | Modify | Add `parquetUrl` prop to `SiftTable` |
| `src/isolated-renderer/sift-renderer.tsx` | Create | CJS renderer plugin — thin wrapper |
| `apps/notebook/vite-plugin-isolated-renderer.ts` | Modify | Add sift plugin build, virtual module, HMR |
| `src/components/isolated/iframe-libraries.ts` | Modify | Add MIME → plugin mapping |
| `src/components/outputs/media-router.tsx` | Modify | Add to `DEFAULT_PRIORITY` |
| `packages/runtimed/src/mime-priority.ts` | Modify | Add to `DEFAULT_MIME_PRIORITY` |

---

### Task 1: Add `parquetUrl` prop to SiftTable

The `SiftTable` React component currently only supports Arrow IPC URLs (`url` prop) and pre-built `TableData` (`data` prop). Parquet loading requires a different pipeline: fetch bytes → `loadParquet()` WASM → `createWasmTableData()` → `createTable()`. Add a `parquetUrl` prop that handles this, following the same progressive row-group loading pattern from `main.ts`.

**Files:**
- Modify: `packages/sift/src/react.tsx`

- [ ] **Step 1: Add `parquetUrl` prop to `SiftTableProps`**

In `packages/sift/src/react.tsx`, add `parquetUrl` to the props type:

```tsx
export type SiftTableProps = {
  /** Pre-built TableData object. Mutually exclusive with `url` and `parquetUrl`. */
  data?: TableData;
  /** Arrow IPC URL to stream from. Mutually exclusive with `data` and `parquetUrl`. */
  url?: string;
  /** Parquet file URL to load via WASM. Mutually exclusive with `data` and `url`. */
  parquetUrl?: string;
  /** Column type overrides keyed by column name. */
  typeOverrides?: Record<string, ColumnType>;
  /** Column display overrides (label, width, sortable). */
  columnOverrides?: Record<string, Partial<Column>>;
  /** Called whenever sort or filter state changes from UI interaction. */
  onChange?: (state: TableEngineState) => void;
  /** CSS class name for the container div. */
  className?: string;
  /** Inline styles for the container div. */
  style?: React.CSSProperties;
};
```

- [ ] **Step 2: Add parquet loading imports**

At the top of `packages/sift/src/react.tsx`, add:

```tsx
import { getModuleSync, isAvailable } from "./predicate";
import { createWasmTableData } from "./wasm-table-data";
```

- [ ] **Step 3: Add parquet URL effect**

In the `SiftTable` component body, after the existing `url` effect (around line 249), add a new effect for `parquetUrl`:

```tsx
  // Load from Parquet URL via nteract-predicate WASM
  useEffect(() => {
    if (!parquetUrl || !containerRef.current) return;

    let cancelled = false;
    const container = containerRef.current;

    async function loadParquet() {
      setStatus("loading");
      setError(null);

      try {
        const [response, wasmOk] = await Promise.all([
          fetch(parquetUrl!),
          isAvailable(),
        ]);

        if (cancelled) return;
        if (!wasmOk) throw new Error("Failed to load nteract-predicate WASM module");
        if (!response.ok) throw new Error(`Failed to fetch: ${response.status} ${response.statusText}`);

        const parquetBytes = new Uint8Array(await response.arrayBuffer());
        if (cancelled) return;

        const mod = getModuleSync();
        const meta = mod.parquet_metadata(parquetBytes);
        const numRowGroups = meta[0];

        // Load first row group → mount table immediately
        const handle = mod.load_parquet_row_group(parquetBytes, 0, 0);
        const { tableData, columns, prefetchViewport } = createWasmTableData(handle);
        tableData.prefetchViewport = prefetchViewport;

        // Compute initial summaries
        const BIN_COUNT = 25;
        updateSummaries(mod, handle, tableData, columns, BIN_COUNT);

        if (cancelled) return;

        // Clean up previous engine
        if (engineRef.current) {
          engineRef.current.destroy();
          engineRef.current = null;
        }

        const engineDiv = document.createElement("div");
        engineDiv.style.height = "100%";
        container.appendChild(engineDiv);

        engineRef.current = createTable(engineDiv, tableData, {
          onChange: stableOnChange,
        });
        setStatus("ready");

        // Stream remaining row groups progressively
        for (let g = 1; g < numRowGroups; g++) {
          if (cancelled) return;
          await new Promise((r) => setTimeout(r, 0));
          if (cancelled) return;
          mod.load_parquet_row_group(parquetBytes, g, handle);
          tableData.rowCount = mod.num_rows(handle);
          updateSummaries(mod, handle, tableData, columns, BIN_COUNT);
          engineRef.current!.onBatchAppended();
        }

        engineRef.current!.setStreamingDone();
      } catch (err) {
        if (cancelled) return;
        setError(err instanceof Error ? err.message : String(err));
        setStatus("error");
      }
    }

    loadParquet();

    return () => {
      cancelled = true;
      engineRef.current?.destroy();
      engineRef.current = null;
    };
  }, [parquetUrl, typeOverrides, columnOverrides, stableOnChange]);
```

- [ ] **Step 4: Add the `updateSummaries` helper**

Add this function before the `SiftTable` component (after the `buildTableState` function around line 104):

```tsx
function updateSummaries(
  mod: ReturnType<typeof getModuleSync>,
  handle: number,
  tableData: TableData,
  columns: Column[],
  binCount: number,
) {
  const numRows = mod.num_rows(handle);
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
        const [trueCount, falseCount, nullCount] = mod.store_filtered_bool_counts(
          handle,
          c,
          new Uint8Array(0),
        );
        return {
          kind: "boolean" as const,
          trueCount,
          falseCount,
          nullCount,
          total: numRows,
        };
      }
      case "numeric":
      case "timestamp": {
        const bins = mod.store_histogram(handle, c, binCount) as {
          x0: number;
          x1: number;
          count: number;
        }[];
        if (bins.length === 0) return null;
        return {
          kind: col.columnType as "numeric" | "timestamp",
          min: bins[0].x0,
          max: bins[bins.length - 1].x1,
          bins,
        };
      }
      default:
        return null;
    }
  });
}
```

- [ ] **Step 5: Destructure `parquetUrl` in the component**

Update the destructuring at the top of the `SiftTable` function body to include `parquetUrl`:

```tsx
export function SiftTable({
  data,
  url,
  parquetUrl,
  typeOverrides,
  columnOverrides,
  onChange,
  className,
  style,
}: SiftTableProps) {
```

- [ ] **Step 6: Export `parquetUrl` in the public API**

No changes needed — `SiftTableProps` is already exported via the `export type` at the bottom of the file, and `SiftTable` is already a named export.

- [ ] **Step 7: Run sift tests**

Run: `cd packages/sift && pnpm test`

Expected: All existing tests pass (parquetUrl effect is only triggered when prop is provided).

- [ ] **Step 8: Commit**

```bash
git add packages/sift/src/react.tsx
git commit -m "feat(sift): add parquetUrl prop to SiftTable for WASM-based parquet loading"
```

---

### Task 2: Create sift renderer plugin

Create the CJS renderer plugin module that will be loaded on demand inside the isolated iframe when `application/vnd.apache.parquet` outputs appear.

**Files:**
- Create: `src/isolated-renderer/sift-renderer.tsx`

- [ ] **Step 1: Create the plugin file**

Create `src/isolated-renderer/sift-renderer.tsx`:

```tsx
/**
 * Sift Renderer Plugin
 *
 * On-demand renderer plugin for application/vnd.apache.parquet outputs.
 * Loads parquet bytes via nteract-predicate WASM and renders with sift.
 * Loaded into the isolated iframe via the renderer plugin API.
 */

import { SiftTable } from "@nteract/sift";
import "@nteract/sift/style.css";

interface RendererProps {
  data: unknown;
  metadata?: Record<string, unknown>;
  mimeType: string;
}

function SiftRenderer({ data }: RendererProps) {
  const url = String(data);
  return (
    <div style={{ height: "min(600px, 80vh)", width: "100%" }}>
      <SiftTable parquetUrl={url} />
    </div>
  );
}

export function install(ctx: {
  register: (mimeTypes: string[], component: React.ComponentType<RendererProps>) => void;
}) {
  ctx.register(["application/vnd.apache.parquet"], SiftRenderer);
}
```

- [ ] **Step 2: Commit**

```bash
git add src/isolated-renderer/sift-renderer.tsx
git commit -m "feat(sift): add sift iframe renderer plugin for parquet MIME type"
```

---

### Task 3: Wire sift into the Vite plugin build

Add sift as a renderer plugin to `vite-plugin-isolated-renderer.ts` so it gets built as a CJS module and exposed as `virtual:renderer-plugin/sift`.

**Files:**
- Modify: `apps/notebook/vite-plugin-isolated-renderer.ts`

- [ ] **Step 1: Add sift entry path**

After the `leafletEntry` declaration (line 55), add:

```typescript
  const siftEntry = path.resolve(__dirname, "../../src/isolated-renderer/sift-renderer.tsx");
```

- [ ] **Step 2: Add sift state variables**

After the `leafletRendererCss` declaration (line 66), add:

```typescript
  let siftRendererCode = "";
  let siftRendererCss = "";
```

- [ ] **Step 3: Add sift to invalidateCache**

In the `invalidateCache()` function, after `leafletRendererCss = "";` (around line 84), add:

```typescript
    siftRendererCode = "";
    siftRendererCss = "";
```

- [ ] **Step 4: Add nteract-predicate alias to `buildRendererPlugin`**

The sift plugin imports `@nteract/sift` which internally imports `nteract-predicate`. The plugin build needs the alias. Update the `buildRendererPlugin` function's `resolve.alias` (line 107-109) to include it:

```typescript
        alias: {
          "@/": `${srcDir}/`,
          "nteract-predicate": path.resolve(__dirname, "../../crates/nteract-predicate/pkg"),
        },
```

- [ ] **Step 5: Add sift to the parallel plugin build**

Update the `Promise.all` in `buildRenderer()` (lines 272-277) to include sift:

```typescript
    const [markdownPlugin, vegaPlugin, plotlyPlugin, leafletPlugin, siftPlugin] = await Promise.all([
      buildRendererPlugin(markdownEntry, "markdown-renderer", srcDir),
      buildRendererPlugin(vegaEntry, "vega-renderer", srcDir),
      buildRendererPlugin(plotlyEntry, "plotly-renderer", srcDir),
      buildRendererPlugin(leafletEntry, "leaflet-renderer", srcDir),
      buildRendererPlugin(siftEntry, "sift-renderer", srcDir),
    ]);
```

After the leaflet assignments (line 285), add:

```typescript
    siftRendererCode = siftPlugin.code;
    siftRendererCss = siftPlugin.css;
```

- [ ] **Step 6: Add sift virtual module in `load()`**

After the `leaflet` plugin block (around line 351), add:

```typescript
      if (pluginName === "sift") {
        return `
export const code = ${JSON.stringify(siftRendererCode)};
export const css = ${JSON.stringify(siftRendererCss)};
`;
      }
```

- [ ] **Step 7: Add sift to HMR invalidation**

Update the `pluginNames` array in `handleHotUpdate` (line 394) to include sift:

```typescript
        const pluginNames = ["markdown", "vega", "plotly", "leaflet", "sift"];
```

- [ ] **Step 8: Commit**

```bash
git add apps/notebook/vite-plugin-isolated-renderer.ts
git commit -m "feat(sift): add sift to isolated renderer plugin build pipeline"
```

---

### Task 4: Register MIME type → plugin mapping

Add `application/vnd.apache.parquet` to the iframe library mapping so `needsPlugin()` returns true and `injectPluginsForMimes()` loads the sift plugin.

**Files:**
- Modify: `src/components/isolated/iframe-libraries.ts`

- [ ] **Step 1: Add parquet to PLUGIN_MIME_TYPES**

In `src/components/isolated/iframe-libraries.ts`, add to the `PLUGIN_MIME_TYPES` object (after the `application/geo+json` entry, line 39):

```typescript
  "application/vnd.apache.parquet": () => import("virtual:renderer-plugin/sift").then(normalize),
```

- [ ] **Step 2: Commit**

```bash
git add src/components/isolated/iframe-libraries.ts
git commit -m "feat(sift): register parquet MIME type to sift renderer plugin"
```

---

### Task 5: Add parquet to MIME priority lists

Add `application/vnd.apache.parquet` to both priority lists so it wins over `text/html` when both are present in an output bundle.

**Files:**
- Modify: `src/components/outputs/media-router.tsx`
- Modify: `packages/runtimed/src/mime-priority.ts`

- [ ] **Step 1: Add to DEFAULT_PRIORITY in media-router.tsx**

In `src/components/outputs/media-router.tsx`, add `application/vnd.apache.parquet` after the `application/geo+json` entry (line 59) and before the `// HTML, PDF, markdown, and LaTeX` comment:

```typescript
  "application/geo+json",
  "application/vnd.apache.parquet",
  // HTML, PDF, markdown, and LaTeX
```

- [ ] **Step 2: Add to DEFAULT_MIME_PRIORITY in mime-priority.ts**

In `packages/runtimed/src/mime-priority.ts`, add `application/vnd.apache.parquet` after `application/geo+json` (line 23) and before the `// HTML, PDF, markdown, and LaTeX` comment:

```typescript
  "application/geo+json",
  "application/vnd.apache.parquet",
  // HTML, PDF, markdown, and LaTeX
```

- [ ] **Step 3: Commit**

```bash
git add src/components/outputs/media-router.tsx packages/runtimed/src/mime-priority.ts
git commit -m "feat(sift): add parquet to MIME priority above text/html"
```

---

### Task 6: Build verification

Verify the plugin builds correctly and the notebook app compiles.

**Files:** None (verification only)

- [ ] **Step 1: Build nteract-predicate WASM**

Ensure the WASM module is built (needed by the sift plugin build):

```bash
cargo xtask wasm sift
```

Expected: WASM output in `crates/nteract-predicate/pkg/`

- [ ] **Step 2: Build the notebook app**

```bash
cargo xtask build
```

Expected: Build succeeds. The sift renderer plugin is built as part of `vite-plugin-isolated-renderer.ts` during the Vite build.

- [ ] **Step 3: Run lint**

```bash
cargo xtask lint --fix
```

Expected: No formatting issues, or issues are auto-fixed.

- [ ] **Step 4: Commit any lint fixes**

```bash
git add -A && git commit -m "chore: lint fixes" || echo "nothing to commit"
```

---

### Task 7: Integration test with dev server

Verify the sift renderer plugin loads and renders inside the iframe using the dev server.

**Files:** None (manual verification)

- [ ] **Step 1: Start the dev daemon and Vite**

Use supervisor tools:
```
supervisor_restart(target="daemon")
supervisor_start_vite
```

Or manually:
```bash
# Terminal 1
cargo xtask dev-daemon
# Terminal 2
cargo xtask vite
```

- [ ] **Step 2: Create a test notebook with a parquet output**

Use MCP tools to create a notebook, add a cell that produces parquet output, and verify sift renders. The exact kernel integration isn't built yet, but you can test the plugin loading path by manually placing a parquet file in the blob store and creating a synthetic output.

Alternatively, confirm the plugin is included in the build by checking the Vite dev server resolves `virtual:renderer-plugin/sift`:
1. Open the notebook app
2. Check browser console for any plugin build errors
3. Verify `needsPlugin("application/vnd.apache.parquet")` returns true (check in dev tools console)

- [ ] **Step 3: Commit if any fixes were needed**

```bash
git add -A && git commit -m "fix(sift): integration test fixes" || echo "nothing to commit"
```
