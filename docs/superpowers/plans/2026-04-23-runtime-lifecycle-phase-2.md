# RuntimeLifecycle Phase 2 — CRDT Keys + Writers

**Goal:** Introduce `kernel/lifecycle`, `kernel/activity`, `kernel/error_reason` CRDT keys alongside the existing `kernel/status` + `kernel/starting_phase`. Add typed writers. Everything is dual-shape — readers of either form still see consistent state.

**Why only this much:** Phase 3 migrates callers. This phase exists so Phase 3's diff is small per crate, and so the CRDT + writer design can be reviewed independently of the migration churn.

**Spec:** `docs/superpowers/specs/2026-04-23-runtime-lifecycle-enum-design.md`
**Prior phase:** `docs/superpowers/plans/2026-04-23-runtime-lifecycle-phase-1.md`

## Scope

- `crates/runtime-doc/src/doc.rs`:
  - Scaffold `kernel/lifecycle` (default `"NotStarted"`), `kernel/activity` (default `""`), `kernel/error_reason` (default `""`) in both `new()` and `new_with_actor()`.
  - Add `KernelState.error_reason: Option<String>` alongside the existing legacy fields.
  - `read_state` prefers the new keys when `kernel/lifecycle` is non-empty; falls back to `RuntimeLifecycle::from_legacy(status, starting_phase)` when it's empty (doc scaffolded pre-Phase-2).
  - New writers: `set_lifecycle`, `set_activity`, `set_lifecycle_with_error`. Each also mirrors into the legacy `status` + `starting_phase` keys so callers that still read the old shape keep working.
  - Keep `set_kernel_status` and `set_starting_phase` fully functional; they are retired in a later phase.

## Out of scope

- Daemon / TS / Python caller migration — Phase 3.
- Removing `set_kernel_status` / `set_starting_phase` / legacy CRDT keys — Phase 4.

## Dual-shape invariants (worth testing explicitly)

- **Typed setters mirror.** `set_lifecycle`, `set_lifecycle_with_error`, and `set_activity` always update both the typed keys (`lifecycle`, `activity`, `error_reason`) AND the string keys (`status`, `starting_phase`). String-shape readers never see stale data written through the typed API.
- **String setters don't mirror.** `set_kernel_status` and `set_starting_phase` touch only the string keys. The typed keys stay at whatever value the last typed writer left them.
- **`read_state` reconciles.** When the string pair disagrees with the typed lifecycle's `to_legacy()` projection, a string setter ran more recently — `resolve_lifecycle` falls through to `RuntimeLifecycle::from_legacy(status, starting_phase)`. When they agree, the typed value wins (preserving `Running(Unknown)` through a lossy string projection, for example).
- **`set_activity` throttle.** A no-op only when BOTH typed `activity` AND string `status` already match the target. If a string setter drifted the string key, `set_activity` re-mirrors even when the typed value looks redundant.
- **`error_reason` ownership.** `set_lifecycle` alone must not clobber an existing `error_reason`; only `set_lifecycle_with_error(lc, None)` clears it. Retry paths that re-enter `Error` via the plain setter keep their diagnosis.
- **Leaving Running clears activity.** `set_lifecycle` always sets `activity = ""` on non-`Running` variants, so a later `Running(Idle)` can't be confused for a stale `Running(Busy)`.

## Acceptance

- `cargo test -p runtime-doc` passes including new tests.
- `cargo check --workspace` clean.
- Existing callers of `set_kernel_status` continue to work unchanged; `read_state` returns the same `status` + `starting_phase` strings as before plus a correct `lifecycle` derivation.

## Test plan — shake out the tough stuff

Happy paths are easy; we need adversarial coverage:

1. `set_lifecycle(Error)` does NOT clear `error_reason` set previously.
2. `set_lifecycle_with_error(lc, None)` DOES clear `error_reason`.
3. Transitioning Running(Busy) → Shutdown → Running(Idle) clears `activity` at the middle step and repopulates on return.
4. `set_activity` while lifecycle is NOT `Running` still writes the CRDT key (callers are expected to have set lifecycle first, but the method shouldn't silently drop the write).
5. `set_activity` with unchanged value does not bump heads (throttle invariant).
6. Old writer (`set_kernel_status("idle")`) followed by `read_state()` returns a `lifecycle` derived via `from_legacy` — the from_legacy fallback path fires when `kernel/lifecycle` has never been written.
7. New writer (`set_lifecycle(Running(Idle))`) populates both shapes — `status == "idle"`, `starting_phase == ""`, AND `lifecycle == Running(Idle)`.
8. Mixed sequence: new writer, then old writer, then new writer again. Each `read_state()` must be internally consistent for whichever shape the test asserts on.
9. Merging a fork that ran `set_lifecycle(Resolving)` with a main doc that ran `set_kernel_status("idle")` — which value wins is Automerge's call; test just pins the behavior so future regressions stand out.
10. `set_lifecycle_with_error(Error, Some("oops"))` followed by `set_lifecycle(NotStarted)` — the spec says `set_lifecycle` leaves `error_reason` alone, so it stays `Some("oops")` even though the lifecycle left Error. (Subtle: retry paths want the reason preserved; explicit clear is via `set_lifecycle_with_error(..., None)`.)
11. `set_lifecycle_with_error` called twice with different reasons — the second call overwrites.
12. `set_lifecycle_with_error(lifecycle, Some(""))` treats empty string as "set, but empty" — distinct from `None` (which clears) in intent but produces the same CRDT value. Test whichever semantics we pick; the writer's doc comment needs to match.
13. Reading a doc that has NEITHER legacy keys NOR new keys (freshly-constructed `new_empty()` — used by clients that sync their state) returns the `KernelState::default()`.
14. Reading a doc that was scaffolded by `new()` but never written since — all new keys at defaults, all legacy keys at defaults — returns `lifecycle == NotStarted` via the new-key path (not the legacy fallback).

## Checkpoint for reviewer

Look at:
- Scaffold symmetry between `new()` and `new_with_actor()`.
- The `read_state` priority order (new keys preferred; legacy fallback when new are unset).
- Whether `set_lifecycle` clearing `activity` on non-Running is the right default (vs. preserving it for an odd transition like `Running(Busy) → Error → Running(Idle)` where someone might want to remember the previous activity).
- The `error_reason` ownership rules. Current proposal: `set_lifecycle` preserves, `set_lifecycle_with_error(None)` clears, `set_lifecycle_with_error(Some("…"))` sets.
