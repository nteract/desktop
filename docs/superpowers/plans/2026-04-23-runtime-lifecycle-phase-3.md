# RuntimeLifecycle Phase 3 — Typed Error Reasons

**Goal:** Replace the free-form `Option<&str>` error-reason argument to `set_lifecycle_with_error` with a typed `KernelErrorReason` enum. Fix the two codex-v3 compat gaps from Phase 2 along the way. Still no caller migration.

**Why only this much:** Phase 2 left `error_reason` as a free-form string at the call-site boundary. Kyle flagged `"missing_ipykernel"` sprinkled through writers and tests as the same string-shape smell we're trying to escape. Phase 3 makes reasons a first-class type. Phase 4 then migrates callers with type-safe construction.

**Spec:** `docs/superpowers/specs/2026-04-23-runtime-lifecycle-enum-design.md`
**Prior phases:** `2026-04-23-runtime-lifecycle-phase-1.md`, `2026-04-23-runtime-lifecycle-phase-2.md`

## Scope

- `crates/runtime-doc/src/types.rs`:
  - New `KernelErrorReason` enum. Closed (no `Other(String)` escape). Variant `MissingIpykernel` only.
  - `as_str()` returns `"missing_ipykernel"` — serves BOTH as the CRDT value AND the legacy `starting_phase` mirror. The two strings are the same because that's how the legacy channel already encoded it.
  - `parse(&str) -> Option<Self>` for reading.
  - Serde round-trip tests + parse-unknown tests.

- `crates/runtime-doc/src/doc.rs`:
  - Change `set_lifecycle_with_error` signature from `Option<&str>` to `Option<KernelErrorReason>`.
  - When `lifecycle == Error && reason.is_some()`, mirror `reason.as_str()` into `starting_phase` (the codex-v3 missing_ipykernel compat fix — now via the enum's typed projection).
  - `set_activity` clears stale `starting_phase` when mirroring to a non-starting status. Throttle widens to include all three keys (activity, status, starting_phase).
  - Tests for both fixes.

- `KernelState.error_reason: Option<String>` — **unchanged**. Snapshot stays stringly on purpose so newer daemons with unknown variants don't break older readers.

## Out of scope

- Any caller migration (Phase 4).
- TS frontend changes (Phase 5).
- Python bindings changes (Phase 6+).
- Retiring string setters (later phase).
- Moving `read_str_if_present` to `automunge` (follow-up).

## Design notes

### Why closed enum, no `Other(String)`

Kyle's direction: "those look very string like … instead of enums." The point of the enum is to make reasons exhaustive and typo-proof. `Other(String)` reintroduces the stringly escape hatch we're trying to close. The cost of "closed" is that adding a reason requires a variant + recompile. That's the right cost.

Today we ship one variant (`MissingIpykernel`). When a future path wants `KernelDied` or `EnvResolveTimeout`, add a variant.

### Why `KernelState.error_reason` stays `Option<String>`

Two reasons:

1. **Schema robustness.** If a newer daemon writes a reason our reader doesn't know, `Option<KernelErrorReason>` would have to either drop it (silent info loss) or panic. `Option<String>` surfaces the raw string so readers can log or pass through.
2. **No reader change needed.** Python bindings and frontend already handle `Option<String>`. Switching to enum in the snapshot would force them to also convert — work that Phase 5/6 will do if there's demand.

The enum lives at the *writer* boundary. Readers see the string it projected to.

### The missing_ipykernel `starting_phase` compat

Codex v3 flagged: the current `NotebookToolbar` gates its pixi-install prompt on `starting_phase == "missing_ipykernel"`. When callers migrate to the typed writer, they'll pass `Error + Some(MissingIpykernel)`. My Phase 2 `set_lifecycle.to_legacy()` returns `("error", "")` for `Error`, wiping the phase.

Fix: in `set_lifecycle_with_error`, when `lifecycle == Error && reason.is_some()`, overwrite `starting_phase` with `reason.as_str()` after the `set_lifecycle` mirror ran. That preserves the legacy channel without leaking strings into the writer — the string lives in the enum's `as_str` impl in exactly one place.

### The `set_activity` stale-phase bug

Codex v3 also flagged: after the old launch path ends with `set_starting_phase("connecting")`, the first IOPub `set_activity(Idle)` mirrors `status = "idle"` but leaves `starting_phase = "connecting"`. `set_kernel_status` would have cleared it. Same rule should apply.

Fix: `set_activity` reads `starting_phase`; if non-empty, clears it. Throttle widens to check all three keys (activity, status, phase) — skip only when every key is already at target.

## Acceptance

- `cargo test -p runtime-doc` passes, including new tests.
- `cargo check --workspace` clean.
- Existing callers of `set_kernel_status` continue to work (not migrated in this phase).
- Existing Phase 2 tests that passed `Some("missing_ipykernel")` updated to `Some(KernelErrorReason::MissingIpykernel)`.

## Test plan — adversarial coverage

1. `KernelErrorReason::MissingIpykernel.as_str() == "missing_ipykernel"` — single source of truth for the string.
2. `parse("missing_ipykernel") == Some(MissingIpykernel)`, `parse("bogus") == None`.
3. Serde round-trip.
4. `set_lifecycle_with_error(Error, Some(MissingIpykernel))` writes `error_reason` AND `starting_phase` to `"missing_ipykernel"` — proves the pixi compat still works via the enum.
5. `set_lifecycle_with_error(Error, None)` writes empty `error_reason` and empty `starting_phase` (the `Error`-variant default mirror).
6. `set_lifecycle_with_error(Launching, Some(MissingIpykernel))` — non-Error with reason — writes `error_reason = "missing_ipykernel"` but does NOT touch `starting_phase` (`set_lifecycle` already wrote `"launching"` there; we don't overwrite on non-Error). Documents the asymmetry.
7. `set_activity(Idle)` after `set_starting_phase("connecting")` clears the phase.
8. `set_activity(Idle)` with all three keys at target is still a no-op (heads don't advance).
9. `set_activity(Idle)` where legacy status drifted to "busy" repairs status WITHOUT touching phase (if phase is already empty).
10. A pre-Phase-3 doc with `error_reason = "missing_ipykernel"` written as raw string parses correctly via `KernelErrorReason::parse` — backward compat with Phase 2 scaffolded docs.

## Checkpoint for reviewer

Look at:
- Whether closed vs `Other(String)` is the right call. I argue closed: reasons are rare enough that adding a variant isn't painful, and closed forces good hygiene. Kyle's "instead of enums" steer supports this.
- Whether `KernelState.error_reason: Option<String>` staying stringly is right. My claim: writer-side type safety is where the enum earns its keep; snapshot-side robustness argues for string.
- Whether `as_str` doubling as both CRDT value and legacy phase is too clever. Alternative: two methods (`as_crdt_str` + `as_legacy_phase`). For one variant where both are identical, one method feels right; if a future variant has distinct values, split then.
