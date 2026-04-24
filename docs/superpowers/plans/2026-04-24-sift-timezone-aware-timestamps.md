# Timezone-Aware Timestamp Display in Sift

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make sift respect the timezone field from Arrow timestamp columns, defaulting to UTC when no timezone is present, and thread that information through to both cell display and sparkline headers consistently.

**Architecture:** Store per-column timezone strings in the WASM `DataStore`, expose them via a new `col_timezone()` export, use `chrono-tz` in Rust to format timestamps in the correct timezone, and pass timezone info to the JS frontend so sparkline headers match cell values. Columns with no timezone get a `"UTC"` annotation in the UI.

**Tech Stack:** Rust (chrono, chrono-tz, arrow-rs, wasm-bindgen), TypeScript (sparkline.tsx, wasm-table-data.ts)

---

## File structure

| File | Responsibility |
|------|---------------|
| `crates/sift-wasm/Cargo.toml` | Add `chrono-tz` dependency |
| `crates/sift-wasm/src/store.rs` | Store per-column timezones, expose `col_timezone()`, timezone-aware `format_timestamp_ms` |
| `packages/sift/src/predicate.ts` | Add `col_timezone` to `PredicateModule` type |
| `packages/sift/src/table.ts` | Add optional `timezone` field to `Column` and `TimestampColumnSummary` types |
| `packages/sift/src/wasm-table-data.ts` | Query `col_timezone()` at init, populate `Column.timezone` |
| `packages/sift/src/sparkline.tsx` | Thread timezone into `formatDateRange`, use `timeZone` option in `toLocaleString`, show UTC default indicator |

---

### Task 1: Add `chrono-tz` dependency and store per-column timezones in DataStore

**Files:**
- Modify: `crates/sift-wasm/Cargo.toml`
- Modify: `crates/sift-wasm/src/store.rs:22-33` (DataStore struct)
- Modify: `crates/sift-wasm/src/store.rs:93-110` (store initialization)

- [ ] **Step 1: Add `chrono-tz` to Cargo.toml**

In `crates/sift-wasm/Cargo.toml`, add after the `chrono` line:

```toml
chrono-tz = "0.10"
```

- [ ] **Step 2: Add `col_timezones` field to `DataStore`**

In `crates/sift-wasm/src/store.rs`, add a new field to the `DataStore` struct (after `col_types`):

```rust
struct DataStore {
    batches: Vec<RecordBatch>,
    batch_offsets: Vec<usize>,
    total_rows: usize,
    num_cols: usize,
    col_names: Vec<String>,
    col_types: Vec<String>,
    col_timezones: Vec<Option<String>>,  // IANA timezone or None
    original_columns: HashMap<usize, (Vec<arrow::array::ArrayRef>, String)>,
}
```

- [ ] **Step 3: Extract timezone from Arrow schema during store initialization**

Add a helper to `DataStore`:

```rust
fn extract_timezone(dt: &DataType) -> Option<String> {
    match dt {
        DataType::Timestamp(_, Some(tz)) => {
            let tz_str = tz.as_ref();
            if tz_str.is_empty() { None } else { Some(tz_str.to_string()) }
        }
        _ => None,
    }
}
```

In the store initialization code (the `load_ipc`, `load_parquet`, `load_parquet_row_group` functions), wherever `col_types` is built from the schema, also build `col_timezones`:

```rust
let col_timezones: Vec<Option<String>> = schema
    .fields()
    .iter()
    .map(|f| DataStore::extract_timezone(f.data_type()))
    .collect();
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p sift-wasm`
Expected: clean compile, no warnings

- [ ] **Step 5: Commit**

```bash
git add crates/sift-wasm/Cargo.toml crates/sift-wasm/src/store.rs
git commit -m "feat(sift-wasm): store per-column timezone from Arrow schema"
```

---

### Task 2: Expose `col_timezone()` WASM export

**Files:**
- Modify: `crates/sift-wasm/src/store.rs:207-213` (near `col_type`)

- [ ] **Step 1: Add `col_timezone` export**

Add this function right after `col_type()`:

```rust
#[wasm_bindgen]
pub fn col_timezone(handle: u32, col: usize) -> Result<Option<String>, JsValue> {
    with_store(handle, |s| s.col_timezones.get(col).cloned().flatten())
        .map_err(|e| JsValue::from_str(&e))
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p sift-wasm`
Expected: clean compile

