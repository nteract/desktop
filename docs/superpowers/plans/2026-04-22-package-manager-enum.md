# PackageManager Enum Refactor — Implementation Plan (PR 1)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace raw `"uv"`/`"conda"`/`"pixi"` strings with a typed `PackageManager` enum across ~10 function signatures in 6 crates. Delete `normalize_package_manager` once callers use the enum's `parse()` method.

**Architecture:** Introduce `PackageManager` (Copy, 3 variants, wire-compatible via `#[serde(rename_all = "lowercase")]`) in `notebook-protocol::connection`. Migrate signatures layer by layer (protocol → sync → daemon → mcp → py → tauri/node) so every task produces a compiling, test-passing workspace. Internal code uses the enum; the MCP JSON output and the PyO3 Python boundary keep strings. The wire handshake is byte-identical before/after.

**Tech Stack:** Rust, serde, PyO3, Automerge (indirect). No frontend changes. No wire or schema version bump — serialization is identical.

**Scope:** This plan covers PR 1 only. PR 2 (`EnvSource` enum, `env_source.starts_with(...)` replacements, prewarmed fallback) is a separate plan tracked by the spec at `docs/superpowers/specs/2026-04-22-package-manager-enum-design.md`.

---

## File Structure

| File | Role in this refactor |
|------|----------------------|
| `crates/notebook-protocol/src/connection.rs` | Define `PackageManager` enum, change `CreateNotebook.package_manager` field type, remove `normalize_package_manager` |
| `crates/notebook-sync/src/connect.rs` | Thread `Option<PackageManager>` through `connect_create` and `connect_create_relay` |
| `crates/runtimed/src/daemon.rs` | `handle_create_notebook` signature + `Handshake::CreateNotebook` destructuring |
| `crates/runtimed/src/notebook_sync_server/load.rs` | `create_empty_notebook`, `build_new_notebook_metadata` signatures |
| `crates/runtimed/src/notebook_sync_server/metadata.rs` | `detect_manager_from_metadata` return type + auto-launch 3-way match |
| `crates/runtimed/tests/integration.rs` | All `connect_create` call sites pass `Option<PackageManager>` |
| `crates/runt-mcp/src/tools/session.rs` | `create_notebook` handler parses to enum, passes through |
| `crates/runt-mcp/src/tools/deps.rs` | `detect_package_manager`, `add_dep_for_manager`, `remove_dep_for_manager`, `get_deps_for_manager*` signatures |
| `crates/runt-mcp/src/project_file.rs` | `ProjectFile::manager()` returns `PackageManager` |
| `crates/runtimed-py/src/async_client.rs` | Parse `Option<String>` → `Option<PackageManager>` at PyO3 boundary |
| `crates/runtimed-py/src/async_session.rs` | `create_notebook_async` signature |
| `crates/runtimed-py/src/session_core.rs` | `connect_create` signature, `get_metadata_env_type` return type |
| `crates/notebook/src/lib.rs` | `connect_create_relay` call site (passes `None`) |
| `crates/runtimed-node/src/session.rs` | `connect_create` call site (passes `None`) |

---

## Task 1: Define `PackageManager` enum in `notebook-protocol`

**Files:**
- Modify: `crates/notebook-protocol/src/connection.rs`

**Goal:** Add the enum, `as_str`, `parse`, `FromStr`, `Display`, and unit tests. Additive — existing `normalize_package_manager` stays for now so dependent crates keep compiling.

- [ ] **Step 1: Write the failing tests**

Append to the bottom of the existing `#[cfg(test)] mod tests` block in `crates/notebook-protocol/src/connection.rs` (immediately after the existing `normalize_package_manager_rejects_unknown` test on line 1016):

```rust
    #[test]
    fn package_manager_as_str_round_trips() {
        assert_eq!(PackageManager::Uv.as_str(), "uv");
        assert_eq!(PackageManager::Conda.as_str(), "conda");
        assert_eq!(PackageManager::Pixi.as_str(), "pixi");
    }

    #[test]
    fn package_manager_parse_valid() {
        assert_eq!(PackageManager::parse("uv").unwrap(), PackageManager::Uv);
        assert_eq!(PackageManager::parse("conda").unwrap(), PackageManager::Conda);
        assert_eq!(PackageManager::parse("pixi").unwrap(), PackageManager::Pixi);
    }

    #[test]
    fn package_manager_parse_aliases() {
        assert_eq!(PackageManager::parse("pip").unwrap(), PackageManager::Uv);
        assert_eq!(PackageManager::parse("mamba").unwrap(), PackageManager::Conda);
    }

    #[test]
    fn package_manager_parse_rejects_unknown() {
        let err = PackageManager::parse("npm").unwrap_err();
        assert!(err.contains("Unsupported package manager 'npm'"));
        assert!(err.contains("Supported: uv, conda, pixi"));
    }

    #[test]
    fn package_manager_fromstr_works() {
        use std::str::FromStr;
        let pm: PackageManager = PackageManager::from_str("conda").unwrap();
        assert_eq!(pm, PackageManager::Conda);
        assert!(PackageManager::from_str("bogus").is_err());
    }

    #[test]
    fn package_manager_display_matches_as_str() {
        assert_eq!(format!("{}", PackageManager::Uv), "uv");
        assert_eq!(format!("{}", PackageManager::Conda), "conda");
        assert_eq!(format!("{}", PackageManager::Pixi), "pixi");
    }

    #[test]
    fn package_manager_serde_is_lowercase() {
        let json = serde_json::to_string(&PackageManager::Conda).unwrap();
        assert_eq!(json, "\"conda\"");
        let pm: PackageManager = serde_json::from_str("\"pixi\"").unwrap();
        assert_eq!(pm, PackageManager::Pixi);
    }
```

