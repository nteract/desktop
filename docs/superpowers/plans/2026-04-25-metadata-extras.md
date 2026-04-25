# Notebook Metadata Extras Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Automerge `NotebookDoc` carry all top-level notebook metadata keys (including unknown third-party keys like `jupytext`, `colab`, `vscode`) so clone and save preserve them without reading the on-disk `.ipynb` at save time.

**Architecture:** Add `extras: BTreeMap<String, Value>` with `#[serde(flatten)]` to `NotebookMetadataSnapshot`, `KernelspecSnapshot`, `LanguageInfoSnapshot`. `RuntMetadata` gets `is_empty()` + `#[serde(default, skip_serializing_if = "RuntMetadata::is_empty")]` so vanilla Jupyter notebooks round-trip without a synthetic `runt` stamp. `NotebookDoc::set_metadata_snapshot` guards against extras keys that collide with typed field names, logs `error!` on collision. `get_metadata_snapshot` and the free-function `get_metadata_snapshot_from_doc` both get a scan loop that collects unknown keys; they share the same helper. `persist.rs` stops reading the existing `.ipynb` for metadata (keeps reading it for the content-hash guard).

**Tech Stack:** Rust, `serde` + `serde_json`, `automerge`, `tokio`, `tracing`.

**Spec:** `docs/superpowers/specs/2026-04-25-metadata-extras.md`

---

## File structure

| File | Role in this plan |
|---|---|
| `crates/notebook-doc/src/metadata.rs` | Struct definitions (`NotebookMetadataSnapshot`, `KernelspecSnapshot`, `LanguageInfoSnapshot`, `RuntMetadata`), `from_metadata_value`, `merge_into_metadata_value`, `RuntMetadata::is_empty`. Unit tests for the extras serde contract. |
| `crates/notebook-doc/src/lib.rs` | `NotebookDoc::set_metadata_snapshot` (collision guard + extras write), `NotebookDoc::get_metadata_snapshot` (scan), free-function `get_metadata_snapshot_from_doc` (same scan, shared helper). Unit tests for the doc-level round-trip. |
| `crates/runtimed/src/notebook_sync_server/persist.rs` | Drop the metadata-rescue read of the existing `.ipynb`. Keep `existing_raw` for the content-hash guard. Integration test added in `tests.rs`. |
| `crates/runtimed/src/notebook_sync_server/tests.rs` | Integration tests: save round-trips unknown top-level keys; clone carries unknown keys to the clone doc. |

---

## Task 1: `RuntMetadata::is_empty` and `skip_serializing_if`

**Files:**
- Modify: `crates/notebook-doc/src/metadata.rs` (RuntMetadata definition around line 26; Default impl around line 638)

- [ ] **Step 1: Write the failing test for `is_empty`**

Append to the unit-test module at the bottom of `crates/notebook-doc/src/metadata.rs` (find `#[cfg(test)] mod tests` — if there is none, add one):

```rust
#[cfg(test)]
mod is_empty_tests {
    use super::*;

    #[test]
    fn default_runt_is_empty() {
        let runt = RuntMetadata::default();
        assert!(runt.is_empty(), "freshly-defaulted RuntMetadata should be empty");
    }

    #[test]
    fn runt_with_env_id_is_not_empty() {
        let mut runt = RuntMetadata::default();
        runt.env_id = Some("abc-123".to_string());
        assert!(!runt.is_empty());
    }

    #[test]
    fn runt_with_uv_is_not_empty() {
        let mut runt = RuntMetadata::default();
        runt.uv = Some(UvInlineMetadata {
            dependencies: vec!["pandas".to_string()],
            requires_python: None,
            prerelease: None,
        });
        assert!(!runt.is_empty());
    }

    #[test]
    fn runt_with_trust_signature_is_not_empty() {
        let mut runt = RuntMetadata::default();
        runt.trust_signature = Some("hmac-sha256:deadbeef".to_string());
        assert!(!runt.is_empty());
    }

    #[test]
    fn runt_with_extra_key_is_not_empty() {
        let mut runt = RuntMetadata::default();
        runt.extra.insert("future_field".to_string(), serde_json::json!(42));
        assert!(!runt.is_empty());
    }

    #[test]
    fn runt_with_modified_schema_version_is_not_empty() {
        let mut runt = RuntMetadata::default();
        runt.schema_version = "2".to_string();
        assert!(!runt.is_empty());
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p notebook-doc --lib is_empty_tests 2>&1 | tail -10`
Expected: FAIL with "no method named `is_empty` found for struct `RuntMetadata`".

- [ ] **Step 3: Implement `is_empty`**

Add after the existing `impl Default for RuntMetadata` block (around line 652 in the current file):

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
            && self.schema_version == "1"
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p notebook-doc --lib is_empty_tests 2>&1 | tail -10`
Expected: PASS, 6 tests passing.

- [ ] **Step 5: Commit**

```bash
git add crates/notebook-doc/src/metadata.rs
git commit -m "feat(notebook-doc): RuntMetadata::is_empty"
```

---

## Task 2: Extras field on `NotebookMetadataSnapshot`, with `#[serde(default, skip_serializing_if = "RuntMetadata::is_empty")]` on runt

**Files:**
- Modify: `crates/notebook-doc/src/metadata.rs` (NotebookMetadataSnapshot definition, currently around line 157)

- [ ] **Step 1: Write the failing tests for the struct shape**

Append to `crates/notebook-doc/src/metadata.rs`:

```rust
#[cfg(test)]
mod snapshot_extras_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn vanilla_snapshot_serializes_without_runt_key() {
        // A NotebookMetadataSnapshot with an empty default RuntMetadata
        // must NOT emit a `runt` key when serialized. This is the
        // "no synthetic runt stamp on vanilla notebooks" invariant.
        let snap = NotebookMetadataSnapshot::default();
        let v = serde_json::to_value(&snap).unwrap();
        let obj = v.as_object().expect("snapshot serializes to object");
        assert!(
            !obj.contains_key("runt"),
            "vanilla snapshot must not emit runt key, got: {v}"
        );
    }

    #[test]
    fn snapshot_with_runt_env_id_serializes_runt_key() {
        let mut snap = NotebookMetadataSnapshot::default();
        snap.runt.env_id = Some("abc".to_string());
        let v = serde_json::to_value(&snap).unwrap();
        assert!(v.as_object().unwrap().contains_key("runt"));
    }

    #[test]
    fn snapshot_deserializes_when_runt_absent() {
        // Vanilla Jupyter notebooks have no runt key. Must not fail.
        let v = json!({
            "kernelspec": {"name": "python3", "display_name": "Python 3"},
        });
        let snap: NotebookMetadataSnapshot = serde_json::from_value(v).unwrap();
        assert!(snap.runt.is_empty());
        assert_eq!(snap.kernelspec.as_ref().unwrap().name, "python3");
    }

    #[test]
    fn extras_round_trip_at_top_level() {
        let v = json!({
            "kernelspec": {"name": "python3", "display_name": "Python 3"},
            "jupytext": {"paired_paths": [["notebook.py", "py:percent"]]},
            "colab": {"kernel": {"name": "python3"}},
        });
        let snap: NotebookMetadataSnapshot = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(snap.extras.len(), 2);
        assert!(snap.extras.contains_key("jupytext"));
        assert!(snap.extras.contains_key("colab"));

        let round_tripped = serde_json::to_value(&snap).unwrap();
        assert_eq!(round_tripped["jupytext"], v["jupytext"]);
        assert_eq!(round_tripped["colab"], v["colab"]);
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p notebook-doc --lib snapshot_extras_tests 2>&1 | tail -10`
Expected: FAIL on all four tests (field `extras` not found, runt key still emitted, etc).

- [ ] **Step 3: Rewrite `NotebookMetadataSnapshot`**

Replace the existing struct definition (currently around lines 155-170 in `crates/notebook-doc/src/metadata.rs`) with:

```rust
/// Typed snapshot of notebook-level metadata for Automerge sync.
///
/// Three named fields (`kernelspec`, `language_info`, `runt`) plus a
/// catch-all `extras` bag for unknown/third-party top-level keys
/// (`jupytext`, `colab`, `vscode`, etc.). The flatten attribute means
/// unknown keys at deserialize land in `extras` automatically; on
/// serialize they emit at top level alongside the typed keys.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct NotebookMetadataSnapshot {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernelspec: Option<KernelspecSnapshot>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub language_info: Option<LanguageInfoSnapshot>,

    /// Runt-namespace metadata. Defaulted on deserialize so a notebook
    /// without `metadata.runt` (i.e. every vanilla Jupyter notebook)
    /// deserializes cleanly. Skipped on serialize when empty so we
    /// don't stamp a synthetic `runt: { schema_version: "1" }` blob
    /// on every save of an unrelated notebook.
    #[serde(default, skip_serializing_if = "RuntMetadata::is_empty")]
    pub runt: RuntMetadata,

    /// Catch-all for unknown/third-party top-level metadata keys.
    /// See `RuntMetadata::extra` for the analogous pattern one level
    /// deeper.
    #[serde(default, flatten)]
    pub extras: std::collections::BTreeMap<String, serde_json::Value>,
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p notebook-doc --lib snapshot_extras_tests 2>&1 | tail -10`
Expected: 4 tests pass.

- [ ] **Step 5: Run the full notebook-doc test suite to catch other breakage**

Run: `cargo test -p notebook-doc 2>&1 | tail -15`
Expected: all pre-existing tests still pass. If any fail with "extras field" mismatches, update them in-place to use `Default::default()` for the new field.

- [ ] **Step 6: Commit**

```bash
git add crates/notebook-doc/src/metadata.rs
git commit -m "feat(notebook-doc): NotebookMetadataSnapshot carries extras, skips empty runt"
```

---

## Task 3: Extras fields on `KernelspecSnapshot` and `LanguageInfoSnapshot`

**Files:**
- Modify: `crates/notebook-doc/src/metadata.rs` (KernelspecSnapshot around line 174, LanguageInfoSnapshot around line 188)

- [ ] **Step 1: Write failing tests**

Append to `crates/notebook-doc/src/metadata.rs`:

```rust
#[cfg(test)]
mod nested_extras_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn kernelspec_extras_round_trip() {
        // Standard Jupyter kernelspec often carries env, interrupt_mode,
        // metadata. These must survive load → save.
        let v = json!({
            "name": "python3",
            "display_name": "Python 3 (ipykernel)",
            "language": "python",
            "env": {"PYTHONPATH": "/opt/extra"},
            "interrupt_mode": "signal",
            "metadata": {"debugger": true},
        });
        let ks: KernelspecSnapshot = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(ks.extras.len(), 3);
        assert!(ks.extras.contains_key("env"));
        assert!(ks.extras.contains_key("interrupt_mode"));
        assert!(ks.extras.contains_key("metadata"));

        let out = serde_json::to_value(&ks).unwrap();
        assert_eq!(out["env"], v["env"]);
        assert_eq!(out["interrupt_mode"], v["interrupt_mode"]);
        assert_eq!(out["metadata"], v["metadata"]);
    }

    #[test]
    fn language_info_extras_round_trip() {
        // Jupyter kernels populate many fields after startup. All must
        // survive round-trip or save churns them on every notebook.
        let v = json!({
            "name": "python",
            "version": "3.11.5",
            "codemirror_mode": {"name": "ipython", "version": 3},
            "mimetype": "text/x-python",
            "file_extension": ".py",
            "nbconvert_exporter": "python",
            "pygments_lexer": "ipython3",
        });
        let li: LanguageInfoSnapshot = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(li.extras.len(), 5);
        for key in ["codemirror_mode", "mimetype", "file_extension",
                    "nbconvert_exporter", "pygments_lexer"] {
            assert!(li.extras.contains_key(key), "missing {key}");
        }

        let out = serde_json::to_value(&li).unwrap();
        assert_eq!(out["codemirror_mode"], v["codemirror_mode"]);
        assert_eq!(out["mimetype"], v["mimetype"]);
        assert_eq!(out["pygments_lexer"], v["pygments_lexer"]);
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p notebook-doc --lib nested_extras_tests 2>&1 | tail -10`
Expected: FAIL (extras field not found).

