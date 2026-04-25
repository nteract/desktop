# Notebook metadata extras: make the Automerge doc authoritative for all top-level metadata keys

## Problem

`NotebookMetadataSnapshot` carries three typed fields: `kernelspec`, `language_info`, `runt`. Every other top-level notebook metadata key (`jupytext`, `colab`, `vscode`, and whatever else a third-party tool stamps in there) is silently discarded when a `.ipynb` loads.

Today the loss is papered over on the save path by reading the existing `.ipynb` from disk and merging the typed snapshot onto it, so known-file-backed notebooks appear to round-trip unknown keys. Two places that breaks:

1. **Clone.** An ephemeral clone has no on-disk file to rescue from. On first Save-As, unknown source keys vanish because the save path sees no existing file to merge with. This is what Codex flagged as F3 on PR #2192.
2. **The layering rule.** `architecture.md` says the Automerge doc is the source of truth for notebook state. Reading metadata off disk at save time violates that, and the fact that it works at all is load-bearing accident.

## Goal

Make the Automerge `NotebookDoc` carry all top-level metadata keys, typed or not. Clone then copies the doc; save writes the doc; external edits merge into the doc. No disk rescue.

## Scope

### In scope

- `NotebookMetadataSnapshot` gains an `extras: BTreeMap<String, serde_json::Value>` field with `#[serde(flatten)]` so unknown top-level keys round-trip via serde.
- `KernelspecSnapshot` gains an `extras: BTreeMap<String, serde_json::Value>` field, matching how `RuntMetadata.extra` already catches unknown sub-keys. Standard Jupyter kernelspec fields like `env`, `interrupt_mode`, `metadata` currently vanish.
- `LanguageInfoSnapshot` gains the same. Standard Jupyter `language_info` fields (`codemirror_mode`, `mimetype`, `file_extension`, `nbconvert_exporter`, `pygments_lexer`) currently vanish. The existing comment on `merge_into_metadata_value` in metadata.rs already flags this ("preserve fields we don't track, like codemirror_mode").
- `NotebookDoc::set_metadata_snapshot` writes extras as siblings of `kernelspec`/`language_info`/`runt` under the `metadata` Automerge Map, each as its own JSON value (one `update_json_at_key` per entry).
- `NotebookDoc::get_metadata_snapshot` reads the three typed keys, then enumerates the rest of the `metadata` Map and collects them into `extras`.
- `NotebookDoc::set_metadata_snapshot` guards against the caller inserting a known-key collision (`kernelspec`, `language_info`, `runt`) into top-level `extras`: logs an `error!` with the colliding key and drops it before writing.
- `save_notebook_to_disk` stops reading the existing `.ipynb` to recover metadata. The snapshot round-trip carries everything.
- `clone_notebook::seed_clone_from_source` benefits automatically since it copies the typed snapshot.
- Unit + integration tests covering: unknown top-level keys round-trip, unknown kernelspec/language_info sub-keys round-trip, clone preserves extras, collision guard drops known top-level keys and logs.

### Out of scope

- Migrating docs that are currently loaded in a running daemon. They lack extras until the next fresh load from disk. Accepted one-time loss: the `.ipynb` on disk still has them until that first save-after-upgrade. Documented, not patched.
- Changing `parse_metadata_from_ipynb`'s signature or callers. The function already takes a `Value`; the snapshot builder change propagates automatically.
- Touching per-cell metadata. Cells already handle arbitrary keys via `CellSnapshot.metadata: serde_json::Value`.
- Output metadata. Separate path (`OutputManifest::metadata: HashMap<String, Value>`), unrelated.
- Anything about the `runt.*` keys. `RuntMetadata.extra` already handles unknowns inside `runt`.

## Architecture

### Data flow

