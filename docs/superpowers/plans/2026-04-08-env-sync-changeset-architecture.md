# Environment Sync Changeset Architecture

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace string-matching and full-metadata-materialization in the daemon's environment sync with patch-based change detection and typed environment intents, aligning with the frontend's CellChangeset pattern.

**Architecture:** The daemon currently runs `check_and_broadcast_sync_state()` after every Automerge sync frame — even keystrokes that only touch cell source. It materializes the full metadata JSON, parses it, and does O(n) string comparison against the launched config baseline. This plan introduces three changes: (1) capture Automerge heads before/after sync to skip non-metadata frames cheaply, (2) replace `env_type: &str` with a typed `EnvKind` enum that carries its data, eliminating string-matching bugs, (3) replace the flat `SyncEnvironment { packages, channels }` protocol message with a properly discriminated type.

**Tech Stack:** Rust (automerge, serde, tokio), TypeScript (React frontend)

**Crate boundaries:**
- `notebook-doc`: New `MetadataChangeset` type + `diff_metadata()` function (parallel to `CellChangeset` + `diff_cells()`)
- `notebook-protocol`: New `EnvKind` enum, updated `SyncEnvironment` variant
- `runtimed`: Updated sync handler, `check_and_broadcast_sync_state()`, `handle_sync_environment()`, runtime agent

**What this does NOT change:**
- Initial kernel launch flow (imperative metadata read is correct at launch time — no "before" heads to diff against)
- Pool/prewarming code
- Frontend dependency UI components
- Execution queue (already CRDT-driven — no changes needed)
- Pixi TOML file-based drift detection (inherently file-based, not CRDT-based)

---

## Background: How It Works Today

### The Problem

`notebook_sync_server.rs:2443` calls `check_and_broadcast_sync_state(room)` after **every** Automerge sync frame. That function:

1. Acquires read lock on `room.doc` and materializes full `NotebookMetadataSnapshot` (parses 3 JSON strings from Automerge)
2. Acquires read lock on `room.runtime_agent_launched_config` and clones it
3. Acquires read lock on `room.state_doc` and reads kernel status
4. Calls `compute_env_sync_diff()` which does linear array search on every dep list
5. If drift detected, acquires write lock on `room.state_doc` and updates env state

This runs ~50x/second during typing (every 20ms debounced sync). 95%+ of these calls are for cell source edits that don't touch metadata at all.

### The Proven Pattern

`diff_cells()` in `notebook-doc/src/diff.rs` solves the same problem for cells:

```rust
let heads_before = doc.get_heads();
doc.receive_sync_message(&mut peer_state, message)?;
let heads_after = doc.get_heads();

if heads_before != heads_after {
    let changeset = diff_cells(doc, &heads_before, &heads_after);
    // Only process cells that actually changed
}
```

Cost: O(delta), not O(document). Only fires when something changed.

### The Goal

```rust
// After receiving sync frame:
let heads_before = doc.get_heads();
doc.receive_sync_message(&mut peer_state, message)?;
let heads_after = doc.get_heads();

let meta_changed = diff_metadata_touched(doc, &heads_before, &heads_after);
if meta_changed {
    check_and_broadcast_sync_state(room).await;
}
```

This eliminates ~95% of `check_and_broadcast_sync_state()` calls.

---

## File Structure

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/notebook-doc/src/diff.rs` | Modify | Add `diff_metadata_touched()` function |
| `crates/notebook-doc/src/lib.rs` | Modify | Re-export new diff function |
| `crates/notebook-protocol/src/protocol.rs` | Modify | Add `EnvKind` enum, update `SyncEnvironment` |
| `crates/runtimed/src/notebook_sync_server.rs` | Modify | Capture heads, gate `check_and_broadcast_sync_state()`, use `EnvKind` |
| `crates/runtimed/src/runtime_agent.rs` | Modify | Use `EnvKind` in SyncEnvironment handler |

---

## Task 1: Add `diff_metadata_touched()` to notebook-doc

Lightweight check: did any patch touch the `metadata` map? No parsing, no materialization — just path inspection.

**Files:**
- Modify: `crates/notebook-doc/src/diff.rs`
- Modify: `crates/notebook-doc/src/lib.rs`
- Test: `crates/notebook-doc/src/diff.rs` (existing test module)

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` in `diff.rs`:

