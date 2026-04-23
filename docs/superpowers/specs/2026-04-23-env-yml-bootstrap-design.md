# Bootstrap environment.yml deps into CRDT at auto-launch

Issue: [#2077](https://github.com/nteract/desktop/issues/2077)

## Problem

When a notebook auto-launches with `conda:env_yml` (detected from an `environment.yml` project file), the daemon does not bootstrap the file's deps into CRDT metadata. This is inconsistent with `pixi.toml` and `pyproject.toml`, which both bootstrap at auto-launch.

Consequences:

- `runt.conda.dependencies` starts empty for env_yml notebooks.
- MCP `get_dependencies` returns nothing until the first `add_dependency` + restart cycle, which happens to re-bootstrap via `promote_inline_deps_to_project`.
- UI/MCP asymmetry: pixi/pyproject notebooks show deps immediately; env_yml notebooks don't.

## Location

`crates/runtimed/src/notebook_sync_server/metadata.rs`, Step 3b bootstrap block (~line 2154). Currently:

```rust
match detected.kind {
    ProjectFileKind::PixiToml => { /* bootstrap pixi deps */ }
    ProjectFileKind::PyprojectToml => { /* bootstrap pyproject deps */ }
    _ => {}
}
```

## Fix

Add an `EnvironmentYml` arm mirroring the neighbors:

1. Call `crate::project_file::parse_environment_yml(&detected.path)`.
2. Extract `env_config.dependencies`.
3. Compare against `snap.runt.conda.as_ref().map(|c| &c.dependencies)`.
4. If different, fork-and-merge the doc, get-or-insert a `CondaInlineMetadata`, assign `dependencies`, call `set_metadata_snapshot`.
5. Log `[notebook-sync] Bootstrapped environment.yml deps into CRDT` on write, `debug!` on skip.

### Scope: dependencies only

This fix writes `dependencies` only. `channels` and `python` are intentionally deferred — they're not causing the observed bug and can be added later if needed. The existing re-bootstrap path in `promote_inline_deps_to_project` (line 3234) also only writes `dependencies`, so this matches that behavior.

## Why this is correct

- Fork-and-merge is used per the CRDT mutation rules — this is a sync mutation in the bootstrap path, identical pattern to pixi/pyproject.
- Equality check guards against redundant writes (no spurious dirty-flag / sync traffic).
- `get_or_insert_with(CondaInlineMetadata { ..default.. })` matches the existing promotion re-bootstrap block (line 3240), so the two paths produce equivalent CRDT state.

## Testing

No dedicated tests exist for the pixi/pyproject bootstrap arms, so no new unit tests are required for consistency. Verify end-to-end through the dev daemon:

1. Create a notebook next to an `environment.yml` with a couple of deps (e.g. `numpy`, `pandas`).
2. Auto-launch the kernel.
3. Call MCP `get_dependencies` — expect the deps to appear immediately, without restarting.

## Non-goals

- `channels` and `python` bootstrap (deferred).
- Fixing #2076 (duplicate dep promotion) — separate issue.
- Fixing #2027 (CreateNotebook auto-launch race) — separate issue.