```
Load (.ipynb on disk)
  ↓
parse_metadata_from_ipynb(value)
  ↓
NotebookMetadataSnapshot {
    kernelspec, language_info, runt,
    extras: BTreeMap { "jupytext": ..., "colab": ..., ... },
}
  ↓
doc.set_metadata_snapshot(&snapshot)
  ↓
Automerge metadata Map {
    kernelspec: Map, language_info: Map, runt: Map,
    jupytext: Map, colab: Map, ...
}
  ↓
Sync to peers (frontend + other daemon clients)
  ↓
doc.get_metadata_snapshot() (at save time)
  ↓
NotebookMetadataSnapshot (same shape)
  ↓
nbformat_convert::build_v4_notebook → typed v4::Notebook
  ↓
nbformat::serialize_notebook → .ipynb on disk
```

### Why siblings at `metadata` root, not nested under an `_extras` key

- Matches the `.ipynb` on-disk shape exactly. No lift/flatten layer at serialize time.
- Matches the pattern `RuntMetadata.extra` already uses one level down.
- Gives each extra its own Automerge Map, so concurrent edits to `metadata.jupytext.paired_paths` from two peers merge per-field instead of stomping at the `_extras` root.

### Why the save path stops reading `existing`

Before: `metadata = read_existing_ipynb().metadata; snapshot.merge_into(&mut metadata); write(&metadata)`.
After: `metadata = serde_json::to_value(&snapshot)?; write(&metadata)`.

The doc is now complete. Re-reading the file means treating disk as authoritative, which contradicts "daemon as source of truth" and hid the original bug.

### Collision guard

`set_metadata_snapshot` iterates `snapshot.extras`. For each `(key, value)`:

```rust
if matches!(key.as_str(), "kernelspec" | "language_info" | "runt") {
    tracing::error!(
        "[notebook-doc] metadata.extras collision: key {:?} is reserved for typed field; dropping to avoid Automerge double-write. This indicates a caller bug.",
        key
    );
    continue;
}
```

`error!` (not `warn!`) because silent data loss is a correctness issue and per `.claude/rules/logging.md` it should be visible on stable channels (which use `warn` default filter).

## Components

### `crates/notebook-doc/src/metadata.rs`

**`NotebookMetadataSnapshot`** gains an extras field:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NotebookMetadataSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernelspec: Option<KernelspecSnapshot>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_info: Option<LanguageInfoSnapshot>,

    pub runt: RuntMetadata,

    /// Catch-all for unknown/third-party top-level metadata keys
    /// (e.g. `jupytext`, `colab`, `vscode`). Preserves fields we don't
    /// model through load → doc → save round-trips. Excludes the typed
    /// keys above; `set_metadata_snapshot` guards against collisions.
    #[serde(default, flatten)]
    pub extras: std::collections::BTreeMap<String, serde_json::Value>,
}
```

**`KernelspecSnapshot`** gains an extras field for unknown kernelspec sub-keys:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KernelspecSnapshot {
    pub name: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    /// Unknown kernelspec sub-fields (e.g. `env`, `interrupt_mode`,
    /// `metadata`) that Jupyter clients write but we don't model. Round-
    /// tripped verbatim so notebooks stay portable.
    #[serde(default, flatten)]
    pub extras: std::collections::BTreeMap<String, serde_json::Value>,
}
```