- [ ] **Step 3: Commit**

```bash
git add crates/sift-wasm/src/store.rs
git commit -m "feat(sift-wasm): add col_timezone() WASM export"
```

---

### Task 3: Make `format_timestamp_ms` timezone-aware

**Files:**
- Modify: `crates/sift-wasm/src/store.rs:258-266` (format_timestamp_ms)
- Modify: `crates/sift-wasm/src/store.rs:268-290` (get_cell_string)

- [ ] **Step 1: Update `format_timestamp_ms` to accept an optional timezone**

```rust
use chrono_tz::Tz;

fn format_timestamp_ms(ms: i64, tz: Option<&str>) -> String {
    let secs = ms / 1000;
    let nanos = ((ms % 1000) * 1_000_000) as u32;
    let Some(utc_dt) = DateTime::from_timestamp(secs, nanos) else {
        return ms.to_string();
    };
    match tz.and_then(|s| s.parse::<Tz>().ok()) {
        Some(tz) => utc_dt.with_timezone(&tz).format("%b %-d, %Y").to_string(),
        None => utc_dt.format("%b %-d, %Y").to_string(),
    }
}
```

- [ ] **Step 2: Thread timezone through `get_cell_string`**

In `get_cell_string`, the timestamp path currently calls `format_timestamp_ms(ms)`. Change it to pass the column timezone:

```rust
// Timestamps -> human-readable date
if let Some(ms) = timestamp_cell_ms(column.as_ref(), local_row) {
    let tz = s.col_timezones.get(col).and_then(|t| t.as_deref());
    return format_timestamp_ms(ms, tz);
}
```

This requires access to `col` (the column index) and `s` (the store) inside the closure. `get_cell_string` already has both via the `with_store` closure and the `col` parameter.

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p sift-wasm`
Expected: clean compile

- [ ] **Step 4: Commit**

```bash
git add crates/sift-wasm/src/store.rs
git commit -m "feat(sift-wasm): timezone-aware timestamp formatting in get_cell_string"
```

---

### Task 4: Add `timezone` to JS types and wire up `col_timezone` in frontend

**Files:**
- Modify: `packages/sift/src/predicate.ts:15-73` (PredicateModule type)
- Modify: `packages/sift/src/table.ts:17-24` (Column type)
- Modify: `packages/sift/src/table.ts:61-68` (TimestampColumnSummary type)
- Modify: `packages/sift/src/wasm-table-data.ts:39-62` (createWasmTableData)

- [ ] **Step 1: Add `col_timezone` to `PredicateModule`**

In `packages/sift/src/predicate.ts`, add after line 29 (`col_type`):

```ts
  col_timezone(handle: number, col: number): string | null;
```

- [ ] **Step 2: Add `timezone` to `Column` type**

In `packages/sift/src/table.ts`, add to the `Column` type:

```ts
export type Column = {
  key: string;
  label: string;
  width: number;
  sortable: boolean;
  numeric: boolean;
  columnType: ColumnType;
  timezone?: string | null;
};
```

- [ ] **Step 3: Add `timezone` to `TimestampColumnSummary`**

In `packages/sift/src/table.ts`:

```ts
export type TimestampColumnSummary = {
  kind: "timestamp";
  min: number;
  max: number;
  bins: { x0: number; x1: number; count: number }[];
  nullCount?: number;
  timezone?: string | null;
};
```

- [ ] **Step 4: Query `col_timezone` in `createWasmTableData`**

In `packages/sift/src/wasm-table-data.ts`, inside the column init loop:

```ts
  for (let c = 0; c < numCols; c++) {
    const wasmType = mod.col_type(handle, c);
    const colType = mapColType(wasmType);
    const timezone = colType === "timestamp" ? mod.col_timezone(handle, c) : null;
    const overrides = columnOverrides?.[names[c]];
    columns.push({
      key: names[c],
      label: overrides?.label ?? names[c],
      width: overrides?.width ?? autoWidth(names[c], colType),
      sortable: overrides?.sortable ?? true,
      numeric: colType === "numeric",
      columnType: colType,
      timezone,
    });
  }
