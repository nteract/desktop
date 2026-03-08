# Review: PR #602 — refactor(notebook): daemon-owned notebook loading — Phase 4 Tauri client

**PR:** #602
**Author:** rgbkrk
**Branch:** `daemon-owned-notebook-loading` → `main`
**Stats:** 12 files changed, ~2011 additions, ~780 deletions (across all commits in the branch)
**Draft:** Yes

---

## Summary

This PR completes the migration from client-side notebook parsing to daemon-owned notebook loading. Instead of the Tauri client reading `.ipynb` files from disk, parsing them, constructing `NotebookState`, and then syncing with the daemon, the daemon now owns the entire loading lifecycle. The client just tells the daemon "open this file" or "create a new notebook" and receives a ready-to-sync connection.

The PR introduces two new handshake variants (`OpenNotebook`, `CreateNotebook`) alongside the existing `NotebookSync` (retained for session restore of untitled notebooks). The architectural shift removes `notebook_state` from `WindowNotebookContext`, deletes `format.rs`, removes `derive_notebook_id()`, `load_notebook_state_for_path()`, and `create_new_notebook_state()`.

Beyond the core refactor, the branch also includes several infrastructure improvements: kernel death detection (heartbeat + process watcher), broadcast lag recovery, IPC handshake timeout, socket/trust-key permission hardening, blob server `nosniff` header, and WASM sync change detection optimization.

---

## Overall Assessment

**This is a well-structured, thoughtfully designed refactoring.** The daemon-owned loading model is the correct architectural direction — it eliminates dual parsing paths, removes an entire class of state synchronization bugs, and simplifies the client significantly. The code is well-documented with clear comments explaining design decisions.

I recommend **merging after addressing a few issues** noted below.

---

## Issues Found

### 1. Race condition in `handle_open_notebook` error path — room cleanup under contention (Medium)

**File:** `crates/runtimed/src/daemon.rs`, `handle_open_notebook()`

When `load_notebook_from_disk` fails, the error path removes the room:
```rust
let mut rooms = self.notebook_rooms.lock().await;
rooms.remove(&notebook_id);
```

If two clients simultaneously request the same notebook and the first fails after `get_or_create_room` but before the second starts loading, the second client's connection may find its room deleted mid-operation. The `cell_count == 0` check-and-load is correctly done under write lock, but room removal after failure doesn't account for other connections that may have already obtained a reference to the `Arc<NotebookRoom>`.

**Mitigation:** This is low-probability since the room just failed to load and other clients would also fail. But consider tracking a "load attempted" flag in the room to distinguish "empty because new" from "empty because load failed" rather than removing the room outright. Or simply add a peer count check before removal.

### 2. `do_initial_sync` timeout-based sync completion is fragile (Low-Medium)

**File:** `crates/runtimed/src/notebook_sync_client.rs`, `do_initial_sync()`

```rust
Err(_) => break, // Timeout — initial sync is done
```

The 100ms timeout to determine "sync is complete" works in practice but could cause issues if the daemon is under load and takes slightly longer between sync messages. This could result in the client thinking sync is done while the daemon still has data to send. The existing `init()` method uses the same pattern, so this is consistent, but it's worth noting as a latent risk.

### 3. `Restore` mode doesn't set `runtime` from session (Low)

**File:** `crates/notebook/src/lib.rs`, `create_notebook_window_for_daemon()`

In the `OpenMode::Restore` branch:
```rust
OpenMode::Restore { notebook_id: _, working_dir } => {
    let runtime = settings::load_settings().default_runtime;
    ...
}
```

This uses the current default runtime setting rather than the `session.runtime` that was saved. If a user changes their default runtime between sessions, a restored untitled notebook would get the wrong runtime. The session data has the correct runtime — it should be threaded through `OpenMode::Restore`.

### 4. Duplicate code in `connect_open_split` / `connect_create_split` (Unix vs Windows) (Low)

**File:** `crates/runtimed/src/notebook_sync_client.rs`

The Unix and Windows implementations of `connect_open_split` and `connect_create_split` duplicate the entire method body except for the connection setup line. This is the existing pattern in the codebase (the existing `connect_split` also duplicates), but it's worth noting that it makes maintenance harder. Four nearly-identical method bodies now exist.

Not a blocker — matches existing conventions.

### 5. `format.rs` deletion — is the Tauri command still referenced? (Low)

**File:** `crates/notebook/src/format.rs` (deleted)

The entire `format.rs` module is deleted. This contained `format_python()` and `format_deno()` functions. I couldn't find a remaining Tauri command calling these, so the deletion appears safe. But confirm there are no frontend calls to a `format_cell` or similar Tauri command that would break.

### 6. `get_preferred_kernelspec` command removed — frontend impact? (Low)

**File:** `crates/notebook/src/lib.rs`

The `get_preferred_kernelspec` Tauri command is removed from the command list and its implementation deleted. This was used as a fallback path reading from `notebook_state`. If the frontend calls `invoke("get_preferred_kernelspec")`, it will now get an error. Ensure the frontend has been updated to no longer call this command.

