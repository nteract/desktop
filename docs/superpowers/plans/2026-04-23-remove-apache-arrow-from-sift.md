# Remove apache-arrow from Sift JS Bundle

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Eliminate the `apache-arrow` JS dependency from `@nteract/sift`, routing all Arrow IPC and Parquet data through the existing sift-wasm (nteract-predicate) WASM module instead. This shrinks the sift renderer plugin from ~6.3 MB to ~2 MB.

**Architecture:** WASM already owns the data store and exposes per-cell accessors (`get_cell_string`, `get_cell_f64`, `is_null`), column metadata (`col_names`, `col_type`), and IPC/Parquet loading (`load_ipc`, `load_parquet_row_group`). The JS side currently uses apache-arrow in three places: (1) viewport cache deserialization via `tableFromIPC()`, (2) JS fallback loading via `RecordBatchReader`, and (3) column type detection via the `Type` enum. All three can be replaced by existing WASM APIs. WASM is always required — no JS fallback path.

**Tech Stack:** TypeScript, Rust/wasm-bindgen (sift-wasm crate), Vite

---

### Task 1: Replace `tableFromIPC` in viewport cache with direct WASM cell access

The hot path: `wasm-table-data.ts` calls `mod.get_viewport_by_indices()` which returns Arrow IPC bytes, then parses them with `tableFromIPC()` just to call `.get(row)` on each cell. WASM already has `get_cell_string`/`get_cell_f64`/`is_null` per-cell accessors that bypass IPC entirely.

**Files:**
- Modify: `packages/sift/src/wasm-table-data.ts`

- [ ] **Step 1: Remove the `tableFromIPC` import and rewrite `prefetchViewport`**

Replace the IPC round-trip with direct per-cell WASM calls. The WASM module already exposes everything needed.

In `packages/sift/src/wasm-table-data.ts`, remove line 8:
```ts
import { tableFromIPC } from "apache-arrow";
```

Then replace the `prefetchViewport` function (lines 69-115) with:

```ts
  function prefetchViewport(dataRowIndices: number[]) {
    if (dataRowIndices.length === 0) return;

    const uncached = dataRowIndices.filter((r) => !cache.has(r));
    if (uncached.length === 0) return;

    for (const dataRow of uncached) {
      const strings: string[] = [];
      const raws: unknown[] = [];

      for (let c = 0; c < numCols; c++) {
        if (mod.is_null(handle, dataRow, c)) {
          strings.push("");
          raws.push(null);
          continue;
        }

        const colType = columns[c].columnType;

        if (colType === "boolean") {
          const s = mod.get_cell_string(handle, dataRow, c);
          const boolVal = s === "true" || s === "Yes";
          strings.push(boolVal ? "Yes" : "No");
          raws.push(boolVal);
        } else if (colType === "timestamp") {
          const numVal = mod.get_cell_f64(handle, dataRow, c);
          strings.push(formatCell("timestamp", numVal));
          raws.push(numVal);
        } else if (colType === "numeric") {
          const numVal = mod.get_cell_f64(handle, dataRow, c);
          strings.push(stringifyValue(numVal));
          raws.push(numVal);
        } else {
          const s = mod.get_cell_string(handle, dataRow, c);
          strings.push(s);
          raws.push(s);
        }
      }

      cache.set(dataRow, { strings, raws });
    }
  }
```

**Performance note:** This trades one bulk WASM→JS IPC transfer + JS-side Arrow parse for N×C individual WASM FFI calls. For typical viewports (~50 rows × ~10 cols = 500 calls), this is faster because it eliminates the IPC serialization/deserialization overhead. If profiling shows regression for very wide tables, a future optimization would be a WASM function returning a flat JSON array for a viewport batch — but that's out of scope here.

- [ ] **Step 2: Verify the unit tests still pass**

Run: `cd packages/sift && npx vp test run`

