# RuntimeLifecycle Phase 5 — Migrate TypeScript Frontend

**Goal:** Give the frontend typed access to `RuntimeLifecycle` and `KernelActivity`, and replace every `status`-string-based read in `packages/runtimed` + `apps/notebook` with pattern-matches on the typed shape. The `NotebookToolbar`'s `startingPhase` prop becomes `lifecycle + errorReason`, which closes the Phase 2 pixi-install-prompt compat bridge from the reader side.

**Why only this much:** frontend migration is a self-contained unit. Python bindings (`runtimed-py`) and the wire-level `NotebookKernelInfo.status` field are separate later phases.

**Spec:** `docs/superpowers/specs/2026-04-23-runtime-lifecycle-enum-design.md`
**Prior phases:** 1 (Rust enum), 2 (CRDT keys + writers), 3 (`KernelErrorReason`), 4 (Rust callers migrated).

## What lands

### `packages/runtimed/src/runtime-state.ts`

- New TS types mirroring the Rust enums:
  - `KernelActivity = "Unknown" | "Idle" | "Busy"`
  - `RuntimeLifecycle` — discriminated union on the `lifecycle` tag, with `{ lifecycle: "Running", activity: KernelActivity }` as the only payload-bearing variant. Matches Rust's serde tag+content format.
- Extend `KernelState` interface with `lifecycle: RuntimeLifecycle`, `activity: KernelActivity`, `error_reason: string | null`. Keep `status` + `starting_phase` for dual-shape compat — nothing deletes them in this phase.
- `DEFAULT_RUNTIME_STATE` gets `lifecycle: { lifecycle: "NotStarted" }`, `activity: "Unknown"`, `error_reason: null`.

### `packages/runtimed/src/derived-state.ts`

- `deriveEnvSyncState` matches on `state.kernel.lifecycle.lifecycle` instead of string comparison. Behavior unchanged.
- `kernelStatus$` is deprecated — it only existed to bridge the stringly `kernel.status` to the UI. Frontend consumers move onto `state.kernel.lifecycle` directly; internal shim retained for backwards compat during the transition, but marked `@deprecated` with a JSDoc pointer to the new path.

### `apps/notebook/src/lib/kernel-status.ts`

- Adds `getLifecycleLabel(lc: RuntimeLifecycle, reason: string | null): string`. Returns the user-facing kernel-state label directly from the typed shape — "resolving environment", "launching kernel", "idle", "busy", "error: missing ipykernel", etc.
- Keeps `getKernelStatusLabel(status, startingPhase)` exported (deprecated) so any caller outside `NotebookToolbar` that we missed still works.

### `apps/notebook/src/components/NotebookToolbar.tsx`

- `startingPhase?: string` prop → `lifecycle: RuntimeLifecycle` + `errorReason: string | null`.
- `kernelStatusText` derived from `getLifecycleLabel(lifecycle, errorReason)`.
- The `missing_ipykernel` gate (line 378 pre-migration) moves from `startingPhase === "missing_ipykernel"` to `errorReason === "missing_ipykernel"`. The reason propagates through the typed `error_reason` CRDT key (Phase 2) and `KernelErrorReason::MissingIpykernel` writer (Phase 3), so this closes the loop.

### `apps/notebook/src/hooks/useDaemonKernel.ts`

- `rawStatus` read replaced with `runtimeState.kernel.lifecycle` + `.activity`.
- Return value grows `lifecycle` and `errorReason`; drops `startingPhase`.

### `apps/notebook/src/App.tsx`

- Threads `lifecycle` and `errorReason` through to the toolbar instead of `startingPhase`.

### Tests

- `packages/runtimed/tests/sync-engine.test.ts` — fixtures gain `lifecycle` / `activity` / `error_reason` fields. Keep the legacy `status` / `starting_phase` so other fixture-sharing tests don't break.
- `apps/notebook/src/components/__tests__/notebook-toolbar.test.tsx` — pixi-prompt tests switch their fixture from `startingPhase="missing_ipykernel"` to `lifecycle={{ lifecycle: "Error" }}` + `errorReason="missing_ipykernel"`. Adds a coverage case for non-Error + error_reason (should NOT show the prompt).

## Out of scope

- Deleting `status` / `starting_phase` from the TS `KernelState` — Phase 6 or retire phase.
- `runtimed-py` / `runtimed-node` bindings — Phase 6.
- Wire `NotebookKernelInfo.status` field — wire-level rename needs its own PR.
- Retiring `getKernelStatusLabel` — waits until greps show no callers outside tests.

## Invariants preserved

- Runtime snapshot JSON shape on the wire is unchanged — Rust already emits `lifecycle` / `activity` / `error_reason` (Phase 2). This phase is just teaching TS to read those fields.
- Dual-shape: the TS `KernelState` keeps `status` and `starting_phase`. Any consumer we don't touch keeps working.
- The toolbar pixi-install prompt continues to fire on `missing_ipykernel` — now through the typed `errorReason` field instead of the legacy `startingPhase` prop. Phase 3 already wired the daemon to populate both the CRDT `error_reason` and the legacy `starting_phase` mirror.

## Acceptance

- `pnpm -C packages/runtimed test` passes.
- `pnpm -C apps/notebook typecheck` and `test` pass.
- `cargo xtask lint` clean (runs vp on the TS files too).
- A manual trace through the pixi-install-prompt path (toolbar test) confirms the prompt still fires on `errorReason === "missing_ipykernel"` but not on other errors.
