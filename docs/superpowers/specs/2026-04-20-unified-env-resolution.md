# Unified environment resolution

**Status:** Design
**Date:** 2026-04-20
**Related:** #1954 (issue), #1915 (pool drain on settings change), #1945 (pool-drain fix), #1939 (dx bootstrap)

## Motivation

Untitled notebooks that were launched with a prewarmed pool env lose their env-identity when reopened. The daemon takes a fresh env from the pool instead of finding the previously-claimed env on disk.

Concrete symptom (observed 2026-04-20 by @rgbkrk): after updating the nightly, three notebooks that had been running pre-update went idle, restarted, and one took ~76 seconds to spin up a kernel. Doctor report was clean — daemon healthy, version match, socket ok. The delay came from cold-start conda warmup draining the pool faster than it could refill.

The root cause is simpler than "cold pool after update." The daemon never even tries to reuse the previously-claimed env. For a notebook with inline `uv.dependencies` / `conda.dependencies`, the flow is content-addressed: `kernel_env::{uv,conda}::prepare_environment(deps, env_id)` computes a hash, looks for the cached env on disk, cache-hits in the common case. For a notebook that only has `env_id` (no inline deps), `auto_launch_kernel` skips that whole machinery and just does `daemon.take_{uv,conda}_env()` — a blind pop from the in-memory pool. The env_id and the previously-claimed on-disk env are ignored.

The invariant people reasonably expect — "same notebook, same env across reopens" — holds for inline-deps notebooks and does not hold for prewarmed ones.

## Non-goals

- Persistent sharing of environments across notebooks via an explicit shared ID. Out of scope; a future feature if demanded.
- Hot-sync semantics beyond the minimum needed to keep the cache coherent. See § Hot sync.
- Pixi. Pixi envs live under `.pixi/envs/default` in a pool-local project dir; `pixi exec` manages its own cache. The design below covers UV and Conda only. Pixi stays as-is.
- Windows-specific pathing issues. The design is platform-agnostic; implementation details (`bin/python` vs `Scripts/python.exe`) follow existing conventions.

## Current state

Multiple hashing rules that don't agree:

| Site | Hash input |
|------|-----------|
| `kernel_env::uv::claim_prewarmed_environment_in` | `(deps=[], env_id)` |
| `kernel_env::conda::claim_prewarmed_environment_in` | `(deps=["ipykernel"], channels=["conda-forge"], env_id)` |
| `kernel_env::{uv,conda}::prepare_environment_in` | `(actual_user_deps, env_id)` |
| `kernel_env::uv::compute_env_hash` | includes `env_id` only when `deps` is empty |
| `kernel_env::conda::compute_env_hash` | always includes `env_id` |
| `auto_launch_kernel` prewarmed reopen | no hash lookup, just `Pool::take()` |

Consequences:

- The env that ends up on disk at `{cache}/{hash}/` for a prewarmed claim is keyed by rule A. Any later lookup for that notebook's env would compute rule B and miss. The mismatch is avoided today by skipping the lookup entirely for prewarmed notebooks, trading correctness for a slow reopen.
- UV and Conda disagree on whether `env_id` participates in the hash for non-empty deps. UV dedupes across notebooks with identical deps; Conda isolates per-notebook. Neither behavior is documented as a user-facing contract.
- The claim path hashes a fictional dep list (`[]` for UV, `["ipykernel"]` for Conda) rather than the actual installed set. Subsequent `prepare_environment` calls with the real dep set cannot find the claimed env.

## Goals

1. **Same notebook, same env across reopens.** Instant cache hit on reopen for any notebook that has an `env_id`.
2. **Self-describing notebooks.** A notebook's metadata fully describes its env. Sharing a notebook means sharing its env description; the recipient's daemon either cache-hits or installs deterministically from the captured dep set.
3. **One hashing rule.** `hash(sorted_deps, env_id)` everywhere. No special cases.
4. **Pool is a bootstrap optimization, not a runtime dependency.** After first launch, the notebook is indistinguishable from an inline-deps notebook. Pool take only happens when the notebook is brand new (no env_id yet).

## Design

### The unified hashing rule

One function per runtime, one input shape:

```rust
// kernel_env::uv
pub fn compute_env_hash(deps: &UvDependencies, env_id: &str) -> String {
    let mut hasher = Sha256::new();
    let mut sorted = deps.dependencies.clone();
    sorted.sort();
    for d in &sorted { hasher.update(d.as_bytes()); hasher.update(b"\n"); }
    if let Some(rp) = &deps.requires_python { hasher.update(b"rp:"); hasher.update(rp.as_bytes()); }
    if let Some(pr) = &deps.prerelease { hasher.update(b"pr:"); hasher.update(pr.as_bytes()); }
    hasher.update(b"env_id:");
    hasher.update(env_id.as_bytes());
    hex::encode(&hasher.finalize()[..8])
}
```

Conda is structurally the same, plus sorted channels.

`env_id` is always included. Every notebook's env is isolated at the disk level. See § Open questions (§ cross-notebook sharing) for why.

### Metadata as source of truth

A notebook's env is `(metadata.runt.{uv,conda}.dependencies, metadata.runt.env_id)`. The daemon's job is to find or build the env matching that description. Captured deps are the signal that a notebook has gone through first-launch capture; `env_id` alone is not, because `create_empty_notebook` already assigns an `env_id` to every fresh notebook.

- **Brand-new notebook.** Has an `env_id` (assigned by `create_empty_notebook`) but no captured deps. Daemon does a pool take for fast first launch, then captures (see next section) so subsequent launches go through the cache-hit path.
- **Reopened notebook (post-capture).** Has `env_id` and captured deps. `auto_launch_kernel` reads them and calls `prepare_environment(deps, env_id)`. Cache-hit → instant.
- **User-edited deps.** User adds `scikit-learn` via the deps UI. Metadata now has deps including sklearn. Hash changes. Next launch misses cache, installs sklearn on top of the existing env (see § Hot sync), or builds fresh if no running kernel. Either way, converges to the new hash.
- **Shared notebook.** Recipient opens a `.ipynb` whose `metadata.runt.uv.dependencies` = `["numpy", "pandas"]` and `env_id = "abc"`. On their machine, first launch misses cache (env_id is different from any local env), builds from scratch deterministically, writes to disk at `hash(["numpy","pandas"], "abc")`. Subsequent launches hit.
- **Pre-upgrade notebook.** Has `env_id` (assigned pre-upgrade) but no captured deps. Treated identically to a brand-new notebook — pool take + capture on next launch. See § 4 Migration.

**Signal summary:** "has captured deps" routes to the reopen path; "no captured deps" routes to first-launch / capture. `env_id` presence is not the discriminator.

### Prewarmed capture

At prewarmed claim, we capture the pool env's user-level dep set into the notebook metadata. Capture happens exactly once per notebook (first claim only), guarded on `metadata.runt.{uv,conda}.dependencies` being absent.

Steps:

1. Take env from pool. `pooled_env.prewarmed_packages` is the full installed set: `[ipykernel, ipywidgets, anywidget, nbformat, uv, <user_defaults...>]` for UV, similar for Conda (minus `uv`).
2. Strip the known base set to derive `user_defaults`. See § Base package constants.
3. If `metadata.runt.{uv,conda}.dependencies` is unset, write `user_defaults` to it. Set `metadata.runt.env_id` if unset.
4. Compute `hash(user_defaults, env_id)` using the unified rule.
5. Rename/claim the prewarmed env to `{cache}/{hash}/`.
6. Vendor the launcher (current behavior, unchanged).

Once captured, metadata + env_id + on-disk location are consistent. Subsequent launches find the env via `prepare_environment(deps_from_metadata, env_id)`.

### auto_launch_kernel flow

Today `auto_launch_kernel` resolves `env_source` via a priority chain: kernelspec → inline `runt.{uv,conda}` deps → project files (`uv:pyproject`, `conda:env_yml`, `pixi:toml`) → runtime default. That priority chain stays intact — this design only changes what happens inside the `uv:prewarmed` / `conda:prewarmed` branches.

High level:

1. **Resolve `env_source`** via the existing priority chain. Unchanged.
2. **Branch on `env_source`:**
   - `uv:pyproject`, `conda:env_yml`, `pixi:toml`: **unchanged.** Project-file envs always reflect the current state of the project file on disk; they do not participate in the captured-deps scheme. Reopening a notebook whose `pyproject.toml` was edited still picks up the edit, because these branches don't read from captured metadata.
   - `uv:inline`, `conda:inline`, `uv:pep723`: **unchanged.** Inline deps already route through `prepare_{uv,conda}_inline_env(deps, env_id)`, which is content-addressed. They cache-hit on reopen today; the design doesn't move them.
   - `uv:prewarmed`, `conda:prewarmed`: **new logic.** Branch on captured deps:
     - **Captured deps present** → `prepare_{uv,conda}_inline_env(deps, env_id)`. Cache-hit, no pool take.
     - **No captured deps** (brand-new or pre-upgrade) → pool take + claim + capture (see § Prewarmed capture). Metadata gets populated, on-disk env lands at the unified hash.
   - `pixi:prewarmed`, `pixi:inline`, `pixi:pep723`, deno, etc.: **unchanged.** Pixi manages its own env cache.

In pseudo-code:

```
env_source = resolve_env_source(metadata, project_files, default_python_env)

match env_source:
    "uv:prewarmed" | "conda:prewarmed":
        if metadata.runt.{uv,conda}.dependencies is Some:
            # Reopen path (post-capture).
            deps = metadata.runt.{uv,conda}.dependencies
            env_id = metadata.runt.env_id
            prepare_{uv,conda}_inline_env(deps, env_id)  # expected cache hit
        else:
            # First-launch path (brand-new or pre-upgrade).
            pooled_env = daemon.take_{uv,conda}_env()
            claim_and_capture(pooled_env, notebook)  # writes metadata, renames to unified hash

    "uv:pyproject" | "conda:env_yml" | "pixi:toml":
        # Unchanged. Project file is source of truth; metadata captured deps are ignored.
        existing_project_file_flow()

    "uv:inline" | "conda:inline" | "uv:pep723":
        # Unchanged. Inline deps already go through content-addressed prepare.
        existing_inline_deps_flow()

    _:  # pixi:*, deno, etc.
        existing_flow()
```

The design's scope is **strictly the two prewarmed branches**. Project-file precedence, inline-deps routing, and pixi/deno paths are untouched.

## Open questions, answered

### 1. Cross-notebook sharing default

**Question:** Should two notebooks with identical deps share an env on disk, or get isolated per-notebook envs?

**Decision:** Always include `env_id` in the hash. Isolate per notebook.

**Reasoning:** Hot-sync on notebook A mutates the env in place. If notebook B is sharing the same env dir, B silently inherits A's changes. That's a correctness trap — two notebooks in the same directory could interfere invisibly. Isolation removes the footgun.

**Disk cost:** Low. UV's hardlink mode shares package bytes via the global `uv` cache; the per-env dir is mostly metadata + tiny shims. Conda similarly shares via `conda-forge` package cache. Per-notebook envs on top of a shared package cache is cheap.

**Future:** If explicit shared-env semantics is ever wanted, expose it as an opt-in: a notebook could declare `runt.shared_env = "team-baseline"` that hashes `(deps, shared_env)` instead of `(deps, env_id)`. Out of scope here.

### 2. Capture timing — first-only vs re-capture every launch

**Decision:** First claim only. Write-once.

**Reasoning:**

- User-edited deps are authoritative. Re-capture would overwrite them silently on the next launch.
- If the daemon's `default_packages` change after the notebook was captured, existing notebooks should not silently drift — their captured list is their history. New notebooks would reflect the new defaults.
- The guard is cheap: `if metadata.runt.{uv,conda}.dependencies.is_none() { capture } else { skip }`.

**Follow-up:** Provide a user-facing "refresh deps from defaults" action in the deps UI that explicitly overwrites. Covers the rare case where someone wants to resync.

### 3. What counts as "base"

**Decision:** A single canonical `BASE_PACKAGES` constant per runtime in `kernel_env`. The daemon's pool warmer and the capture path both strip the same set.