The unit tests in `accumulators.test.ts`, `react.test.tsx`, `table.test.ts` etc. should still pass — none of them exercise `prefetchViewport` directly (it's an integration concern tested via E2E).

- [ ] **Step 3: Commit**

```bash
git add packages/sift/src/wasm-table-data.ts
git commit -m "refactor(sift): replace tableFromIPC viewport cache with direct WASM cell access

Eliminates the apache-arrow tableFromIPC() call in the viewport hot path.
Instead of WASM→IPC bytes→JS Arrow parse→cell read, cells are now read
directly via get_cell_string/get_cell_f64/is_null WASM FFI calls."
```

---

### Task 2: Remove JS fallback path from `main.ts`

The `loadLocalArrowJs$` function and `buildTableState` at the bottom of `main.ts` are the JS fallback path using `RecordBatchReader` from apache-arrow. Since WASM is always available, the `loadLocalArrow$` function's WASM-unavailable branch is dead code.

**Files:**
- Modify: `packages/sift/src/main.ts`

- [ ] **Step 1: Remove the apache-arrow imports**

In `packages/sift/src/main.ts`, remove lines 1-2:
```ts
import type { RecordBatch } from "apache-arrow";
import { RecordBatchReader } from "apache-arrow";
```

- [ ] **Step 2: Remove the `detectColumnType` import from accumulators**

On line 7, remove `detectColumnType` from the import (it's only used by the JS fallback `buildTableState`). The remaining imports from accumulators are still needed: `formatCell` is used indirectly via `wasm-table-data.ts`, and the accumulator classes are used by `buildTableState` — but we're deleting that too.

After this step, line 6-12 should become:
```ts
import { formatCell, stringifyValue } from "./accumulators";
```

Wait — check if `formatCell` and `stringifyValue` are still used in `main.ts` after removing the fallback. They are NOT directly used in `main.ts` after the fallback is removed (they're only called from `wasm-table-data.ts`). The accumulator imports are all for the JS fallback. Remove the entire accumulators import line.

Actually, re-check: `main.ts` line 6-12 imports:
```ts
import {
  BooleanAccumulator,
  CategoricalAccumulator,
  detectColumnType,
  formatCell,
  NumericAccumulator,
  type SummaryAccumulator,
  TimestampAccumulator,
} from "./accumulators";
```

All of these are used by `buildTableState` (line 649) and `loadLocalArrowJs$` (line 299). After deleting both, none of these are used in `main.ts`. Remove the entire import block.

- [ ] **Step 3: Simplify `loadLocalArrow$` to always use WASM**

Remove the `isAvailable()` check and the WASM-unavailable fallback branch. The function currently:
1. Fetches the arrow file
2. Checks WASM availability in parallel
3. If WASM unavailable, falls back to `loadLocalArrowJs$`
4. If WASM available, loads via `loadIpc`

Simplify to just steps 1 and 4 (always WASM). Replace `loadLocalArrow$` (lines 234-294) with:

```ts
function loadLocalArrow$(dataset: DatasetEntry, tableRoot: HTMLElement): Observable<void> {
  return defer(
    () =>
      new Observable<void>((subscriber) => {
        let cancelled = false;

        (async () => {
          const response = await fetch(`${import.meta.env.BASE_URL}${dataset.path}`);
          if (!response.ok) {
            tableRoot.innerHTML =
              '<div class="sift-loading">Missing data.arrow — run <code>npm run generate</code> first.</div>';
            subscriber.complete();
            return;
          }

          renderLoadingSkeleton(tableRoot, "Loading data…");

          const arrowBytes = new Uint8Array(await response.arrayBuffer());

          if (cancelled) return;

          renderLoadingSkeleton(tableRoot, "Loading into WASM…");
          const handle = await loadIpc(arrowBytes);

          const { tableData, columns, prefetchViewport } = createWasmTableData(
            handle,
            generatedColumnOverrides,
          );
          tableData.prefetchViewport = prefetchViewport;
          const mod = getModuleSync();
          tableData.recomputeSummaries = () => updateWasmSummaries(mod, handle, tableData, columns);

          updateWasmSummaries(mod, handle, tableData, columns);

          if (cancelled) return;
          tableRoot.innerHTML = "";
          currentEngine = createTable(tableRoot, tableData);
          currentEngine.setStreamingDone();
          subscriber.next();
          subscriber.complete();
        })().catch((err) => {
          if (!cancelled) subscriber.error(err);
        });

        return () => {
          cancelled = true;
        };
      }),
  );
}
```

- [ ] **Step 4: Delete `loadLocalArrowJs$` function (lines 299-356)**

Delete the entire function — it's no longer called.

- [ ] **Step 5: Delete `buildTableState` function (lines 648-695)**

Delete the entire function — it was only used by `loadLocalArrowJs$`.

- [ ] **Step 6: Remove the `isAvailable` import**

On line 16, remove `isAvailable` from the predicate import:
```ts
import { getModuleSync, loadIpc } from "./predicate";
```

(Remove `isAvailable` — no longer needed in this file.)

- [ ] **Step 7: Verify tests pass**

Run: `cd packages/sift && npx vp test run`

- [ ] **Step 8: Commit**

```bash
git add packages/sift/src/main.ts
git commit -m "refactor(sift): remove JS Arrow fallback from main.ts, always use WASM

Deletes loadLocalArrowJs$, buildTableState, and all apache-arrow imports
from main.ts. The local Arrow loading path now always routes through
WASM load_ipc, matching the HuggingFace/Parquet path."
```

---

### Task 3: Remove JS fallback path from `react.tsx`

Same pattern as `main.ts`: the `loadArrowIpc` function in the React `SiftTable` component uses `RecordBatchReader` for Arrow IPC streaming. Replace it with the WASM path (buffer → `load_ipc` → `createWasmTableData`).

**Files:**
- Modify: `packages/sift/src/react.tsx`

- [ ] **Step 1: Remove apache-arrow imports**

Remove lines 16-17:
```ts
import type { RecordBatch, Schema } from "apache-arrow";
import { RecordBatchReader } from "apache-arrow";
```

- [ ] **Step 2: Remove accumulator imports used only by JS fallback**

Lines 20-27 import accumulator classes and `detectColumnType` — all only used by the JS fallback `buildTableState` and `loadArrowIpc`. Remove them:

```ts
import {
  BooleanAccumulator,
  CategoricalAccumulator,
  detectColumnType,
  formatCell,
  NumericAccumulator,
  type SummaryAccumulator,
  TimestampAccumulator,
} from "./accumulators";
```

Replace with nothing — none of these are used after removing the fallback. (The `ensureModule` and `getModuleSync` imports on line 28 stay.)

- [ ] **Step 3: Add `loadIpc` to the predicate import**

Line 28 currently has:
```ts
import { ensureModule, getModuleSync } from "./predicate";
```

Change to:
```ts
import { ensureModule, getModuleSync, loadIpc } from "./predicate";
```

(`loadIpc` is the async wrapper around `mod.load_ipc`.)

- [ ] **Step 4: Replace `loadArrowIpc` with WASM path**

Replace the `loadArrowIpc` function (lines 400-449) with:

```ts
    async function loadArrowIpc(source: Response | ReadableStream<Uint8Array>) {
      await ensureModule();
      if (cancelled) return;

      const bytes =
        source instanceof Response
          ? new Uint8Array(await source.arrayBuffer())
          : await streamToBytes(source);
      if (cancelled) return;

      const handle = await loadIpc(bytes);
      wasmHandle = handle;

      const mod = getModuleSync();
      const { tableData, columns, prefetchViewport } = createWasmTableData(handle, columnOverrides);
      tableData.prefetchViewport = prefetchViewport;
      tableData.recomputeSummaries = () => updateWasmSummaries(mod, handle, tableData, columns);
      updateWasmSummaries(mod, handle, tableData, columns);

      if (cancelled) return;
      mountEngine(tableData);
      setStatus("ready");
      engineRef.current?.setStreamingDone();
    }
```

Add a `streamToBytes` helper above `loadArrowIpc`:

```ts
    async function streamToBytes(stream: ReadableStream<Uint8Array>): Promise<Uint8Array> {
      const reader = stream.getReader();
      const chunks: Uint8Array[] = [];
      while (true) {
        const { value, done } = await reader.read();
        if (done) break;
        chunks.push(value);
      }
      const totalLength = chunks.reduce((sum, c) => sum + c.length, 0);
      const result = new Uint8Array(totalLength);
      let offset = 0;
      for (const chunk of chunks) {
        result.set(chunk, offset);
        offset += chunk.length;
      }
      return result;
    }
```

- [ ] **Step 5: Delete `buildTableState` helper (lines 235-280)**

Delete the entire function — it was only used by the JS fallback `loadArrowIpc`.

- [ ] **Step 6: Remove `autoWidth` import**

Line 59:
```ts
import { autoWidth } from "./auto-width";
```

This was only used by `buildTableState`. Remove it.

- [ ] **Step 7: Verify tests pass**

Run: `cd packages/sift && npx vp test run`

- [ ] **Step 8: Commit**

```bash
git add packages/sift/src/react.tsx
git commit -m "refactor(sift): remove JS Arrow fallback from SiftTable, always use WASM

The SiftTable React component now buffers Arrow IPC data and loads it
through WASM load_ipc + createWasmTableData, matching the Parquet path.
Removes RecordBatchReader, buildTableState, and all accumulator imports."
```

---

### Task 4: Remove `detectColumnType` from accumulators and public API

`detectColumnType` uses `apache-arrow`'s `Field` type and `Type` enum. It's now dead code — the WASM `col_type()` function handles column type detection in Rust. Remove it from `accumulators.ts` and the public `index.ts` re-export.

**Files:**
- Modify: `packages/sift/src/accumulators.ts`
- Modify: `packages/sift/src/index.ts`

- [ ] **Step 1: Remove apache-arrow imports from accumulators.ts**

Remove lines 1-2:
```ts
import type { Field } from "apache-arrow";
import { Type } from "apache-arrow";
```

- [ ] **Step 2: Delete `detectColumnType` function (lines 19-38)**

Delete the entire function.

- [ ] **Step 3: Remove `detectColumnType` from index.ts re-exports**

In `packages/sift/src/index.ts`, line 21-28, remove `detectColumnType` from the named exports:

```ts
export {
  BooleanAccumulator,
  CategoricalAccumulator,
  formatCell,
  isNullSentinel,
  NumericAccumulator,
  refineColumnType,
  stringifyValue,
  TimestampAccumulator,
} from "./accumulators";
```

- [ ] **Step 4: Verify tests pass**

Run: `cd packages/sift && npx vp test run`

The `accumulators.test.ts` tests don't test `detectColumnType` (it required Arrow Field objects which are hard to construct in tests). All existing tests should pass.

- [ ] **Step 5: Commit**

```bash
git add packages/sift/src/accumulators.ts packages/sift/src/index.ts
git commit -m "refactor(sift): remove detectColumnType and apache-arrow from accumulators

Column type detection now happens entirely in WASM via col_type().
Removes the last apache-arrow imports from the sift library code."
```

---

### Task 5: Remove `apache-arrow` from package.json and verify clean build

**Files:**
- Modify: `packages/sift/package.json`
- Modify: `packages/sift/CLAUDE.md`

- [ ] **Step 1: Remove `apache-arrow` from peerDependencies**

In `packages/sift/package.json`, remove line 39:
```json
    "apache-arrow": ">=15",
```

- [ ] **Step 2: Update `packages/sift/CLAUDE.md` stack section**

Remove `apache-arrow` from the Stack section and update the Architecture section to reflect WASM-only data flow. In the Stack list, remove:
```
- **apache-arrow** — columnar data, streamed via `RecordBatchReader`
```

Replace the "Data flow" section under Architecture with:
```
1. **Local**: `fetch('data.arrow')` → buffer → WASM `load_ipc` → `createWasmTableData`
2. **HuggingFace**: fetch Parquet → WASM `load_parquet_row_group` → `createWasmTableData`
3. Column types detected by WASM `col_type()`, summaries computed in Rust
4. Virtual scroll viewport: direct WASM cell access via `get_cell_string`/`get_cell_f64`
5. On filter change: WASM crossfilter computes filtered summaries, re-render
```

- [ ] **Step 3: Verify full build**

Run: `cd packages/sift && npx vp test run`

Then verify the renderer plugin builds clean (this is the actual bundle that ships):

Run: `cd /Users/kyle/code/src/github.com/nteract/desktop && cargo xtask wasm sift`

(Only needed if the WASM crate changed — it didn't, but verify it still links.)

- [ ] **Step 4: Verify TypeScript compiles clean**

Run: `cd packages/sift && npx tsc --noEmit`

- [ ] **Step 5: Commit**

```bash
git add packages/sift/package.json packages/sift/CLAUDE.md
git commit -m "chore(sift): remove apache-arrow peer dependency

apache-arrow is no longer used by @nteract/sift. All Arrow IPC and
Parquet data is handled by the sift-wasm (nteract-predicate) WASM module."
```

---

### Task 6: Rebuild renderer plugins and verify bundle size reduction

**Files:**
- Modify: `apps/notebook/src/renderer-plugins/sift.js` (rebuilt artifact)
- Modify: `apps/notebook/src/renderer-plugins/sift.css` (rebuilt artifact)

- [ ] **Step 1: Rebuild all renderer plugins**

Run: `cargo xtask renderer-plugins`

This rebuilds `sift.js` (and all other plugins) via the `buildRendererPlugin` pipeline in `src/build/renderer-plugin-builder.ts`.

- [ ] **Step 2: Verify bundle size reduction**

Run: `ls -lh apps/notebook/src/renderer-plugins/sift.js`

Expected: ~2 MB or less (down from 6.3 MB). The bulk of the savings is from removing apache-arrow (~4+ MB after minification).

- [ ] **Step 3: Run lint**

Run: `cargo xtask lint --fix`

- [ ] **Step 4: Commit the rebuilt artifacts**

```bash
git add apps/notebook/src/renderer-plugins/sift.js apps/notebook/src/renderer-plugins/sift.css
git commit -m "chore(sift): rebuild renderer plugin after apache-arrow removal

sift.js bundle reduced from ~6.3 MB to ~X MB (exact size TBD after build)."
```

---

### Task 7: Clean up `generate-data.ts` (keep apache-arrow as dev-only)

`generate-data.ts` imports `tableFromArrays` and `tableToIPC` from apache-arrow to generate test fixtures. This file is dev-only (`npm run generate`) and is NOT bundled into the renderer plugin. It's fine to keep apache-arrow as a `devDependency` for this script.

**Files:**
- Modify: `packages/sift/package.json`

- [ ] **Step 1: Add apache-arrow to devDependencies**

In `packages/sift/package.json`, add to the `devDependencies` block:
```json
    "apache-arrow": "^18.0.0",
```

(Use whatever version is currently installed in the pnpm lockfile.)

- [ ] **Step 2: Run pnpm install**

Run: `pnpm install`

- [ ] **Step 3: Verify generate-data still works**

Run: `cd packages/sift && npm run generate`

Expected: `public/data.arrow` is regenerated without errors.

- [ ] **Step 4: Commit**

```bash
git add packages/sift/package.json pnpm-lock.yaml
git commit -m "chore(sift): move apache-arrow to devDependencies for generate-data script"
```