- [ ] **Step 2: Run the tests — they should fail to compile (enum not defined)**

Run: `cargo test -p notebook-protocol --lib package_manager 2>&1 | head -40`
Expected: compile error: `cannot find type 'PackageManager' in this scope`.

- [ ] **Step 3: Add the enum definition**

At the top of `crates/notebook-protocol/src/connection.rs`, replace the imports block (line 30) to add `std::fmt` and `std::str::FromStr`:

```rust
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
```

Then, immediately after the module-level doc comments and imports (before the existing `MAX_FRAME_SIZE` constant at ~line 34, or any other natural location before the first type definition), insert the enum:

```rust
/// Supported package managers for Python notebooks.
///
/// The wire format is the lowercase variant name (`"uv"`, `"conda"`, `"pixi"`),
/// matching the historical `normalize_package_manager` output. `parse()`
/// additionally accepts `"pip"` (→ Uv) and `"mamba"` (→ Conda) aliases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageManager {
    Uv,
    Conda,
    Pixi,
}

impl PackageManager {
    /// The canonical wire string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Uv => "uv",
            Self::Conda => "conda",
            Self::Pixi => "pixi",
        }
    }

    /// Parse a package manager name with alias support.
    ///
    /// Accepts `"uv"`, `"conda"`, `"pixi"` (canonical), plus `"pip"` (→ Uv)
    /// and `"mamba"` (→ Conda).
    pub fn parse(input: &str) -> Result<Self, String> {
        match input {
            "uv" => Ok(Self::Uv),
            "conda" => Ok(Self::Conda),
            "pixi" => Ok(Self::Pixi),
            "pip" => Ok(Self::Uv),
            "mamba" => Ok(Self::Conda),
            _ => Err(format!(
                "Unsupported package manager '{}'. Supported: uv, conda, pixi.",
                input
            )),
        }
    }
}

impl fmt::Display for PackageManager {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for PackageManager {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse(s)
    }
}
```

- [ ] **Step 4: Run the new tests — they should pass**

Run: `cargo test -p notebook-protocol --lib package_manager`
Expected: 7 tests pass (`package_manager_as_str_round_trips`, `..._parse_valid`, `..._parse_aliases`, `..._parse_rejects_unknown`, `..._fromstr_works`, `..._display_matches_as_str`, `..._serde_is_lowercase`).

- [ ] **Step 5: Run the full protocol test suite — nothing regressed**

Run: `cargo test -p notebook-protocol`
Expected: all tests pass, including the pre-existing `normalize_package_manager_*` tests (unchanged).

- [ ] **Step 6: Commit**

```bash
git add crates/notebook-protocol/src/connection.rs
git commit -m "feat(notebook-protocol): add PackageManager enum"
```

---

## Task 2: Migrate the `CreateNotebook` handshake + `connect_create*` signatures

**Files:**
- Modify: `crates/notebook-protocol/src/connection.rs` (struct field + 3 serialization tests)
- Modify: `crates/notebook-sync/src/connect.rs` (2 public functions + 1 private)
- Modify: `crates/runtimed/src/daemon.rs` (`handle_create_notebook` signature + destructuring)
- Modify: `crates/runtimed/src/notebook_sync_server/load.rs` (`create_empty_notebook`, `build_new_notebook_metadata`)
- Modify: `crates/runtimed/src/notebook_sync_server/metadata.rs` (none yet — just a spot-check)
- Modify: `crates/runtimed/tests/integration.rs` (all `connect_create` calls)
- Modify: `crates/runtimed-py/src/session_core.rs` (`connect_create` helper)
- Modify: `crates/runtimed-py/src/async_session.rs` (`create_notebook_async`)
- Modify: `crates/runtimed-py/src/async_client.rs` (PyO3 boundary)
- Modify: `crates/runt-mcp/src/tools/session.rs` (`create_notebook` tool handler)
- Modify: `crates/notebook/src/lib.rs` (Tauri relay call site)
- Modify: `crates/runtimed-node/src/session.rs` (Node binding call site)

**Goal:** Change `Option<String>`/`Option<&str>` to `Option<PackageManager>` everywhere the handshake flows. Internal callers construct the enum at the boundary (PyO3 argument parsing, MCP JSON argument parsing). Wire format is unchanged (serde produces `"uv"`/`"conda"`/`"pixi"`). `normalize_package_manager` is still present — it's deleted in Task 7.

This is a single atomic commit: compile goes green at the end. Steps walk through each file; do not commit in the middle.

### Step 1: Change the handshake struct field type

- [ ] **Edit `crates/notebook-protocol/src/connection.rs`**

Change the `package_manager` field on `Handshake::CreateNotebook` (line 150-154):

```rust
        /// Package manager preference: uv, conda, or pixi.
        /// When set, the daemon creates only this manager's metadata section.
        /// When None, the daemon uses its default_python_env setting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        package_manager: Option<PackageManager>,
```

Update the 3 handshake serialization tests (lines 741-780): the `package_manager: None` values still compile (the enum is the inner type of the Option). No changes needed there — but verify by reading the file.

### Step 2: Change `connect_create` and `connect_create_relay` signatures

