# LLM preview fields for stream and error manifests

**Status:** Draft
**Date:** 2026-04-15
**Scope:** `runtimed` (RuntimeAgent IOPub → manifest writer), `notebook-doc`, `runtimed-client::output_resolver`, `runt-mcp`

## Problem

When a cell's stream text or error traceback exceeds the 1 KB inline threshold, the manifest stored in `RuntimeStateDoc` spills the content to the blob store. Only a `ContentRef::Blob { hash, size }` remains in the manifest.

Downstream, `runt mcp` serves LLM clients a text representation of the cell's outputs. Its resolver (`runtimed-client::output_resolver::resolve_text_ref`) tries the local blob path, then an HTTP fetch. When both fail — or when the path returns the raw URL to the caller — the LLM sees either:

1. A stream output whose `text` field is just the blob URL (`http://localhost:NNNN/blob/<hash>`), indistinguishable from a program that legitimately printed that URL.
2. An error output with no traceback at all (because resolution failed and the whole output is dropped or reduced to `ename`/`evalue`).

**Source:** the shakedown report (2026-04-15) — issues #2 (traceback blob as bare string), #5 (interrupted cell has no traceback; related but handled separately), #8 (stream blob as string), #9 (bare blob URL ambiguous).

Secondary problem: even when the blob *can* be fetched, pulling 50 000 lines of stdout into the LLM context is wasteful when a head/tail sample plus a count is sufficient for the model to reason about the output.

## Insight

The RuntimeAgent sees the full, unspilled text at the moment it writes the manifest. This is the cheapest place to synthesize a bounded, LLM-friendly preview — exactly analogous to how `text/llm+plain` is synthesized for dx/parquet at write time.

## Design

### Manifest shape changes

Add an optional `llm_preview` field to the two affected output variants in `notebook-doc::output_store::OutputManifest`:

```rust
pub enum OutputManifest {
    Stream {
        name: String,
        text: ContentRef,
        /// Present when `text` is a Blob. Inline string ≤ 2 KiB,
        /// always safe to include in LLM context. None when `text`
        /// itself is inline (the preview would be redundant).
        llm_preview: Option<StreamPreview>,
    },
    Error {
        ename: String,
        evalue: String,
        traceback: ContentRef,
        /// Present when `traceback` is a Blob. Inline string ≤ 2 KiB.
        llm_preview: Option<ErrorPreview>,
    },
    DisplayData { .. },   // unchanged
    ExecuteResult { .. }, // unchanged
}

pub struct StreamPreview {
    /// First N lines/chars of the full stream. Capped at 1 KiB.
    pub head: String,
    /// Last M lines/chars of the full stream. Capped at 1 KiB.
    /// Empty when the full text is short enough that head covers it.
    pub tail: String,
    /// Total byte count of the full text.
    pub total_bytes: u64,
    /// Total line count of the full text.
    pub total_lines: u64,
}

pub struct ErrorPreview {
    /// Last frame of the traceback (the "bottom" line — usually the
    /// most useful one for the LLM). ANSI-stripped. Capped at 1 KiB.
    pub last_frame: String,
    /// Total byte count of the stringified traceback array.
    pub total_bytes: u64,
    /// Total number of traceback frames.
    pub frames: u32,
}
```

`llm_preview` is:
- **`None`** when the content is inline — no loss of information, the inline text is already a preview.
- **`Some(...)`** when the content spilled to blob — callers have a self-contained summary even if they can't reach the blob.

### Writer side (`crates/runtimed/src/output_store.rs` + RuntimeAgent IOPub)

Generate previews inside `create_manifest` at the moment `ContentRef::from_data` decides to spill:

```rust
// Stream branch
let text_str = normalize_text(&text_value);
let (text, llm_preview) =
    if text_str.len() < threshold {
        (ContentRef::Inline { inline: text_str }, None)
    } else {
        let preview = StreamPreview::from_text(&text_str);  // head/tail/counts
        let hash = blob_store.put(text_str.as_bytes(), "text/plain").await?;
        (ContentRef::Blob { blob: hash, size: text_str.len() as u64 }, Some(preview))
    };
OutputManifest::Stream { name, text, llm_preview }
```

Same pattern for `Error`: build `ErrorPreview::from_traceback(&traceback_value)` when the JSON-stringified traceback exceeds threshold.

Caps:
- `StreamPreview::head`: up to first 40 lines or 1 KiB, whichever comes first. Measured on ANSI-stripped text.
- `StreamPreview::tail`: last 40 lines or 1 KiB, whichever comes first. Empty if `head` already covers the entire text (i.e. total < head cap).
- `ErrorPreview::last_frame`: last non-empty element of the traceback array, ANSI-stripped, truncated at 1 KiB.

These caps are conservative; the total preview is always ≤ 2 KiB. We pay this cost once per spilled output, inline in the CRDT.

### Reader side

**`runtimed-client::output_resolver::resolve_output_for_llm`** (`crates/runtimed-client/src/output_resolver.rs:495`):

When resolving a stream/error whose `ContentRef` is `Blob`, use `llm_preview` instead of fetching the blob. Specifically:

- Stream: synthesize a text representation like
  ```
  <head>
  … [{elided_lines} lines elided, {total_bytes} bytes total — full text at {blob_url}] …
  <tail>
  ```
  When `tail` is empty (preview covered the whole thing), omit the elision marker and the tail.

- Error: return `Output::error(ename, evalue, vec![last_frame, elision_marker])` where the elision marker is
  ```
  … [{frames} traceback frames, {total_bytes} bytes total — full traceback at {blob_url}] …
  ```

