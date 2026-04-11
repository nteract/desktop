# Sift

Crossfilter data explorer. [Pretext](https://github.com/chenglou/pretext) × [Arrow](https://arrow.apache.org/).

![Sift — crossfilter in action](https://img.runt.run/sift/crossfilter-demo.gif)

[`@nteract/sift`](https://github.com/nteract) on npm. Try it at [rgbkrk.github.io/sift](https://rgbkrk.github.io/sift/).

## Features

- **Stream Arrow IPC or load Parquet directly** — WASM-native, no server needed
- **Crossfilter** — brush histograms to filter, all other summaries update instantly
- **Column profiling** — null%, distinct count shown inline in headers
- **Smart summaries** — histograms, category bars, boolean ratios, binary value bars, searchable popovers. Adapts visualization to column type × cardinality.
- **Column type casting** — right-click to treat text as numbers, dates, booleans — with full undo
- **Smart column detection** — pandas index columns, HuggingFace ClassLabel features
- **Virtual scroll** — 100k+ rows at 120fps. Pretext layout at 0.0002ms/cell.
- **Odometer status bar** — rolling digit animation for row count, visible range, FPS
- **Multi-instance** — embed multiple tables on one page (notebook demo included)
- **RxJS streaming** — cancellable dataset loading via `switchMap`
- **Mobile** — tap any row for a detail sheet with all column values
- **Dark mode**, keyboard nav, pinned columns, ARIA grid
- **8 HuggingFace datasets** built into the demo

## Quick start

```sh
npm install
npm run dev
```

## Use as a library

```tsx
import { SiftTable } from '@nteract/sift'

<SiftTable url="/data.arrow" onChange={console.log} />
```

Load Parquet directly — decoded entirely in WASM:

```tsx
<SiftTable url="https://huggingface.co/datasets/spotify/resolve/main/data.parquet" />
```

## Engine API

```ts
engine.setSort('age', 'desc')
engine.setFilter(colIndex, { kind: 'range', min: 18, max: 65 })
engine.clearAllFilters()

// State → SQL/pandas
const state = engine.getState()
engineStateToExplorerState(state).filters.map(predicateToSQL)
```

See [`docs/embedding-guide.md`](docs/embedding-guide.md) for the full API.

## WASM compute (`nteract-predicate`)

Rust crate wrapping arrow-rs. Parquet decoding, sort, histograms, value counts, string search, column casting — all in one WASM binary. Data lives in WASM memory; JS holds handles.

```sh
cd crates/nteract-predicate
wasm-pack build --target web --release
```

## Stack

| Layer | What |
|-------|------|
| Layout | [@chenglou/pretext](https://github.com/chenglou/pretext) — DOM-free text measurement |
| Data | [Apache Arrow](https://arrow.apache.org/) — columnar, streamed via RecordBatchReader |
| Compute | [nteract-predicate](crates/nteract-predicate/) — arrow-rs WASM kernels |
| Reactivity | [RxJS](https://rxjs.dev/) — streaming pipeline |
| Charts | SVG histograms + HTML bars |

## Development

```sh
npm install              # deps
npm run dev              # vite dev server
npm test                 # vitest unit tests
npm run test:e2e         # playwright E2E
npm run build            # production build (demo)
npm run build:lib        # library build → lib/index.js
```

## Demo pages

- `/` — main demo with dataset picker
- `/notebook.html` — multi-table notebook simulation
