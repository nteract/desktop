# Widget sync stall — detection, recovery, and CRDT-first projection

**Status:** In progress. Track B (detection + recovery) shipping on branch `chore/widget-sync-tracing`. Track A (architectural refactor) planned.

## Problem

ipywidgets in an active notebook sporadically stop reflecting kernel-side
state changes. Symptoms reproduced with a minimal matplotlib `@interact`
FloatSlider:

- User drags the slider with arrow keys.
- The slider thumb moves in the UI (optimistic local update).
- The matplotlib image embedded in the `Output` widget freezes at a
  stale value — plot title says `sin(1.5x)` while the slider sits at
  `2.70`.
- New `execute_cell` requests dispatched to the daemon appear to
  complete on the daemon side (log shows kernel processed them, CRDT
  has up-to-date state) but produce no visible change in the UI.
- **Reloading the notebook window fixes everything instantly.**

That last bit is load-bearing: the CRDT has the correct final state.
The stall is entirely frontend-side — the daemon→frontend pipeline
stops delivering updates the UI can render, silently.

## Root-cause surface

Two architectural choices combine to produce the stall:

### 1. Two sources of truth for widget state

The `WidgetStore` is updated from two independent paths:

- **Optimistic** (`WidgetUpdateManager.updateAndPersist` → `store.updateModel`):
  every user interaction synchronously mutates the store for instant UI
  feedback, then writes to the CRDT on a 50 ms debounce.
- **CRDT projection** (`SyncEngine.commChanges$` → App.tsx subscriber
  → `store.updateModel`): every sync frame from the daemon runs through
  `projectComms` and pushes the resolved state back into the store.

An echo-suppression layer (`WidgetUpdateManager.shouldSuppressEcho`)
filters optimistic keys out of incoming CRDT projections so a stale
kernel echo doesn't clobber an in-flight drag. Under rapid input the
suppression's bookkeeping gets stuck — `optimisticKeys` are tracked
per-comm with no bounded lifetime and a `!writer` early-return in
`flushComm` can leave them populated indefinitely, silently dropping
every subsequent echo for that comm.

### 2. Silent sync-layer failures

Even without the echo-suppression tangle, the Automerge sync layer can
drift silently:

- `sendFrame` can fail (pipe buffer full, daemon slow) without the
  caller knowing the frame wasn't delivered. The `sent_hashes` state
  on the WASM handle advances anyway, permanently filtering the change.
- Bloom-filter false positives convince both peers they're in sync
  when they aren't.
- The frontend only surfaces a recovery signal when
  `receive_sync_message` fails outright — the "frame never arrived" and
  "heads drifted" failure modes are invisible.

## Fix strategy

Two tracks. They compose but ship independently.

### Track B — safety net (no architecture change)

Makes existing failure modes visible and recoverable.

1. **SyncError observability** (`SyncEngine.syncErrors$`). WASM already
   auto-recovers from failed `receive_sync_message`; the engine already
   forwards the recovery reply. Surface it via a top-level banner
   (`SyncRecoveryBanner`) so silent recovery becomes visible. Arms a
   transient "Sync recovered" message; recurring recoveries bump a
   counter ("recovered N times recently — connection may be unhealthy").
2. **Runtime-state stall watchdog**. After an outbound
   `RUNTIME_STATE_SYNC` flush, start a 3 s timer. Clear only when
   `generate_runtime_state_sync_reply()` returns `null` (the precise
   "daemon has fully caught up with our writes" signal — not mere
   inbound traffic, which fires constantly during kernel execution).
   On timeout: log, `handle.reset_sync_state()`, re-flush, emit on
   `syncErrors$`.
