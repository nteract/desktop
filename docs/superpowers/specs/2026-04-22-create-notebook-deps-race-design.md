# Fix: CreateNotebook auto-launch races CRDT dep sync

**Issue:** [#2027](https://github.com/nteract/desktop/issues/2027)

## Problem

`CreateNotebook` with explicit dependencies launches the kernel on a bare prewarmed env. The declared deps only take effect after a manual `restart_kernel`.

The MCP `create_notebook` tool writes deps into the CRDT *after* the handshake. By then, `auto_launch_kernel` has already read the (empty) metadata snapshot and fallen through to `uv:prewarmed`.

Timeline:
1. MCP calls `connect_create()` - handshake carries runtime + working_dir, no deps
2. Daemon creates room, calls `create_empty_notebook` with empty dep lists
3. `handle_notebook_sync_connection` starts, first peer triggers `auto_launch_kernel`
4. `auto_launch_kernel` reads metadata - deps empty, picks prewarmed
5. MCP writes deps to CRDT via `add_dep_for_manager` - too late

## Fix

Pass `dependencies` and `package_manager` through the `CreateNotebook` request so the daemon seeds them into the Automerge doc *before* auto-launch reads it.

No changes to auto-launch, env resolution, or the CRDT read paths. The fix is entirely on the write side of notebook creation.

## Changes

### 1. Protocol: `CreateNotebook` request

**File:** `crates/notebook-protocol/src/connection.rs`

Add two optional fields to the `CreateNotebook` variant:

```rust
CreateNotebook {
    runtime: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    working_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notebook_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ephemeral: Option<bool>,
    // New fields:
    #[serde(default, skip_serializing_if = "Option::is_none")]
    package_manager: Option<String>,  // "uv", "conda", "pixi"
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dependencies: Vec<String>,
}
```

`dependencies` defaults to empty vec (serde default). `package_manager` is `Option` - see section 3 for resolution rules when omitted.

### 2. Daemon handler: `handle_create_notebook`

**File:** `crates/runtimed/src/daemon.rs`

Accept the new fields and pass them to `create_empty_notebook`:

```rust
async fn handle_create_notebook<S>(
    self: Arc<Self>,
    stream: S,
    runtime: String,
    working_dir: Option<String>,
    notebook_id_hint: Option<String>,
    ephemeral: Option<bool>,
    package_manager: Option<String>,   // new
    dependencies: Vec<String>,         // new
    client_protocol_version: u8,
) -> anyhow::Result<()>
```

Thread `package_manager` and `dependencies` into `create_empty_notebook`.

### 3. Notebook creation: `create_empty_notebook` + `build_new_notebook_metadata`

**File:** `crates/runtimed/src/notebook_sync_server/load.rs`

Extend both functions to accept and use the dep info:

```rust
pub fn create_empty_notebook(
    doc: &mut NotebookDoc,
    runtime: &str,
    default_python_env: PythonEnvType,
    env_id: Option<&str>,
    package_manager: Option<&str>,   // new
    dependencies: &[String],         // new
) -> Result<String, String>
```

Inside `build_new_notebook_metadata`, the package manager resolution follows this priority:

1. **Explicit `package_manager`** - use it, even with empty deps. This lets callers request a conda or pixi env without pre-declaring packages (e.g. interactive `conda install` workflow). The metadata section is created with whatever deps are provided (possibly empty).
2. **No `package_manager`, non-empty `dependencies`** - use `default_python_env` from daemon settings. This respects the user's configured default rather than hardcoding uv.
3. **Neither provided** - preserve current behavior (empty section based on `default_python_env`).

When `package_manager` is explicit, `build_new_notebook_metadata` creates *only* that manager's section and clears competing sections (matching `ensure_package_manager_metadata` exclusivity semantics).

The metadata snapshot gets written to the doc before the function returns. By the time `handle_notebook_sync_connection` hands the room to the sync loop and auto-launch fires, deps are already in the CRDT.

### 4. Sync client: `connect_create`

**File:** `crates/notebook-sync/src/connect.rs`

Add the new fields to `connect_create` so callers can pass them through the handshake:

```rust
pub async fn connect_create(
    socket_path: PathBuf,
    runtime: &str,
    working_dir: Option<PathBuf>,
    actor_label: &str,
    ephemeral: bool,
    package_manager: Option<&str>,   // new
    dependencies: Vec<String>,       // new
) -> Result<CreateResult, SyncError>
```

Serialize into the `CreateNotebook` handshake frame.

### 5. MCP tool: `create_notebook`

**File:** `crates/runt-mcp/src/tools/session.rs`

Pass `dependencies` and `package_manager` through `connect_create` in the handshake. Remove the post-creation dep-writing loop:

- Remove: `confirm_sync()` call before metadata writes
- Remove: `ensure_package_manager_metadata()` call
- Remove: `add_dep_for_manager()` loop
- Keep: the `deps` and `explicit_pkg_manager` argument parsing (still needed to populate the handshake)

The MCP tool becomes simpler - it passes deps in the request and the daemon handles everything.

### 6. Python bindings (if `connect_create` is exposed)

**File:** `crates/runtimed-py/src/lib.rs` or `crates/notebook-sync/src/connect.rs`

If the Python `create_notebook` binding calls `connect_create`, update its signature too. Check whether `runtimed-py` exposes `create_notebook` and thread the new args if so.

## What doesn't change

- `auto_launch_kernel` in `metadata.rs` - reads CRDT as before, now finds deps already present
- `initial_metadata` path in `peer.rs` - not involved (CreateNotebook uses `create_empty_notebook`)
- `resolve_metadata_snapshot` - untouched
- `add_dependency` MCP tool - still works for adding deps after creation
- `ensure_package_manager_metadata` - still used by `add_dependency` tool, just no longer called during `create_notebook`

## Testing

1. **Unit test:** `create_empty_notebook` with deps produces a metadata snapshot containing them
2. **Integration test:** `create_notebook` with conda deps, verify auto-launch picks `conda:inline` (not `uv:prewarmed`)
3. **Manual:** Run the data-scientist gremlin, confirm first `import pandas` succeeds without restart
