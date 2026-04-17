# `.ipynb` save: external blob refs instead of base64 inlining

**Status:** Shipped (#1769)
**Date:** 2026-04-14
**Related:** #1762 (dx), [spec: blob GC correctness](./2026-04-14-blob-gc-correctness-design.md)

## Motivation

`dx.display(df)` pushes parquet through the IOPub `buffers` envelope to avoid base64'ing megabytes inside JSON on the wire. The save path then re-inlines those same bytes as base64 back into the `.ipynb`. A 100 MiB parquet becomes ~133 MiB of base64 on disk. Every save. That defeats most of dx's benefit for users who commit notebooks to git.

The blob store already has the bytes; `.ipynb` should carry a reference, not a copy.

## Shape

Saved output bundles for anything resolved to a `ContentRef::Blob` that exceeds a size threshold emit the wire-format blob-ref MIME instead of a base64 string. Everything else is unchanged.

Before (today):
```json
{
  "output_type": "display_data",
  "data": {
    "application/vnd.apache.parquet": "<100 MiB of base64>",
    "text/html": "<table>…</table>",
    "text/plain": "…",
    "text/llm+plain": "DataFrame (pandas): 148820 rows × 12 cols"
  }
}
```

After (Spec 2):
```json
{
  "output_type": "display_data",
  "data": {
    "application/vnd.nteract.blob-ref+json": {
      "hash": "sha256-…",
      "content_type": "application/vnd.apache.parquet",
      "size": 104857600
    },
    "text/html": "<table>…</table>",
    "text/plain": "…",
    "text/llm+plain": "DataFrame (pandas): 148820 rows × 12 cols"
  }
}
```

The original binary MIME key disappears; the ref-MIME takes its place. All non-binary MIMEs stay inline unchanged.

Symmetry: this is the same shape dx emits on the IOPub wire. `create_manifest` already has a `BLOB_REF_MIME` branch that composes `ContentRef::Blob` under the wrapped `content_type` — so the load path works with zero changes.

## Compatibility

- **Vanilla Jupyter** (any host that doesn't know the ref MIME): the entry is skipped as unknown; frontends fall through to `text/html` / `text/plain`. For `dx.display(df)` the HTML table renders; for any custom binary display that lacked a fallback it renders as a missing output — same result as today if the base64 were corrupted or the host's renderer disabled.
- **nteract reopen** (via daemon): `create_manifest` sees `BLOB_REF_MIME`, verifies the blob exists in the store, composes a `ContentRef::Blob`. If the blob is missing (different machine, GC'd past the grace period), the ref entry is dropped — the HTML fallback renders instead. No crash, no dangling reference in the inline manifest.
- **`.ipynb` diffability**: storing hashes instead of 133 MiB of base64 makes diffs actually reviewable. Changing a DataFrame changes a hash, not a quarter-million-line textblob.

## Threshold

A size threshold governs the rewrite:

- `content_ref.size >= REF_MIME_SAVE_THRESHOLD` → emit `BLOB_REF_MIME`.
- Smaller → inline base64 as today.

Rationale: small images (plot thumbnails, icons, small exports) are useful to ship self-contained. Large payloads (parquet, ML model dumps) are what we need to externalize. Default: **1 MiB**. Tunable via env var `RUNTIMED_REF_MIME_SAVE_THRESHOLD` for dev + edge cases.

This preserves the "drop a `.ipynb` into any Jupyter host and it renders" promise for common cases (matplotlib PNGs are typically 10–300 KiB) while removing the parquet-in-JSON anti-pattern.

## Implementation

### Crate changes

`crates/runtimed/src/output_store.rs::resolve_data_bundle` gets one new branch before the existing binary/json/text discrimination:

```rust
for (mime_type, content_ref) in data {
    // Spec 2: large binary blobs get externalized as a ref-MIME entry
    // instead of inline base64. See 2026-04-14-ipynb-save-blob-refs-design.md.
    if is_binary_mime(mime_type) {
        if let ContentRef::Blob { blob: hash, size } = content_ref {
            if *size >= ref_mime_save_threshold() {
                let ref_body = json!({
                    "hash": hash,
                    "content_type": mime_type,
                    "size": size,
                });
                result.insert(BLOB_REF_MIME.to_string(), ref_body);
                continue; // skip the inline-base64 branch below
            }
        }
    }
    // existing resolution (base64 / json / text) …
}
```

`ref_mime_save_threshold()` reads `RUNTIMED_REF_MIME_SAVE_THRESHOLD` once (cached via `OnceLock`), defaults to 1 MiB.

The function signature stays the same — no other caller changes.

### Load path

No code changes needed. `create_manifest`'s existing `BLOB_REF_MIME` branch (added in the dx PR) already handles the inverse: it reads the ref MIME from the bundle, verifies the hash via `BlobStore::exists`, composes `ContentRef::from_hash` under the wrapped `content_type`, and omits the ref MIME entry from the resolved manifest.

### Tests

- Unit in `output_store.rs`: `resolve_data_bundle` emits `BLOB_REF_MIME` for a binary `ContentRef::Blob` above the threshold; falls back to base64 below the threshold.
- Round-trip: `create_manifest` of a manifest containing `BLOB_REF_MIME` → `resolve_manifest` with the threshold set low → `create_manifest` again. Hash is stable; manifest shape is idempotent.
- Integration: `save_notebook_to_disk` on a cell with a large parquet produces a `.ipynb` whose parquet entry is a ref-MIME, not a base64 string. Reopening the notebook reconstructs a valid `ContentRef::Blob` pointing at the same blob.
- Fixture update: the existing `test_save_notebook_to_disk_with_outputs` fixtures may need regeneration if any of their outputs cross the threshold. Audit.

## Migration

Not a migration — existing `.ipynb` files with base64-inlined binary outputs keep working on load (the normal `is_binary_mime` path reads base64 → decodes → stores in the blob store → composes `ContentRef::Blob`). Only new saves use the ref MIME path. Old files get progressively upgraded as users reopen and re-save them.

## Dependencies

- **Spec 1 (blob GC correctness)** must ship first, or concurrently. Saved `.ipynb` files now depend on the blob store surviving long enough for the user to reopen. The 30-day grace period and the persisted-automerge walk from Spec 1 are the safety net.

## Rollout

Land behind a feature gate? **No.** The change is internal: the save format is already a moving target, and the load path has been ref-MIME-aware since dx landed. No external consumer of the `.ipynb` format needs a migration window.

Version bump: the save-path change doesn't warrant its own version; it rides whatever package version is being cut next.
