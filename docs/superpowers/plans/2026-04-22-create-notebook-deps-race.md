# CreateNotebook Deps Race Fix — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Pass `dependencies` and `package_manager` through the `CreateNotebook` handshake so deps are in the CRDT before auto-launch reads it. Fixes [#2027](https://github.com/nteract/desktop/issues/2027).

**Architecture:** Add two fields to the `CreateNotebook` wire type. Thread them through the daemon handler into `build_new_notebook_metadata`, which already creates the metadata snapshot. The MCP tool stops writing deps post-creation and stops doing a shutdown+relaunch cycle. All downstream callers (`connect_create`, `connect_create_relay`, Python bindings, Node bindings, Tauri relay, integration tests) get updated signatures with defaulted new args.

**Tech Stack:** Rust (notebook-protocol, runtimed, notebook-sync, runt-mcp, runtimed-py, runtimed-node, notebook crate)

**Spec:** `docs/superpowers/specs/2026-04-22-create-notebook-deps-race-design.md`

---

### Task 1: Protocol — add fields to `CreateNotebook`

**Files:**
- Modify: `crates/notebook-protocol/src/connection.rs:129-150`

- [ ] **Step 1: Add `package_manager` and `dependencies` to `CreateNotebook`**

In `crates/notebook-protocol/src/connection.rs`, add two fields to the `CreateNotebook` variant:

```rust
    CreateNotebook {
        /// Runtime type: "python" or "deno".
        runtime: String,
        /// Working directory for project file detection (pyproject.toml, pixi.toml, environment.yml).
        /// Used since untitled notebooks have no path to derive working_dir from.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_dir: Option<String>,
        /// Optional notebook_id hint for restoring an untitled notebook from a previous session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        notebook_id: Option<String>,
        /// When true, the notebook exists only in memory — no .automerge persisted to disk.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ephemeral: Option<bool>,
        /// Package manager preference: "uv", "conda", or "pixi".
        /// When set, the daemon creates only this manager's metadata section.
        /// When None, the daemon uses its default_python_env setting.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        package_manager: Option<String>,
        /// Dependencies to seed into the notebook metadata before auto-launch.
        /// Passed to build_new_notebook_metadata so they're in the CRDT before
        /// auto_launch_kernel reads the metadata snapshot.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        dependencies: Vec<String>,
    },
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p notebook-protocol 2>&1 | tail -5`
Expected: compilation succeeds (serde defaults make this backward compatible on the wire)

- [ ] **Step 3: Commit**

```bash
git add crates/notebook-protocol/src/connection.rs
git commit -m "$(cat <<'EOF'
feat(protocol): add package_manager and dependencies to CreateNotebook

Seeds dependency metadata into the handshake so the daemon can write
it to the CRDT before auto_launch_kernel fires. Fixes the race where
deps arrive after auto-launch falls through to prewarmed.

Part of #2027.
EOF
)"
```

---

### Task 2: Notebook creation — accept deps in `build_new_notebook_metadata` and `create_empty_notebook`

**Files:**
- Modify: `crates/runtimed/src/notebook_sync_server/load.rs:725-844`

- [ ] **Step 1: Extend `build_new_notebook_metadata` signature and logic**

In `crates/runtimed/src/notebook_sync_server/load.rs`, change the function signature and the Python branch to use request-level deps:

```rust
/// Build default metadata for a new notebook based on runtime.
///
/// Package manager resolution priority:
/// 1. Explicit `package_manager` — use it, even with empty deps.
/// 2. No `package_manager`, non-empty `dependencies` — use `default_python_env`.
/// 3. Neither — preserve current behavior (empty section based on `default_python_env`).
pub(crate) fn build_new_notebook_metadata(
    runtime: &str,
    env_id: &str,
    default_python_env: crate::settings_doc::PythonEnvType,
    package_manager: Option<&str>,
    dependencies: &[String],
) -> NotebookMetadataSnapshot {
    use crate::notebook_metadata::{
        CondaInlineMetadata, KernelspecSnapshot, LanguageInfoSnapshot, RuntMetadata,
        UvInlineMetadata,
    };

    let (kernelspec, language_info, runt) = match runtime {
        "deno" => (
            KernelspecSnapshot {
                name: "deno".to_string(),
                display_name: "Deno".to_string(),
                language: Some("typescript".to_string()),
            },
            LanguageInfoSnapshot {
                name: "typescript".to_string(),
                version: None,
            },
            RuntMetadata {
                schema_version: "1".to_string(),
                env_id: Some(env_id.to_string()),
                uv: None,
                conda: None,
                pixi: None,
                deno: None,
                trust_signature: None,
                trust_timestamp: None,
                extra: std::collections::BTreeMap::new(),
            },
        ),
        _ => {
            // Python (default)
            //
            // Resolve which package manager section to create:
            //   1. Explicit package_manager from the request
            //   2. No explicit manager but deps provided — use default_python_env
            //   3. Neither — use default_python_env with empty deps
            let effective_manager: &str = match package_manager {
                Some(pm) => pm,
                None => match default_python_env {
                    crate::settings_doc::PythonEnvType::Conda => "conda",
                    crate::settings_doc::PythonEnvType::Pixi => "pixi",
                    _ => "uv",
                },
            };

            let deps = dependencies.to_vec();

            let (uv, conda, pixi) = match effective_manager {
                "conda" => (
                    None,
                    Some(CondaInlineMetadata {
                        dependencies: deps,
                        channels: vec!["conda-forge".to_string()],
                        python: None,
                    }),
                    None,
                ),
                "pixi" => (
                    None,
                    None,
                    Some(notebook_doc::metadata::PixiInlineMetadata {
                        dependencies: deps,
                        pypi_dependencies: vec![],
                        channels: vec!["conda-forge".to_string()],
                        python: None,
                    }),
                ),
                _ => (
                    Some(UvInlineMetadata {
                        dependencies: deps,
                        requires_python: None,
                        prerelease: None,
                    }),
                    None,
                    None,
                ),
            };

            (
                KernelspecSnapshot {
                    name: "python3".to_string(),
                    display_name: "Python 3".to_string(),
                    language: Some("python".to_string()),
                },
                LanguageInfoSnapshot {
                    name: "python".to_string(),
                    version: None,
                },
                RuntMetadata {
                    schema_version: "1".to_string(),
                    env_id: Some(env_id.to_string()),
                    uv,
                    conda,
                    pixi,
                    deno: None,
                    trust_signature: None,
                    trust_timestamp: None,
                    extra: std::collections::BTreeMap::new(),
                },
            )
        }
    };

    NotebookMetadataSnapshot {
        kernelspec: Some(kernelspec),
        language_info: Some(language_info),
        runt,
    }
}
```

- [ ] **Step 2: Extend `create_empty_notebook` to pass through the new args**

Same file, update `create_empty_notebook`:

```rust
pub fn create_empty_notebook(
    doc: &mut NotebookDoc,
    runtime: &str,
    default_python_env: crate::settings_doc::PythonEnvType,
    env_id: Option<&str>,
    package_manager: Option<&str>,
    dependencies: &[String],
) -> Result<String, String> {
    let env_id = env_id
        .map(|s| s.to_string())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let metadata_snapshot =
        build_new_notebook_metadata(runtime, &env_id, default_python_env, package_manager, dependencies);

    doc.set_metadata_snapshot(&metadata_snapshot)
        .map_err(|e| format!("Failed to set metadata: {}", e))?;

    Ok(env_id)
}
```

- [ ] **Step 3: Fix compilation — update all other callers of `build_new_notebook_metadata` and `create_empty_notebook`**

Search for all callers:

Run: `grep -rn 'build_new_notebook_metadata\|create_empty_notebook' crates/ --include="*.rs" | grep -v 'target/'`

Known call sites beyond the ones already updated:
- `crates/runtimed/src/daemon.rs:1985` (another `create_empty_notebook` call in `handle_open_notebook`)
- `crates/runtimed/src/daemon.rs:2164` (handled in Task 3)
- `crates/runtimed/src/notebook_sync_server/tests.rs` — multiple `build_new_notebook_metadata` calls (around lines 2001, 2021, 2045) and `create_empty_notebook` calls (around lines 2073, 2091, 2112, 4052, 4166)

