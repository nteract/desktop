# Sift

Crossfilter data explorer. Demo: [rgbkrk.github.io/sift](https://rgbkrk.github.io/sift/). Published as `@nteract/sift`.

## Stack

- **Vite** — dev server + build
- **TypeScript** — vanilla TS (React only for header charts + popover)
- **@chenglou/pretext** — DOM-free text measurement & layout
- **parquet-wasm** — loads HuggingFace Parquet files in the browser
- **React** — header summary charts + category popovers
- **Rust/WASM** (`nteract-predicate`) — arrow-rs compute kernels

## Commands

```sh
npm install              # install deps
npm run generate         # 100k rows in 20 batches → public/data.arrow
npm run dev              # start Vite dev server
npm run build            # production build (demo app)
npm run build:lib        # library build → lib/index.js (35KB ESM)
npm test                 # vitest unit tests
npm run test:e2e         # playwright E2E tests

# WASM compute crate (from repo root)
cargo xtask wasm sift
```

## Architecture

### Data flow

1. **Local**: `fetch('data.arrow')` → buffer → WASM `load_ipc` → `createWasmTableData`
2. **HuggingFace**: fetch Parquet → WASM `load_parquet_row_group` → `createWasmTableData`
3. Column types detected by WASM `col_type()`, summaries computed in Rust
4. Virtual scroll viewport: direct WASM cell access via `get_cell_string`/`get_cell_f64`
5. On filter change: WASM crossfilter computes filtered summaries, re-render

### Key files

- `src/main.ts` — Entry point, dataset picker, data loading, type refinement
- `src/table.ts` — The table engine (`createTable` → `TableEngine`). Virtual scroll, sort, filter, crossfilter summaries, keyboard nav, ARIA
- `src/sparkline.tsx` — Header summary charts: histograms, category bars, boolean ratio bars, column profiling (null%/distinct), searchable category popover
- `src/accumulators.ts` — Summary accumulators (numeric, timestamp, categorical, boolean) + type detection/refinement
- `src/react.tsx` — `<SiftTable>` React wrapper for embedding (supports multi-instance)
- `src/notebook.tsx` — Multi-table notebook demo page
- `src/filter-schema.ts` — Predicate types, ExplorerState, compilers to SQL/pandas/English
- `src/predicate.ts` — Lazy-loading TS wrapper for nteract-predicate WASM
- `src/datasets.ts` — HuggingFace dataset catalog
- `src/parquet-loader.ts` — Parquet fetch + conversion via parquet-wasm
- `src/index.ts` — Public API entry point for library consumers
- `src/style.css` — All styles, light/dark theme via CSS custom properties

### WASM crate (`../../crates/nteract-predicate/`)

Rust crate wrapping arrow-rs for compute operations. Built with `wasm-pack`.

- `src/lib.rs` — wasm-bindgen entry points
- `src/summary.rs` — `value_counts`, `histogram` (handles dictionary-encoded columns)
- `src/filter.rs` — `filter_rows` (boolean mask), `string_contains` (substring search)

### Key pretext insight

```ts
const prepared = prepare(cellText, '14px Inter')  // one-time per cell
const { height } = layout(prepared, columnWidth, lineHeight)  // ~0.0002ms, pure arithmetic
```

`layout()` is so fast that recalculating heights for thousands of visible cells on every column drag frame is cheaper than a single DOM reflow. This is the foundation.

### Column types

Detected from Arrow schema with data-driven refinement (string→timestamp, null sentinels).

| Type | Cell rendering | Header summary | Sort |
|------|---------------|----------------|------|
| numeric | plain text | histogram + visible overlay | numeric, nulls last |
| categorical | plain text | top-3 bars + searchable popover | string, nulls last |
| boolean | green/red badge | ratio bar (Yes/No/null %) | boolean, nulls last |
| timestamp | formatted date | date histogram + visible overlay | numeric, nulls last |

### Engine API

```ts
const engine = createTable(container, tableData, {
  onChange: (state) => {
    // state: { sort, filters, filteredCount, totalCount }
    const explorer = engineStateToExplorerState(state)
    // explorer → Automerge, SQL, pandas, English
  }
})

engine.getSort()          // { column, direction } | null
engine.setSort('name', 'asc')
engine.getFilters()       // { column, filter }[]
engine.getState()         // full snapshot
engine.setFilter(colIndex, { kind: 'range', min: 10, max: 50 })
engine.clearAllFilters()
```

### Naming

- **Sift** — this demo/site
- **`@nteract/sift`** — the npm package
- **`nteract-predicate`** — the WASM compute crate
