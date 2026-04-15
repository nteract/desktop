# Plan: Enrich dx's `text/llm+plain` summary

## Motivation

When dx renders a DataFrame, the frontend shows a rich sift grid (backed by
`application/vnd.apache.parquet`). The agent sees the `text/llm+plain` summary
dx generates — which is currently just shape + dtypes + a raw `head(10).to_string()`
dump. That's noticeably less useful than the `repr-llm::summarize_parquet`
Rust-side synthesis that kicks in when dx *isn't* in the loop: repr-llm
includes per-column stats (numeric range, distinct/top for strings, null
counts). Because dx pre-emits `text/llm+plain`, the synthesis path is skipped,
so the agent gets less signal than it would on a non-dx parquet output.

Secondary bug: for text-heavy DataFrames, dx's head preview expands every
cell in full (e.g. 200-char bio strings × 10 rows). The summary balloons past
the 1 KB inline threshold, gets blob-stored, and shows up in MCP structured
content as a URL rather than inlined text.

Tertiary: `datasets.Dataset` (HuggingFace) falls through to IPython's bare
metadata repr because dx has no handler for it.

## Goals

1. **Head truncation.** Each cell in the head preview truncated to ~80 chars
   with `…[+N chars]` suffix — keeps the summary comfortably inline.
2. **Per-column stats.** Match repr-llm's shape:
   - Numeric: range (`min – max`)
   - String: distinct count + top 3 values with counts
   - Temporal: min/max if available
   - Null counts (already present)
3. **Dataset handler.** Register a dx ipython_display / mimebundle formatter
   on `datasets.Dataset` that emits a `text/llm+plain` with:
   - feature names + dtypes from `ds.features`
   - num_rows
   - maybe `ds[0]` peek for one sample row
   - **No `.to_pandas()`** — preserves the lazy design and avoids OOM on
     huge Arrow-backed datasets.

## Non-goals

- Changing the wire format or daemon-side synthesis.
- Auto-materializing `datasets.Dataset` into parquet.
- Reworking the MCP structured-content blob→inline behavior (the
  enrichment+truncation together should keep dx summaries inline, so the
  structured-content-URL footgun stops firing for dx outputs).

## File-by-file changes

### `python/dx/src/dx/_summary.py`

1. Add a `_truncate_cell(value: Any, max_chars: int = 80) -> str` helper that
   renders the cell as its `str()` and truncates with `…[+N chars]` suffix
   when longer.

2. Rewrite `summarize_dataframe` to:
   - Produce the existing header line.
   - Produce per-column summary lines with stats:
     ```
     Columns:
       - id (int64) · range 1 – 1,200
       - name (string) · 5 distinct, top: "alice" (1), "bob" (1), "carol" (1)
       - score (float64) · range 0.12 – 0.99 · 3 null (2%)
       - ts (datetime64[ns]) · 2024-01-01 – 2024-12-31
     ```
   - Produce a `Head (N):` section with truncated-cell rendering.

3. Add flavor-specific stat extractors. Keep them cheap — operate on the
   already-in-memory DataFrame, no parquet re-read:
   - `_pandas_column_stats(df)` → list of `ColumnStatsRow` (dict or tuple):
     - Numeric: `df[col].min()`, `df[col].max()`, null count
     - String/object: `df[col].value_counts(dropna=True).head(3)`, nunique
     - Datetime: `df[col].min()`, `df[col].max()`
   - `_polars_column_stats(df)` → same shape, via polars methods (`df[col].min()`,
     `df[col].value_counts().head(3)`).

4. For the head preview: render via `df.head(N)` (pandas) or `df.head(N)` (polars),
   then for each column apply `_truncate_cell`. **Use pandas's own `.to_string(max_colwidth=80)`**
   if that flag exists and behaves — that's the simplest path. Fallback to
   manual per-cell truncation if not.

### `python/dx/src/dx/_format_install.py`

1. Import any HuggingFace `datasets` module at the top with an ImportError
   guard (match the existing pandas/polars/narwhals pattern).

2. In `install()`, add the dataset handler registration alongside the
   existing pandas/polars/narwhals blocks:
   ```python
   try:
       import datasets  # noqa: PLC0415
       mimebundle.for_type(datasets.Dataset, _dataset_mimebundle)
       ipython_display.for_type(datasets.Dataset, _dataset_ipython_display)
   except ImportError:
       log.debug("dx: datasets not installed, skipping handler")
   ```

3. Add `_dataset_mimebundle(ds, include=None, exclude=None) -> dict | None`
   that:
   - Calls `summarize_dataset(ds)` (new, in `_summary.py`) to produce a
     `text/llm+plain` string.
   - Returns `{"text/llm+plain": summary}`. **Does NOT** emit the parquet
     ref MIME — leaves the dataset lazy. Lets IPython fill in `text/plain`
     from the dataset's own repr as fallback (`Dataset({features: [...], num_rows: N})`)
     so frontends without the llm+plain path still see something sensible.
   - On any exception (e.g. remote dataset not yet loaded), return `None`
     so IPython's default repr takes over.