**`LanguageInfoSnapshot`** gains the same:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LanguageInfoSnapshot {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Unknown language_info sub-fields (e.g. `codemirror_mode`,
    /// `mimetype`, `file_extension`, `nbconvert_exporter`,
    /// `pygments_lexer`). Jupyter clients populate these after kernel
    /// startup; without this, they vanished on every save.
    #[serde(default, flatten)]
    pub extras: std::collections::BTreeMap<String, serde_json::Value>,
}
```

Both `KernelspecSnapshot` and `LanguageInfoSnapshot` gain `Default` as a side-effect of `BTreeMap: Default`. That lines up with other recently-added `Default`s on the nbformat side.

**`from_metadata_value`** switches from manual per-field extraction to `serde_json::from_value::<Self>(value.clone())` once the new flattened extras fields are in place. That's where the flatten payoff lands — serde does the "known fields go to typed slots, unknowns go to extras" dispatch automatically for all three levels at once.

The legacy `uv`/`conda` fallback becomes a one-line post-processing step: if the resulting `runt.uv` / `runt.conda` is `None` and the incoming value had top-level `uv` / `conda`, fold them into `runt`. Explicitly strip them from `extras` after, otherwise they'd serialize twice on save (once inside `runt`, once as a top-level sibling).

```rust
pub fn from_metadata_value(metadata: &serde_json::Value) -> Self {
    // Serde handles kernelspec / language_info / runt as typed fields
    // with their own extras, and drops everything else into top-level
    // extras. A malformed input just produces a default snapshot.
    let mut snapshot: Self = serde_json::from_value(metadata.clone())
        .unwrap_or_default();

    // Legacy fallback: if runt.uv / runt.conda weren't populated,
    // try the legacy top-level paths.
    if snapshot.runt.uv.is_none() {
        if let Some(raw_uv) = snapshot.extras.remove("uv") {
            snapshot.runt.uv = serde_json::from_value(raw_uv).ok();
        }
    }
    if snapshot.runt.conda.is_none() {
        if let Some(raw_conda) = snapshot.extras.remove("conda") {
            snapshot.runt.conda = serde_json::from_value(raw_conda).ok();
        }
    }

    snapshot
}
```

One real behavioral change to name: **partial-failure tolerance.**

Today's manual code extracts each field via `.get("kernelspec").and_then(from_value::<KernelspecSnapshot>).ok()`, so a malformed `kernelspec` doesn't prevent `language_info` or `runt` from being captured. With `serde_json::from_value::<Self>(..).unwrap_or_default()`, a malformed value anywhere inside the metadata object triggers the `unwrap_or_default()` and we lose everything.

Two mitigations worth considering:

1. **Manual per-field deserialize plus an extras pass at the end.** Keeps today's per-field tolerance. Loses the "one serde call does it all" clarity.
2. **Trust serde.** The only way `from_value` fails here is malformed input JSON, which means a corrupted or non-Jupyter `.ipynb`. The rest of the load path already assumes valid JSON. Losing a few metadata fields on a truly malformed notebook is acceptable.

I'd go with option 2 and note it in a comment. If this bites in practice we can add the per-field tolerance back, but it's premature optimization for a failure mode that hasn't been reported.

If user disagrees: fall back to option 1 during implementation, keeping today's `.and_then(..)` per-field pattern for the three typed fields and adding a separate extras scan over the raw `metadata` object (filtering out the known keys explicitly).

**`merge_into_metadata_value`** needs to also write extras. Currently it sets only the three typed keys on the target value. After the change, it iterates `extras` and sets each one via direct `obj.insert`. Callers today (if any remain) that relied on merging with a pre-populated target continue to work — extras just fill in more siblings.

### `crates/notebook-doc/src/lib.rs`

**`NotebookDoc::set_metadata_snapshot`** gets the collision guard and an extras loop:

```rust
pub fn set_metadata_snapshot(
    &mut self,
    snapshot: &metadata::NotebookMetadataSnapshot,
) -> Result<(), AutomergeError> {
    let meta_id = /* unchanged setup */;

    // Known typed keys unchanged
    // ... kernelspec, language_info, runt ...

    // Write extras. Each key gets its own Automerge Map so concurrent
    // edits merge per-field. Guard against callers that stuff known
    // keys into extras — those would produce duplicate writes and
    // Automerge conflicts at the same key.
    for (key, value) in &snapshot.extras {
        if matches!(key.as_str(), "kernelspec" | "language_info" | "runt") {
            tracing::error!(
                "[notebook-doc] metadata.extras collision: key {:?} \
                 is reserved for typed field; dropping. This indicates \
                 a caller bug in snapshot construction.",
                key
            );
            continue;
        }
        update_json_at_key(&mut self.doc, &meta_id, key, value)?;
    }

    Ok(())
}
```

**`NotebookDoc::get_metadata_snapshot`** gets the scanning loop for extras:

```rust
pub fn get_metadata_snapshot(&self) -> Option<metadata::NotebookMetadataSnapshot> {
    let meta_id = self.metadata_map_id()?;

    let kernelspec = /* unchanged */;
    let language_info = /* unchanged */;
    let runt = /* unchanged */;

    // Scan the metadata map for keys we don't model.
    let mut extras = std::collections::BTreeMap::new();
    for key in self.doc.keys(&meta_id) {
        if matches!(key.as_str(), "kernelspec" | "language_info" | "runt") {
            continue;
        }
        if let Some(value) = read_json_value(&self.doc, &meta_id, &key) {
            extras.insert(key, value);
        }
    }

    if kernelspec.is_some() || language_info.is_some() || runt.is_some()
        || !extras.is_empty() {
        return Some(metadata::NotebookMetadataSnapshot {
            kernelspec,
            language_info,
            runt: runt.unwrap_or_default(),
            extras,
        });
    }
    None
}
```

### `crates/runtimed/src/notebook_sync_server/persist.rs`

Drop the existing-file read. The full rewrite of the metadata prep is about 20 lines less than today's code:

```rust
// Build metadata from the doc snapshot. The doc carries unknown top-
// level keys as sibling extras under NotebookMetadataSnapshot.extras,
// so there's nothing left to recover from disk.
let metadata = metadata_snapshot
    .as_ref()
    .map(|s| serde_json::to_value(s).unwrap_or(serde_json::json!({})))
    .unwrap_or(serde_json::json!({}));

// nbformat_minor: still pull from existing file for the 4.5 floor, or
// default to 5. This isn't carried in the doc schema today.
let existing_minor = tokio::fs::read(&notebook_path)
    .await
    .ok()
    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
    .and_then(|nb| nb.get("nbformat_minor").and_then(|v| v.as_u64()))
    .unwrap_or(5) as i32;
let nbformat_minor = std::cmp::max(existing_minor, 5);
```

The `existing_raw` / `existing` bindings and the JSON re-parse for metadata purposes go away. `existing` is still checked for the content-hash guard; we keep that part (content-hash is a different concern — "would this write be a no-op" is still true).

Actually — on re-read, the content-hash guard uses `existing_raw.as_slice() == content_with_newline.as_bytes()` which needs `existing_raw`. So the `existing_raw` read stays for the no-op-skip case. What we drop is the `existing` parse to recover metadata. Cleaner structure:

```rust
let existing_raw: Option<Vec<u8>> = /* unchanged read for hash guard */;

// nbformat_minor floor — separate concern, just needs the numeric field.
let existing_minor = existing_raw
    .as_ref()
    .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok())
    .and_then(|nb| nb.get("nbformat_minor").and_then(|v| v.as_u64()))
    .unwrap_or(5) as i32;
