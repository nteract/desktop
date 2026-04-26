# Kill notebook-mutation Tauri proxies (phase 1)

Delete three Tauri commands from `crates/notebook` that duplicate work the daemon already owns. Part of a larger sweep to thin out `crates/notebook`'s role to "OS-chrome and window lifecycle", not "request router and re-verifier".

## What goes away

Three `#[tauri::command]` functions in `crates/notebook/src/lib.rs`:

| Command | What it does now | Replacement |
|---------|-----------------|-------------|
| `save_notebook` | Wraps `NotebookRequest::SaveNotebook { format_cells: true, path: None }` | Frontend sends the request directly via `host.transport.sendRequest` |
| `has_notebook_path` | Reads `context.path` from `WindowNotebookRegistry` | Frontend reads `runtimeState.path` from `useRuntimeState()` |
| `verify_notebook_trust` | Fetches metadata from daemon, re-runs `runt_trust::verify_notebook_trust`, returns `TrustInfo` | Frontend composes an equivalent shape from `runtimeState.trust.status` + `useDependencies()` + `useCondaDependencies()` |

All three are pure pass-throughs or CRDT re-reads. The daemon already does the work and publishes authoritative state.

## What stays

- `save_notebook_as` — has Tauri-only side effects (`recent::record_open`, `refresh_native_menu`, kernel relaunch on path change). Moves in phase 2.
- `clone_notebook_to_ephemeral` — opens a new window, inherently Tauri-side. Can be slimmed in phase 2 by letting the frontend drive the daemon request and a separate window-open command.
- `approve_notebook_trust` — signs with the HMAC key. Moves in phase 2 together with the `runt-trust` crate-dep drop.
- `apply_path_changed` — real Tauri state (session restore, window title fallback cache). Stays.

After this phase, `crates/notebook` still links `runt-trust` because `approve_notebook_trust` needs it. The dep drop is phase 2.

## Architecture

### Trust verification (the only non-trivial change)

Today, `useTrust()` has two data sources:

1. `host.trust.verify()` → `TrustInfo { status, uv_dependencies, conda_dependencies, conda_channels }`
2. `runtimeState.trust.needs_approval` (already daemon-sourced)

After:

1. `runtimeState.trust.status` → `"trusted" | "untrusted" | "signature_invalid" | "no_dependencies"` (already written by daemon in `room.rs` and `metadata.rs`)
2. `useDependencies().dependencies.dependencies` → UV package list
3. `useCondaDependencies().dependencies.{dependencies,channels}` → Conda list + channels
4. `runtimeState.trust.needs_approval` → unchanged

`useTrust()` composes these into the existing `TrustInfo` shape locally. The `TrustInfo` type becomes a frontend-only aggregate (still exported from `@nteract/notebook-host` for the `TrustDialog` prop). No wire type changes.

Callers that read `trustInfo.status`, `trustInfo.uv_dependencies`, `trustInfo.conda_dependencies`, `trustInfo.conda_channels` continue to work unchanged. The `TrustDialog` component is unaffected.

### Save

`saveNotebook(host, flushSync, hasPath)` takes a `hasPath` boolean as a third parameter. The one caller (`useAutomergeNotebook.ts`) computes it from `runtimePath != null` and passes it in. Removes a Tauri round-trip on every save.

The `save_notebook` request handling in the frontend uses a new helper `notebookClient.saveNotebook({ formatCells: true })`. It wraps `transport.sendRequest` with the `NotebookResponse::NotebookSaved | SaveError | Error` match that lives on the Rust side today, and re-uses the same user-facing error mapping (`SaveErrorKind::PathAlreadyOpen` → "Cannot save: {path} is already open…").

Put this helper on the existing `NotebookClient` class in `packages/runtimed/src/notebook-client.ts`, next to `launchKernel` / `executeCell` etc. Frontend callers that don't want to instantiate a `NotebookClient` can call `transport.sendRequest(...)` directly — the helper is convenience, not a required path.

### Has-path

Direct CRDT read. No new code surface. Just wire `runtimeState.path != null` into the `saveNotebook` call.

## Changed files (estimate)

**Rust (`crates/notebook/src/lib.rs`)**
- Delete fns: `save_notebook` (~50 lines), `has_notebook_path` (~8 lines), `verify_notebook_trust` (~14 lines).
- Remove the three entries from the `tauri::generate_handler![]` invocation in `run()`.
- The `format_save_error` function becomes unused — delete it.
- `SaveErrorKind` import becomes unused — remove.

**TypeScript**
- `apps/notebook/src/lib/notebook-file-ops.ts` — `saveNotebook` signature change (adds `hasPath: boolean`), remove the `has_notebook_path` and `save_notebook` `invoke` calls, route save through `notebookClient.saveNotebook`.
- `apps/notebook/src/hooks/useAutomergeNotebook.ts` — pass `runtimePath != null` through the `save` callback.
- `apps/notebook/src/hooks/useTrust.ts` — replace `host.trust.verify()` with composition of `runtimeState.trust.status` + `useDependencies()` + `useCondaDependencies()`.
- `packages/notebook-host/src/types.ts` — drop `verify` from `HostTrust`.
- `packages/notebook-host/src/tauri/index.ts` — drop `trust.verify` implementation.
- `packages/runtimed/src/notebook-client.ts` — add `saveNotebook({ formatCells })` helper plus a `SaveNotebookError` typed error that carries `SaveErrorKind`.
- `apps/notebook/src/lib/__tests__/notebook-file-ops.test.ts` — update for new `saveNotebook` signature (no `has_notebook_path` mock; pass `hasPath` directly).
- `packages/notebook-host/tests/tauri-host.test.ts` — drop the `trust.verify` test if one exists.

## Out of scope

- `runt-trust` crate-dep removal (still used by `approve_notebook_trust`).
- `save_notebook_as` removal (side effects to migrate).
- Ephemeral clone window-open refactor.
- Any changes to how the daemon writes trust state — it already does this correctly.
- MIME / WASM changes. Any `useSyncedSettings`/`rotate_install_id`-style daemon proxies (those are a separate lane).

## Verification

1. `cargo xtask lint` — formatting + clippy clean.
2. `cargo build -p notebook` — compiles without `runt_trust::verify_notebook_trust` import.
3. Tauri app: Cmd+S on an untitled notebook → opens save dialog, writes file, title updates. Cmd+S on a saved notebook → silent save, no dialog, no round-trip to `has_notebook_path`.
4. Trust dialog: open a notebook with inline UV deps, no signature → dialog shows uv/conda dep lists, typosquat warnings render, approve button still works via `host.trust.approve()`.
5. Dep edit from another peer (daemon writes `RuntimeStateDoc.trust.status = "signature_invalid"`): dialog re-renders with the correct status without a Tauri round-trip.
6. `notebook-file-ops.test.ts` passes with the new signature.

## Rollout

One PR. Small enough to review in a sitting. No feature flag needed — the daemon already publishes the state we're switching to read from.
