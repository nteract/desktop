# Embedding Sift

Use `@nteract/sift` to explore columnar data in your app. WASM-powered, Arrow-native, sub-millisecond layout via [pretext](https://github.com/chenglou/pretext).

## Install

```sh
npm install @nteract/sift apache-arrow
```

---

## React

```tsx
import { SiftTable } from '@nteract/sift'

function App() {
  return (
    <div style={{ height: 600 }}>
      <SiftTable
        url="/data.arrow"
        onChange={(state) => {
          // state: { sort, filters, filteredCount, totalCount }
        }}
      />
    </div>
  )
}
```

Streams Arrow IPC from the URL. First batch mounts the table, rest appended progressively. All compute runs in WASM via `nteract-predicate`.

### Parquet

```tsx
<SiftTable url="https://huggingface.co/datasets/spotify/resolve/main/data.parquet" />
```

Parquet files are loaded and decoded entirely in WASM — no server-side conversion needed.

### Overrides

```tsx
<SiftTable
  url="/events.arrow"
  typeOverrides={{ created_at: 'timestamp' }}
  columnOverrides={{ id: { width: 80, sortable: false } }}
/>
```

---

## Vanilla JS

```ts
import { createTable, type TableData } from '@nteract/sift'

const container = document.getElementById('table')!
const engine = createTable(container, tableData)

// Later
engine.destroy()
```

---

## Engine API

```ts
// Sort
engine.setSort('age', 'desc')
engine.getSort()  // { column: 'age', direction: 'desc' }

// Filter
engine.setFilter(colIndex, { kind: 'range', min: 18, max: 65 })
engine.setFilter(colIndex, { kind: 'set', values: new Set(['CA', 'NY']) })
engine.setFilter(colIndex, { kind: 'boolean', value: true })
engine.clearAllFilters()

// State → SQL/pandas
const state = engine.getState()
import { engineStateToExplorerState, predicateToSQL, predicateToPandas } from '@nteract/sift'
const explorer = engineStateToExplorerState(state)
explorer.filters.map(predicateToSQL)    // ["age BETWEEN 18 AND 65"]
explorer.filters.map(predicateToPandas) // ["df[(df['age'] >= 18) & (df['age'] <= 65)]"]

// Streaming
engine.onBatchAppended()   // after new data arrives
engine.setStreamingDone()  // all batches loaded
```

---

## WASM Compute

All compute runs through `nteract-predicate` — an arrow-rs WASM crate that handles:

- **Parquet decoding** — load HuggingFace datasets directly
- **Sort** — returns sorted indices via arrow-rs kernels
- **Histograms** — binned aggregation for numeric/timestamp columns
- **Value counts** — frequency tables for categorical columns
- **String search** — substring matching for filter popovers
- **Viewport access** — returns Arrow IPC for visible rows (no per-cell FFI)
- **Column casting** — change types in-place with undo support

Data lives in WASM memory. The JS side holds handles, not copies.

### Include the WASM artifacts

```sh
cd crates/nteract-predicate && wasm-pack build --target web --release
```

The Vite plugin copies the WASM pkg to `public/wasm/` automatically at build time.

---

## Theming

CSS custom properties, scoped to `:root`:

| Variable | Light | Dark |
|----------|-------|------|
| `--page` | `#f5f2ec` | `#1a1816` |
| `--panel` | `#fffdf9` | `#242120` |
| `--ink` | `#1e1a18` | `#e8e2dc` |
| `--muted` | `#6e655f` | `#9a918a` |
| `--rule` | `#d8cec3` | `#3a3533` |
| `--accent` | `#955f3b` | `#d4896a` |

Dark mode via `@media (prefers-color-scheme: dark)` or `[data-theme="dark"]`.

```css
.my-app {
  --page: var(--my-app-bg);
  --ink: var(--my-app-text);
  --accent: var(--my-app-primary);
}
```

The container must have a defined height. The table fills its parent.

---

## Library Build

```sh
npm run build:lib   # → lib/index.js (ESM)
```

External dependencies (react, apache-arrow, pretext) are not bundled.