- [ ] **Edit `crates/notebook-sync/src/connect.rs`**

At line 272-292 (`connect_create`), change `package_manager: Option<&str>` to `package_manager: Option<PackageManager>`:

```rust
pub async fn connect_create(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    actor_label: &str,
    ephemeral: bool,
    package_manager: Option<notebook_protocol::connection::PackageManager>,
    dependencies: Vec<String>,
) -> Result<CreateResult, SyncError> {
    connect_create_inner(
        socket_path,
        runtime,
        working_dir,
        None,
        actor_label,
        ephemeral,
        package_manager,
        dependencies,
    )
    .await
}
```

At line 295-304 (`connect_create_inner`), same change:

```rust
async fn connect_create_inner(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    notebook_id: Option<String>,
    actor_label: &str,
    ephemeral: bool,
    package_manager: Option<notebook_protocol::connection::PackageManager>,
    dependencies: Vec<String>,
) -> Result<CreateResult, SyncError> {
```

And inside, change the handshake construction (was `package_manager: package_manager.map(String::from)` at line 321):

```rust
        package_manager,
```

At line 488-497 (`connect_create_relay`), same change:

```rust
pub async fn connect_create_relay(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    notebook_id: Option<String>,
    frame_tx: mpsc::UnboundedSender<Vec<u8>>,
    ephemeral: bool,
    package_manager: Option<notebook_protocol::connection::PackageManager>,
    dependencies: Vec<String>,
) -> Result<RelayCreateResult, SyncError> {
```

And at line 514 (the handshake construction inside `connect_create_relay`), change:

```rust
        package_manager,
```

### Step 3: Update the runtimed daemon

- [ ] **Edit `crates/runtimed/src/daemon.rs`**

At line 1701-1720 (the `Handshake::CreateNotebook` arm), the destructuring `package_manager` now binds a `Option<PackageManager>` instead of `Option<String>`. The subsequent call to `self.handle_create_notebook(...)` is positional; no source change needed at the call site, but the callee's signature changes in the next substep.

At line 2112-2123 (`handle_create_notebook` signature), change:

```rust
    async fn handle_create_notebook<S>(
        self: Arc<Self>,
        stream: S,
        runtime: String,
        working_dir: Option<String>,
        notebook_id_hint: Option<String>,
        ephemeral: Option<bool>,
        package_manager: Option<notebook_protocol::connection::PackageManager>,
        dependencies: Vec<String>,
        client_protocol_version: u8,
    ) -> anyhow::Result<()>
```

At line 2180 (the call into `create_empty_notebook`), change `package_manager.as_deref()` to `package_manager` (now we pass the enum through, not an `&str`):

```rust
                match crate::notebook_sync_server::create_empty_notebook(
                    &mut doc,
                    &runtime,
                    default_python_env.clone(),
                    Some(&notebook_id),
                    package_manager,
                    &dependencies,
                ) {
```

At line 1875-1885 (`self.handle_create_notebook(...)` call from a different path; passes `None` for package_manager), no change needed — `None` is polymorphic.

### Step 4: Update `create_empty_notebook` and `build_new_notebook_metadata`

- [ ] **Edit `crates/runtimed/src/notebook_sync_server/load.rs`**

At line 725-732 (`create_empty_notebook` signature), change `package_manager: Option<&str>` to `Option<PackageManager>`:

```rust
pub fn create_empty_notebook(
    doc: &mut NotebookDoc,
    runtime: &str,
    default_python_env: crate::settings_doc::PythonEnvType,
    env_id: Option<&str>,
    package_manager: Option<notebook_protocol::connection::PackageManager>,
    dependencies: &[String],
) -> Result<String, String> {
```

At line 770-776 (`build_new_notebook_metadata` signature), same change:

```rust
pub(crate) fn build_new_notebook_metadata(
    runtime: &str,
    env_id: &str,
    default_python_env: crate::settings_doc::PythonEnvType,
    package_manager: Option<notebook_protocol::connection::PackageManager>,
    dependencies: &[String],
) -> NotebookMetadataSnapshot {
```

Inside `build_new_notebook_metadata`, replace the `effective_manager: &str` block at lines 809-821 with an enum-based resolution. Keep the subsequent `match effective_manager { ... }` block working by matching on the enum:

```rust
        _ => {
            // Resolve which package manager section to create:
            //   1. Explicit package_manager from the request
            //   2. No explicit manager - use default_python_env
            use notebook_protocol::connection::PackageManager;
            let effective_manager: PackageManager = package_manager.unwrap_or_else(|| {
                match default_python_env {
                    crate::settings_doc::PythonEnvType::Conda => PackageManager::Conda,
                    crate::settings_doc::PythonEnvType::Pixi => PackageManager::Pixi,
                    _ => PackageManager::Uv,
                }
            });

            let deps = dependencies.to_vec();

            let (uv, conda, pixi) = match effective_manager {
                PackageManager::Conda => (
                    None,
                    Some(CondaInlineMetadata {
                        dependencies: deps,
                        channels: vec!["conda-forge".to_string()],
                        python: None,
                    }),
                    None,
                ),
                PackageManager::Pixi => (
                    None,
                    None,
                    Some(notebook_doc::metadata::PixiInlineMetadata {
                        dependencies: deps,
                        pypi_dependencies: vec![],
                        channels: vec!["conda-forge".to_string()],
                        python: None,
                    }),
                ),
                PackageManager::Uv => (
                    Some(UvInlineMetadata {
                        dependencies: deps,
                        requires_python: None,
                        prerelease: None,
```