### 7. Inconsistent `daemon:ready` payload for Restore path (Medium)

**File:** `crates/notebook/src/lib.rs`

The `OpenMode::Restore` path uses the legacy `initialize_notebook_sync()` which does **not** call `setup_sync_receivers()` and therefore does **not** emit `daemon:ready` with a `DaemonReadyPayload`. If the frontend expects this event to know when the notebook is ready, restored untitled notebooks will hang in a loading state. The Open/Create paths both emit this event via `setup_sync_receivers()`.

**Fix:** Either unify the Restore path to also call `setup_sync_receivers()`, or have the legacy `initialize_notebook_sync()` emit `daemon:ready` with an equivalent payload.

### 8. Wrong `runtime` recorded for opened notebooks (Medium)

**File:** `crates/notebook/src/lib.rs`, `create_notebook_window_for_daemon()`

In the `OpenMode::Open` branch:
```rust
let runtime = settings::load_settings().default_runtime;
```

This uses the user's *default* runtime instead of the notebook's *actual* runtime (which is embedded in the kernelspec metadata). A Deno notebook opened by a user whose default is Python will have `runtime: Python` in `WindowNotebookContext`, causing incorrect session save data. The actual runtime isn't known until the daemon responds with `NotebookConnectionInfo`, so this may need to be updated after the daemon connection completes.

### 9. Heartbeat monitor creates a new ZMQ connection every 5 seconds (Medium)

**File:** `crates/runtimed/src/kernel_manager.rs`

```rust
loop {
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    let check = async {
        let mut hb = runtimelib::create_client_heartbeat_connection(&hb_conn_info).await?;
        hb.single_heartbeat().await
    };
    ...
}
```

Each heartbeat check creates a new ZMQ connection, sends a heartbeat, and drops it. With many kernels, this creates significant connection churn. The connection should be created once and reused across heartbeat checks, falling back to reconnection only on error.

### 10. Single heartbeat failure kills the kernel (Medium)

**File:** `crates/runtimed/src/kernel_manager.rs`