```rust
#[test]
fn test_diff_metadata_touched_returns_false_for_cell_source_edit() {
    let mut doc = AutoCommit::new();

    // Bootstrap minimal doc structure
    let cells = doc.put_object(automerge::ROOT, "cells", automerge::ObjType::Map).unwrap();
    let cell = doc.put_object(&cells, "cell-1", automerge::ObjType::Map).unwrap();
    let source = doc.put_object(&cell, "source", automerge::ObjType::Text).unwrap();
    doc.splice_text(&source, 0, 0, "hello").unwrap();

    let metadata = doc.put_object(automerge::ROOT, "metadata", automerge::ObjType::Map).unwrap();
    doc.put(&metadata, "runt", automerge::ScalarValue::Str("{}".into())).unwrap();

    let heads_before = doc.get_heads();

    // Edit cell source only
    doc.splice_text(&source, 5, 0, " world").unwrap();
    let heads_after = doc.get_heads();

    assert!(!diff_metadata_touched(&mut doc, &heads_before, &heads_after));
}

#[test]
fn test_diff_metadata_touched_returns_true_for_metadata_edit() {
    let mut doc = AutoCommit::new();

    let cells = doc.put_object(automerge::ROOT, "cells", automerge::ObjType::Map).unwrap();
    let cell = doc.put_object(&cells, "cell-1", automerge::ObjType::Map).unwrap();
    let source = doc.put_object(&cell, "source", automerge::ObjType::Text).unwrap();
    doc.splice_text(&source, 0, 0, "hello").unwrap();

    let metadata = doc.put_object(automerge::ROOT, "metadata", automerge::ObjType::Map).unwrap();
    doc.put(&metadata, "runt", automerge::ScalarValue::Str("{}".into())).unwrap();

    let heads_before = doc.get_heads();

    // Edit metadata
    doc.put(&metadata, "runt", automerge::ScalarValue::Str("{\"uv\":{}}".into())).unwrap();
    let heads_after = doc.get_heads();

    assert!(diff_metadata_touched(&mut doc, &heads_before, &heads_after));
}

#[test]
fn test_diff_metadata_touched_empty_before_returns_false() {
    let mut doc = AutoCommit::new();
    let heads_after = doc.get_heads();
    // Empty before = initial sync, skip (caller does full materialization)
    assert!(!diff_metadata_touched(&mut doc, &[], &heads_after));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p notebook-doc -- diff_metadata_touched -v`
Expected: FAIL — `diff_metadata_touched` not found.

- [ ] **Step 3: Implement `diff_metadata_touched()`**

Add to `crates/notebook-doc/src/diff.rs`, after the `diff_cells` function:

```rust
/// Check whether any patch between two head sets touches the `metadata` map.
///
/// This is a cheap pre-filter: it walks the patches from `doc.diff()` and
/// checks if any patch path passes through the "metadata" key at the root.
/// Cost is O(delta) — proportional to what changed, not document size.
///
/// Returns `false` for empty `before` (initial sync) or equal heads.
pub fn diff_metadata_touched(
    doc: &mut AutoCommit,
    before: &[ChangeHash],
    after: &[ChangeHash],
) -> bool {
    if before.is_empty() || before == after {
        return false;
    }

    let patches = doc.diff(before, after);

    for patch in &patches {
        // Check if any segment in the path is the "metadata" key at root level.
        // Metadata patches have path: [(ROOT, "metadata"), ...] or the action
        // is on ROOT with key "metadata".
        let touches_metadata = patch
            .path
            .iter()
            .any(|(_, prop)| matches!(prop, Prop::Map(k) if k == "metadata"));

        if touches_metadata {
            return true;
        }

        // Also check: action on ROOT that targets "metadata" key
        if patch.path.is_empty() || patch.path.len() == 1 {
            match &patch.action {
                PatchAction::PutMap { key, .. } if key == "metadata" => return true,
                PatchAction::DeleteMap { key } if key == "metadata" => return true,
                _ => {}
            }
        }
    }

    false
}
```