4. `_dataset_ipython_display(ds)` mirrors `_pandas_ipython_display`: builds
   the mimebundle and calls `publish_display_data`. This is what makes a
   bare `ds` last-expression produce `display_data` (not `execute_result`),
   consistent with the existing DataFrame path.

### `python/dx/src/dx/_summary.py` (cont.)

5. Add `summarize_dataset(ds: Any) -> str`:
   - Header: `HuggingFace Dataset: {num_rows} rows × {n_features} features`
   - Features block: one line per feature with its dtype (from `ds.features`,
     which is a `Features` mapping of `Value`/`Sequence`/etc.)
   - Sample row: show `ds[0]` with each field truncated to ~80 chars. Gate
     on `num_rows > 0`.
   - If `ds.info.description` exists and is non-empty, include a short
     excerpt (first 200 chars).
   - Do NOT call `.to_pandas()`, `.select(...)`, or any materializing op
     that loads more than row 0.

## Tests

### `python/dx/tests/test_summary_unit.py` (new if not present)

- `test_pandas_summary_includes_numeric_range`
- `test_pandas_summary_includes_string_distinct_and_top`
- `test_pandas_summary_truncates_long_cells` — pin the `…[+N chars]` suffix
  and verify total summary stays < 1 KB for a DataFrame with 5 rows of
  200-char text columns.
- `test_polars_summary_matches_pandas_shape` — same asserts, polars input.
- `test_dataset_summary_from_features_only` — build a tiny `Dataset.from_dict`,
  assert the summary mentions features, num_rows, and `ds[0]` sample. Skip
  if `datasets` not importable (existing pattern).

### `python/dx/tests/test_dx_integration.py` (extend)

- `test_dx_text_heavy_dataframe_summary_stays_inline` — build a 5-row
  DataFrame with 200-char text columns, `dx.display(df)`, assert the
  output's `text/llm+plain` ContentRef is **inline** (not `Blob`).
  Regression guard: this is the actual bug the user reported.
- `test_dx_dataset_emits_display_data_with_summary` — if `datasets`
  installed, `ds = Dataset.from_dict({...}); ds` produces
  `output_type == "display_data"` (not `execute_result`), and data
  includes `text/llm+plain` mentioning the feature names and num_rows.

## Verification

```bash
# Unit tests
uv run pytest python/dx/tests/test_summary_unit.py -v

# Integration tests (requires workspace venv + dev daemon)
cargo xtask integration test_dx

# Manual E2E via MCP
# 1. Fresh notebook, add deps: dx[pandas], datasets
# 2. Create cell: ds = datasets.Dataset.from_dict({"bio": ["hello"*200]*5, "id": list(range(5))})
# 3. Execute `ds` — inspect MCP response: text/llm+plain should mention features, num_rows
# 4. Execute `ds.to_pandas()` — inspect: output is display_data with text/llm+plain INLINE
#    (not a blob URL in structured content)
```

Run `cargo xtask lint` before committing. This PR touches only Python files
so ruff + ty cover it.

## Commit / PR

- Branch name: `feat/dx-llm-summary-enrichment`
- Commit: `feat(dx): richer per-column stats + Dataset handler in text/llm+plain`
- PR body sections: Summary / Changes / Tests / Before-after example of
  the summary output for a text-heavy DataFrame.

Before opening the PR: `codex review --base main`. Expected friction:
- Columns with all-null → stats should gracefully say "all null" rather
  than blow up on `.min()`.
- Very wide DataFrames (100+ columns) — don't let the summary go unbounded;
  cap at ~40 columns and append `…[+N more columns]` if exceeded.
- Polars temporal dtypes — make sure the stats path handles `Datetime`,
  `Date`, `Duration` without crashing.

## Dependency note

`datasets` is optional. Don't add it to dx's required deps. The import in
`_format_install.py` is guarded by `ImportError` so dx stays importable on
installs that don't want `datasets`.

## Related background

- User observation: "how do I see the polars dataframe but the agent doesn't"
  — frontend renders parquet via sift; agent reads `text/llm+plain`. Current
  dx summary is less useful than `repr-llm::summarize_parquet`, so the agent
  is genuinely getting less info than the grid shows.
- Current `repr-llm::summarize_parquet` (`crates/repr-llm/src/parquet.rs`)
  is a reference for the stat shapes we want to match on the Python side.
- `synthesize_llm_plain_for_parquet` in
  `crates/runtimed-client/src/output_resolver.rs` is skipped when
  `text/llm+plain` is already present. We're keeping that logic — the fix
  is to make dx's own summary as rich as what that synthesis would produce.