- [ ] **Step 3: Rewrite `KernelspecSnapshot` and `LanguageInfoSnapshot`**

Replace the existing `KernelspecSnapshot` definition (around lines 173-183):

```rust
/// Kernelspec snapshot for Automerge sync.
///
/// Mirrors standard Jupyter `kernelspec` fields plus an `extras` bag
/// so sub-keys we don't model (`env`, `interrupt_mode`, `metadata`)
/// still round-trip.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct KernelspecSnapshot {
    pub name: String,
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,

    /// Catch-all for unknown kernelspec sub-fields.
    #[serde(default, flatten)]
    pub extras: std::collections::BTreeMap<String, serde_json::Value>,
}
```

Replace the existing `LanguageInfoSnapshot` definition (around lines 188-195):

```rust
/// Language info snapshot for Automerge sync.
///
/// Jupyter kernels populate many fields here after startup
/// (`codemirror_mode`, `mimetype`, `file_extension`, `nbconvert_exporter`,
/// `pygments_lexer`). Extras bag preserves them.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LanguageInfoSnapshot {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,

    /// Catch-all for unknown language_info sub-fields.
    #[serde(default, flatten)]
    pub extras: std::collections::BTreeMap<String, serde_json::Value>,
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p notebook-doc --lib nested_extras_tests 2>&1 | tail -10`
Expected: 2 tests pass.

- [ ] **Step 5: Run the full notebook-doc test suite**

Run: `cargo test -p notebook-doc 2>&1 | tail -15`
Expected: all pass. Any pre-existing construction sites for `KernelspecSnapshot { name, display_name, language }` now need `..Default::default()` — fix them if compilation complains.

- [ ] **Step 6: Commit**

```bash
git add crates/notebook-doc/src/metadata.rs
git commit -m "feat(notebook-doc): KernelspecSnapshot + LanguageInfoSnapshot carry extras"
```

---

## Task 4: Rewrite `from_metadata_value` to use serde

**Files:**
- Modify: `crates/notebook-doc/src/metadata.rs` (from_metadata_value around line 205)

- [ ] **Step 1: Write failing tests for the new behavior**

Append to `crates/notebook-doc/src/metadata.rs`:

```rust
#[cfg(test)]
mod from_metadata_value_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn vanilla_jupyter_notebook_deserializes() {
        // Vanilla Jupyter notebook: kernelspec + language_info, no runt.
        // Must deserialize cleanly with an empty default RuntMetadata.
        let v = json!({
            "kernelspec": {
                "name": "python3",
                "display_name": "Python 3 (ipykernel)",
                "language": "python",
            },
            "language_info": {
                "name": "python",
                "version": "3.11.5",
            },
        });
        let snap = NotebookMetadataSnapshot::from_metadata_value(&v);
        assert!(snap.kernelspec.is_some());
        assert!(snap.language_info.is_some());
        assert!(snap.runt.is_empty());
        assert!(snap.extras.is_empty());
    }

    #[test]
    fn unknown_top_level_keys_become_extras() {
        let v = json!({
            "kernelspec": {"name": "python3", "display_name": "Python 3"},
            "jupytext": {"paired_paths": [["x.py", "py:percent"]]},
            "colab": {"kernel": {"name": "python3"}},
        });
        let snap = NotebookMetadataSnapshot::from_metadata_value(&v);
        assert!(snap.extras.contains_key("jupytext"));
        assert!(snap.extras.contains_key("colab"));
        assert!(!snap.extras.contains_key("kernelspec"));
    }

    #[test]
    fn legacy_top_level_uv_is_absorbed_into_runt() {
        // Legacy notebooks had metadata.uv at top level.
        // from_metadata_value must fold it into runt.uv so the save
        // path emits it at the new path, not both.
        let v = json!({
            "uv": {"dependencies": ["pandas"]},
        });
        let snap = NotebookMetadataSnapshot::from_metadata_value(&v);
        assert!(snap.runt.uv.is_some());
        assert_eq!(snap.runt.uv.as_ref().unwrap().dependencies, vec!["pandas"]);
        assert!(!snap.extras.contains_key("uv"),
            "legacy uv must be folded into runt, not left in extras");
    }

    #[test]
    fn legacy_top_level_conda_is_absorbed_into_runt() {
        let v = json!({
            "conda": {
                "dependencies": ["numpy"],
                "channels": ["conda-forge"],
            },
        });
        let snap = NotebookMetadataSnapshot::from_metadata_value(&v);
        assert!(snap.runt.conda.is_some());
        assert_eq!(snap.runt.conda.as_ref().unwrap().dependencies, vec!["numpy"]);
        assert!(!snap.extras.contains_key("conda"));
    }

    #[test]
    fn runt_wins_when_both_typed_and_legacy_present() {
        // Typed runt.uv wins over legacy top-level uv.
        let v = json!({
            "runt": {
                "schema_version": "1",
                "uv": {"dependencies": ["fresh"]},
            },
            "uv": {"dependencies": ["stale"]},
        });
        let snap = NotebookMetadataSnapshot::from_metadata_value(&v);
        assert_eq!(
            snap.runt.uv.as_ref().unwrap().dependencies,
            vec!["fresh"],
            "runt.uv must win over legacy top-level uv"
        );
        assert!(!snap.extras.contains_key("uv"));
    }

    #[test]
    fn full_round_trip_preserves_all_levels() {
        // Top-level extras, kernelspec extras, language_info extras,
        // and runt all present; every key must survive load → save.
        let v = json!({
            "kernelspec": {
                "name": "python3",
                "display_name": "Python 3",
                "language": "python",
                "env": {"A": "1"},
            },
            "language_info": {
                "name": "python",
                "version": "3.11.5",
                "codemirror_mode": {"name": "ipython", "version": 3},
                "file_extension": ".py",
            },
            "runt": {
                "schema_version": "1",
                "uv": {"dependencies": ["pandas"]},
            },
            "jupytext": {"paired_paths": [["x.py", "py:percent"]]},
            "vscode": {"extension": {"id": "ms-python.python"}},
        });
        let snap = NotebookMetadataSnapshot::from_metadata_value(&v);
        let out = serde_json::to_value(&snap).unwrap();

        assert_eq!(out["kernelspec"]["env"], v["kernelspec"]["env"]);
        assert_eq!(
            out["language_info"]["codemirror_mode"],
            v["language_info"]["codemirror_mode"]
        );
        assert_eq!(
            out["language_info"]["file_extension"],
            v["language_info"]["file_extension"]
        );
        assert_eq!(out["runt"]["uv"], v["runt"]["uv"]);
        assert_eq!(out["jupytext"], v["jupytext"]);
        assert_eq!(out["vscode"], v["vscode"]);
    }
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p notebook-doc --lib from_metadata_value_tests 2>&1 | tail -20`
Expected: several FAIL. The existing `from_metadata_value` doesn't populate extras.

