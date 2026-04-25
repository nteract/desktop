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
- `get_metadata_snapshot_from_doc` (the free-function variant in `crates/notebook-doc/src/lib.rs` used by `NotebookSnapshot::from_doc` in notebook-sync and `DocHandle::get_notebook_metadata`) gets the same scan. Without this, the Python bindings and frontend sync snapshot keep dropping extras even after the `&self` method is fixed.
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

**`NotebookMetadataSnapshot.runt`** gets `#[serde(default)]` and `#[serde(skip_serializing_if = "RuntMetadata::is_empty")]` so vanilla Jupyter notebooks (no `metadata.runt` key) deserialize cleanly *and* round-trip without the daemon stamping a `runt: { schema_version: "1" }` blob into every notebook on first save.

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NotebookMetadataSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernelspec: Option<KernelspecSnapshot>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_info: Option<LanguageInfoSnapshot>,

    #[serde(default, skip_serializing_if = "RuntMetadata::is_empty")]
    pub runt: RuntMetadata,

    #[serde(default, flatten)]
    pub extras: std::collections::BTreeMap<String, serde_json::Value>,
}
```

**`RuntMetadata::is_empty`** is a small helper that returns true when every field is at its default:

```rust
impl RuntMetadata {
    /// Returns true when this metadata carries no daemon-relevant state.
    /// Used by `skip_serializing_if` so vanilla Jupyter notebooks don't
    /// get a synthetic `runt: { schema_version: "1" }` stamped on first
    /// save, which would churn git-tracked notebooks.
    pub fn is_empty(&self) -> bool {
        self.env_id.is_none()
            && self.uv.is_none()
            && self.conda.is_none()
            && self.pixi.is_none()
            && self.deno.is_none()
            && self.trust_signature.is_none()
            && self.trust_timestamp.is_none()
            && self.extra.is_empty()
            && self.schema_version == default_schema_version()
    }
}

fn default_schema_version() -> String {
    "1".to_string()
}
```

`schema_version` was already defaulting to `"1"` via `Default`; this just names the default so `is_empty` can check against it.

**`from_metadata_value`** becomes a single `serde_json::from_value` call plus a legacy `uv`/`conda` fallback:

```rust
pub fn from_metadata_value(metadata: &serde_json::Value) -> Self {
    // Serde handles kernelspec / language_info / runt as typed fields
    // with their own sub-extras, and drops everything else into
    // top-level extras. `#[serde(default)]` on `runt` means a notebook
    // without it (the common case for Jupyter-written files) still
    // deserializes cleanly.
    let mut snapshot: Self = serde_json::from_value(metadata.clone())
        .unwrap_or_default();

    // Legacy fallback: if runt.uv / runt.conda weren't populated,
    // try the top-level keys. Strip from extras so save doesn't
    // emit them at both depths.
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

Behavioral notes:
- Missing `runt` (vanilla Jupyter notebook) deserializes to an `is_empty()` `RuntMetadata`, which `skip_serializing_if` drops on save. No daemon stamp on unrelated notebooks.
- `runt` present and non-empty: round-trips unchanged.
- Malformed field anywhere in the metadata object (e.g. `kernelspec: 42`) fails the whole `from_value` call and `unwrap_or_default()` fires, producing an empty snapshot. This is a behavior change from today's per-field tolerance; judged acceptable because the "one malformed field, siblings intact" failure mode is rare and has never been reported, while the cleaner single-call path is easier to audit and maintain.

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

**`get_metadata_snapshot_from_doc`** (the free-function variant at `crates/notebook-doc/src/lib.rs:1930`, callable from anywhere holding an `&AutoCommit`) gets the same scan. `NotebookSnapshot::from_doc` in notebook-sync routes through it, and the Python-binding `DocHandle::get_notebook_metadata` pulls its value from a `NotebookSnapshot`. If this function skips the extras scan while the `&self` method does it, daemon save/clone preserves extras but Python and the frontend sync snapshot silently drop them (Codex P2 review on PR #2198).

```rust
pub fn get_metadata_snapshot_from_doc(
    doc: &AutoCommit,
) -> Option<metadata::NotebookMetadataSnapshot> {
    let meta_id = /* unchanged: locate the metadata Map */;

    let kernelspec = /* unchanged */;
    let language_info = /* unchanged */;
    let runt = /* unchanged */;

    let mut extras = std::collections::BTreeMap::new();
    for key in doc.keys(&meta_id) {
        if matches!(key.as_str(), "kernelspec" | "language_info" | "runt") {
            continue;
        }
        if let Some(value) = read_json_value(doc, &meta_id, &key) {
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

Consider factoring the shared scan into a small helper (`fn scan_metadata_extras(doc: &AutoCommit, meta_id: &ObjId) -> BTreeMap<...>`) so the method and the free function stay in sync. Decide at implementation time based on how clean the call sites end up.

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

- `set_get_metadata_snapshot_round_trips_extras` — set a snapshot with `extras: {"jupytext": {...}, "colab": {...}}`; get back equal snapshot via `NotebookDoc::get_metadata_snapshot`.
- `get_metadata_snapshot_from_doc_reads_extras` — set a snapshot via `NotebookDoc::set_metadata_snapshot`; read back via the free-function `get_metadata_snapshot_from_doc`; assert extras land in the returned snapshot. Guards the notebook-sync / Python-bindings path.
- `set_metadata_snapshot_drops_extras_collision_with_kernelspec` — insert `"kernelspec"` into extras; assert log emits (captured via a test-mode layer or just assert the doc's `kernelspec` Map was not overwritten); typed `kernelspec` stays intact.
- `get_metadata_snapshot_returns_none_when_empty` — unchanged baseline test.
- `vanilla_notebook_save_does_not_stamp_runt` — build a snapshot with `RuntMetadata::default()` (what a vanilla Jupyter notebook produces after `from_metadata_value`); `serde_json::to_value(&snapshot)` must NOT contain a `runt` key. This pins the no-stamp behavior so a future `#[serde(skip_serializing_if)]` removal gets caught.

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