3. **Observability primitives** already landed: abort hung blob fetches
   (so one stuck fetch doesn't poison the serial `commEmitQueue`), and
   log (instead of silently swallow) subscriber errors in the frame bus
   and runtime-state store.

### Track A — architectural refactor (CRDT-first)

Eliminates the drift class entirely by collapsing two sources of truth
into one.

1. **Route local writes through `projectComms`**. Today `projectComms`
   only runs on inbound `runtime_state_sync_applied`. After
   `set_comm_state_batch` on the local handle, fire a synthetic event
   (or call `projectComms` directly) so the WidgetStore updates from
   the same pipeline regardless of whether the change originated
   locally or arrived from the daemon. Paves the way for A2.
2. **Remove the optimistic path**. `WidgetUpdateManager.updateAndPersist`
   stops calling `store.updateModel` synchronously. All store updates
   come from `projectComms`. Delete `shouldSuppressEcho`,
   `optimisticKeys`, the `!writer` retry loop. The remaining
   `WidgetUpdateManager` collapses to ~50 lines of debounced CRDT
   writes. Single source of truth.
3. **jslink re-derivation**. Today `link-subscriptions.ts` propagates
   source→target synchronously via `store.updateModel`. After A2,
   instead of snapshotting, the subscription runs inside each
   `projectComms` emission: read source from CRDT, write derived value
   to target. Same behavior, different source. Custom messages
   (buttons, `model.send()`) and the canvas manager router are on a
   separate broadcast pipeline (`commBroadcasts$`) and don't change.

### Supporting work

- **Real-WASM test harness** (`packages/runtimed/tests/wasm-harness.ts`
  + `widget-sync-stall.test.ts`). Scripts the `@interact` scenario
  headlessly: two `NotebookHandle`s (server + client) connected via
  `DirectTransport`, real RuntimeStateDoc sync, assertions on
  `commChanges$` emissions. Lets every subsequent refactor validate
  against the exact stall case without manual slider-dragging.
- **`put_comm_for_test`** on the WASM. The real daemon opens comms
  on kernel `comm_open`; exposing the same operation as a clearly
  test-only method lets the harness simulate the daemon end to end.
- **Move more logic into shared `runtimed-js` / `runtimed-wasm`**. The
  `commChanges$` subscriber loop, `resolveCommOutputs`, and
  `WidgetUpdateManager` are all framework-agnostic today but live in
  `apps/notebook`. As Track A lands, push each into the shared library
  so Deno harnesses, Python clients, and future frontends wire up
  identically. Each commit in Track A naturally produces a candidate.

## Known limitations (follow-up)

- **Rare echo-clobber from validator widgets.**
  The local store is mirrored on every tick in
  `WidgetUpdateManager.updateAndPersist` so slider thumbs move
  smoothly even when CRDT writes are throttled to 50 ms/comm.
  `projectLocalState → commChanges$ → diffResolvedState` makes the
  normal round-trip a no-op against what we already wrote. If a
  validator widget on the kernel side rewrites the incoming value
  (e.g., clamps `value=150` to `max=100`), the kernel echo lands in
  the CRDT as a distinct Automerge write and projects back through
  `commChanges$` with a *different* value than the local store — so
  the diff is non-empty and the user sees a momentary snap to the
  validated value mid-drag. This is the expected correction in
  practice; the only surprise is that it reaches the UI immediately
  rather than after the drag settles. Documented here because the
  earlier design pre-empted this class with explicit echo suppression
  bookkeeping (`optimisticKeys`), which we removed in Track A.

- **iframe jslink targets don't propagate to other views of the same
  widget.** Each output iframe runs its own `WidgetStore` shadow
  (see `src/isolated-renderer/widget-provider.tsx`) that the parent
  `CommBridgeManager` syncs into. jslink is frontend-only by
  ipywidgets semantics, so its target writes deliberately skip the
  kernel — but they currently also skip the parent store. If the
  same model is rendered in multiple cells (multiple iframes), only
  the iframe where the source tick fired sees the target update.
  The shipping compromise: iframe-local-only is correct for the
  common case (one cell per model) and matches how Jupyter Lab
  handles jslink within a single output area. A cross-iframe fix
  would need either a new "local-only" bridge RPC that updates the
  parent store without forwarding to the kernel, or routing jslink
  through the CRDT (blocked on the throttle-stutter follow-up
  above). Deferred.

## What this is NOT

- Not persisting the sync state across reloads (would be nice; deferred).
- Not a full reconnect on stall — `reset_sync_state` is cheaper and
  sufficient for the observed failure modes.
- Not a full rewrite of the widget store — the store itself is fine;
  only the dual-write dispatch path on top of it is the problem.

## Validation

- Unit tests in `sync-engine.test.ts` cover the `syncErrors$` observable
  and the stall watchdog (fires on timeout, clears only on true
  convergence, re-arms correctly).
- Real-WASM tests in `widget-sync-stall.test.ts` lock in the expected
  `commChanges$` emission shape for the `@interact` scenario. These
  are the invariants Track A must preserve.
- Manual repro: matplotlib `@interact` + FloatSlider, hammer arrow
  keys. Before Track B: silent stall. After Track B: banner fires on
  recovery, watchdog resets the sync state if the pipe dropped a
  frame. After Track A: no drift possible; optimistic path and
  suppression heuristic both deleted.

## How we got here — jslink archaeology

The optimistic dual-write was introduced in
[PR #1580](https://github.com/nteract/desktop/pull/1580)
(`feat(widgets): debounced CRDT writes + jslink echo suppression`).
`WidgetUpdateManager` landed as part of that PR with three concurrent
goals:

1. **Slider flooding**: raw slider drags generated ~60 CRDT writes/sec.
   Debouncing to 50 ms per comm cut that to ~20/sec — healthier sync
   traffic without losing interactivity.
2. **Instant UI feedback**: waiting for a round-trip echo before
   showing the new slider value would feel laggy. Synchronous
   `store.updateModel` gives the user unmistakable feedback during a
   drag.
3. **jslink feedback loops**: two linked widgets (e.g., paired
   min/max sliders) used to oscillate because each local change
   echoed back through the CRDT and re-triggered the link handler.
   `shouldSuppressEcho` filters the echo of optimistic keys out of the
   incoming CRDT projection so the link doesn't bounce.

All three are legitimate concerns. Track A doesn't retreat on any of
them — it addresses them from a different angle:

1. Slider flooding → the debounce stays (pure outbound coalescer), it
   just loses the reconciliation bookkeeping it currently does against
   the optimistic store.
2. Instant UI feedback → achieved by making the local CRDT write fire
   `projectComms` synchronously in the same tick (Track A1). The UI
   updates from the same pipeline as remote changes, at local-write
   speed.
3. jslink loops → solved by deriving targets from the CRDT on each
   `projectComms` emission rather than cascading through the store.
   The source's CRDT value is the single source of truth for the
   link's computation; no oscillation possible because there's no
   parallel optimistic state to drift from.

The stall that motivated this investigation isn't a regression in
#1580 — it's an emergent interaction between the echo-suppression's
bookkeeping and edge cases (silent sync failures, `!writer` early
returns leaving `optimisticKeys` populated indefinitely). The fix is
to remove the need for the bookkeeping entirely, not to patch it.

## References

- `crates/runtimed-wasm/src/lib.rs` — `receive_frame`, `reset_sync_state`,
  `normalize_sync_state`, `put_comm_for_test`.
- `packages/runtimed/src/sync-engine.ts` — stall watchdog, `syncErrors$`,
  `projectComms`.
- `src/components/widgets/widget-update-manager.ts` — current dual-write
  path (target for deletion in A2).
- `src/components/widgets/link-subscriptions.ts` — jslink (A3 target).
- `~/code/src/github.com/automerge/automerge-repo/packages/automerge-repo/src/DocHandle.ts:241-260`
  — canonical headsAreSame short-circuit, ours at
  `crates/runtimed-wasm/src/lib.rs:1604-1605`.
