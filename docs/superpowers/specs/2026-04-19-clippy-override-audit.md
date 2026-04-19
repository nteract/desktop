# Clippy override audit: non-test `unwrap_used` / `expect_used` allows

Date: 2026-04-19

## Scope

Workspace-level lints (`Cargo.toml`) configure:

```toml
[workspace.lints.clippy]
unwrap_used = "warn"
expect_used = "warn"
```

A companion PR (`chore(lints): allow unwrap/expect in test code workspace-wide`)
added `#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]`
at each crate root, so test modules no longer need the boilerplate
`#[allow(clippy::unwrap_used, clippy::expect_used)]` attribute.

This spec audits the **remaining non-test** sites that still carry
`#[allow(clippy::unwrap_used)]` / `#[allow(clippy::expect_used)]` (and
related variants). For each, the question is:

1. **Keep** — the unwrap/expect is legitimately safe and documented; the
   allow is earning its place.
2. **Fix** — the unwrap/expect is lazy and should become proper error
   handling (`?`, `Result`, `let … else`).
3. **Verify** — needs a closer look (often: is this really a panic-safe
   invariant, or is it a latent bug?).
4. **Gone** — the allow attribute is stale; the surrounding code no
   longer contains the unwrap/expect it was suppressing.

This is the deliverable — fixes are a follow-up.

## Summary

| Status  | Count |
|---------|------:|
| Keep    |    48 |
| Fix     |     1 |
| Verify  |     5 |
| Gone    |     1 |
| **Total** | **55** |

Totals below sum to 55 non-test override sites across 16 files.

## Sites

### `crates/notebook-doc/src/pool_state.rs`

| Line | Attribute | Target | Status | Notes |
|------|-----------|--------|--------|-------|
| 89   | `expect_used, new_without_default` | `PoolDoc::new()` | **Keep** | Scaffolds Automerge schema with hardcoded actor id. `put_object` on a fresh doc is infallible; an error here means a corrupted Automerge build, and panicking is the right response. The `expect("scaffold uv")` messages are informative and localized to construction. |

### `crates/notebook-doc/src/runtime_state.rs`

All 29 sites follow the same pattern: daemon-side CRDT setters call `get_map("kernel" | "queue" | "env" | "trust" | …)` which returns `Option<ObjId>`, and then `.expect("<foo> map must exist")`. The map is guaranteed to exist because `RuntimeStateDoc::new()` scaffolds all top-level maps before the doc is ever shared, and no code path removes those maps. This invariant is documented in the daemon architecture doc.

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 251  | `new()` (schema scaffold) | **Keep** | Same rationale as `pool_state::new()`. |
| 331  | `new_with_actor()` | **Keep** | Same. |
| 640  | `set_trust()` | **Keep** | Relies on scaffolded top-level map. |
| 662  | `set_kernel_status()` | **Keep** | Same. |
| 685  | `set_starting_phase()` | **Keep** | Same. |
| 699  | `set_kernel_info()` | **Keep** | Same. |
| 721  | `set_runtime_agent_id()` | **Keep** | Same. |
| 735  | `set_queue()` | **Keep** | Same. |
| 831  | `create_execution()` | **Keep** | Same. |
| 931  | `set_execution_running()` | **Keep** | Same. |
| 953  | `set_execution_count()` | **Keep** | Same. |
| 970  | `set_execution_done()` | **Keep** | Same. |
| 994  | `mark_inflight_executions_failed()` | **Keep** | Same. |
| 1130 | `ensure_output_list()` | **Keep** | Same. |
| 1155 | `append_output()` | **Keep** | Same. |
| 1170 | `set_outputs()` | **Keep** | Same. |
| 1205 | `clear_execution_outputs()` | **Keep** | Same. |
| 1346 | `trim_executions()` | **Keep** | Same. |
| 1395 | `set_env_sync()` | **Keep** | Same. |
| 1469 | `set_prewarmed_packages()` | **Keep** | Same. |
| 1499 | `set_last_saved()` | **Keep** | Same. |
| 1542 | `put_comm()` | **Keep** | Same. |
| 1588 | `set_comm_state_property()` | **Keep** | Same. |
| 1619 | `merge_comm_state_delta()` | **Keep** | Same. |
| 1664 | `set_comm_capture_msg_id()` | **Keep** | Same. |
| 1701 | `remove_comm()` | **Keep** | Same. |
| 1716 | `clear_comms()` | **Keep** | Same. |
| 1752 | `append_comm_output()` | **Keep** | Same. |
| 1774 | `clear_comm_outputs()` | **Keep** | Same. |