```rust
// crates/kernel-env/src/uv.rs
pub const UV_BASE_PACKAGES: &[&str] =
    &["ipykernel", "ipywidgets", "anywidget", "nbformat", "uv"];

// crates/kernel-env/src/conda.rs
pub const CONDA_BASE_PACKAGES: &[&str] =
    &["ipykernel", "ipywidgets", "anywidget", "nbformat"];
```

Stripping is set-subtraction: `user_defaults = prewarmed_packages.into_iter().filter(|p| !base.contains(&p.as_str())).collect()`.

**Versioning:** No explicit version field. If a future change adds `jupyter-client` to the UV base, existing notebooks with captured deps stay referenced exactly as they were — their envs live at their existing hash, and the base change only affects newly-created envs. Old notebooks that have `ipykernel` in their user_defaults (because the old strip set didn't include it) keep working; the env still has ipykernel installed.

**Compatibility with `bootstrap_dx`:** When `bootstrap_dx` is on, the daemon's `uv_prewarmed_packages` adds `dx` to the install set. `dx` is user-level (not in `UV_BASE_PACKAGES`) so it lands in `user_defaults` at capture and is part of the hash. Flipping `bootstrap_dx` on an existing notebook does not retroactively add `dx` to its captured deps; that's correct — the user can add `dx` manually via the UI if they want it on a pre-captured notebook.

### 4. Migration

**Decision:** Accept one-time cache invalidation. No migration code.

**Reasoning:** Existing claimed envs on disk are at rule-A hash paths. Under the new rule they'd be at rule-B paths. Rewriting them is fragile (rename races, incomplete fallback, reboot mid-migration). Instead: on first launch post-upgrade, any notebook that has `env_id` but no captured deps goes through the first-launch path: pool take, capture, rehash. The old env dir sits on disk until the GC grace period (max_age_secs, default 2 days) reaps it. This treats pre-upgrade notebooks identically to brand-new notebooks — both match the "no captured deps" signal.

**Cost:** Every pre-upgrade notebook does one slow first launch post-upgrade. Matches the current slow-reopen cost people already live with; no regression.

**Log entry:** At auto-launch time, when we detect "env_id present but no captured deps," log once per notebook at info level: `"[runtimed] Migrating notebook {id} to captured-deps layout"`. Gives users and support a marker.

### 5. Pixi

**Decision:** Out of scope. Pixi's env lifecycle is owned by `pixi exec` and `pixi install`, which use their own content-addressed caching under `.pixi/`. The daemon pool warms a full pixi project dir and hands it off; there's no hashed-cache lookup to unify.

Pixi notebooks today work reasonably: env gets warmed, kernel gets launched, reopen re-runs `pixi run`/`pixi exec` which hits `.pixi/envs/default` directly. The slow-reopen problem this design addresses does not apply to pixi in the same way.

### 6. Hot sync

**Question:** `sync_environment` adds user packages to a running env. How does the unified cache stay coherent?

**Decision:** In-session, env stays at its old hashed path. `sync_environment` installs new packages into the existing env in place (today's behavior). Metadata is updated with the new dep list. On kernel shutdown, the env dir is renamed atomically to the new hash path. Next launch cache-hits at the new hash.

**Flow:**

1. User triggers `sync_environment` with new packages.
2. Daemon runs `uv pip install` / `conda install` against the running env's python. Installation succeeds, kernel keeps running.
3. Daemon updates `metadata.runt.{uv,conda}.dependencies` with the new list.
4. Env dir on disk is still at `{cache}/{old_hash}/`. A sidecar file `.pending_rehash` (optional) notes the new hash.
5. User shuts down kernel (or closes notebook).
6. On shutdown completion, daemon renames `{cache}/{old_hash}/` → `{cache}/{new_hash}/`.
7. Reopen: `prepare_environment(new_deps, env_id)` → cache hit at `{new_hash}`.

**Crash recovery:** If the daemon (or kernel) crashes between hot-sync install and shutdown-rename, the env is at `{old_hash}` but metadata claims `{new_hash}`. Next launch misses cache, rebuilds fresh. Old env becomes orphaned → GC. Correctness preserved at the cost of a one-time rebuild. Acceptable: hot-sync is a convenience feature and its value is the running-kernel install, not the persistence.

**Alternative considered:** Rename on install completion (not shutdown). Rejected because the running kernel has `sys.executable` pointing at the old path, and renaming the parent dir on some filesystems (notably older macOS) invalidates that handle. Keep the rename scoped to "no process has handles into the dir."

**Alternative considered:** Don't rename; instead record the "effective hash" in a sidecar and have `prepare_environment` try both. Rejected because it keeps two hashing rules in play (the directory name and the sidecar), reintroducing exactly the confusion this design aims to remove.

### 7. Env_id provenance

**Decision:** env_id is a random UUID, assigned once, persistent forever for that notebook. Existing behavior. No change.

For saved notebooks the UUID is in the `.ipynb` metadata. For untitled notebooks it's in the persisted Automerge doc (`notebook-docs/{hash}.automerge`). Both survive reopens; both travel with the notebook.

**Clone semantics:** `notebook_sync_server::clone_notebook` (today) assigns a fresh env_id. That remains correct under this design — a cloned notebook is a new notebook and gets a new env.

### 8. Bootstrap_dx interaction

**Decision:** `bootstrap_dx` affects the user-level install set (`dx` added to UV envs when on). That flows naturally into the captured deps: at capture time, `user_defaults` includes `dx` if it was installed. The hash differs between flag-on and flag-off notebooks, which is correct — they really do have different envs.

**Edge case:** If a user flips `bootstrap_dx` on and opens an already-captured notebook that was captured with the flag off, the captured deps don't include `dx`. The env is at its old hash; it does not have `dx` installed; the kernel will try to `import dx` and fail. Two options:

- Leave the notebook's deps alone. The user turned the flag on globally, but they haven't told this particular notebook to use dx. They can add it via the deps UI if they want it.
- Auto-add `dx` to any already-captured notebook when the flag is on. Surprising; breaks the "metadata is authoritative" invariant.

Going with the first option. Bootstrap_dx only affects new captures.

## Implementation plan

Split into three PRs. Each self-contained, each leaves the system in a working state.

### PR 1: Unify `compute_env_hash`

**Scope:**

- `kernel_env::uv::compute_env_hash` always includes `env_id` (drops the "empty deps only" special case).
- `kernel_env::conda::compute_env_hash` unchanged in behavior (already always includes env_id), but factor out any duplicate logic for consistency with UV.
- Expose `UV_BASE_PACKAGES` / `CONDA_BASE_PACKAGES` constants.
- Add a `kernel_env::strip_base(prewarmed_packages, base)` helper for reuse.
- Tests: hash stability, hash uniqueness across env_ids, hash symmetry under dep reordering, strip_base correctness.

**Compat risk:** None at runtime. This PR changes only the hash computation. No caller yet uses the new shape for lookup; the existing lookups keep using the pre-change rule and find their envs. The shape shifts under PR 2.

### PR 2: Capture at claim, route reopen through cache

**Scope:**

- `claim_prewarmed_environment_in` (both UV and Conda) gain a `user_defaults: &[String]` parameter. Hash becomes `compute_env_hash(user_defaults + requires_python etc., env_id)`. Daemon's claim call sites pass the stripped list.
- Daemon's post-claim logic (in `notebook_sync_server.rs`) writes captured deps + env_id to the notebook's `metadata.runt.{uv,conda}` if not already present. Uses `doc.fork_and_merge` to respect the async-CRDT-mutation rule.
- Inside `auto_launch_kernel`'s `uv:prewarmed` and `conda:prewarmed` branches (not the project-file or inline-deps branches, which stay as-is): check for captured deps.
  - Captured deps present → route through `prepare_{uv,conda}_inline_env(deps, env_id)` with no pool take.
  - No captured deps → existing pool-take flow plus the capture step from PR 1's new API. Covers brand-new and pre-upgrade notebooks.
- Migration log: on first post-upgrade launch of a notebook that lacks captured deps, emit `[runtimed] Capturing prewarmed env into notebook metadata: {id}` at info level.
- Tests: capture idempotence (second claim doesn't overwrite), pool take behavior unchanged for new notebooks, reopen cache-hits at the same hash after first capture.

**Compat risk:** Notebooks captured under the old rule-A claim hash become orphaned. They sit on disk until GC. One-time slow launch per pre-upgrade notebook. Acceptable and documented.

### PR 3: Hot-sync coherence

**Scope:**

- `sync_environment` updates `metadata.runt.{uv,conda}.dependencies` after a successful install.
- On kernel shutdown, if the env's on-disk path does not match `hash(current_deps, env_id)`, atomically rename (best-effort — on rename failure, log and fall through; rebuild on next launch is the safe fallback).
- Tests: hot-sync followed by reopen finds the env at the new hash; crash-simulated test where rename never runs results in rebuild on next launch.

**Compat risk:** Low. Hot-sync's in-session behavior is unchanged. The shutdown-rename is new but gated behind a best-effort with fallback.

## Testing strategy

Per-PR unit tests as noted above. Additional integration coverage:

- **E2E: reopen round-trip.** Create new notebook → kernel launches, capture occurs → close → reopen. Assert kernel launches in < 2s (cache hit). Test fixture with known deps. One test per runtime (UV, Conda).
- **E2E: shared notebook.** Create notebook A → save to disk → open A in a fresh daemon session → assert metadata.runt deps and env_id survive → launch → cache miss first time (fresh daemon, empty cache), hit on subsequent launches.
- **E2E: hot-sync and reopen.** Create notebook → launch → hot-sync adds `httpx` → close → reopen → assert httpx is present and launch is fast.
- **Unit: migration path.** Synthesize a notebook with env_id but no captured deps → auto_launch detects migration → pool take + capture → on-disk layout matches rule-B.

## Drawbacks

- **More writes to the Automerge doc at first launch.** Each first-launch now mutates metadata. One additional fork+merge per notebook. Negligible cost.
- **Orphaned envs from migration.** Pre-upgrade notebooks leave a rule-A env dir on disk; it's only reaped after GC's 2-day grace. Mitigation: run GC once at startup in the first post-upgrade session. Cost: a few hundred MB of stale env dirs for a few days. Tolerable.
- **Auto-captured deps are visible to users.** Users who didn't explicitly set deps now see their notebook's deps populated. Could confuse users who thought prewarmed notebooks were "dep-free." UI should label the capture source ("Inherited from defaults") and allow editing. Mitigation: ship alongside a small UI note in the deps header explaining.

## Alternatives considered

- **Skip capture; use env_id alone as the cache key.** Keeps two hashing rules (env_id-only for prewarmed, deps+env_id for inline). Rejected because it doesn't address the reproducibility goal — shared notebooks still lack a dep description.
- **Rehash on reopen if mismatch detected.** Walk the cache, find the env by env_id via a sidecar, rename to match the current hash. Rejected because it doubles the moving parts and couples reopen to disk-rewrite correctness. The explicit migration-at-first-launch approach is simpler.
- **Decouple "disk cache" from "metadata description."** Make deps in metadata purely reproducibility metadata; keep lookup keyed on env_id. Rejected because it gives up content-addressed dedup (which UV users have come to rely on for inline-deps notebooks) without a compensating benefit.

## Rollout

1. Land PR 1 (hash unification). Ship in a nightly. No user-visible change.
2. Land PR 2 (capture + reopen routing). Ship in a nightly. Users see first-launch-after-upgrade slow path once per notebook, then fast reopens thereafter. Announce in release notes.
3. Land PR 3 (hot-sync coherence). Ship in a nightly. Users with hot-sync workflows see persistent hot-synced envs across reopens.
4. Monitor daemon logs for migration log entries; confirm most notebooks migrate within a week of upgrade.
5. Document in `contributing/environments.md`: metadata as source of truth, the unified hashing rule, capture semantics.

## Open work after this design

Not blockers but worth tracking:

- **User-facing "rebuild env" action.** Forces cache invalidation for a single notebook. Useful when a user wants to pick up a newer default package set or recover from a corrupted env.
- **"Refresh deps from defaults" action.** Overwrites captured deps with current defaults. Opt-in counterpart to the write-once capture.
- **Shared-env escape hatch.** If users ask for explicit cross-notebook env sharing, add `runt.shared_env` or similar. Keep env_id as the default.
- **Base-set versioning.** If the base package list churns (it has been stable for months), revisit whether we need a version field to invalidate old captures cleanly.