`blob_url` is available from `blob_base_url + hash`. When `blob_base_url` is None, omit the URL and keep the size/count info — the preview still stands on its own.

**Existing non-LLM path** (`resolve_output`, `resolve_content_ref`) keeps the current behavior: read the blob from disk or HTTP, return full content. The frontend, `.ipynb` save path, and tests are unaffected.

### MCP serialization

`runt-mcp`'s `formatting::format_output_text` (`crates/runt-mcp/src/formatting.rs:70`) continues to consume `Output` and produces the final LLM-facing text. No change needed there — the resolver now delivers preview-shaped text for spilled content.

`runt-mcp`'s `structured::manifest_output_to_structured` builds the MCP App widget JSON and currently emits the bare blob URL. Update it to also emit the preview fields so the widget renderer can show a summary when it chooses not to fetch the blob. The widget frontend can still fetch the full blob on demand — the preview is additive.

### Schema / compatibility

`OutputManifest` serializes through Automerge + serde. The new field is `Option` and defaults to `None`:

- **Old daemon + new reader:** preview is `None`, reader falls back to fetching the blob (current behavior). No regression.
- **New daemon + old reader:** old reader ignores the unknown field. Output still displays normally because the existing `text`/`traceback` ContentRef is still present and still resolvable.
- **Persisted notebooks (`.ipynb`):** preview fields do **not** round-trip through `.ipynb`. On load, the daemon regenerates outputs from the saved base64/text, and `create_manifest` re-computes previews from scratch. No schema bump to nbformat.
- **`RuntimeStateDoc` CRDT:** adding a new field to an Automerge Map is safe — it merges as an independent key. No migration.

### Blob spillover UX — the sentinel question

Issue #9 in the shakedown asked for a clear sentinel so the LLM doesn't confuse "the program printed a URL" with "the output was elided." This design answers that implicitly: with a preview, the LLM always sees real program content (head/tail/last frame) surrounding a structured elision marker. The bare URL is never the only thing returned.

For the rare case where even the preview can't be built (e.g. manifest created by an old daemon, blob lost), the resolver's fallback is:

```
[stream output elided — {size} bytes at {blob_url}]
```

— still unambiguous, framed in square brackets like other markers, never a bare URL.

## Out of scope

- **Stream snapshot duplication (#1):** separate fix in `runt-mcp::execution` polling loop. Tracked independently.
- **execution_count staleness (#3):** response-builder fix. Independent.
- **Interrupted-cell traceback (#5):** daemon-side fix that ensures a KeyboardInterrupt error output is written on interrupt. May benefit from the `ErrorPreview` format but does not depend on it.
- **move_cell / timeout / get_cell formatting polish:** separate one-file fixes.

## Files touched

| File | Change |
|------|--------|
| `crates/runtimed/src/output_store.rs` | Add `StreamPreview`, `ErrorPreview`, extend `OutputManifest` variants, build previews in `create_manifest`, serialize/deserialize in `to_json`/`from_json`, update `resolve_manifest` pass-through |
| `crates/notebook-doc/src/runtime_state.rs` | `upsert_stream_output` preserves `llm_preview` when updating in-place (for streaming appends, preview recomputes each write) |
| `crates/runtimed-client/src/output_resolver.rs` | `resolve_output_for_llm` stream/error branches use preview when `ContentRef::Blob`; helper `render_stream_preview`, `render_error_preview` |
| `crates/runtimed-client/src/resolved_output.rs` | No change — still returns `Output` with `text`/`traceback` strings |
| `crates/runt-mcp/src/structured.rs` | Emit `llm_preview` alongside blob URL in stream/error structured content (additive field) |
| Tests | Writer preview caps, resolver preview→text rendering, Automerge round-trip with `None`/`Some` preview, old-manifest (no preview field) backwards compat |

## Testing

1. Unit tests in `output_store.rs`: small stream → no preview; large stream → preview with head+tail+counts; small traceback → no preview; large traceback → preview with last_frame.
2. Unit tests in `output_resolver.rs`: stream with preview + blob URL → expected rendered text; stream with preview + no `blob_base_url` → rendered text without URL; stream without preview + resolvable blob → full text (backwards compat); error with preview → `ename: evalue\nlast_frame\n…[N frames elided…]`.
3. Integration test: write a 100 KiB stream through a test kernel, fetch via `runt mcp`, assert output contains head, tail, elision marker, blob URL.
4. Integration test: trigger a recursion-error traceback (~8 KiB), fetch via `runt mcp`, assert output contains `ename`, `evalue`, last frame, frame count.
5. Schema compatibility test: parse a pre-change manifest JSON (no `llm_preview` field) through the new `OutputManifest::from_json` — deserializes with `llm_preview: None`.

## Open questions

1. **Head/tail thresholds (40 lines / 1 KiB each).** Reasonable? Too small for "I want to see the progress bar output" style logs? Could be configurable per-daemon, but starting with a fixed value keeps this tight.
2. **Streaming append + preview recompute.** `upsert_stream_output` updates the same manifest in place as more stream chunks arrive. Each update recomputes the preview from the new full text. Cheap (text is in hand) but worth noting.
3. **Do we want the preview on `display_data` / `execute_result` too?** Text-like MIMEs in a display bundle can spill too. Out of scope for this spec — the LLM path there already uses `text/llm+plain` synthesis and `best_text_from_data` truncation, which is a different story. Can be revisited.