A single heartbeat timeout or connection error immediately triggers `QueueCommand::KernelDied`. Transient network issues or heavy computation (which may delay the kernel's ZMQ event loop) will cause false positives. Consider requiring 2-3 consecutive failures before declaring the kernel dead.

### 11. Empty notebooks (0 cells) cause re-load on every connection (Low-Medium)

**File:** `crates/runtimed/src/daemon.rs`, `handle_open_notebook()`

```rust
if existing_count == 0 {
    // First connection - load from disk
```

Using `cell_count == 0` as a proxy for "not loaded yet" means a legitimately empty `.ipynb` file (or one with only metadata) will trigger a re-load from disk on every new connection. Consider using a `loaded: bool` flag in the room instead.

### 12. Error-path stream drain has no timeout (Low)

**File:** `crates/runtimed/src/daemon.rs`, `handle_open_notebook()` and `handle_create_notebook()`

```rust
let _ = tokio::io::copy(&mut reader, &mut tokio::io::sink()).await;
```

If the client never closes the connection after receiving an error response, this drain will block the task indefinitely. Add a timeout (e.g., 5 seconds) or simply drop the stream.

### 13. Silent failure on poisoned mutex (Low)

**File:** `crates/notebook/src/lib.rs`, `initialize_notebook_sync_open()` / `_create()`

```rust
if let Ok(mut id) = notebook_id.lock() {
    *id = info.notebook_id.clone();
}
```

A poisoned mutex silently leaves a stale placeholder ID. This should at least log a warning on the `Err` branch.

### 14. Iopub task leaks on launch failure (Low-Medium)

**File:** `crates/runtimed/src/kernel_manager.rs`

If `kernel_info_reply` times out during kernel launch, the error path aborts `process_watcher_task` but does **not** abort the `iopub_task` (which was spawned earlier). The iopub task continues running, reading from a dead/unresponsive kernel, and will eventually send `KernelDied` into a channel nobody is polling (since the launch failed and `cmd_rx` is stored but the sync server won't use it). This is a resource leak.

### 15. Doc write lock held during async disk I/O (Low-Medium)

**File:** `crates/runtimed/src/daemon.rs`, `handle_open_notebook()`

The doc write lock is held while `load_notebook_from_disk` does `tokio::fs::read_to_string`. For large notebooks, this blocks all other operations on that room's doc. The file I/O could be done outside the lock (read and parse first, then acquire write lock and populate if still empty).

### 16. `runtime` field in `CreateNotebook` not validated (Low)

**File:** `crates/runtimed/src/daemon.rs` / `notebook_sync_server.rs`

The `runtime` string from `CreateNotebook` is matched with `_ =>` defaulting to Python. A typo like `"pyhton"` silently creates a Python notebook. Consider validating the value is exactly `"python"` or `"deno"` and returning an error otherwise.

### 17. Broadcast lag recovery doesn't include kernel status (Low)

**File:** `crates/runtimed/src/notebook_sync_server.rs`

When a peer lags and misses broadcasts, the recovery sends an Automerge doc sync (which contains cell outputs) but does **not** re-send `KernelStatus` or `QueueChanged` broadcasts. The peer's UI could show stale kernel status. Consider also re-sending the current kernel status and queue state after a lag event.

---

## Positive Observations

### Well-handled concerns

1. **Atomic check-and-load** in `handle_open_notebook`: The `cell_count == 0` check under write lock prevents the race where two concurrent opens would both load cells into the same room.

2. **Generation counter for stale cleanup**: `sync_generation` correctly prevents a slow-dying broadcast task from clearing a newer connection's handle.

3. **Broadcast lag recovery**: The new `Err(broadcast::error::RecvError::Lagged(n))` handler in `run_sync_loop_v2` gracefully recovers by sending an Automerge sync message to catch the peer up. This is a solid approach.

4. **Kernel death detection**: The dual-signal approach (process watcher + heartbeat monitor) covers both immediate crashes (`os._exit(1)`) and hung kernels. The `kernel_died()` method is properly idempotent.

5. **Socket permission hardening**: Setting `0o600` on the daemon socket and trust key is a good security improvement.

6. **`DaemonReadyPayload`**: Emitting `daemon:ready` with `notebook_id`, `cell_count`, and `needs_trust_approval` gives the frontend everything it needs without additional round-trips. Clean protocol design.

7. **Session save simplification**: Reading `runtime` directly from `WindowNotebookContext` instead of deserializing from `NotebookState` is much simpler and avoids lock contention.

8. **`send_frame` size validation**: Adding the `MAX_FRAME_SIZE` check to `send_frame` prevents silent truncation of the u32 length field. Good defensive coding.

9. **WASM `get_heads()` optimization**: Replacing `doc.save().len()` with `doc.get_heads()` for change detection is a significant performance win — O(heads) vs O(document size).

10. **Clean error responses**: Both `handle_open_notebook` and `handle_create_notebook` return structured `NotebookConnectionInfo` with error messages, then drain the reader to avoid broken pipe errors. Thorough.

### Test coverage

Good new test coverage for:
- `NotebookConnectionInfo` serialization
- `send_frame` size limit
- `build_new_notebook_metadata` (Python/UV, Python/Conda, Deno)
- `create_empty_notebook` (Python, Deno, custom env_id)
- Handshake serialization for new variants

---

## Architecture Notes

The three-mode connection approach is clean:
- **`OpenNotebook`** — saved notebooks (daemon loads from disk, derives path-based ID)
- **`CreateNotebook`** — new notebooks (daemon creates room, generates UUID ID)
- **`NotebookSync`** (legacy) — session restore of untitled notebooks (reconnect by env_id)

The legacy `NotebookSync` path will eventually be replaceable once the daemon persists Automerge docs across restarts reliably enough to handle all session restore cases via `OpenNotebook`. The `Restore` variant in `OpenMode` correctly bridges this gap.

The `skip_capabilities` boolean parameter on `handle_notebook_sync_connection` is a pragmatic approach to reuse the existing sync loop while avoiding sending duplicate protocol responses. This is preferable to duplicating the sync loop.

---

## Suggestions (Non-blocking)

1. **Thread `runtime` through `OpenMode::Restore`** from the session data rather than re-reading settings.

2. **Consider a `room.load_state` enum** (`NotLoaded | Loading | Loaded | Failed`) instead of using `cell_count == 0` as a proxy for "not loaded yet". This would make the intent clearer and handle edge cases like notebooks that genuinely have 0 cells.

3. **Extract common connection logic** in `notebook_sync_client.rs` — the `init_open_notebook`, `init_create_notebook`, and existing `init` all share the pattern of sending a handshake, receiving a response, and calling `do_initial_sync`. A helper that takes a handshake enum and response parser could reduce the duplication.

4. **The `build_new_notebook_metadata` function** in `notebook_sync_server.rs` duplicates logic that previously lived in `NotebookState::new_empty_with_runtime`. Consider whether both are still needed or if one can be removed.

5. **Unify receiver spawning** — `initialize_notebook_sync` (Restore path) inlines the same receiver/broadcast/raw-sync spawning that `setup_sync_receivers` extracts for Open/Create. This is the root cause of the inconsistent `daemon:ready` behavior and should be unified.

6. **Reuse heartbeat ZMQ connection** — create it once and reuse across checks, with reconnection on error. Add a consecutive-failure threshold (e.g., 3) before declaring the kernel dead.

7. **Replace 6-element tuple return type** in `connect_open_split` / `connect_create_split` with a named struct for readability.

8. **Add a timeout** to the error-path `tokio::io::copy` drain, or simply drop the stream.

9. **Document the invariant** that `notebook_id == env_id` for untitled notebooks — session restore silently fails if this breaks.