**Optional refactor (not a blocking fix).** The 29 `.expect("kernel map must exist")` call sites are repetitive. A private helper like:

```rust
fn map_or_panic(&self, key: &'static str) -> ObjId {
    self.get_map(key).unwrap_or_else(||
        panic!("{key} map must exist; RuntimeStateDoc was not properly scaffolded"))
}
```

…could centralize the invariant check and remove the per-method `#[allow(clippy::expect_used)]` attributes. Marked **Keep** above since the current form is correct; a cleanup pass is a separate chore, not a bug.

### `crates/notebook/build.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 98   | `write_git_metadata()` | **Keep** | `env::var("OUT_DIR").unwrap()` — OUT_DIR is always set by cargo for build scripts. Documented. |
| 111  | `write_if_changed()` | **Verify** | The `fs::write(path, content).unwrap()` on line 118 will panic on e.g. a full disk or RO filesystem. Build scripts legitimately abort on I/O errors, but using `?` or `.expect("write OUT_DIR file")` would produce nicer diagnostics. Low priority. |

### `crates/runt-mcp/src/formatting.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 12   | `ANSI_RE` regex constructor | **Keep** | Static `LazyLock<Regex>` with a hardcoded pattern. Build-time tested via the test suite. |

### `crates/runt-mcp/src/lib.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 109  | `get_info()` MCP extensions | **Keep** | `serde_json::from_value(json!({}))` where the input literal is a valid empty object. Infallible. |

### `crates/runt-mcp/src/tools/mod.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 63   | `schema_for<T>()` first unwrap | **Keep** | `serde_json::to_value(schemars::schema_for!(T))` — schemars always produces valid JSON. |
| 65   | `schema_for<T>()` second allow | **Gone** | The line is `Arc::new(value.as_object().cloned().unwrap_or_default())`. There is no `unwrap()` here — `unwrap_or_default()` is not flagged by `clippy::unwrap_used`. The allow is stale and can be removed. |

### `crates/runt/build.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 20   | `write_git_hash()` | **Keep** | Same rationale as `notebook/build.rs` — OUT_DIR is cargo-provided. |

### `crates/runt/src/kernel_client.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 54   | `start_from_kernelspec()` | **Keep** | `petname(2, "-")` — the upstream crate only returns `None` when `word_count == 0`. Documented inline. Consider: a tiny `fn generate_kernel_id() -> String` helper that uses `unwrap_or_else(\|\| uuid::Uuid::new_v4().to_string())` would remove the panic entirely. |
| 102  | `start_from_command()` | **Keep** | Same reasoning as above; identical call. |

### `crates/runt/src/main.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 1560 | `pool_command()` | **Verify** | Two-page function that's a CLI subcommand handler. `#[allow(clippy::unwrap_used, clippy::expect_used)]` at the function level is very broad. Most of the function uses proper `?` error handling; the allow may be for a small number of specific calls. Audit the body for actual `.unwrap()`/`.expect()` usage and narrow the allow or replace with context-specific `?`. |
| 1947 | `daemon_command()` | **Verify** | Same pattern — broad function-scope allow on a long handler. Narrow or remove. |

### `crates/runtimed-client/build.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 16   | `write_git_hash()` | **Keep** | OUT_DIR unwrap; same as other build scripts. |

### `crates/runtimed-client/src/settings_doc.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 736  | `ensure_map()` helper | **Keep** | Rust-doc `# Panics` section explains the invariant — `put_object` on a fresh Automerge doc only fails on fundamental corruption. Best-in-class documentation of a kept allow. |

### `crates/runtimed/build.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 35   | `write_git_hash()` | **Keep** | OUT_DIR; same as other build scripts. |

### `crates/runtimed/src/blob_server.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 145  | `text_response()` | **Keep** | `Response::builder().status(StatusCode::…).body(…)` — `Response::builder` only fails with invalid `StatusCode`. The function takes `status: StatusCode` (an enum), so all inputs are valid. Commented inline. Consider: wrap as a small `fn text_response_ok(...) -> Response<Full<Bytes>>` that uses `expect` internally but exposes only infallible external surface. Already effectively done. |

### `crates/runtimed/src/main.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 437  | SIGTERM signal registration | **Keep** | Signal-handler setup failure is a fundamental OS-level problem with no recovery path. Documented inline. |
| 440  | SIGINT signal registration | **Keep** | Same. |