All non-Task-3 callers pass `None, &[]` for the new args (preserving existing behavior). `cargo check` alone won't catch test breakage - run `cargo test -p runtimed --no-run` to verify test compilation too.

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p runtimed 2>&1 | tail -5`
Expected: may fail on daemon.rs (Task 3 fixes that) — check that load.rs itself is clean.

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed/src/notebook_sync_server/load.rs
git commit -m "$(cat <<'EOF'
feat(runtimed): accept deps in build_new_notebook_metadata

Seeds package_manager and dependencies into the metadata snapshot at
creation time. Priority: explicit package_manager > default_python_env.
Deps are written to the CRDT before auto_launch_kernel reads it.

Part of #2027.
EOF
)"
```

---

### Task 3: Daemon handler — thread new fields through `handle_create_notebook`

**Files:**
- Modify: `crates/runtimed/src/daemon.rs:1701-1716` (dispatch) and `crates/runtimed/src/daemon.rs:2104-2175` (handler)

- [ ] **Step 1: Update the dispatch match arm**

In `crates/runtimed/src/daemon.rs` around line 1701, update the `CreateNotebook` destructure:

```rust
            Handshake::CreateNotebook {
                runtime,
                working_dir,
                notebook_id,
                ephemeral,
                package_manager,
                dependencies,
            } => {
                self.handle_create_notebook(
                    stream,
                    runtime,
                    working_dir,
                    notebook_id,
                    ephemeral,
                    package_manager,
                    dependencies,
                    client_protocol_version,
                )
                .await
            }
```

- [ ] **Step 2: Update `handle_create_notebook` signature**

Around line 2104:

```rust
    async fn handle_create_notebook<S>(
        self: Arc<Self>,
        stream: S,
        runtime: String,
        working_dir: Option<String>,
        notebook_id_hint: Option<String>,
        ephemeral: Option<bool>,
        package_manager: Option<String>,
        dependencies: Vec<String>,
        client_protocol_version: u8,
    ) -> anyhow::Result<()>
    where
        S: AsyncRead + AsyncWrite + Unpin,
    {
```

- [ ] **Step 3: Thread args into `create_empty_notebook` call**

Around line 2164, update the call:

```rust
                match crate::notebook_sync_server::create_empty_notebook(
                    &mut doc,
                    &runtime,
                    default_python_env.clone(),
                    Some(&notebook_id),
                    package_manager.as_deref(),
                    &dependencies,
                ) {
```

- [ ] **Step 4: Update the `needs_trust_approval` comment**

Around line 2212, the comment says "New notebooks have no deps, so no trust approval needed." Update it:

```rust
        // Trust approval is handled later via CRDT trust state, not at creation time.
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p runtimed 2>&1 | tail -5`
Expected: compilation succeeds for the daemon crate

- [ ] **Step 6: Commit**

```bash
git add crates/runtimed/src/daemon.rs
git commit -m "$(cat <<'EOF'
feat(runtimed): thread deps through handle_create_notebook

Passes package_manager and dependencies from the CreateNotebook
handshake into create_empty_notebook so they're in the CRDT before
auto_launch_kernel fires.

Part of #2027.
EOF
)"
```

---

### Task 4: Sync client — update `connect_create` and `connect_create_inner`

**Files:**
- Modify: `crates/notebook-sync/src/connect.rs:271-316` (`connect_create` + `connect_create_inner`)

- [ ] **Step 1: Update `connect_create` public API**