let nbformat_minor = std::cmp::max(existing_minor, 5);

// Metadata comes entirely from the doc.
let metadata = metadata_snapshot
    .as_ref()
    .map(|s| serde_json::to_value(s).unwrap_or(serde_json::json!({})))
    .unwrap_or(serde_json::json!({}));
```

The hash guard block further down stays as-is. The `merge_into_metadata_value` call goes away.

### `crates/runtimed/src/requests/clone_notebook.rs`

No change required. `seed_clone_from_source` already does `doc.get_metadata_snapshot()` → mutate `env_id` / leave `trust_*` → `set_metadata_snapshot`. Once the snapshot carries extras, they flow through unchanged.

## Data flow

```
                                    ┌────────────────────────────┐
(load)  .ipynb on disk  ──────────▶ │ parse_metadata_from_ipynb   │
                                    │   (now populates extras)    │
                                    └────────────┬────────────────┘
                                                 ▼
                                    NotebookMetadataSnapshot
                                      { kernelspec, language_info,
                                        runt, extras }
                                                 │
                         ┌───────────────────────┴────────────────┐
                         ▼                                        ▼
            doc.set_metadata_snapshot                serde round-trip for clone
                         │                                        │
                         ▼                                        ▼
            Automerge metadata Map:                   clone's doc gets the same
              kernelspec, language_info, runt,        snapshot (extras included)
              jupytext, colab, vscode, ...
                         │
                         │ (sync to peers via Automerge)
                         ▼
                  Frontend + MCP server
                         │
                         │ (at save time)
                         ▼
            doc.get_metadata_snapshot()
              (scans all top-level keys, returns
               typed + extras)
                         │
                         ▼