- [ ] **Step 4: Export from lib.rs**

In `crates/notebook-doc/src/lib.rs`, find the existing `diff` re-exports and add:

```rust
pub use diff::diff_metadata_touched;
```

(Check where `diff_cells` is re-exported and add alongside it.)

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p notebook-doc -- diff_metadata_touched -v`
Expected: 3 tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/notebook-doc/src/diff.rs crates/notebook-doc/src/lib.rs
git commit -m "feat(notebook-doc): add diff_metadata_touched() for patch-based metadata change detection"
```

---

## Task 2: Gate `check_and_broadcast_sync_state()` with heads comparison

Use the new `diff_metadata_touched()` to skip the expensive metadata check when only cells changed.

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server.rs:2380-2450`

- [ ] **Step 1: Write the test (behavioral — verify check is skipped for cell-only changes)**

This is best tested by observation: add a `debug!` log to `check_and_broadcast_sync_state` entry and verify it only fires for metadata changes. But the real test is a unit test for `diff_metadata_touched` (already done in Task 1). The integration behavior can be verified via daemon logs.

Add a counter metric or debug log at the top of `check_and_broadcast_sync_state()`:

```rust
async fn check_and_broadcast_sync_state(room: &NotebookRoom) {
    debug!("[notebook-sync] check_and_broadcast_sync_state called");
    // ... existing code
}
```

- [ ] **Step 2: Capture heads before/after sync in the sync handler**

In `notebook_sync_server.rs`, modify the sync handler around line 2385. The key constraint: heads must be captured **inside the doc write lock**, before and after `receive_sync_message`.

Find this block (around line 2385):

```rust
let (persist_bytes, reply_encoded) = {
    let mut doc = room.doc.write().await;

    let recv_result = catch_automerge_panic("doc-receive-sync", || {
        doc.receive_sync_message(&mut peer_state, message)
    });
```

Replace with:

```rust
let (persist_bytes, reply_encoded, metadata_changed) = {
    let mut doc = room.doc.write().await;

    let heads_before = doc.get_heads();

    let recv_result = catch_automerge_panic("doc-receive-sync", || {
        doc.receive_sync_message(&mut peer_state, message)
    });
    match recv_result {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            warn!("[notebook-sync] receive_sync_message error: {}", e);
            continue;
        }
        Err(e) => {
            warn!("{}", e);
            doc.rebuild_from_save();
            peer_state = sync::State::new();
            continue;
        }
    }

    let heads_after = doc.get_heads();
    let meta_touched = notebook_doc::diff_metadata_touched(
        &mut doc, &heads_before, &heads_after,
    );

    let bytes = doc.save();

    // Notify other peers in this room
    let _ = room.changed_tx.send(());

    let encoded = match catch_automerge_panic("doc-sync-reply", || {
        doc.generate_sync_message(&mut peer_state)
            .map(|reply| reply.encode())
    }) {
        Ok(encoded) => encoded,
        Err(e) => {
            warn!("{}", e);
            doc.rebuild_from_save();
            peer_state = sync::State::new();
            doc.generate_sync_message(&mut peer_state)
                .map(|reply| reply.encode())
        }
    };

    (bytes, encoded, meta_touched)
};
```

Then update the existing code to use the tuple:

```rust
// Send reply (unchanged)
if let Some(encoded) = reply_encoded {
    connection::send_typed_frame(writer, NotebookFrameType::AutomergeSync, &encoded).await?;
}

// Send to debounced persistence task (unchanged)
let _ = room.persist_tx.send(Some(persist_bytes));

// GATED: Only check metadata drift if metadata actually changed
if metadata_changed {
    check_and_broadcast_sync_state(room).await;
}