```

- [ ] **Step 5: Verify tests pass**

Run: `cd packages/sift && npx vp test run`
Expected: 137 tests pass (types are optional, no test breakage)

- [ ] **Step 6: Commit**

```bash
git add packages/sift/src/predicate.ts packages/sift/src/table.ts packages/sift/src/wasm-table-data.ts
git commit -m "feat(sift): wire timezone from WASM to Column and TimestampColumnSummary types"
```

---

### Task 5: Make sparkline `formatDateRange` timezone-aware

**Files:**
- Modify: `packages/sift/src/sparkline.tsx:976-1038` (formatDateRange)
- Modify: `packages/sift/src/sparkline.tsx:1040-1053` (TimestampHistogram)
- Modify: `packages/sift/src/sparkline.tsx:1164-1173` (renderColumnSummary timestamp case)

- [ ] **Step 1: Add `timezone` parameter to `formatDateRange`**

Replace the current `formatDateRange` (lines 976-1038) with:

```tsx
function formatDateRange(
  minMs: number,
  maxMs: number,
  timezone?: string | null,
): [string, string] {
  const tz = timezone ?? "UTC";
  const min = new Date(minMs);
  const max = new Date(maxMs);
  const spanDays = (maxMs - minMs) / (1000 * 60 * 60 * 24);

  if (maxMs === minMs) {
    const fmt = (d: Date) =>
      d.toLocaleString(undefined, {
        year: "numeric",
        month: "short",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
        timeZone: tz,
        timeZoneName: "short",
      });
    return [fmt(min), fmt(max)];
  }

  if (spanDays < 1) {
    const fmt = (d: Date) =>
      d.toLocaleTimeString(undefined, {
        hour: "2-digit",
        minute: "2-digit",
        timeZone: tz,
      });
    return [fmt(min), fmt(max)];
  }
  if (spanDays > 730) {
    return [
      min.toLocaleDateString(undefined, { year: "numeric", timeZone: tz }),
      max.toLocaleDateString(undefined, { year: "numeric", timeZone: tz }),
    ];
  }
  if (spanDays > 60) {
    return [
      min.toLocaleDateString(undefined, { year: "numeric", month: "short", timeZone: tz }),
      max.toLocaleDateString(undefined, { year: "numeric", month: "short", timeZone: tz }),
    ];
  }
  return [
    min.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      timeZone: tz,
    }),
    max.toLocaleDateString(undefined, {
      year: "numeric",
      month: "short",
      day: "numeric",
      timeZone: tz,
    }),
  ];
}
```

- [ ] **Step 2: Thread timezone into `TimestampHistogram`**

Add `timezone` prop to the component and pass it to `formatDateRange`:

```tsx
function TimestampHistogram({
  summary,
  width,
  visibleBins,
  activeFilter,
  onFilter,
  timezone,
}: {
  summary: TimestampColumnSummary;
  width: number;
  visibleBins?: number[];
  activeFilter?: RangeFilter | null;
  onFilter: FilterCallback;
  timezone?: string | null;
}) {
  const [minLabel, maxLabel] = formatDateRange(summary.min, summary.max, timezone);
```

- [ ] **Step 3: Pass timezone from `renderColumnSummary`**

The `renderColumnSummary` function needs access to the column's timezone. It currently receives `summary` and `width` but not column metadata. Add a `timezone` parameter:

In `packages/sift/src/sparkline.tsx`, find the `renderColumnSummary` export (line 1221). Add `timezone?: string | null` to its signature. Then in the timestamp case (line 1164), pass it through:

```tsx
    case "timestamp":
      return (
        <TimestampHistogram
          summary={summary}
          width={width}
          visibleBins={visibleBins}
          activeFilter={activeFilter?.kind === "range" ? activeFilter : null}
          onFilter={onFilter}
          timezone={timezone}
        />
      );
```

- [ ] **Step 4: Thread timezone through `renderColumnSummary` -> `ColumnSummaryChart` -> `TimestampHistogram`**

In `packages/sift/src/sparkline.tsx`, update `renderColumnSummary` (line 1221) to accept `timezone`:

```tsx
export function renderColumnSummary(
  container: HTMLElement,
  summary: NonNullSummary,
  width: number,
  visibleBins?: number[],
  activeFilter?: ColumnFilter,
  onFilter?: FilterCallback,
  unfilteredSummary?: NonNullSummary,
  timezone?: string | null,
) {
```

Pass it to `ColumnSummaryChart` in the `root.render` call (line 1236):

```tsx
  root.render(
    <ColumnSummaryChart
      summary={summary}
      unfilteredSummary={unfilteredSummary}
      width={width}
      visibleBins={visibleBins}
      activeFilter={activeFilter ?? null}
      onFilter={onFilter ?? (() => {})}
      timezone={timezone}
    />,
  );
```

Add `timezone` prop to `ColumnSummaryChart` (line 1134) and pass it through to `TimestampHistogram` in the timestamp case (line 1166):

```tsx
function ColumnSummaryChart({
  summary,
  unfilteredSummary,
  width,
  visibleBins,
  activeFilter,
  onFilter,
  timezone,
}: {
  summary: NonNullSummary;
  unfilteredSummary?: NonNullSummary;
  width: number;
  visibleBins?: number[];
  activeFilter?: ColumnFilter;
  onFilter: FilterCallback;
  timezone?: string | null;
}) {
  // ... in the timestamp case:
    case "timestamp":
      return (
        <TimestampHistogram
          summary={summary}
          width={width}
          visibleBins={visibleBins}
          activeFilter={activeFilter?.kind === "range" ? activeFilter : null}
          onFilter={onFilter}
          timezone={timezone}
        />
      );
```

- [ ] **Step 5: Pass timezone from table.ts call site**

In `packages/sift/src/table.ts`, the `renderSummary` function (line 583) calls `renderColumnSummary`. Add `data.columns[c].timezone` as the last argument:

```ts
  function renderSummary(c: number, visibleBins?: number[]) {
    const summary = data.columnSummaries[c];
    if (summary) {
      const unfiltered = hasActiveFilters() ? (unfilteredSummaries[c] ?? undefined) : undefined;
      renderColumnSummary(
        summaryContainers[c],
        summary,
        colWidths[c] - CELL_PAD_H,
        visibleBins,
        filters[c],
        filterCallbacks[c],
        unfiltered,
        data.columns[c].timezone,
      );
    }
  }
```

- [ ] **Step 5: Verify tests pass**

Run: `cd packages/sift && npx vp test run`

- [ ] **Step 6: Commit**

```bash
git add packages/sift/src/sparkline.tsx packages/sift/src/table.ts
git commit -m "feat(sift): timezone-aware sparkline date range labels"
```

---

### Task 6: Show UTC default indicator on timestamp column headers

**Files:**
- Modify: `packages/sift/src/sparkline.tsx` (TimestampHistogram range label area)

- [ ] **Step 1: Add UTC indicator when timezone is not explicitly set**

In `TimestampHistogram`, after the range labels, add a small indicator when displaying with the UTC default:

```tsx
  const isDefaultUtc = !timezone;

  // In the JSX where the range is rendered, append a subtle indicator:
  // Find the range label span and add:
  {isDefaultUtc && (
    <span
      className="sift-tz-default"
      title="No timezone in data, displayed as UTC"
    >
      UTC
    </span>
  )}
```

- [ ] **Step 2: Add minimal CSS for the indicator**

In `packages/sift/src/style.css`, add:

```css
.sift-tz-default {
  font-size: 9px;
  opacity: 0.5;
  margin-left: 4px;
  vertical-align: super;
}
```

- [ ] **Step 3: Verify tests pass and visual check**

Run: `cd packages/sift && npx vp test run`

- [ ] **Step 4: Commit**

```bash
git add packages/sift/src/sparkline.tsx packages/sift/src/style.css
git commit -m "feat(sift): show UTC default indicator on timezone-naive timestamp columns"
```

---

### Task 7: Run lint, build WASM, rebuild plugins, verify end-to-end

**Files:**
- Rebuilt artifacts (LFS)

- [ ] **Step 1: Run lint**

Run: `cargo xtask lint --fix`

- [ ] **Step 2: Run clippy**

Run: `cargo xtask clippy`

- [ ] **Step 3: Build WASM and renderer plugins**

Run: `CC=/opt/homebrew/opt/llvm/bin/clang AR=/opt/homebrew/opt/llvm/bin/llvm-ar cargo xtask wasm sift`

- [ ] **Step 4: Verify bundle size**

Run: `ls -lh apps/notebook/src/renderer-plugins/sift.js`

Expected: slight increase from chrono-tz IANA data in WASM, but should be under 200KB additional.

- [ ] **Step 5: Commit rebuilt artifacts**

```bash
git add apps/notebook/src/renderer-plugins/ crates/runt-mcp/assets/plugins/ crates/sift-wasm/pkg/
git commit -m "chore(sift): rebuild WASM and renderer plugins with timezone support"
```