Continue through the existing `_ => ( Some(UvInlineMetadata { ... }), None, None)` trailing arm — rename the `_` arm to `PackageManager::Uv` so the match is exhaustive. (Read lines 845-870 in the file to see the full existing trailing arm; keep its body verbatim but change the arm label from `_` to `PackageManager::Uv`.)

Remove the call to `normalize_package_manager` (line 814); the enum carries aliases internally via `PackageManager::parse` at the boundary.

### Step 5: Update the runtimed integration tests

- [ ] **Edit `crates/runtimed/tests/integration.rs`**

Every `connect_create(...)` call in this file currently passes `None` for `package_manager` (or, in the case of the create_notebook-with-deps tests, parses a string). Grep the file to confirm, and change any `Some("uv")`-style calls to `Some(PackageManager::Uv)`:

```bash
rg 'connect_create\s*\(' crates/runtimed/tests/integration.rs -n -A 8
```

For every call site, if the `package_manager` argument (7th positional for `connect_create`, after `ephemeral`) is `None`, no change is required. If it's `Some("uv"|"conda"|"pixi")`, replace with the enum value and add:

```rust
use notebook_protocol::connection::PackageManager;
```

near the top of the test file (or the local `mod` if tests are grouped).

### Step 6: Update runtimed-py

- [ ] **Edit `crates/runtimed-py/src/session_core.rs`**

At line 407-414 (`connect_create` helper signature):

```rust
pub(crate) async fn connect_create(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    actor_label: Option<&str>,
    package_manager: Option<notebook_protocol::connection::PackageManager>,
    dependencies: Vec<String>,
) -> PyResult<(String, SessionState, NotebookConnectionInfo)> {
```

Inside, the forwarded `notebook_sync::connect::connect_create(..., package_manager, dependencies)` call keeps working with the new type.

- [ ] **Edit `crates/runtimed-py/src/async_session.rs`**

At line 75-94 (`create_notebook_async`), change the parameter type and pass-through:

```rust
    pub(crate) async fn create_notebook_async(
        socket_path: PathBuf,
        runtime: String,
        working_dir: Option<PathBuf>,
        peer_label: Option<String>,
        package_manager: Option<notebook_protocol::connection::PackageManager>,
        dependencies: Vec<String>,
    ) -> PyResult<Self> {
        let peer_label = Some(peer_label.unwrap_or_else(session_core::default_peer_label));
        let actor_label = peer_label.as_deref().map(session_core::make_actor_label);
        let (notebook_id, mut state, _info) = session_core::connect_create(
            socket_path,
            &runtime,
            working_dir,
            actor_label.as_deref(),
            package_manager,
            dependencies,
        )
        .await?;
```

- [ ] **Edit `crates/runtimed-py/src/async_client.rs`**

At lines 217-240 (the argument-parsing block inside `create_notebook`). The Python API keeps `Option<String>`; parse into the enum at the PyO3 boundary and forward the enum downstream:

```rust
        // Validate and normalize package_manager before entering the async block.
        // Python API accepts "uv"/"conda"/"pixi" plus aliases ("pip", "mamba").
        let parsed_pm: Option<notebook_protocol::connection::PackageManager> = match &package_manager {
            Some(pm) => Some(
                notebook_protocol::connection::PackageManager::parse(pm)
                    .map_err(pyo3::exceptions::PyValueError::new_err)?,
            ),
            None => None,
        };

        let label = peer_label.or_else(|| self.peer_label.clone());
        let socket_path = self.socket_path.clone();
        let runtime = runtime.to_string();
        let working_dir_buf = working_dir.map(PathBuf::from);
        let deps = dependencies.unwrap_or_default();
        future_into_py(py, async move {
            AsyncSession::create_notebook_async(
                socket_path,
                runtime,
                working_dir_buf,
                label,
                parsed_pm,
                deps,
```

The rest of `create_notebook_async` forwards `parsed_pm` verbatim; no changes to the trailing call lines.

### Step 7: Update runt-mcp `create_notebook` tool

- [ ] **Edit `crates/runt-mcp/src/tools/session.rs`**

At lines 423-443 (the block that parses `package_manager` argument and calls `connect_create`), replace `normalize_package_manager` with `PackageManager::parse`, and thread the enum through:

```rust
    let deps: Vec<String> = arg_string_array(request, "dependencies").unwrap_or_default();
    let explicit_pkg_manager = match arg_str(request, "package_manager") {
        Some(pm) => {
            let parsed = notebook_protocol::connection::PackageManager::parse(pm)
                .map_err(|msg| McpError::invalid_params(msg, None))?;
            Some(parsed)
        }
        None => None,
    };

    let prev = previous_notebook_id(server).await;

    match notebook_sync::connect::connect_create(
        server.socket_path.clone(),
        runtime,
        working_dir,
        &server.get_peer_label().await,
        ephemeral,
        explicit_pkg_manager,
        deps.clone(),
    )
    .await
```

Lines 456-458 (the `pkg_manager: String` local) currently fall back to `detect_package_manager`, which returns a `String` today. That function is migrated in **Task 4**. For this task, keep the existing behavior intact using `.as_str().to_string()` on the enum:

```rust
            let pkg_manager: String = explicit_pkg_manager
                .map(|pm| pm.as_str().to_string())
                .unwrap_or_else(|| super::deps::detect_package_manager(&result.handle));
```