// These still run unconditionally (they're cheap or have their own guards)
check_and_update_trust_state(room).await;
process_markdown_assets(room).await;
```

**Important:** `check_and_update_trust_state` and `process_markdown_assets` may also benefit from gating, but that's out of scope for this plan. Only gate `check_and_broadcast_sync_state` for now.

- [ ] **Step 3: Verify it compiles**

Run: `cargo build -p runtimed`
Expected: Compiles without errors.

- [ ] **Step 4: Verify behavior with daemon logs**

Start the dev daemon and open a notebook. Type in a cell — `check_and_broadcast_sync_state called` should NOT appear. Add a dependency in the metadata — the log SHOULD appear.

Run: `cargo xtask dev-daemon` in one terminal, then use a notebook in another.

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server.rs
git commit -m "perf(runtimed): gate metadata drift check with patch-based detection

Skip check_and_broadcast_sync_state() for cell-only sync frames.
Uses diff_metadata_touched() to inspect Automerge patches before
materializing metadata. Eliminates ~95% of unnecessary metadata
materialization during typing."
```

---

## Task 3: Add `EnvKind` typed enum to notebook-protocol

Replace string-based `env_type` with a proper discriminated union that carries environment-specific data.

**Files:**
- Modify: `crates/notebook-protocol/src/protocol.rs`

- [ ] **Step 1: Define the `EnvKind` enum**

Add to `crates/notebook-protocol/src/protocol.rs`, near the top with other data structs (after line ~98):

```rust
/// Typed environment kind for sync operations.
///
/// Replaces string-based env_type ("uv", "conda") with a discriminated union
/// that carries environment-specific data. Makes illegal states unrepresentable.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "env_kind", rename_all = "snake_case")]
pub enum EnvKind {
    /// UV environment (inline or prewarmed).
    Uv {
        packages: Vec<String>,
    },
    /// Conda environment (inline or prewarmed).
    Conda {
        packages: Vec<String>,
        channels: Vec<String>,
    },
}

impl EnvKind {
    /// The packages to install, regardless of environment type.
    pub fn packages(&self) -> &[String] {
        match self {
            EnvKind::Uv { packages } | EnvKind::Conda { packages, .. } => packages,
        }
    }
}
```

- [ ] **Step 2: Update `RuntimeAgentRequest::SyncEnvironment`**

Replace the existing variant:

```rust
    /// Hot-install packages into the running kernel's environment.
    /// Supported for UV and Conda inline dependencies (additions only).
    /// The `channels` field is required for conda, ignored for UV/Deno.
    SyncEnvironment {
        packages: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        channels: Option<Vec<String>>,
    },
```

With:

```rust
    /// Hot-install packages into the running kernel's environment.
    /// Supported for UV and Conda inline dependencies (additions only).
    SyncEnvironment(EnvKind),
```

- [ ] **Step 3: Verify compilation fails at expected call sites**

Run: `cargo build -p runtimed 2>&1 | head -40`
Expected: Compilation errors in `notebook_sync_server.rs` and `runtime_agent.rs` where `SyncEnvironment { packages, channels }` is constructed/destructured. This confirms we've found all the call sites.

- [ ] **Step 4: Update `notebook_sync_server.rs` — construct `EnvKind`**

In `handle_sync_environment()`, replace the section that constructs the sync request. Find:

```rust
    let sync_env_type = env_type;
    // ...
    let channels = if env_type == "conda" {
        Some(get_inline_conda_channels(&current_metadata))
    } else {
        None
    };

    let sync_request = notebook_protocol::protocol::RuntimeAgentRequest::SyncEnvironment {
        packages: packages_to_install.clone(),
        channels,
    };
```

Replace with:

```rust
    let env_kind = if env_type == "uv" {
        notebook_protocol::protocol::EnvKind::Uv {
            packages: packages_to_install.clone(),
        }
    } else {
        notebook_protocol::protocol::EnvKind::Conda {
            packages: packages_to_install.clone(),
            channels: get_inline_conda_channels(&current_metadata),
        }
    };

    let sync_request = notebook_protocol::protocol::RuntimeAgentRequest::SyncEnvironment(
        env_kind.clone(),
    );
```