### `crates/runtimed/src/output_store.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 395  | `ANSI_RE` regex | **Keep** | Static `LazyLock<Regex>` with a literal pattern (mirror of `runt-mcp::formatting::ANSI_RE`). |
| 465  | `OutputManifest::to_json()` | **Keep** | `serde_json::to_value(self)` on a type that only contains `String`, `i32`, `Option`, `HashMap<String, ContentRef>`, and other serialize-safe types. Infallible in practice. Consider: return `serde_json::Value` directly via a different path that doesn't need `expect`, e.g. by constructing the JSON value inline. Not urgent. |

### `crates/xtask/src/main.rs`

| Line | Target | Status | Notes |
|------|--------|--------|-------|
| 1402 | `generate_launch_agent_plist()` | **Keep** | xtask is a dev tool run at build time by humans; panics with context are acceptable. Documented inline. |
| 1491 | `cmd_install_nightly()` | **Keep** | Same rationale — dev tool, user sees the panic. |
| 2628 | `get_host_target()` | **Keep** | `rustc` must be available when running xtask; if it isn't, the build has bigger problems. |
| 3109 | `cmd_sync_tool_cache()` | **Keep** | Tool-cache sync is a dev operation. However: this function mixes `.expect()` with `unwrap_or_else(\|e\| panic!(...))`. The latter doesn't need `expect_used` allow. Consider a small refactor to unify on `.expect()` and remove the sprawl. |
| 3223 | `dump_mcp_tools()` | **Fix** | This function spawns `runt mcp`, writes a JSON-RPC request, reads stdout, and returns the tools JSON. The numerous `.expect("stdin")`, `.expect("stdout")`, `.expect("serialize tools")`, and a terminal `panic!("Failed to get tools/list response")` are a pattern that should return `Result<String, anyhow::Error>` and bubble up to the CLI error path. It's dev-only, but the panic messages are cryptic enough that a proper `Result` with `?` + context would pay for itself. |
| 3265 | `update_manifest_tools()` | **Verify** | `serde_json::from_str(manifest_json).expect("parse manifest")` — if the manifest file on disk is malformed, xtask panics. A CLI error with the file path and parse error would be much nicer. Likely a 10-line change to return `anyhow::Result<String>`. |

## Recommendations

### Priority 1 (follow-up PR material)

- **`runt-mcp/src/tools/mod.rs:65`** — Stale allow; remove the attribute. 1-line change.
- **`runt/src/main.rs:1560`, `:1947`** — Narrow the function-scope allow. Audit the `pool_command()` and `daemon_command()` bodies for actual `.unwrap()`/`.expect()`, replace each with `?` + `anyhow::Context`, and delete the function-level `#[allow(...)]`.
- **`xtask/src/main.rs:3223`, `:3265`** — Convert `dump_mcp_tools()` and `update_manifest_tools()` to return `anyhow::Result<_>` with `?` + `.context(...)`. Dev tool error messages should name the broken file or RPC message; today they die silently.

### Priority 2 (nice to have, not urgent)

- **`notebook/build.rs:111`** — Replace the bare `.unwrap()` in `write_if_changed()` with `.expect("write git metadata to OUT_DIR")` or return `io::Result<()>` from `main()`.
- **`runtimed/src/output_store.rs:465`** — Consider returning a prebuilt `serde_json::Value` directly from `OutputManifest` construction so `to_json()` becomes infallible by construction.
- **`runtimed-state.rs` (29 sites)** — Collapse the repetitive `get_map(…).expect(…)` pattern into a single `map_or_panic` helper. Removes 29 `#[allow(clippy::expect_used)]` attributes in one change. Pure cleanup.

### Priority 3 (keep as-is, document the invariant)

The build-script OUT_DIR unwraps, regex `LazyLock` expects, signal-handler expects, `runt-mcp` static JSON unwraps, and `kernel_client` petname expects are all legitimately safe. They carry short inline comments documenting why. No action needed.

## Appendix: audit methodology

1. `grep -rn '#\[allow(clippy::(unwrap_used|expect_used)' crates/ --include='*.rs'` to enumerate all sites (116 total).
2. Exclude sites directly preceded by `#[cfg(test)]` (61 sites — handled by the companion PR via crate-root `#![cfg_attr(test, allow(...))]`).
3. Exclude `crates/*/tests/`, `benches/`, `examples/` trees.
4. Remaining 55 non-test sites classified by reading surrounding code and comments.

Sites are reported by file + line of the allow attribute (line numbers
against `main` at 528f9f3 / pre-PR 1 baseline — they are unchanged in
this branch).