This preserves the JSON output shape (`"package_manager": "conda"`) without breaking `get_deps_for_manager_pub(&s.handle, &pkg_manager)` — which still takes `&str` at this point. Task 4 will migrate that together with `detect_package_manager`.

### Step 8: Update the Tauri relay call site

- [ ] **Edit `crates/notebook/src/lib.rs`**

At line 730-740, the `connect_create_relay` call passes `None` for `package_manager`. With the new enum, `None` is polymorphic — no source change required:

```rust
    let result = notebook_sync::connect::connect_create_relay(
        socket_path,
        &runtime,
        working_dir,
        notebook_id_hint,
        frame_tx,
        false,
        None,
        vec![],
    )
    .await
    .map_err(|e| format!("sync connect (create): {}", e))?;
```

Build-check only. Do not modify.

### Step 9: Update the Node binding call site

- [ ] **Edit `crates/runtimed-node/src/session.rs`**

At line 273-282, same situation — passes `None`. Build-check only.

### Step 10: Build the workspace — no errors, warnings, or `clippy` diagnostics

Run: `cargo xtask clippy 2>&1 | tail -40`
Expected: no new warnings or errors. `clippy` excludes `runtimed-py`; that's fine — CI covers it.

Run: `cargo build --workspace --all-targets 2>&1 | tail -40`
Expected: clean build.

- [ ] **Step 11: Run protocol + sync + daemon tests**

Run:
```bash
cargo test -p notebook-protocol
cargo test -p notebook-sync
cargo test -p runtimed --lib
```
Expected: all green.

- [ ] **Step 12: Run the runtimed daemon integration tests**

Run: `cargo xtask integration create_notebook 2>&1 | tail -20`
Expected: tests pass. This exercises the full handshake path.

- [ ] **Step 13: Commit**

```bash
git add -u
git commit -m "refactor: thread PackageManager through CreateNotebook handshake"
```

---

## Task 3: Migrate `detect_manager_from_metadata` + auto-launch match

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server/metadata.rs`

**Goal:** Change `detect_manager_from_metadata` to return `Option<PackageManager>` and make the two auto-launch sites (lines ~2249-2261) exhaustive-match on the enum. Do not touch `env_source` strings — that is PR 2's job.

- [ ] **Step 1: Change the helper**

In `crates/runtimed/src/notebook_sync_server/metadata.rs`, replace `detect_manager_from_metadata` (lines 56-66):

```rust
fn detect_manager_from_metadata(
    snapshot: &NotebookMetadataSnapshot,
) -> Option<notebook_protocol::connection::PackageManager> {
    use notebook_protocol::connection::PackageManager;
    if snapshot.runt.pixi.is_some() {
        Some(PackageManager::Pixi)
    } else if snapshot.runt.conda.is_some() {
        Some(PackageManager::Conda)
    } else if snapshot.runt.uv.is_some() {
        Some(PackageManager::Uv)
    } else {
        None
    }
}
```

- [ ] **Step 2: Update the one caller at the auto-launch site**

At lines 2246-2267 (inside the `Some("python")` kernel-type arm), change the `match manager { Some("conda") => ..., Some("pixi") => ..., Some("uv") => ..., _ => ... }` block to match on the enum option:

```rust
            } else {
                // Check if the metadata has an explicit manager section
                // (e.g. create_notebook(package_manager="conda") with empty deps).
                // Use that to pick the pool type instead of default_python_env.
                use notebook_protocol::connection::PackageManager;
                let manager = metadata_snapshot
                    .as_ref()
                    .and_then(detect_manager_from_metadata);
                let prewarmed = match manager {
                    Some(PackageManager::Conda) => "conda:prewarmed",
                    Some(PackageManager::Pixi) => "pixi:prewarmed",
                    Some(PackageManager::Uv) => "uv:prewarmed",
                    None => match default_python_env {
                        crate::settings_doc::PythonEnvType::Conda => "conda:prewarmed",
                        crate::settings_doc::PythonEnvType::Pixi => "pixi:prewarmed",
                        _ => "uv:prewarmed",
                    },
                };
                info!(
                    "[notebook-sync] Auto-launch: using prewarmed ({})",
                    prewarmed
                );
                prewarmed.to_string()
            };
```

The match is now exhaustive over `Option<PackageManager>`; removing one variant would be a compile error.

- [ ] **Step 3: Build**

Run: `cargo build -p runtimed --all-targets 2>&1 | tail -20`
Expected: clean.

- [ ] **Step 4: Run the daemon tests**

Run: `cargo test -p runtimed --lib`
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server/metadata.rs
git commit -m "refactor(runtimed): use PackageManager in auto-launch metadata resolution"
```

---

## Task 4: Migrate `runt-mcp` dependency helpers

**Files:**
- Modify: `crates/runt-mcp/src/tools/deps.rs` (5 functions)
- Modify: `crates/runt-mcp/src/tools/session.rs` (follow-up to Task 2 Step 7)
- Modify: `crates/runt-mcp/src/project_file.rs` (`ProjectFile::manager()`)

**Goal:** `detect_package_manager` returns `PackageManager`; `add_dep_for_manager`, `remove_dep_for_manager`, and `get_deps_for_manager*` take `PackageManager`. The MCP JSON output still emits strings via `.as_str()`.

- [ ] **Step 1: Migrate `detect_package_manager` in `deps.rs`**

Replace lines 48-79 in `crates/runt-mcp/src/tools/deps.rs`:

```rust
pub(crate) fn detect_package_manager(
    handle: &notebook_sync::handle::DocHandle,
) -> notebook_protocol::connection::PackageManager {
    use notebook_protocol::connection::PackageManager;
    // Priority 1: metadata declares which package manager section exists.
    if let Some(meta) = handle.get_notebook_metadata() {
        if meta.runt.pixi.is_some() {
            return PackageManager::Pixi;
        }
        if meta.runt.conda.is_some() {
            return PackageManager::Conda;
        }
        if meta.runt.uv.is_some() {
            return PackageManager::Uv;
        }
    }
    // Priority 2: env_source from running kernel (fallback for notebooks
    // with no runt metadata yet).
    if let Ok(state) = handle.get_runtime_state() {
        let src = &state.kernel.env_source;
        if src.starts_with("conda:") {
            return PackageManager::Conda;
        }
        if src.starts_with("pixi:") {
            return PackageManager::Pixi;
        }
        if src.starts_with("uv:") {
            return PackageManager::Uv;
        }
    }
    PackageManager::Uv
}
```

- [ ] **Step 2: Migrate `add_dep_for_manager`, `remove_dep_for_manager`, `get_deps_for_manager*`**

Replace lines 82-97 (`add_dep_for_manager`):

```rust
pub(crate) fn add_dep_for_manager(
    handle: &notebook_sync::handle::DocHandle,
    package: &str,
    manager: notebook_protocol::connection::PackageManager,
) -> Result<(), String> {
    use notebook_protocol::connection::PackageManager;
    match manager {
        PackageManager::Conda => handle
            .add_conda_dependency(package)
            .map_err(|e| format!("Failed to add conda dependency: {e}")),
        PackageManager::Pixi => handle
            .add_pixi_dependency(package)
            .map_err(|e| format!("Failed to add pixi dependency: {e}")),
        PackageManager::Uv => handle
            .add_uv_dependency(package)
            .map_err(|e| format!("Failed to add uv dependency: {e}")),
    }
}
```

Replace lines 101-117 (`remove_dep_for_manager`):

```rust
fn remove_dep_for_manager(
    handle: &notebook_sync::handle::DocHandle,
    package: &str,
    manager: notebook_protocol::connection::PackageManager,
) -> Result<bool, String> {
    use notebook_protocol::connection::PackageManager;
    match manager {
        PackageManager::Conda => handle
            .remove_conda_dependency(package)
            .map_err(|e| format!("Failed to remove conda dependency: {e}")),
        PackageManager::Pixi => handle
            .remove_pixi_dependency(package)
            .map_err(|e| format!("Failed to remove pixi dependency: {e}")),
        PackageManager::Uv => handle
            .remove_uv_dependency(package)
            .map_err(|e| format!("Failed to remove uv dependency: {e}")),
    }
}
```

Replace lines 398-415 (`get_deps_for_manager_pub` + `get_deps_for_manager`):

```rust
pub(crate) fn get_deps_for_manager_pub(
    handle: &notebook_sync::handle::DocHandle,
    manager: notebook_protocol::connection::PackageManager,
) -> Vec<String> {
    get_deps_for_manager(handle, manager)
}

fn get_deps_for_manager(
    handle: &notebook_sync::handle::DocHandle,
    manager: notebook_protocol::connection::PackageManager,
) -> Vec<String> {
    use notebook_protocol::connection::PackageManager;
    handle
        .get_notebook_metadata()
        .map(|m| match manager {
            PackageManager::Conda => m.conda_dependencies().to_vec(),
            PackageManager::Pixi => m.pixi_dependencies().to_vec(),
            PackageManager::Uv => m.uv_dependencies().to_vec(),
        })
        .unwrap_or_default()
}
```

- [ ] **Step 3: Fix the callers of these helpers in `deps.rs`**

In `add_dependency` (lines 120-266), update these sites:
- Line 139: `let manager = detect_package_manager(&handle);` — unchanged (type inference picks up the new return type).
- Line 141: `add_dep_for_manager(&handle, package, &manager)` → `add_dep_for_manager(&handle, package, manager)` (`PackageManager: Copy`).
- Line 150: `let deps = get_deps_for_manager(&handle, &manager);` → `let deps = get_deps_for_manager(&handle, manager);`
- Line 155 (`"package_manager": manager`) → `"package_manager": manager.as_str()`.

In `remove_dependency` (lines 269-297):
- Line 278: same — unchanged local.
- Line 280: `remove_dep_for_manager(&handle, package, &manager)` → `remove_dep_for_manager(&handle, package, manager)`.
- Line 288: `get_deps_for_manager(&handle, &manager)` → `get_deps_for_manager(&handle, manager)`.
- Line 294: `"package_manager": manager` → `"package_manager": manager.as_str()`.

In `get_dependencies` (lines 300-338):
- Line 306: `let manager = detect_package_manager(&handle);` — unchanged.
- Line 307: `get_deps_for_manager(&handle, &manager)` → `get_deps_for_manager(&handle, manager)`.
- Line 328: `"package_manager": manager` → `"package_manager": manager.as_str()`.

- [ ] **Step 4: Fix the callers in `session.rs`**

In `crates/runt-mcp/src/tools/session.rs`:

Remove the `String` local from Task 2 Step 7 — with `detect_package_manager` now returning `PackageManager`, the local should be the enum. Replace lines 456-458:

```rust
            let pkg_manager: notebook_protocol::connection::PackageManager = explicit_pkg_manager
                .unwrap_or_else(|| super::deps::detect_package_manager(&result.handle));
```