- [ ] **Step 5: Update success handler to use `EnvKind`**

In the success handler (after `send_runtime_agent_request`), replace the `sync_env_type`-based branching:

```rust
        Ok(notebook_protocol::protocol::RuntimeAgentResponse::EnvironmentSynced {
            synced_packages,
        }) => {
            {
                let mut lc = room.runtime_agent_launched_config.write().await;
                if let Some(ref mut config) = *lc {
                    match &env_kind {
                        notebook_protocol::protocol::EnvKind::Uv { .. } => {
                            if config.uv_deps.is_none() {
                                config.uv_deps = Some(vec![]);
                            }
                            if let Some(ref mut deps) = config.uv_deps {
                                for pkg in &synced_packages {
                                    if !deps.contains(pkg) {
                                        deps.push(pkg.clone());
                                    }
                                }
                            }
                        }
                        notebook_protocol::protocol::EnvKind::Conda { .. } => {
                            if config.conda_deps.is_none() {
                                config.conda_deps = Some(vec![]);
                            }
                            if let Some(ref mut deps) = config.conda_deps {
                                for pkg in &synced_packages {
                                    if !deps.contains(pkg) {
                                        deps.push(pkg.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // ... rest of success handler unchanged
```

- [ ] **Step 6: Update `runtime_agent.rs` — destructure `EnvKind`**

Replace the SyncEnvironment handler in `handle_runtime_agent_request()`. Find the `RuntimeAgentRequest::SyncEnvironment { packages, channels }` match arm and replace with:

```rust
        RuntimeAgentRequest::SyncEnvironment(env_kind) => {
            let packages = env_kind.packages().to_vec();
            info!("[runtime-agent] SyncEnvironment: installing {:?}", packages);

            if let Some(ref kernel_ref) = kernel {
                let es = kernel_ref.env_source().to_string();

                // Deno doesn't support hot-sync - requires kernel restart
                if es == "deno" {
                    return (
                        RuntimeAgentResponse::Error {
                            error: "Hot-sync not supported for Deno environments. Kernel restart required.".to_string(),
                        },
                        None,
                    );
                }

                let launched = kernel_ref.launched_config();
                let venv_path = match &launched.venv_path {
                    Some(p) => p.clone(),
                    None => {
                        return (
                            RuntimeAgentResponse::Error {
                                error: "No venv path available".to_string(),
                            },
                            None,
                        );
                    }
                };
                let python_path = match &launched.python_path {
                    Some(p) => p.clone(),
                    None => {
                        return (
                            RuntimeAgentResponse::Error {
                                error: "No python path available".to_string(),
                            },
                            None,
                        );
                    }
                };

                match env_kind {
                    notebook_protocol::protocol::EnvKind::Uv { packages } => {
                        let uv_env = kernel_env::uv::UvEnvironment {
                            venv_path,
                            python_path,
                        };
                        match kernel_env::uv::sync_dependencies(&uv_env, &packages).await {
                            Ok(()) => (
                                RuntimeAgentResponse::EnvironmentSynced {
                                    synced_packages: packages,
                                },
                                None,
                            ),
                            Err(e) => {
                                error!(
                                    "[runtime-agent] Failed to sync UV packages {:?}: {}",
                                    packages, e
                                );
                                (
                                    RuntimeAgentResponse::Error {
                                        error: format!("Failed to install packages: {}", e),
                                    },
                                    None,
                                )
                            }
                        }
                    }
                    notebook_protocol::protocol::EnvKind::Conda { packages, channels } => {
                        let conda_env = kernel_env::conda::CondaEnvironment {
                            env_path: venv_path,
                            python_path,
                        };
                        let conda_deps = kernel_env::conda::CondaDependencies {
                            dependencies: packages.clone(),
                            channels: if channels.is_empty() {
                                vec!["conda-forge".to_string()]
                            } else {
                                channels
                            },
                            python: None,
                            env_id: None,
                        };
                        match kernel_env::conda::sync_dependencies(&conda_env, &conda_deps).await {
                            Ok(()) => (
                                RuntimeAgentResponse::EnvironmentSynced {
                                    synced_packages: packages,
                                },
                                None,
                            ),
                            Err(e) => {
                                error!(
                                    "[runtime-agent] Failed to sync Conda packages {:?}: {}",
                                    packages, e
                                );
                                (
                                    RuntimeAgentResponse::Error {
                                        error: format!("Failed to install packages: {}", e),
                                    },
                                    None,
                                )
                            }
                        }
                    }
                }
            } else {
                (
                    RuntimeAgentResponse::Error {
                        error: "No kernel running".to_string(),
                    },
                    None,
                )
            }
        }
```