```rust
pub async fn connect_create(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    actor_label: &str,
    ephemeral: bool,
    package_manager: Option<&str>,
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

- [ ] **Step 2: Update `connect_create_inner`**

```rust
async fn connect_create_inner(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    notebook_id: Option<String>,
    actor_label: &str,
    ephemeral: bool,
    package_manager: Option<&str>,
    dependencies: Vec<String>,
) -> Result<CreateResult, SyncError> {
```

And update the `Handshake::CreateNotebook` construction around line 306:

```rust
    let handshake = Handshake::CreateNotebook {
        runtime: runtime.to_string(),
        working_dir: working_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
        notebook_id,
        ephemeral: if ephemeral { Some(true) } else { None },
        package_manager: package_manager.map(String::from),
        dependencies,
    };
```

- [ ] **Step 3: Update `connect_create_relay`**

Around line 477, add the same two parameters and thread them into the handshake:

```rust
pub async fn connect_create_relay(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    notebook_id: Option<String>,
    frame_tx: mpsc::UnboundedSender<Vec<u8>>,
    ephemeral: bool,
    package_manager: Option<&str>,
    dependencies: Vec<String>,
) -> Result<RelayCreateResult, SyncError> {
```

And update the handshake construction around line 494:

```rust
    let handshake = Handshake::CreateNotebook {
        runtime: runtime.to_string(),
        working_dir: working_dir
            .as_ref()
            .map(|p| p.to_string_lossy().to_string()),
        notebook_id,
        ephemeral: if ephemeral { Some(true) } else { None },
        package_manager: package_manager.map(String::from),
        dependencies,
    };
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p notebook-sync 2>&1 | tail -5`
Expected: succeeds (downstream callers will break, fixed in next tasks)

- [ ] **Step 5: Commit**

```bash
git add crates/notebook-sync/src/connect.rs
git commit -m "$(cat <<'EOF'
feat(notebook-sync): add deps to connect_create handshake

Extends connect_create, connect_create_inner, and connect_create_relay
to accept package_manager and dependencies, serialized into the
CreateNotebook handshake frame.

Part of #2027.
EOF
)"
```

---

### Task 5: MCP tool — pass deps in handshake, remove post-creation workaround

**Files:**
- Modify: `crates/runt-mcp/src/tools/session.rs:408-598`

- [ ] **Step 1: Validate package_manager before `connect_create`**

Move the `package_manager` validation (currently around line 451) to *before* the `connect_create` call, so we can pass validated args in the handshake:

```rust
pub async fn create_notebook(
    server: &NteractMcp,
    request: &CallToolRequestParams,
) -> Result<CallToolResult, McpError> {
    let kernel_alias = arg_str(request, "kernel");
    let runtime_arg = arg_str(request, "runtime");
    let used_kernel_alias = kernel_alias.is_some() && runtime_arg.is_none();
    let runtime = runtime_arg.or(kernel_alias).unwrap_or("python");

    let working_dir = arg_str(request, "working_dir")
        .map(|s| PathBuf::from(resolve_path(s)))
        .or_else(|| std::env::current_dir().ok());
    let ephemeral = arg_bool(request, "ephemeral").unwrap_or(true);

    let deps: Vec<String> = arg_string_array(request, "dependencies").unwrap_or_default();
    let explicit_pkg_manager = arg_str(request, "package_manager");

    // Validate package_manager before handshake
    if let Some(pm) = explicit_pkg_manager {
        if !matches!(pm, "uv" | "conda" | "pixi") {
            return tool_error(&format!(
                "Invalid package_manager '{}'. Must be 'uv', 'conda', or 'pixi'.",
                pm
            ));
        }
    }

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
    {
```

- [ ] **Step 2: Remove the post-creation dep-writing workaround**

Remove the entire block from `confirm_sync` through the kernel restart loop (approximately lines 460-551 in the current code). Replace with just the session setup and runtime info collection:

```rust
        Ok(result) => {
            if let Err(e) = result.handle.await_session_ready().await {
                return tool_error(&format!("Notebook created but did not become ready: {}", e));
            }

            let notebook_id = result.handle.notebook_id().to_string();

            // Announce presence so the peer is visible immediately
            let peer_label = server.get_peer_label().await;
            crate::presence::announce(&result.handle, &peer_label).await;

            // Effective package manager: explicit arg, or what the daemon set.
            let pkg_manager: String = explicit_pkg_manager
                .map(String::from)
                .unwrap_or_else(|| super::deps::detect_package_manager(&result.handle));

            let session = NotebookSession {
                handle: result.handle,
                broadcast_rx: result.broadcast_rx,
                notebook_id: notebook_id.clone(),
                notebook_path: None,
            };
            *server.session.write().await = Some(session);

            // Collect resolved runtime info for the response
            let runtime_info = {
                let guard = server.session.read().await;
                if let Some(s) = guard.as_ref() {
                    collect_runtime_info(&s.handle).await
                } else {
                    serde_json::json!({ "language": runtime })
                }
            };

            let all_deps = {
                let guard = server.session.read().await;
                guard.as_ref().map_or_else(Vec::new, |s| {
                    super::deps::get_deps_for_manager_pub(&s.handle, &pkg_manager)
                })
            };

            let mut info = serde_json::json!({
                "notebook_id": notebook_id,
                "runtime": runtime_info,
                "dependencies": all_deps,
                "added_dependencies": deps,
                "package_manager": pkg_manager,
                "ephemeral": ephemeral,
            });

            if let Some(ref prev_id) = prev {
                if *prev_id != notebook_id {
                    info["switched_from"] = serde_json::json!(prev_id);
                }
            }

            if used_kernel_alias {
                info["info"] = serde_json::json!("Used 'kernel' parameter (alias for 'runtime')");
            }

            Ok(CallToolResult::success(vec![Content::text(
                serde_json::to_string_pretty(&info).unwrap_or_default(),
            )]))
        }
```

The key removals:
- `confirm_sync()` before metadata writes
- `ensure_package_manager_metadata()` call
- `add_dep_for_manager()` loop
- `metadata_changed` variable
- `needs_restart` check and the entire `ShutdownKernel` + `LaunchKernel` + poll loop

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p runt-mcp 2>&1 | tail -5`
Expected: compilation succeeds

- [ ] **Step 4: Commit**

```bash
git add crates/runt-mcp/src/tools/session.rs
git commit -m "$(cat <<'EOF'
fix(runt-mcp): pass deps in CreateNotebook handshake, remove relaunch workaround

Dependencies and package_manager are now sent in the handshake. The
daemon seeds them into the CRDT before auto-launch, so the post-creation
confirm_sync + ensure_package_manager + add_deps + shutdown + relaunch
cycle is no longer needed.

Fixes #2027.
EOF
)"
```

---

### Task 6: Update downstream callers — Node bindings, Tauri relay

**Files:**
- Modify: `crates/runtimed-node/src/session.rs:273-280`
- Modify: `crates/notebook/src/lib.rs:730-737`

These callers don't create notebooks with deps, so they pass `None, vec![]`.

- [ ] **Step 1: Update Node bindings `session.rs`**

In `crates/runtimed-node/src/session.rs` around line 273:

```rust
    let result = notebook_sync::connect::connect_create(
        socket_path.clone(),
        &runtime,
        working_dir.clone(),
        &actor_label,
        /* ephemeral */ false,
        None,     // package_manager
        vec![],   // dependencies
    )
    .await
    .map_err(to_napi_err)?;
```

- [ ] **Step 2: Update Tauri relay `connect_create_relay` call**

In `crates/notebook/src/lib.rs` around line 730:

```rust
    let result = notebook_sync::connect::connect_create_relay(
        socket_path,
        &runtime,
        working_dir,
        notebook_id_hint,
        frame_tx,
        false,
        None,     // package_manager — frontend creates notebooks without deps
        vec![],   // dependencies
    )
    .await
    .map_err(|e| format!("sync connect (create): {}", e))?;
```

- [ ] **Step 3: Commit**

```bash
git add crates/runtimed-node/src/session.rs crates/notebook/src/lib.rs
git commit -m "$(cat <<'EOF'
chore: update connect_create callers for new deps args

Node bindings and Tauri relay pass None/empty for the new
package_manager and dependencies parameters.

Part of #2027.
EOF
)"
```

---

### Task 7: Thread deps through Python bindings end-to-end

**Files:**
- Modify: `crates/runtimed-py/src/session_core.rs:401-422`
- Modify: `crates/runtimed-py/src/async_client.rs:190-221`
- Modify: `python/runtimed/src/runtimed/_client.py:42-60`

The Python API already accepts `dependencies` on `AsyncClient.create_notebook()` but adds them post-creation via `add_dependencies` - the same race. Thread them into the handshake.

- [ ] **Step 1: Add `package_manager` and `dependencies` to `session_core::connect_create`**

In `crates/runtimed-py/src/session_core.rs` around line 401:

```rust
pub(crate) async fn connect_create(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    actor_label: Option<&str>,
    package_manager: Option<&str>,
    dependencies: Vec<String>,
) -> PyResult<(String, SessionState, NotebookConnectionInfo)> {
    let default_label;
    let label = match actor_label {
        Some(l) => l,
        None => {
            default_label = make_actor_label(DEFAULT_ACTOR_LABEL);
            &default_label
        }
    };
    let result = notebook_sync::connect::connect_create(
        socket_path.clone(),
        runtime,
        working_dir.clone(),
        label,
        false,
        package_manager,
        dependencies,
    )
    .await
    .map_err(to_py_err)?;
```

- [ ] **Step 2: Add `package_manager` and `dependencies` to `AsyncClient.create_notebook` (Rust PyO3)**

In `crates/runtimed-py/src/async_client.rs` around line 190:

```rust
    #[pyo3(signature = (runtime="python", working_dir=None, peer_label=None, package_manager=None, dependencies=None))]
    fn create_notebook<'py>(
        &self,
        py: Python<'py>,
        runtime: &str,
        working_dir: Option<&str>,
        peer_label: Option<String>,
        package_manager: Option<&str>,
        dependencies: Option<Vec<String>>,
    ) -> PyResult<Bound<'py, PyAny>> {
```

Thread `package_manager` and `dependencies.unwrap_or_default()` into the `session_core::connect_create` call.

- [ ] **Step 3: Update Python wrapper `_client.py` to pass deps in the handshake**

In `python/runtimed/src/runtimed/_client.py`, update `create_notebook`:

```python
    async def create_notebook(
        self,
        runtime: str = "python",
        working_dir: str | None = None,
        peer_label: str | None = None,
        dependencies: list[str] | None = None,
        package_manager: str | None = None,
    ) -> Notebook:
        """Create a new notebook and return a connected Notebook.

        Dependencies are passed in the creation handshake so the kernel
        launches with the correct environment on first try.
        """
        session = await self._native.create_notebook(
            runtime, working_dir, peer_label, package_manager, dependencies,
        )
        return Notebook(session)
```

Remove the post-creation `add_dependencies` call - deps are now in the handshake.

- [ ] **Step 4: Update the `async_session.rs` caller of `session_core::connect_create`**

In `crates/runtimed-py/src/async_session.rs` around line 84, thread the new args (this is called from `create_notebook_async`).

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p runtimed-py 2>&1 | tail -5`
Expected: compilation succeeds

- [ ] **Step 6: Commit**

```bash
git add crates/runtimed-py/src/session_core.rs crates/runtimed-py/src/async_client.rs crates/runtimed-py/src/async_session.rs python/runtimed/src/runtimed/_client.py
git commit -m "$(cat <<'EOF'
feat(runtimed-py): thread deps through Python create_notebook handshake

The Python API previously added deps post-creation via add_dependencies,
hitting the same CRDT race. Now package_manager and dependencies flow
through the CreateNotebook handshake so the daemon seeds them before
auto-launch.

Part of #2027.
EOF
)"
```

---

### Task 8: Update integration tests

**Files:**
- Modify: `crates/runtimed/tests/integration.rs`

- [ ] **Step 1: Update all existing `connect_create` calls**

Every `connect_create` call in integration.rs needs the two new args. They all pass `None, vec![]` to preserve existing behavior. Find them all:

Run: `grep -n 'connect_create(' crates/runtimed/tests/integration.rs`

Update each one. Example for line 431:

```rust
    let result1 = connect::connect_create(socket_path.clone(), "python", None, "test", false, None, vec![])
        .await
        .expect("client1 should connect");
```

Repeat for every call site in the file.

- [ ] **Step 2: Add a test for `create_notebook` with deps — verify metadata AND env source**

Add tests that verify (a) deps are in the metadata snapshot and (b) auto-launch picks the correct env source from RuntimeStateDoc, not `uv:prewarmed`.

```rust
#[tokio::test]
async fn test_create_notebook_with_deps() {
    let (_dir, socket_path) = start_daemon().await;
    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    // Create notebook with conda deps
    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        Some("conda"),
        vec!["pandas".to_string(), "numpy".to_string()],
    )
    .await
    .expect("should connect");

    assert!(
        wait_for_session_ready(&result.handle, Duration::from_secs(2)).await,
        "should reach session-ready"
    );

    // Verify metadata has the conda deps
    let metadata = result.handle.get_notebook_metadata();
    let metadata = metadata.expect("metadata should be present");
    assert!(metadata.runt.uv.is_none(), "uv section should not exist");
    let conda = metadata.runt.conda.expect("conda section should exist");
    assert_eq!(conda.dependencies, vec!["pandas", "numpy"]);

    // Verify auto-launch picked conda:inline, not uv:prewarmed
    // Poll RuntimeStateDoc for up to 10s — auto-launch is async
    let start = std::time::Instant::now();
    let mut env_source = String::new();
    while start.elapsed() < Duration::from_secs(10) {
        if let Ok(state) = result.handle.get_runtime_state() {
            if !state.kernel.env_source.is_empty() {
                env_source = state.kernel.env_source.clone();
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert!(
        env_source.starts_with("conda:"),
        "expected conda env source, got: {}",
        env_source
    );
}

#[tokio::test]
async fn test_create_notebook_with_explicit_manager_no_deps() {
    let (_dir, socket_path) = start_daemon().await;
    let pool_client = PoolClient::new(socket_path.clone());
    assert!(wait_for_daemon(&pool_client, Duration::from_secs(5)).await);

    // Create notebook with explicit pixi manager but no deps
    let result = connect::connect_create(
        socket_path.clone(),
        "python",
        None,
        "test",
        false,
        Some("pixi"),
        vec![],
    )
    .await
    .expect("should connect");

    assert!(
        wait_for_session_ready(&result.handle, Duration::from_secs(2)).await,
        "should reach session-ready"
    );

    // Verify metadata has pixi section (not uv or conda)
    let metadata = result.handle.get_notebook_metadata();
    let metadata = metadata.expect("metadata should be present");
    assert!(metadata.runt.uv.is_none(), "uv section should not exist");
    assert!(metadata.runt.conda.is_none(), "conda section should not exist");
    assert!(metadata.runt.pixi.is_some(), "pixi section should exist");
}
```

Note: The env_source assertion in `test_create_notebook_with_deps` is the critical regression test. Without this fix, it would show `uv:prewarmed`.

- [ ] **Step 3: Run the tests**

Run: `cargo test -p runtimed --test integration test_create_notebook_with_deps test_create_notebook_with_explicit_manager_no_deps -- --nocapture 2>&1 | tail -20`
Expected: both tests pass

- [ ] **Step 4: Run all integration tests to check for regressions**

Run: `cargo test -p runtimed --test integration 2>&1 | tail -20`
Expected: all existing tests still pass

- [ ] **Step 5: Commit**

```bash
git add crates/runtimed/tests/integration.rs
git commit -m "$(cat <<'EOF'
test(runtimed): add integration tests for create_notebook with deps

Verifies that deps passed in the CreateNotebook handshake are present
in the CRDT metadata snapshot. Covers conda-with-deps and
explicit-pixi-no-deps cases.

Part of #2027.
EOF
)"
```

---

### Task 9: Lint and final verification

- [ ] **Step 1: Run formatter**

Run: `cargo xtask lint --fix 2>&1 | tail -10`
Expected: clean or auto-fixed

- [ ] **Step 2: Run clippy**

Run: `cargo xtask clippy 2>&1 | tail -20`
Expected: no new warnings

- [ ] **Step 3: Run full test suite**

Run: `cargo test 2>&1 | tail -20`
Expected: all tests pass

- [ ] **Step 4: Commit any lint fixes**

```bash
git add -u
git commit -m "chore: lint fixes for #2027"
```

(Skip if no changes.)