At line 480-482 (`get_deps_for_manager_pub(&s.handle, &pkg_manager)` → by-value):

```rust
                    super::deps::get_deps_for_manager_pub(&s.handle, pkg_manager)
```

At line 489 (`"package_manager": pkg_manager`):

```rust
                "package_manager": pkg_manager.as_str(),
```

- [ ] **Step 5: Migrate `ProjectFile::manager()`**

In `crates/runt-mcp/src/project_file.rs`, lines 24-30:

```rust
    pub fn manager(&self) -> notebook_protocol::connection::PackageManager {
        use notebook_protocol::connection::PackageManager;
        match self.kind {
            ProjectFileKind::PyprojectToml => PackageManager::Uv,
            ProjectFileKind::PixiToml => PackageManager::Pixi,
        }
    }
```

Grep for callers (`rg 'project_file.*\.manager\(\)' crates/runt-mcp/src -n`) and fix each to treat the return value as a `PackageManager`. Likely fixes: `.as_str()` before `format!`/JSON insertion, or direct comparison via `== PackageManager::Uv`.

- [ ] **Step 6: Build + lint + test**

Run:
```bash
cargo build -p runt-mcp --all-targets 2>&1 | tail -20
cargo xtask clippy 2>&1 | tail -20
cargo test -p runt-mcp
```
Expected: green across the board.

- [ ] **Step 7: Commit**

```bash
git add crates/runt-mcp/src/tools/deps.rs crates/runt-mcp/src/tools/session.rs crates/runt-mcp/src/project_file.rs
git commit -m "refactor(runt-mcp): use PackageManager enum in dependency helpers"
```

---

## Task 5: Migrate `get_metadata_env_type` in `runtimed-py`

**Files:**
- Modify: `crates/runtimed-py/src/session_core.rs`
- Modify: `crates/runtimed-py/src/async_session.rs` (call site at line 1019)

**Goal:** `get_metadata_env_type` returns `Option<PackageManager>` internally. The PyO3 boundary continues to expose `Option<str>` — Python sees the same API.

- [ ] **Step 1: Change the helper**

In `crates/runtimed-py/src/session_core.rs`, replace lines 143-154:

```rust
pub(crate) fn get_metadata_env_type(
    snapshot: &NotebookMetadataSnapshot,
) -> Option<notebook_protocol::connection::PackageManager> {
    use notebook_protocol::connection::PackageManager;
    if snapshot.runt.pixi.is_some() {
        return Some(PackageManager::Pixi);
    }
    if snapshot.runt.conda.is_some() {
        return Some(PackageManager::Conda);
    }
    if snapshot.runt.uv.is_some() {
        return Some(PackageManager::Uv);
    }
    None
}
```

- [ ] **Step 2: Update the caller at the PyO3 boundary**

In `crates/runtimed-py/src/async_session.rs` at line 1019 (the body of the `get_metadata_env_type` Python method), convert the enum to a Python string before returning:

```rust
        Ok(session_core::get_metadata_env_type(&snapshot)
            .map(|pm| pm.as_str().to_string())
```

Keep the surrounding PyO3 wrapping logic (`into_py`/`into_pyobject`) intact — only the inner `Option<String>` → `Option<PackageManager>` transformation changes, then `.map(|pm| pm.as_str().to_string())` restores the string for Python.

- [ ] **Step 3: Check for any other callers of `get_metadata_env_type`**

Run: `rg "get_metadata_env_type" crates/runtimed-py/src -n`

Every caller should now call `.map(|pm| pm.as_str().to_string())` (for string output) or use the enum directly (internal Rust). Fix any remaining sites.

- [ ] **Step 4: Build + test**

Run:
```bash
cargo build -p runtimed-py 2>&1 | tail -20
```
Expected: clean.

Rebuild the Python bindings and run the Python tests:
```bash
up rebuild=true
```

Then run the integration tests that exercise `get_metadata_env_type`:
```bash
cargo xtask integration async_session 2>&1 | tail -20
```
Expected: all green.

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed-py/src/session_core.rs crates/runtimed-py/src/async_session.rs
git commit -m "refactor(runtimed-py): return PackageManager from get_metadata_env_type"
```

---

## Task 6: Update the Python docstring and .pyi stub

**Files:**
- Modify: `crates/runtimed-py/src/async_client.rs` (docstring on `create_notebook`)
- Modify: `python/runtimed/src/runtimed/_internals.pyi` (the stub declares `package_manager: str | None = None` at one site)

**Goal:** The Python-facing docstring already lists `"uv"/"conda"/"pixi"` correctly. Verify the `.pyi` stub matches. No source code change.

- [ ] **Step 1: Verify the docstring**

Read `crates/runtimed-py/src/async_client.rs` around line 189. Confirm the line:
```
package_manager: Package manager ("uv", "conda", "pixi"). When None, daemon uses default_python_env.
```
already matches the enum's accepted values. If aliases ("pip", "mamba") are worth documenting, append:
```
Also accepts "pip" (→ uv) and "mamba" (→ conda) aliases.
```

- [ ] **Step 2: Check the type stub**

Run: `rg "package_manager" python/runtimed/src/runtimed/_internals.pyi`
Expected: one match — `package_manager: str | None = None,`. No change required; the Python API is still `str | None`, backed by `PackageManager::parse` inside the PyO3 boundary.

- [ ] **Step 3: Commit (if anything changed)**

```bash
git add -u
git commit -m "docs(runtimed-py): document package_manager aliases"
```

If nothing changed, skip the commit.

---

## Task 7: Delete `normalize_package_manager` and its tests

**Files:**
- Modify: `crates/notebook-protocol/src/connection.rs`
- Modify: `crates/runt-mcp/src/tools/session.rs` (delete the `normalize_package_manager` test block)

**Goal:** Retire the band-aid now that every caller uses `PackageManager::parse`.

- [ ] **Step 1: Delete the function**

In `crates/notebook-protocol/src/connection.rs`, delete lines 161-182 (the doc comment, the `pub fn normalize_package_manager` body, and any surrounding blank lines).

- [ ] **Step 2: Delete its tests**

Delete the three tests (lines 999-1016):
- `normalize_package_manager_valid`
- `normalize_package_manager_aliases`
- `normalize_package_manager_rejects_unknown`

Coverage is fully carried by the `PackageManager::parse*` tests from Task 1.

- [ ] **Step 3: Delete the mirrored test in runt-mcp**

In `crates/runt-mcp/src/tools/session.rs`, grep for the `#[test]` block that uses `normalize_package_manager` (around lines 690-705):