- [ ] **Step 7: Remove the dead `env_type != "uv" && env_type != "conda"` guard**

In `handle_sync_environment()`, find and remove the unreachable guard:

```rust
    if env_type != "uv" && env_type != "conda" {
        return NotebookResponse::SyncEnvironmentFailed {
            error: "Hot-sync only supported for UV and Conda environments. Deno requires restart."
                .to_string(),
            needs_restart: true,
        };
    }
```

This is now unreachable since `EnvKind` construction only happens for "uv" or "conda" — the function returns early for all other types before reaching this point.

- [ ] **Step 8: Run lint and tests**

Run: `cargo xtask lint --fix && cargo test -p runtimed -- sync_environment -v`
Expected: All pass. No clippy warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/notebook-protocol/src/protocol.rs crates/runtimed/src/notebook_sync_server.rs crates/runtimed/src/runtime_agent.rs
git commit -m "refactor(protocol): replace flat SyncEnvironment with typed EnvKind enum

Eliminates string-based env_type matching and the launched config update
bug (can't update wrong deps field when the type carries its own data).
Removes dead unreachable guard code."
```

---

## Task 4: Remove `sync_env_type` variable and clean up `handle_sync_environment`

After Task 3, the `env_type: &str` variable and `sync_env_type` copy are no longer needed. The `EnvKind` enum carries the type. Clean up the remaining string-based code.

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server.rs`

- [ ] **Step 1: Audit remaining `env_type` usage in `handle_sync_environment`**

Search for all remaining uses of the `env_type` local variable. After Task 3, the only remaining use should be in the initial `let env_type = ...` block and the `env_kind` construction. The goal is to construct `EnvKind` directly instead of going through a string intermediary.

- [ ] **Step 2: Refactor env_type determination to construct EnvKind directly**

Replace the two-step pattern (determine string → construct enum) with direct construction. Find the block that starts with:

```rust
let env_type = if launched.uv_deps.is_some() { "uv" } ...
```

And the later `env_kind` construction. Merge them into a single step that produces `EnvKind` directly from `packages_to_install` and the launched config.

```rust
let env_kind = if launched.uv_deps.is_some() {
    notebook_protocol::protocol::EnvKind::Uv {
        packages: packages_to_install.clone(),
    }
} else if launched.conda_deps.is_some() {
    notebook_protocol::protocol::EnvKind::Conda {
        packages: packages_to_install.clone(),
        channels: get_inline_conda_channels(&current_metadata),
    }
} else {
    return NotebookResponse::SyncEnvironmentFailed {
        error: "Hot-sync only supported for UV and Conda environments".to_string(),
        needs_restart: true,
    };
};
```

Remove the separate `env_type`, `sync_env_type`, and `channels` variables.

- [ ] **Step 3: Run lint and full test suite**

Run: `cargo xtask lint --fix && cargo test -p runtimed -v`
Expected: All pass.

- [ ] **Step 4: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server.rs
git commit -m "refactor(runtimed): eliminate env_type string intermediary in handle_sync_environment

EnvKind is now constructed directly from launched config state.
No intermediate string variable that can get out of sync."
```

---

## Task 5: Add wire compatibility tests for EnvKind

Ensure old messages (without `env_kind`) and new messages serialize/deserialize correctly.

**Files:**
- Modify: `crates/notebook-protocol/src/protocol.rs` (test module)

- [ ] **Step 1: Write serialization round-trip tests**

Add to the test module in `protocol.rs`:

```rust
#[test]
fn env_kind_uv_round_trip() {
    let kind = EnvKind::Uv {
        packages: vec!["numpy".into(), "pandas".into()],
    };
    let json = serde_json::to_string(&kind).unwrap();
    let parsed: EnvKind = serde_json::from_str(&json).unwrap();
    assert_eq!(kind, parsed);
}

#[test]
fn env_kind_conda_round_trip() {
    let kind = EnvKind::Conda {
        packages: vec!["scipy".into()],
        channels: vec!["conda-forge".into()],
    };
    let json = serde_json::to_string(&kind).unwrap();
    assert!(json.contains("\"env_kind\":\"conda\""));
    let parsed: EnvKind = serde_json::from_str(&json).unwrap();
    assert_eq!(kind, parsed);
}

#[test]
fn sync_environment_request_round_trip() {
    let req = RuntimeAgentRequest::SyncEnvironment(EnvKind::Conda {
        packages: vec!["numpy".into()],
        channels: vec!["conda-forge".into(), "bioconda".into()],
    });
    let json = serde_json::to_string(&req).unwrap();
    let parsed: RuntimeAgentRequest = serde_json::from_str(&json).unwrap();
    // Verify the packages accessor works
    match &parsed {
        RuntimeAgentRequest::SyncEnvironment(kind) => {
            assert_eq!(kind.packages(), &["numpy".to_string()]);
        }
        _ => panic!("wrong variant"),
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p notebook-protocol -- env_kind -v`
Expected: All 3 tests pass.

- [ ] **Step 3: Commit**

```bash
git add crates/notebook-protocol/src/protocol.rs
git commit -m "test(protocol): add wire compatibility tests for EnvKind enum"
```

---

## Task 6: Final verification — lint, test, and manual check

- [ ] **Step 1: Run full lint**

Run: `cargo xtask lint`
Expected: All checks passed.

- [ ] **Step 2: Run all Rust tests**

Run: `cargo test --workspace`
Expected: All pass. Pay attention to any `notebook_sync_server` or `runtime_agent` test failures.

- [ ] **Step 3: Verify daemon starts cleanly**

If supervisor tools are available:
```
supervisor_restart(target="daemon")
supervisor_status
supervisor_logs
```

Otherwise:
```bash
cargo xtask dev-daemon
```

Check logs for any panics or unexpected errors.

- [ ] **Step 4: Manual verification with a notebook**

Open a notebook, verify:
1. Typing in a cell does NOT trigger `check_and_broadcast_sync_state` debug log
2. Adding a UV dependency DOES trigger drift detection
3. UV hot-sync works (add package → sync → import succeeds)
4. Conda hot-sync works (add package → sync → import succeeds)
5. Progress indicator clears after sync

- [ ] **Step 5: Commit any final fixes**

If any issues found, fix and commit with appropriate message.

---

## What This Achieves

| Before | After |
|--------|-------|
| `check_and_broadcast_sync_state()` runs on every keystroke | Only runs when metadata patches detected |
| `env_type` is a string (`"uv"`, `"conda"`) | `EnvKind` enum carries typed data |
| `SyncEnvironment { packages, channels }` — flat, channels meaningless for UV | `SyncEnvironment(EnvKind)` — discriminated, type-safe |
| Launched config update can hit wrong field | Match on `EnvKind` variant — compiler enforces correctness |
| Dead unreachable guards | Removed |

## What This Does NOT Change (Future Work)

- **Initial launch flow**: Still reads full metadata imperatively. This is correct — no "before" heads at launch time.
- **`env_source` strings throughout the codebase**: These are used for kernel selection, not just sync. A broader refactor (string → enum for all env_source) is a separate effort.
- **CRDT-driven auto-sync**: The frontend still explicitly sends `SyncEnvironment` request. Making this reactive (daemon auto-syncs when it detects safe additions) is a future enhancement.
- **Pixi TOML drift**: Reads file on disk, inherently not CRDT-based. Left as-is.