(save)  serialize to .ipynb  ◀──── build_v4_notebook(snapshot_value, ...)
```

## Error handling

- **Serialization failure in extras.** `serde_json::to_value(&snapshot)` can fail only if a `Value` somewhere is numerically NaN or similar. Treat it the same as any other save failure — `Err` propagates up through `set_metadata_snapshot` → save path. In practice unreachable.
- **Collision on write.** `set_metadata_snapshot` logs `error!` and drops. Save proceeds with the typed key intact.
- **Extras key invalid as Automerge map name.** Automerge map keys are arbitrary UTF-8 strings; any valid JSON key is valid. Nothing to guard.

## Testing

### Unit (`crates/notebook-doc/src/metadata.rs`)

- `from_metadata_value_collects_unknown_top_level_keys` — input has `jupytext`, `colab`; assert both land in `extras`.
- `from_metadata_value_skips_known_top_level_keys` — input has `kernelspec`, `language_info`, `runt`; assert none leak into top-level `extras`.
- `from_metadata_value_skips_legacy_uv_conda` — legacy top-level `uv`/`conda` keys are absorbed into `runt` by existing fallback; assert they don't *also* appear in top-level `extras`.
- `kernelspec_extras_round_trip` — input kernelspec has `{name, display_name, language, env: {...}, interrupt_mode}`; deserialize → serialize → assert `env` and `interrupt_mode` survive in `kernelspec.extras`.
- `language_info_extras_round_trip` — input language_info has `{name, version, codemirror_mode: {...}, mimetype, file_extension, nbconvert_exporter, pygments_lexer}`; assert all five unknown fields land in `language_info.extras` and round-trip.
- Full-metadata round-trip: `from_metadata_value(v)` → `serde_json::to_value(snapshot)` preserves all keys at all three levels (top-level extras + kernelspec extras + language_info extras).

### Unit (`crates/notebook-doc/src/lib.rs`)

- `set_get_metadata_snapshot_round_trips_extras` — set a snapshot with `extras: {"jupytext": {...}, "colab": {...}}`; get back equal snapshot.
- `set_metadata_snapshot_drops_extras_collision_with_kernelspec` — insert `"kernelspec"` into extras; assert log emits (captured via a test-mode layer or just assert the doc's `kernelspec` Map was not overwritten); typed `kernelspec` stays intact.
- `get_metadata_snapshot_returns_none_when_empty` — unchanged baseline test.

### Integration (`crates/runtimed/src/notebook_sync_server/tests.rs`)

- `test_save_preserves_unknown_top_level_metadata` — create file-backed room with an `.ipynb` containing `metadata.jupytext`; let load populate the doc; save; re-read `.ipynb`; assert `metadata.jupytext` unchanged.
- `test_clone_as_ephemeral_carries_unknown_metadata_to_clone` — extend the existing clone test: source has `metadata.jupytext`; assert clone doc's snapshot has the same value in extras.

## Migration

Docs currently loaded in a running daemon have no extras in their Automerge doc. On upgrade:

- File-backed rooms: the on-disk `.ipynb` still has unknown keys until the first save after upgrade. If the user saves before reopening, the keys vanish because the doc doesn't have them and (after this change) we no longer read the existing file to recover.
- Untitled rooms: if any had unknown keys synthesized in memory (unlikely; the load path never put them there), they'd be lost. Not a real case.

Accepted. Documented in the PR body. The rationale: the keys are still on disk in the one place they live, a force-reload (close and reopen) pulls them back in, and the alternative (one-shot migration code) adds maintenance burden for a one-time concern.

## Rollout

- Single PR, atomic. No feature flag. Behavior change is additive (extras now preserved; typed fields unchanged).
- No protocol or wire format change. `NotebookMetadataSnapshot` serializes the same way with or without the extras flatten; the serde flatten just means unknown fields that would previously be dropped now land in a named field.
- No downstream schema migrations.