```bash
rg -n "normalize_package_manager" crates/runt-mcp/src/tools/session.rs
```

Delete the whole test function (from its `#[test]` or `#[cfg(test)]` scope start to the closing brace). This test was redundant coverage of the shared `notebook_protocol::connection::normalize_package_manager` function.

- [ ] **Step 4: Verify no lingering references**

Run: `rg "normalize_package_manager" --type rust -n`
Expected: zero matches.

- [ ] **Step 5: Build workspace + lint + run tests**

Run:
```bash
cargo build --workspace --all-targets 2>&1 | tail -10
cargo xtask clippy 2>&1 | tail -10
cargo test -p notebook-protocol
cargo test -p runt-mcp
cargo test -p runtimed --lib
```
Expected: all green.

- [ ] **Step 6: Run the daemon integration tests**

Run: `cargo xtask integration 2>&1 | tail -30`
Expected: all green. Exercises the full create_notebook-with-deps + package-manager path.

- [ ] **Step 7: Commit**

```bash
git add -u
git commit -m "refactor: delete normalize_package_manager in favor of PackageManager::parse"
```

---

## Task 8: End-to-end verification

**Goal:** Confirm the wire format is byte-identical to main and that a real dev-daemon session works.

- [ ] **Step 1: Manually inspect wire format**

Run the unit test that dumps a `CreateNotebook` handshake as JSON:

```bash
cargo test -p notebook-protocol test_handshake_serialization -- --nocapture 2>&1 | grep 'create_notebook'
```

Expected: lines like
```
{"channel":"create_notebook","runtime":"python"}
```
identical to before. (The enum's `rename_all = "lowercase"` produces `"uv"`/`"conda"`/`"pixi"` identical to the old `String` values.)

- [ ] **Step 2: Rebuild Python bindings and run MCP create_notebook**

```bash
up rebuild=true
```

Then, via nteract-dev MCP, create a notebook with an explicit conda package manager and a dependency:

```
create_notebook(runtime="python", dependencies=["numpy"], package_manager="conda", ephemeral=true)
```

Expected response: includes `"package_manager": "conda"` and auto-launches a conda kernel. `get_dependencies()` on the same notebook returns `{"package_manager": "conda", "dependencies": ["numpy"], ...}`.

Also test an alias:

```
create_notebook(runtime="python", dependencies=["pandas"], package_manager="mamba", ephemeral=true)
```

Expected: same as above but with `"package_manager": "conda"` (alias folded).

And an unknown value:

```
create_notebook(runtime="python", package_manager="poetry", ephemeral=true)
```

Expected: tool error `Unsupported package manager 'poetry'. Supported: uv, conda, pixi.`

- [ ] **Step 3: Final commit (if any doc or stray fix needed)**

If Steps 1-2 surfaced a defect, fix it and commit. Otherwise skip.

- [ ] **Step 4: Run `cargo xtask lint --fix`**

Run: `cargo xtask lint --fix`
Expected: no diffs produced (code is already formatted).

If diffs appear, amend the appropriate commit (or create a small `chore: fmt` commit).

---

## Testing Summary

| What | Why |
|------|-----|
| `cargo test -p notebook-protocol` | Enum definition, parse/FromStr/Display/serde roundtrip |
| `cargo test -p notebook-sync` | Handshake serialization invariant |
| `cargo test -p runtimed --lib` | Auto-launch metadata resolution, `build_new_notebook_metadata` |
| `cargo test -p runt-mcp` | MCP tool handlers (create_notebook, add_dependency, etc.) |
| `cargo xtask integration` | End-to-end daemon + create_notebook + deps |
| `up rebuild=true` + MCP `create_notebook` | Real PyO3 boundary behaves as expected |
| `cargo xtask clippy` | No new warnings, exhaustive-match coverage |

## Deliverables

At the end of PR 1:
- New enum: `notebook_protocol::connection::PackageManager` (Copy, 3 variants, serde-lowercase).
- All 10 call sites through the 6 crates carry the enum internally.
- `normalize_package_manager` deleted; its tests retired.
- Wire format identical; Python API identical; MCP JSON identical.
- PR 2 (EnvSource enum) stays in `docs/superpowers/specs/2026-04-22-package-manager-enum-design.md` for a follow-up plan.