- [ ] **Step 3: Rewrite `from_metadata_value`**

Replace the existing implementation (currently around lines 205-244) with:

```rust
/// Build a snapshot from raw notebook metadata JSON (from an .ipynb).
///
/// Uses `serde_json::from_value::<Self>` so all three snapshot levels
/// (top, kernelspec, language_info) populate their `extras` bags in
/// one pass. `#[serde(default)]` on `runt` means vanilla Jupyter
/// notebooks (no `metadata.runt` key) deserialize cleanly.
///
/// After the serde pass, runs one legacy fallback: if `runt.uv` or
/// `runt.conda` is unset but a top-level `uv` or `conda` exists (old
/// pre-`runt.*` notebooks), fold them into `runt` and remove from
/// extras so save doesn't emit them at both depths.
pub fn from_metadata_value(metadata: &serde_json::Value) -> Self {
    let mut snapshot: NotebookMetadataSnapshot =
        serde_json::from_value(metadata.clone()).unwrap_or_default();

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

- [ ] **Step 4: Run the new tests**

Run: `cargo test -p notebook-doc --lib from_metadata_value_tests 2>&1 | tail -15`
Expected: 6 tests pass.

- [ ] **Step 5: Run the full notebook-doc suite to catch regressions**

Run: `cargo test -p notebook-doc 2>&1 | tail -15`
Expected: all pass. Pre-existing `from_metadata_value` tests may still pass because the behavior contract for known keys hasn't changed. If any fail, inspect: the only behavior change from today is "malformed field nukes whole snapshot" vs. today's "per-field tolerance." Acceptable per spec; update test expectations if needed.

- [ ] **Step 6: Commit**

```bash
git add crates/notebook-doc/src/metadata.rs
git commit -m "feat(notebook-doc): from_metadata_value uses serde flatten for extras"
```

---

## Task 5: Collision guard and extras write in `NotebookDoc::set_metadata_snapshot`

**Files:**
- Modify: `crates/notebook-doc/src/lib.rs` (set_metadata_snapshot around line 393)

- [ ] **Step 1: Write failing tests**

Find or create a test module in `crates/notebook-doc/src/lib.rs`. Use an existing `#[cfg(test)] mod tests` block (there's one already) and append:

```rust
#[test]
fn set_metadata_snapshot_writes_extras_as_siblings() {
    use crate::metadata::NotebookMetadataSnapshot;
    let mut doc = NotebookDoc::new_with_actor("test-nb", "test");
    let mut snap = NotebookMetadataSnapshot::default();
    snap.extras.insert(
        "jupytext".to_string(),
        serde_json::json!({"paired_paths": [["x.py", "py:percent"]]}),
    );
    snap.extras.insert(
        "colab".to_string(),
        serde_json::json!({"kernel": {"name": "python3"}}),
    );
    doc.set_metadata_snapshot(&snap).unwrap();

    let round_tripped = doc.get_metadata_snapshot().unwrap();
    assert_eq!(round_tripped.extras.len(), 2);
    assert_eq!(
        round_tripped.extras.get("jupytext"),
        Some(&serde_json::json!({"paired_paths": [["x.py", "py:percent"]]}))
    );
    assert_eq!(
        round_tripped.extras.get("colab"),
        Some(&serde_json::json!({"kernel": {"name": "python3"}}))
    );
}

#[test]
fn set_metadata_snapshot_drops_extras_colliding_with_kernelspec() {
    use crate::metadata::{KernelspecSnapshot, NotebookMetadataSnapshot};
    let mut doc = NotebookDoc::new_with_actor("test-nb", "test");

    let mut snap = NotebookMetadataSnapshot::default();
    snap.kernelspec = Some(KernelspecSnapshot {
        name: "python3".to_string(),
        display_name: "Python 3".to_string(),
        language: Some("python".to_string()),
        extras: Default::default(),
    });
    // Caller bug: sneaking "kernelspec" into extras would double-write.
    // Guard must drop the extras entry and leave the typed kernelspec
    // intact.
    snap.extras.insert(
        "kernelspec".to_string(),
        serde_json::json!({"BAD": true}),
    );

    doc.set_metadata_snapshot(&snap).unwrap();

    let round_tripped = doc.get_metadata_snapshot().unwrap();
    assert_eq!(
        round_tripped.kernelspec.as_ref().unwrap().name,
        "python3",
        "typed kernelspec must survive collision; extras must be dropped"
    );
    assert!(
        !round_tripped.extras.contains_key("kernelspec"),
        "collision-dropped key must not appear in extras on read"
    );
}

#[test]
fn set_metadata_snapshot_drops_extras_colliding_with_language_info() {
    use crate::metadata::{LanguageInfoSnapshot, NotebookMetadataSnapshot};
    let mut doc = NotebookDoc::new_with_actor("test-nb", "test");

    let mut snap = NotebookMetadataSnapshot::default();
    snap.language_info = Some(LanguageInfoSnapshot {
        name: "python".to_string(),
        version: Some("3.11.5".to_string()),
        extras: Default::default(),
    });
    snap.extras.insert(
        "language_info".to_string(),
        serde_json::json!({"BAD": true}),
    );

    doc.set_metadata_snapshot(&snap).unwrap();

    let round_tripped = doc.get_metadata_snapshot().unwrap();
    assert_eq!(
        round_tripped.language_info.as_ref().unwrap().name,
        "python"
    );
    assert!(!round_tripped.extras.contains_key("language_info"));
}

#[test]
fn set_metadata_snapshot_drops_extras_colliding_with_runt() {
    use crate::metadata::NotebookMetadataSnapshot;
    let mut doc = NotebookDoc::new_with_actor("test-nb", "test");

    let mut snap = NotebookMetadataSnapshot::default();
    snap.runt.env_id = Some("real-env-id".to_string());
    snap.extras.insert(
        "runt".to_string(),
        serde_json::json!({"env_id": "bogus"}),
    );

    doc.set_metadata_snapshot(&snap).unwrap();

    let round_tripped = doc.get_metadata_snapshot().unwrap();
    assert_eq!(
        round_tripped.runt.env_id.as_deref(),
        Some("real-env-id"),
        "typed runt must win over colliding extras"
    );
    assert!(!round_tripped.extras.contains_key("runt"));
}
```

- [ ] **Step 2: Run to confirm failure**

Run: `cargo test -p notebook-doc --lib set_metadata_snapshot_ 2>&1 | tail -15`
Expected: all FAIL. `set_metadata_snapshot` doesn't write extras yet, and `get_metadata_snapshot` doesn't read them.

- [ ] **Step 3: Update `NotebookDoc::set_metadata_snapshot`**

Locate the current implementation (around line 393 in `crates/notebook-doc/src/lib.rs`). Add the extras loop after the existing `update_json_at_key(&mut self.doc, &meta_id, "runt", &runt_v)?;` line. The full method now looks like:

```rust
pub fn set_metadata_snapshot(
    &mut self,
    snapshot: &metadata::NotebookMetadataSnapshot,
) -> Result<(), AutomergeError> {
    let meta_id = match self.metadata_map_id() {
        Some(id) => id,
        None => self
            .doc
            .put_object(automerge::ROOT, "metadata", ObjType::Map)?,
    };

    match &snapshot.kernelspec {
        Some(ks) => {
            let v = serde_json::to_value(ks).map_err(|e| {
                AutomergeError::InvalidObjId(format!("serialize kernelspec: {}", e))
            })?;
            update_json_at_key(&mut self.doc, &meta_id, "kernelspec", &v)?;
        }
        None => {
            let _ = self.doc.delete(&meta_id, "kernelspec");
        }
    }

    match &snapshot.language_info {
        Some(li) => {
            let v = serde_json::to_value(li).map_err(|e| {
                AutomergeError::InvalidObjId(format!("serialize language_info: {}", e))
            })?;
            update_json_at_key(&mut self.doc, &meta_id, "language_info", &v)?;
        }
        None => {
            let _ = self.doc.delete(&meta_id, "language_info");
        }
    }

    // Write runt only when non-empty so vanilla Jupyter notebooks
    // round-trip without stamping a synthetic runt blob.
    if snapshot.runt.is_empty() {
        let _ = self.doc.delete(&meta_id, "runt");
    } else {
        let runt_v = serde_json::to_value(&snapshot.runt)
            .map_err(|e| AutomergeError::InvalidObjId(format!("serialize runt: {}", e)))?;
        update_json_at_key(&mut self.doc, &meta_id, "runt", &runt_v)?;
    }

    // Write extras. Each key becomes its own Automerge Map so concurrent
    // edits to metadata.jupytext.* from two peers merge per-field.
    // Guard against callers that stuff known top-level keys into
    // extras — those would double-write at the same Automerge key.
    for (key, value) in &snapshot.extras {
        if matches!(key.as_str(), "kernelspec" | "language_info" | "runt") {
            tracing::error!(
                "[notebook-doc] metadata.extras collision: key {:?} \
                 is reserved for a typed field; dropping to avoid \
                 Automerge double-write. This indicates a caller bug \
                 in snapshot construction.",
                key
            );
            continue;
        }
        update_json_at_key(&mut self.doc, &meta_id, key, value)?;
    }

    Ok(())
}
```

Note that the runt-empty branch now calls `delete` — this is the other half of the "no stamping" behavior: if a user clears all runt content, we remove the key from the doc entirely.

- [ ] **Step 4: Run tests (expect 2 of 4 still failing)**

Run: `cargo test -p notebook-doc --lib set_metadata_snapshot_ 2>&1 | tail -15`
Expected: the "collision drops" tests pass; the "writes extras as siblings" test still FAILS until `get_metadata_snapshot` is taught to scan (Task 6).

If any test fails for the wrong reason (e.g. a type mismatch), fix before proceeding.

- [ ] **Step 5: Commit**

```bash
git add crates/notebook-doc/src/lib.rs
git commit -m "feat(notebook-doc): set_metadata_snapshot writes extras + collision guard"
```

---

## Task 6: Shared extras-scan helper; wire both `get_metadata_snapshot` paths

**Files:**
- Modify: `crates/notebook-doc/src/lib.rs` (`NotebookDoc::get_metadata_snapshot` around line 368; free-function `get_metadata_snapshot_from_doc` around line 1930)

- [ ] **Step 1: Add a private helper for the scan**

Insert this function in `crates/notebook-doc/src/lib.rs`, near the existing `fn read_cell_metadata` (around line 1883):

```rust
/// Scan an Automerge metadata Map for top-level keys that aren't
/// modeled by `NotebookMetadataSnapshot`'s typed fields.
///
/// Shared by `NotebookDoc::get_metadata_snapshot` and the free-function
/// `get_metadata_snapshot_from_doc`. Both must behave identically —
/// different behavior would mean the frontend sync snapshot and
/// Python bindings disagree with the daemon's view of the same doc.
fn scan_metadata_extras(
    doc: &AutoCommit,
    meta_id: &ObjId,
) -> std::collections::BTreeMap<String, serde_json::Value> {
    let mut extras = std::collections::BTreeMap::new();
    for key in doc.keys(meta_id) {
        if matches!(key.as_str(), "kernelspec" | "language_info" | "runt") {
            continue;
        }
        if let Some(value) = read_json_value(doc, meta_id, &key) {
            extras.insert(key, value);
        }
    }
    extras
}
```

- [ ] **Step 2: Update `NotebookDoc::get_metadata_snapshot`**

Replace the current body (around lines 368-387 in `crates/notebook-doc/src/lib.rs`):

```rust
pub fn get_metadata_snapshot(&self) -> Option<metadata::NotebookMetadataSnapshot> {
    let meta_id = self.metadata_map_id()?;

    let kernelspec = read_json_value(&self.doc, &meta_id, "kernelspec")
        .and_then(|v| serde_json::from_value::<metadata::KernelspecSnapshot>(v).ok());
    let language_info = read_json_value(&self.doc, &meta_id, "language_info")
        .and_then(|v| serde_json::from_value::<metadata::LanguageInfoSnapshot>(v).ok());
    let runt = read_json_value(&self.doc, &meta_id, "runt")
        .and_then(|v| serde_json::from_value::<metadata::RuntMetadata>(v).ok());

    let extras = scan_metadata_extras(&self.doc, &meta_id);

    if kernelspec.is_some()
        || language_info.is_some()
        || runt.is_some()
        || !extras.is_empty()
    {
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

- [ ] **Step 3: Update the free-function `get_metadata_snapshot_from_doc`**

Replace the current body (around lines 1930-1958 in `crates/notebook-doc/src/lib.rs`):

```rust
pub fn get_metadata_snapshot_from_doc(
    doc: &AutoCommit,
) -> Option<metadata::NotebookMetadataSnapshot> {
    let meta_id = doc
        .get(automerge::ROOT, "metadata")
        .ok()
        .flatten()
        .and_then(|(value, id)| match value {
            automerge::Value::Object(ObjType::Map) => Some(id),
            _ => None,
        })?;

    let kernelspec = read_json_value(doc, &meta_id, "kernelspec")
        .and_then(|v| serde_json::from_value::<metadata::KernelspecSnapshot>(v).ok());
    let language_info = read_json_value(doc, &meta_id, "language_info")
        .and_then(|v| serde_json::from_value::<metadata::LanguageInfoSnapshot>(v).ok());
    let runt = read_json_value(doc, &meta_id, "runt")
        .and_then(|v| serde_json::from_value::<metadata::RuntMetadata>(v).ok());

    let extras = scan_metadata_extras(doc, &meta_id);

    if kernelspec.is_some()
        || language_info.is_some()
        || runt.is_some()
        || !extras.is_empty()
    {
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

- [ ] **Step 4: Add a test for the free-function path**

Append to the `#[cfg(test)] mod tests` block in `crates/notebook-doc/src/lib.rs`:

```rust
#[test]
fn get_metadata_snapshot_from_doc_reads_extras() {
    use crate::metadata::NotebookMetadataSnapshot;
    let mut doc = NotebookDoc::new_with_actor("test-nb", "test");
    let mut snap = NotebookMetadataSnapshot::default();
    snap.extras.insert(
        "jupytext".to_string(),
        serde_json::json!({"paired_paths": [["x.py", "py:percent"]]}),
    );
    doc.set_metadata_snapshot(&snap).unwrap();

    // Read via the free-function path (used by notebook-sync +
    // Python bindings, not the &self method).
    let round_tripped = crate::get_metadata_snapshot_from_doc(doc.doc())
        .expect("free function should surface extras");
    assert!(round_tripped.extras.contains_key("jupytext"));
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p notebook-doc --lib 2>&1 | tail -20`
Expected: all pass. The earlier-failing `set_metadata_snapshot_writes_extras_as_siblings` from Task 5 also passes now.

- [ ] **Step 6: Commit**

```bash
git add crates/notebook-doc/src/lib.rs
git commit -m "feat(notebook-doc): extras scan on both get_metadata_snapshot paths"
```

---

## Task 7: Drop the metadata-rescue read in `persist.rs`

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server/persist.rs` (the metadata-build block around lines 66-165)

- [ ] **Step 1: Rewrite the metadata-prep block**

In `save_notebook_to_disk`, replace the block from "Read existing .ipynb as raw bytes" through "Build metadata by merging synced snapshot onto existing" / "snapshot.merge_into_metadata_value" with the simpler version:

```rust
    // Read existing .ipynb as raw bytes. Used for two things: the
    // content-hash guard further down (skip no-op writes), and the
    // `nbformat_minor` floor (we don't carry that in the doc today).
    // We no longer read metadata from disk — the doc carries unknown
    // top-level keys as extras, so everything round-trips through
    // the snapshot.
    let existing_raw: Option<Vec<u8>> = match tokio::fs::read(&notebook_path).await {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            warn!(
                "[notebook-sync] Failed to read existing notebook {:?}: {}, \
                 will create new",
                notebook_path, e
            );
            None
        }
    };
```

Delete the `let existing: Option<serde_json::Value> = ...` parse (around lines 80-93) and everything from "Build metadata by merging synced snapshot onto existing" through the `snapshot.merge_into_metadata_value` call. Replace with:

```rust
    // nbformat_minor: pull from existing file if present, floor at 5.
    let existing_minor = existing_raw
        .as_ref()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).ok())
        .and_then(|nb| nb.get("nbformat_minor").and_then(|v| v.as_u64()))
        .unwrap_or(5) as i32;
    let nbformat_minor = std::cmp::max(existing_minor, 5);

    // Metadata comes entirely from the doc. No disk rescue; the
    // snapshot carries unknown keys as extras.
    let metadata = metadata_snapshot
        .as_ref()
        .map(|s| serde_json::to_value(s).unwrap_or_else(|_| serde_json::json!({})))
        .unwrap_or_else(|| serde_json::json!({}));
```

- [ ] **Step 2: Run persist unit tests to confirm compile + behavior**

Run: `cargo test -p runtimed --lib notebook_sync_server::persist 2>&1 | tail -10`
Expected: compiles. If there are failures, read carefully — most likely they're about the `existing` binding no longer existing. Update the affected test code.

- [ ] **Step 3: Run the full runtimed sync-server suite**

Run: `cargo test -p runtimed --lib notebook_sync_server 2>&1 | tail -10`
Expected: pass or a small number of test failures related to pre-existing expectations about metadata preservation that are now handled differently (through the doc, not via disk rescue).

If any failures, they likely come from tests that constructed a `NotebookRoom` without going through the load path, so the doc lacks metadata the tests then expected to see after save. Inspect + fix the test to set the expected metadata on the doc via `set_metadata_snapshot` before saving.

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server/persist.rs
git commit -m "refactor(runtimed): persist.rs stops reading existing .ipynb for metadata"
```

---

## Task 8: Integration test — save round-trips unknown metadata

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server/tests.rs` (append a new `#[tokio::test]`)

- [ ] **Step 1: Append the test**

Add to the bottom of `crates/runtimed/src/notebook_sync_server/tests.rs`:

```rust
#[tokio::test]
async fn test_save_round_trips_unknown_top_level_metadata() {
    // Regression test for Codex F3 on PR #2192: unknown top-level
    // metadata keys (jupytext, colab, etc.) must survive save.
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, notebook_path) = test_room_with_path(&tmp, "with-jupytext.ipynb");

    // Write an .ipynb with unknown metadata keys, then load it through
    // the normal load path so the doc carries extras.
    std::fs::write(
        &notebook_path,
        r#"{
 "cells": [],
 "metadata": {
  "kernelspec": {"name": "python3", "display_name": "Python 3", "language": "python"},
  "language_info": {"name": "python", "version": "3.11.5"},
  "jupytext": {"paired_paths": [["x.py", "py:percent"]]},
  "colab": {"kernel": {"name": "python3"}}
 },
 "nbformat": 4,
 "nbformat_minor": 5
}"#,
    )
    .unwrap();

    // Seed the room's doc from the file via the normal load path.
    {
        let mut doc = room.doc.write().await;
        crate::notebook_sync_server::load_notebook_from_disk(
            &mut doc,
            &notebook_path,
            &room.blob_store,
        )
        .await
        .unwrap();
    }

    // Save, then re-read the file.
    save_notebook_to_disk(&room, None).await.unwrap();

    let written = std::fs::read_to_string(&notebook_path).unwrap();
    let nb: serde_json::Value = serde_json::from_str(&written).unwrap();

    assert_eq!(
        nb["metadata"]["jupytext"],
        serde_json::json!({"paired_paths": [["x.py", "py:percent"]]}),
        "jupytext key must survive save round-trip"
    );
    assert_eq!(
        nb["metadata"]["colab"],
        serde_json::json!({"kernel": {"name": "python3"}}),
        "colab key must survive save round-trip"
    );
}

#[tokio::test]
async fn test_save_does_not_stamp_synthetic_runt_on_vanilla_notebook() {
    // Vanilla Jupyter notebook: no metadata.runt. Save must NOT add
    // `runt: { schema_version: "1" }` — that would churn every
    // git-tracked Jupyter notebook the user opens.
    let tmp = tempfile::TempDir::new().unwrap();
    let (room, notebook_path) = test_room_with_path(&tmp, "vanilla.ipynb");

    std::fs::write(
        &notebook_path,
        r#"{
 "cells": [],
 "metadata": {
  "kernelspec": {"name": "python3", "display_name": "Python 3", "language": "python"},
  "language_info": {"name": "python", "version": "3.11.5"}
 },
 "nbformat": 4,
 "nbformat_minor": 5
}"#,
    )
    .unwrap();

    {
        let mut doc = room.doc.write().await;
        crate::notebook_sync_server::load_notebook_from_disk(
            &mut doc,
            &notebook_path,
            &room.blob_store,
        )
        .await
        .unwrap();
    }

    save_notebook_to_disk(&room, None).await.unwrap();

    let written = std::fs::read_to_string(&notebook_path).unwrap();
    let nb: serde_json::Value = serde_json::from_str(&written).unwrap();

    assert!(
        !nb["metadata"].as_object().unwrap().contains_key("runt"),
        "vanilla notebook save must not stamp metadata.runt, got: {}",
        nb["metadata"]
    );
}
```

- [ ] **Step 2: Run the new tests**

Run: `cargo test -p runtimed --lib test_save_round_trips_unknown_top_level_metadata test_save_does_not_stamp_synthetic_runt_on_vanilla_notebook 2>&1 | tail -15`

Note: cargo test accepts multiple filters. If the above doesn't match, run them one at a time.

Expected: both tests pass.

- [ ] **Step 3: Full suite**

Run: `cargo test -p runtimed --lib notebook_sync_server 2>&1 | tail -10`
Expected: pass.

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server/tests.rs
git commit -m "test(runtimed): save round-trips unknown metadata + no vanilla runt stamp"
```

---

## Task 9: Integration test — clone carries unknown metadata to the clone doc

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server/tests.rs` (extend the existing clone test or add a sibling)

- [ ] **Step 1: Append a new clone test**

Add to the bottom of `crates/runtimed/src/notebook_sync_server/tests.rs`:

```rust
#[tokio::test]
async fn test_clone_as_ephemeral_carries_unknown_metadata_extras() {
    // Codex F3 on PR #2192: clone must preserve unknown top-level
    // metadata keys from source (jupytext, colab, vscode, etc.).
    let tmp = tempfile::TempDir::new().unwrap();
    let (rooms, path_index, docs_dir, blob_store) = clone_test_scaffolding(&tmp);

    let source_uuid = Uuid::new_v4();
    let source_room = get_or_create_room(
        &rooms,
        &path_index,
        source_uuid,
        Some(tmp.path().join("source.ipynb")),
        &docs_dir,
        blob_store.clone(),
        false,
    )
    .await;

    // Seed source doc with kernelspec (required for snapshot to be
    // Some) plus unknown extras.
    {
        let mut doc = source_room.doc.write().await;
        let mut snap = snapshot_empty();
        snap.kernelspec = Some(crate::notebook_metadata::KernelspecSnapshot {
            name: "python3".to_string(),
            display_name: "Python 3".to_string(),
            language: Some("python".to_string()),
            extras: Default::default(),
        });
        snap.extras.insert(
            "jupytext".to_string(),
            serde_json::json!({"paired_paths": [["x.py", "py:percent"]]}),
        );
        snap.extras.insert(
            "vscode".to_string(),
            serde_json::json!({"extension": {"id": "ms-python.python"}}),
        );
        doc.set_metadata_snapshot(&snap).unwrap();
    }

    let response = crate::requests::clone_notebook::handle_inner(
        &rooms,
        &path_index,
        &docs_dir,
        blob_store.clone(),
        source_uuid.to_string(),
    )
    .await;

    let clone_id = match response {
        NotebookResponse::NotebookCloned { notebook_id, .. } => notebook_id,
        other => panic!("Expected NotebookCloned, got {other:?}"),
    };
    let clone_uuid = Uuid::parse_str(&clone_id).unwrap();
    let clone_room = rooms
        .lock()
        .await
        .get(&clone_uuid)
        .cloned()
        .expect("clone room should be registered");

    let clone_snap = clone_room
        .doc
        .read()
        .await
        .get_metadata_snapshot()
        .expect("clone has metadata");
    assert!(
        clone_snap.extras.contains_key("jupytext"),
        "jupytext must survive clone; extras: {:?}",
        clone_snap.extras.keys().collect::<Vec<_>>()
    );
    assert!(
        clone_snap.extras.contains_key("vscode"),
        "vscode must survive clone; extras: {:?}",
        clone_snap.extras.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        clone_snap.extras["jupytext"],
        serde_json::json!({"paired_paths": [["x.py", "py:percent"]]})
    );
}
```

- [ ] **Step 2: Run**

Run: `cargo test -p runtimed --lib test_clone_as_ephemeral_carries_unknown_metadata_extras 2>&1 | tail -10`
Expected: pass.

- [ ] **Step 3: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server/tests.rs
git commit -m "test(runtimed): clone carries unknown metadata extras"
```

---

## Task 10: Final sweep, lint, and PR-ready verification

**Files:** none new

- [ ] **Step 1: Sweep for any remaining `merge_into_metadata_value` callers**

Run: `grep -rn "merge_into_metadata_value" crates/ 2>&1 | grep -v target`
Expected: only the definition in `crates/notebook-doc/src/metadata.rs` and the unit test at line ~913. If there are other prod callers, revisit Task 7 — the persist path should be the only consumer.

If the unit test at line 913 remains and still calls the function, leave it. The function is still valid public API even if no prod code uses it (callers outside this repo may depend on it via the crate).

- [ ] **Step 2: Clippy + fmt**

Run: `cargo clippy -p notebook-doc -p runtimed --tests -- -D warnings 2>&1 | tail -5`
Expected: clean.

Run: `cargo fmt --check -p notebook-doc -p runtimed 2>&1 | tail -3`
Expected: no output (clean).

- [ ] **Step 3: Workspace build**

Run: `cargo check --workspace 2>&1 | tail -5`
Expected: clean.

- [ ] **Step 4: Full impacted-crate test suites**

```bash
cargo test -p notebook-doc 2>&1 | tail -10
cargo test -p runtimed --lib notebook_sync_server 2>&1 | tail -5
cargo test -p notebook-sync 2>&1 | tail -5
```

Expected: all pass.

- [ ] **Step 5: Manual QA checklist (human)**

- [ ] Open a notebook that has `metadata.jupytext` on disk (any jupytext-paired notebook works). Confirm the daemon loads it. Edit a cell; wait for autosave. Re-read the file. `metadata.jupytext` must still be present and unchanged.
- [ ] Open a vanilla Jupyter notebook with no `metadata.runt`. Edit and save. Re-read the file. `metadata.runt` must NOT have been added.
- [ ] Open the same jupytext notebook, Clone it. Save the clone to a new path. `metadata.jupytext` must appear in the new file too.

---

## Self-review notes

**Spec coverage check:**
- Extras on NotebookMetadataSnapshot: Task 2.
- Extras on KernelspecSnapshot + LanguageInfoSnapshot: Task 3.
- RuntMetadata::is_empty + skip_serializing_if: Tasks 1 + 2.
- from_metadata_value rewrite (serde + default, legacy fallback): Task 4.
- NotebookDoc::set_metadata_snapshot (collision guard + extras write + runt-empty delete): Task 5.
- NotebookDoc::get_metadata_snapshot scan: Task 6.
- Free-function get_metadata_snapshot_from_doc scan (Codex P2): Task 6 (shared helper).
- persist.rs drops existing-file metadata read: Task 7.
- Tests pinning vanilla-no-runt-stamp invariant: Tasks 2 + 8.
- Tests pinning notebook-sync free-function path: Task 6.
- Clone carries unknown metadata: Task 9.

All spec items have a task.

**Type consistency:**
- `NotebookMetadataSnapshot.extras: BTreeMap<String, Value>` — same shape across Task 2 (definition), Task 4 (populated in `from_metadata_value`), Task 5 (iterated in `set_metadata_snapshot`), Task 6 (populated in scan).
- `scan_metadata_extras` signature: `(doc: &AutoCommit, meta_id: &ObjId) -> BTreeMap<String, Value>`. Used identically by both get-snapshot paths in Task 6.
- `RuntMetadata::is_empty`: free method on `RuntMetadata`, no args, returns `bool`. Task 1 defines; Task 2 uses in `skip_serializing_if`; Task 5 uses in `set_metadata_snapshot` to choose between write and delete.

No inconsistencies.

**Placeholder check:** All steps have concrete code. No "similar to" references; repeated blocks are repeated verbatim. No TODO/TBD.

One flagged implementation-time concern in Task 7 Step 3: pre-existing tests in `notebook_sync_server` may fail if they constructed rooms without going through load and depended on disk rescue for metadata. The plan says "inspect + fix the test." If many tests need fixing, that's a signal we missed something. Batch-review those together rather than forcing the test update one-by-one.
