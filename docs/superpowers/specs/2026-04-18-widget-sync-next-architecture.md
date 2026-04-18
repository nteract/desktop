# Widget sync: immediate fix + cleanup plan

**Status:** Proposal. Two existing PRs are open as reference
(#1880, #1881); neither is merged. Don't treat them as current
state — main has no watchdog, no `SyncRecoveryBanner`, no
`syncErrors$` observable. What main has is a 15-line
`shouldSuppressEcho` filter in `WidgetUpdateManager` and a raw
`listen("notebook:frame")` handler with no exception trapping.

## Lead with the bug

Under rapid widget interaction (matplotlib `@interact`
`FloatSlider` driven with arrow keys), the notebook app
occasionally stops reflecting kernel-side changes. Slider thumb
moves in the UI; plot underneath freezes mid-drag. Reloading fixes
it instantly.

The best hypothesis from the evidence is that the webview's
`listen("notebook:frame")` handler unsubscribes after an
uncaught exception. Tauri's event system drops callbacks whose
handlers throw. Daemon keeps sending frames; nothing is listening.
The outbound path (WASM → Tauri invoke) still works, which is why
flushes complete without error — only the return channel is dead.

**The fix is five lines.** `apps/notebook/src/lib/tauri-transport.ts`
today has no `try` / `catch` around the listener callback. Adding
one — plus a `console.error` log — both confirms the theory and
prevents the listener from dying. Reproduce the stall before and
after, look for the exception in the log, harden against whatever
shows up.

This should ship first, before any of the larger investigations
below. If it turns out the listener doesn't die (no exception
logged, stall still happens), the theory is wrong and the
cleanup plan below needs revisiting. Either way, the experiment
takes 30 minutes and de-risks everything that follows.

## Cleanup worth doing around the fix

With the bug patched, what's still worth cleaning up? Two
orthogonal things:

1. **A real recovery path (investigation 3)** — so the user can
   recover without a full reload if the bug ever regresses or a
   different failure mode produces a similar wedge. The building
   blocks exist (`TauriTransport.disconnect`, `SyncEngine.start`,
   `resetForBootstrap`); this is wiring them to a trigger, plus
   verifying Tauri actually releases subscriptions cleanly
   (investigation 0.5, a 15-minute verification).

2. **Stop reconstructing authorship from merged JSON
   (investigation 1)** — a code-smell refactor justified on its
   own merits, not on the stall. The current `shouldSuppressEcho`
   works today; the argument for replacing it is that we already
   have an actor-based precedent in `useCrdtBridge.tsx:154` and
   the filter's trajectory under load (#1880's review history)
   points at trouble. This is refactor-on-its-own-merits, not
   part of the bug fix.

A third investigation — heads-gossip as a liveness probe — stays
deferred. The dead-pipe argument is already enough to shelve it.

PR #1881's watchdog could be dropped entirely once (0) + (3)
land: if the listener is protected *and* we have a reconnect
primitive, the watchdog is detecting a failure mode that can't
occur. That's a decision for when those PRs actually land.

## Problem (in detail)

Under rapid widget interaction (the canonical reproducer is a
matplotlib `@interact` `FloatSlider` driven with arrow keys), the
notebook app occasionally stops reflecting kernel-side state
changes. The slider thumb moves in the UI (optimistic local update);
the plot beneath it freezes mid-drag; the title reads `sin(2.10x)`
while the slider sits at `2.80`. Reloading the window clears it
instantly — the CRDT has the correct final state. The drift is
entirely frontend-side; some part of the client→daemon pipeline has
silently stopped delivering updates the UI can render.

See `2026-04-17-widget-sync-stall-design.md` for the original
investigation write-up and `2026-04-18-widget-sync-recovery-design.md`
for what PR #1881 proposes to address.

## What we've tried

### Pre-existing: optimistic dual-write + echo suppression

`WidgetUpdateManager` writes every widget interaction to two places:
a React-local store (instant UI) and a debounced CRDT write to the
daemon. An `shouldSuppressEcho` filter tracks "optimistic keys" so
the projected CRDT round-trip doesn't clobber an in-flight drag
value. This is what shipped before the investigation began. The
stall is an emergent interaction between this reconciliation layer
and silent sync-layer failures — not a bug in it, exactly, but a
surface where the failure becomes visible.

### PR #1880 (deferred): CRDT-first projection

Attempted to collapse the two sources of truth into one by routing
all local writes through the same `projectComms → commChanges$ →
WidgetStore` pipeline as remote inbound changes. Idea: if the store
is always a projection of the CRDT, the "echo of my own write"
category stops existing.

In practice, eleven rounds of review surfaced a sequence of corner
cases: slider smoothness required per-tick local store mirroring
(reintroducing "which value is mine?"), which required pending-write
bookkeeping, which needed value-history instead of latest-only
(microtask race), which needed consume-on-match semantics (peer
collaborative writes), which needed direct-writer hooks (anywidget
`save_changes`), which needed buffer/throttle ordering. Each fix
was defensible; the aggregate is ~200 lines of reconciliation
machinery that reinvents authorship detection from merged state.

**Decision:** defer. The approach isn't wrong, but it's the wrong
*level* — Automerge already tracks authorship at the change level;
we were reconstructing it.

### PR #1881 (also open): detection + recovery safety net

Orthogonal to the write-path question. Makes two silent failure
modes visible:

- **WASM auto-recovery** (`receive_sync_message` errors, doc rebuilt,
  fresh sync message sent) — now surfaced via `SyncEngine.syncErrors$`
  and a top-level banner.
- **Silent flush drop** (transport accepts the frame but daemon never
  processes it) — a 3s watchdog after `flush_runtime_state_sync`;
  any inbound runtime-state frame clears it; timeout calls
  `reset_sync_state()` and re-flushes.

Real-world repro confirms the watchdog catches the stall class. It
does **not** fully recover from it — `reset_sync_state()` rebuilds
`sync::State` for all three docs (sent hashes and bloom filter are
implicitly rebuilt with it), but the user's evidence shows the
watchdog firing every 3s in a loop. Rebuilding sync state doesn't
help when the underlying channel isn't delivering in the first
place. The banner narrows the user's gap from "is it stuck?" to
"yes, stuck — reload." Real recovery is out of scope for that PR.

## Where we've been fighting Automerge

Honest assessment of why the above composes poorly:

1. **Reinvented authorship detection.** On current main,
   `WidgetUpdateManager.shouldSuppressEcho` is a set membership
   check on `optimisticKeys` — ~15 lines of filter. It's small
   today, and it works for the happy path. What the #1880
   review threads demonstrated is what that filter grows into
   under load: the trajectory was `optimisticKeys` → pending-value
   tracking → value-history (microtask race) → consume-on-match
   (peer collaboration) → direct-writer hooks (anywidget) →
   buffer/throttle ordering. Each step was defensible individually;
   the aggregate was ~100 lines of reconciliation machinery
   reinventing authorship detection from merged state.

   Automerge already records authorship at the change level. The
   filter keeps growing corners because we're looking at merged
   JSON and reconstructing what the CRDT already knows. The fix
   isn't to make the filter smarter — it's to stop needing it.

2. **Watchdog times out on a proxy signal.** Our watchdog arms on
   any outbound runtime-state flush and clears on any inbound
   runtime-state frame. Neither edge of that bound is tightly
   correlated with "did the peer actually receive what I sent."
   The false-positive shape (fire on converged-idle) is well
   understood; the false-*negative* shape matters more here — the
   real-world repro shows the watchdog firing every 3s while
   recovery never converges, because the signal we're timing out on
   ("inbound traffic") isn't the signal we care about ("my change
   was received"). See investigation (2) for one way to close that
   gap, and the caveat there about whether it's worth the surface
   area.

3. **Three sync protocols, three recovery paths.** NotebookDoc,
   RuntimeStateDoc, PoolDoc each have their own `sent_hashes`, their
   own recovery handler, their own frame type. The watchdog we built
   is runtime-state-only. When another class wedges, we'll bolt on
   another watchdog.

4. **Transport-contract mismatch.** `sendFrame` resolving `Ok`
   means "Tauri IPC queued it" — not "daemon processed it." The gap
   between those is where the stall lives. `sent_hashes` advances on
   the optimistic outcome, and nothing at the sync layer detects
   the divergence.

## Why recovery doesn't currently recover

The watchdog log from the user's repro is diagnostic. It fires
every 3s in a tight loop:

```
[WARN] runtime-state flush stalled ... — resetting sync state and re-flushing
[WARN] runtime-state flush stalled ... — resetting sync state and re-flushing
[WARN] runtime-state flush stalled ... — resetting sync state and re-flushing
...
```

That rules out most of the "obvious" hypotheses:

- **Not a bloom-filter false positive.** `reset_sync_state()` rebuilds
  the entire `sync::State`, which forces the next flush to do a full
  sync handshake with no bloom. A false positive cannot survive that
  reset. Since the reset isn't helping, the protocol-level sync
  state is not the problem.
- **Not daemon death.** Windows opened *after* the first one stalls
  work normally against the same daemon. The daemon is reading,
  writing, and talking to the kernel.
- **Not a WASM panic.** Those produce `sync_error` events that the
  engine handles and the banner would show as `auto_recovered`. The
  log shows only `stall_detected`, meaning the WASM side is happily
  generating messages but nothing is coming back.

What *does* survive `reset_sync_state()` is a dead pipe. If the
outbound path silently drops, we re-send everything into the void;
the daemon never responds because it never received; the next
watchdog fires; repeat. That matches the loop exactly.

The most likely specific failure mode is the webview's
`listen("notebook:frame")` handler unsubscribing. Tauri's event
system drops callbacks if an exception escapes the handler — a
single malformed frame, decoder assertion, or mid-receive exception
and the listener is dead from that point on. The daemon keeps
sending frames, Tauri keeps dispatching them, and nothing on our
side is listening. The outbound side (WASM → Tauri invoke) is still
fine, which is why the flush completes without error; only the
return channel is dead.

That explains the reload-fixes-it behavior: reload tears down the
webview entirely, rebuilding the listener registration along with
everything else.

### Experiments to confirm

A new implementer should be able to reproduce the stall and then:

1. **Open a second window on the same notebook while the first is
   stalled.** If the second window syncs normally, the daemon and
   inter-window sync are fine — only the first window's listener
   is dead. Strongly implicates the Tauri listener theory.
2. **Instrument `listen("notebook:frame")` with a counter.** If the
   counter stops incrementing after the stall but other listeners
   on the same window (presence, broadcasts) keep firing, a
   specific listener has been silently unregistered.
3. **Add a `console.error` trap around the frame handler.** If any
   exceptions are escaping the handler, they should be visible —
   and that's the signal to fix the handler to catch them itself.

## Proposed investigations

Four items. Investigation 0 is a 30-minute experiment that gates
the scope of (2) and (3). Investigation 1 is high-leverage on its
own regardless of 0's result.

### 0. Confirm the listener-death diagnosis

Before committing to a reconnect primitive or a heads-gossip
protocol, run one experiment: wrap the `listen("notebook:frame")`
callback in `apps/notebook/src/lib/tauri-transport.ts` with a
`try / catch` that logs to `console.error` and reproduces the
stall.

- If an exception shows up in the log right before the stall,
  the diagnosis holds and (3) is the right shape. The specific
  exception also tells you the malformed-frame or decoder
  assertion to harden against.
- If no exception appears and the listener still stops delivering,
  the theory is wrong. (3) would be building a reconnect for the
  wrong problem; revise from there.

This is cheap and de-risks the larger investigations. Do it first.

**Where to look:**
- `apps/notebook/src/lib/tauri-transport.ts` — the `listen`
  callback currently has no `try / catch`; frame decoding happens
  downstream in WASM which has its own error handling, but any
  exception between `listen` and the WASM call will unregister the
  listener silently.

### 0.5. Verify Tauri's unlisten actually releases

Before committing to investigation (3)'s reconnect primitive,
verify that Tauri's `unlisten` fully releases the underlying
subscription rather than leaving a zombie handler that a new
`listen` would layer on top of.

- Add a counter to the `notebook:frame` handler. Register, then
  unlisten, then re-register. Fire frames from the daemon while
  doing this and watch the count.
- Expected clean release: counter stops incrementing during the
  unlistened window, resumes from zero after re-register.
- Failure shape: counter continues incrementing via the "dead"
  subscription during the unlistened window, or double-fires after
  re-register.

15 minutes. Determines whether (3) is "wiring" or "wiring +
working around Tauri internals." If the latter, (3) needs a
fallback plan: for example, recreate the `Webview` entirely, or
expose a Tauri Rust command that drops and rebuilds the per-window
event subscription.

### 1. Actor-ID-based echo detection

**Justification upfront:** this is a refactor, not a bug fix.
Today `shouldSuppressEcho` is ~15 lines and works. The case for
replacing it rests on two things: (a) there's already an
actor-based precedent that widget code should probably have been
using originally, and (b) #1880's review history suggests the
current filter grows corners under load. It should be pursued on
those merits, independent of the stall class (0) + (3) are
fixing.

**The precedent:** `apps/notebook/src/hooks/useCrdtBridge.tsx:154`
already does actor-based echo filtering for text edits:

```ts
if (attr.actors.length === 1 && attr.actors[0] === localActor) {
  continue;
}
```

This is the same shape widget code should use. The text path has
the `attributions` payload from `text_attribution` broadcasts
giving it this info cheaply. Widget code doesn't — the authorship
information exists in the CRDT, but isn't surfaced to JS. That gap
is what investigation 1 closes.

Sketch:

- Each frontend window already has a stable actor ID (baked into
  every Automerge op it generates).
- For each key in a projected comm update, query the CRDT: what's
  the actor of the most recent op on this key?
- If it's our actor, skip (our echo).
- If it's a different actor, apply (authoritative — daemon, peer,
  kernel validator).
- Delete: `optimisticKeys`, `shouldSuppressEcho`.
- Keep: per-tick local store mirror; throttled outbound CRDT
  writes.

**Load-bearing unknown — prototype this before committing:**
`automerge` 0.8 has no indexed "who last wrote key K" query.
Answering authorship per key requires walking
`doc.get_changes(&heads)` and filtering for ops touching K —
O(total history). Per widget tick, per key, on every incoming
diff. On a long session with hammered sliders, history grows
unbounded. **Before committing to this investigation**, write a
~50-line Rust prototype that measures the walk cost on a
realistic doc (10k changes, hammered-slider workload). If it's
microseconds per key, ship. If it's milliseconds, the whole
direction needs to change shape — either an indexed query in
notebook-doc (more work), a coarse-grained authorship snapshot
taken less often, or accepting that the current filter stays.

**Where to look:**
- `apps/notebook/src/hooks/useCrdtBridge.tsx:150-156` — the
  existing actor-filter precedent.
- `crates/notebook-doc/` — the attribution code already walks
  `Change::actor_id()` for text. Per-key comm authorship is the
  same primitive against the comms map.
- `crates/runtimed-wasm/src/lib.rs` — needs a new export. Options:
  richer `resolve_comm_state` carrying per-key actor info, or a
  standalone `get_comm_authorship(comm_id)` map. Shape depends on
  the prototype's cost profile.
- `packages/runtimed/src/comm-diff.ts:146-191` — **structural
  change**. Today diffs whole-comm state JSON via
  `JSON.stringify` + string compare. Per-key actor checks require
  a per-key diff, so the emission shape of `commChanges$` changes.
  Downstream consumers of `commChanges$` (`App.tsx`'s widget-store
  subscriber, any test harness using the observable, the isolated
  renderer's widget bridge) all read the whole-state payload
  today — audit call sites before committing.
- `src/components/widgets/link-subscriptions.ts` — **not gated on
  this refactor**. jslink is pure local-store sync and doesn't
  touch the CRDT.

**Rust-forward angle:** the authorship query belongs in
`notebook-doc` / `runtimed-wasm` regardless of who consumes it. A
`get_comm_authorship` Rust API gives both the WASM binding (for
the notebook frontend) and `runt mcp` (for agent tooling that
wants to know "who wrote this widget state") the same answer from
the same code path. This fits the pattern repr-llm, sift-wasm, and
nteract-predicate already use: one Rust crate serving WASM and MCP
symmetrically.

**Why it's worth it:**
- Deletes the machinery that kept growing corners through #1880.
- Handles collaborative peers correctly by construction — two
  peers writing the same value concurrently have different actor
  IDs, no ambiguity.
- No TTL, no microtask ordering, no consume-on-match. Direct
  authorship check.
- Useful beyond the notebook frontend: MCP gets the same
  information for free.

**Risks to investigate:**
- How expensive is "most-recent actor for a key"? Automerge-core
  has this (attribution code walks it), but the cost profile for
  per-emission use isn't characterized.
- `automerge` 0.8 has `get_changes(from_heads)` but not
  `get_changes_added` — which API shape you land on affects how
  the per-key query composes.
- The comm-diff restructure is not purely additive; it's
  structural. Budget for that.

### 2. Heads-gossip for stall detection (speculative)

**Thesis:** the stall watchdog should signal on actual sync
progress, not a proxy.

Sketch:

- Both peers periodically publish current heads for each doc.
- Frontend tracks: "last I announced my heads moved to X"
  + "last I saw peer's heads were Y."
- Divergence over a window → silent-drop detected.
- Recovery: re-send the specific changes the peer is missing via
  `get_changes(from_heads)`.

**Important caveats before building this:**

- **Not a standard Automerge primitive.** Sync messages carry
  heads, but "periodic out-of-band gossip for liveness probing" is
  a design choice, not a convention. If we build it we'd be
  originating the pattern for our purposes, not picking up
  something the Automerge ecosystem already does this way. That's
  allowed, just honestly scoped.
- **Rides the same dead pipe as the thing it's detecting.** If the
  Tauri listener is dead (the diagnosed failure mode), the daemon's
  heads announcement can't arrive either. Heads-gossip detects the
  stall faster than the 3s watchdog, but detection speed was never
  the gap — recovery was, and (3) addresses that directly.
- **Rust-forward framing cuts against this one.** The current
  sketch is frontend-authored timers + new frame types in both
  Rust and JS. If we do build a liveness probe, it should be a
  daemon-side "am I caught up with peer X's heads?" query exposed
  through both WASM and MCP — not a frontend-originated heartbeat.

**Recommendation:** defer this until (0) + (1) + (3) land. If the
stall class is genuinely solved by listener reconnect + actor-ID
echo detection, the remaining need for liveness detection is
small and can be satisfied by something simpler (e.g., "no frames
in 5s → try reconnecting" without heads).

**If built anyway, where to look:**
- `crates/runtimed-wasm/src/lib.rs` — would need `get_heads()` and
  a per-doc change export (note: `automerge` 0.8 has
  `get_changes(from_heads)` not `get_changes_added`).
- automerge-repo's `DocSynchronizer` for prior art on heads-based
  sync — reference only.

### 3. Escalating recovery with a reconnect primitive

**Thesis:** `reset_sync_state()` is the wrong hammer for a dead
pipe. We need a recovery path that can rebuild the listener and
re-handshake, not just reset sync state on a channel that isn't
delivering.

Today, the only signals that take action on stall are WASM auto-
recovery (fine for `receive_sync_message` failures, does nothing
for dead listeners) and the runtime-state watchdog (calls
`reset_sync_state()`, which doesn't help when inbound is dead).
The user's only real recovery is `Cmd+R`.

Proposed recovery hierarchy:

| Signal | Recovery | Cost |
|---|---|---|
| WASM `receive_sync_message` error | WASM rebuilds doc; engine forwards recovery reply | cheap; already exists |
| Heads diverged but inbound otherwise healthy | `reset_sync_state()`; re-flush | cheap |
| Heads diverged + reset didn't help after N rounds | Tear down the `listen("notebook:frame")` handler; re-register it; re-handshake from the daemon's current heads | moderate — full sync round, but no UI state loss |
| Reconnect failed or listener won't re-register | Surface "Reload notebook" in the banner with a one-click reload button | last resort; user loses nothing meaningful (CRDT is durable) |

The third level is the missing recovery step. It's also cheaper
than it first looks — the building blocks already exist:

- `TauriTransport.disconnect()` already unlistens the
  `notebook:frame` handler.
- `SyncEngine.start()` already re-registers the listener via the
  transport and reinitializes the sync pipeline.
- `SyncEngine.resetForBootstrap()` already clears engine-side state
  that would be stale across a reconnect.

So this investigation is largely about **wiring** these existing
primitives to a new trigger, plus making sure the listener can be
cleanly re-registered for the same event on the same window.

Implementation sketch:

- New signal on escalation (N consecutive stall_detected events,
  or a one-shot "Reconnect" action in the banner).
- Call `transport.disconnect()`; `engine.stop()`.
- Instantiate a fresh transport (or reset the existing one's
  listener state); `engine.start()` to re-register.
- On the WASM handle: `reset_sync_state()` so the next flush does
  a full handshake from the daemon's current heads.

**Where to look:**
- `apps/notebook/src/lib/tauri-transport.ts` — contains the `listen`
  call and `disconnect`. Add explicit handler-exception trapping
  here (investigation 0's experiment graduates to production
  hardening).
- `packages/runtimed/src/sync-engine.ts` — `resetForBootstrap`
  is the same shape of reset already driven by `daemon:ready`.
  Reuse rather than build anew.
- `apps/notebook/src/components/SyncRecoveryBanner.tsx` — an
  escalated state with a "Reconnect" button wired to the trigger.

**Why it's worth it:**
- Eliminates the reload requirement for the failure mode we
  actually observed (listener death on a still-healthy daemon).
- Reuses the existing `disconnect` + `start` + `resetForBootstrap`
  infrastructure — small code footprint for a big user-visible win.
- The watchdog in PR #1881 becomes level 2 of the hierarchy rather
  than the terminal recovery.
- Tauri-specific and not shared with `runt mcp` (which uses a
  different transport entirely) — that's fine; this investigation
  has no useful MCP angle.

**Risk to investigate:** does Tauri cleanly release the underlying
subscription when an `unlisten` promise resolves? If there's a
hidden reference cycle or the drop doesn't fully release, the
"reconnect" could silently layer a new listener on top of the
dead one. Verify with the listener-counter instrumentation from
investigation 0.

## What not to revisit

- **Moving runtime state off CRDT to broadcast.** Considered and
  rejected: broadcast doesn't handle late joiners or temporary
  disconnects. New windows / reconnecting clients would have no way
  to catch up on current kernel status, execution queue, trust
  state. Any fix for that re-derives a sync engine — at which point
  we've rebuilt Automerge badly.

- **Another form of value-history echo filter.** PR #1880's history
  showed the shape of that dead end. Each round found a new corner
  (microtask race, peer collaboration, bootstrap drain, direct
  writers, buffer ordering). Authorship at the change level renders
  all of those moot.

- **Extending the current stall watchdog.** The proxy signal ("any
  inbound frame clears it") is the core issue; tuning the window or
  adding reset-escalation is patching at the wrong layer.

- **Adopting automerge-repo wholesale.** Possibly right in the long
  run, but a huge migration. Worth reading for design ideas but
  don't assume the conclusion is "swap it in." Our daemon is not a
  pure Automerge peer — it owns the execution queue, the blob store,
  pool lifecycle, and the runtime-agent subprocess dance. Fitting
  that into automerge-repo's model is a larger project.

## Starting points for a fresh implementer

Read, roughly in order:

1. `2026-04-17-widget-sync-stall-design.md` — the stall symptom and
   why the dual-write was brittle.
2. `2026-04-18-widget-sync-recovery-design.md` — what the proposed
   detection/recovery PR does and explicitly doesn't.
3. `crates/runtimed-wasm/src/lib.rs`, particularly the
   `FrameEvent::SyncError` variants and `reset_sync_state` — how
   recovery currently happens.
4. `packages/runtimed/src/sync-engine.ts` — the three-doc sync
   pipeline. Read critically; the structure here is what a new
   design needs to replace or simplify.
5. Automerge-core docs on actor IDs, `get_heads`, `get_changes_added`,
   and the sync protocol's `Message` / `SyncState`. The second two
   in particular are underused in our stack.
6. automerge-repo's `DocSynchronizer` — not to copy, but as
   reference for heads-based sync in practice.

Don't feel obligated to keep any part of PRs #1880 / #1881. #1881's
observability primitives (abort hung fetches, log subscriber errors)
and the recovery banner are small and useful as-is; the watchdog
itself is exactly what investigation (2) replaces, and the pending-
write filter is exactly what investigation (1) replaces.

## Recommended ordering

Fix the bug, then do the cleanup:

1. **(0)** — try/catch around the listener + reproduce. 30 min.
   Ships the actual fix and confirms the diagnosis in one motion.
2. **(0.5)** — verify Tauri unlisten cleanly releases. 15 min.
   Gates whether (3) is wiring or a larger Tauri workaround.
3. **(3)** — escalating recovery, conditional on (0) + (0.5).
   Mostly wiring of existing primitives; surfaces a real
   non-reload recovery path.
4. **(1)** — actor-ID echo refactor, independent. Start with the
   cost-profile prototype before committing. Parallelizable with
   the above or can wait.
5. **(2)** — deferred. Revisit only if (0) + (3) close the stall
   and a distinct liveness gap remains.

PR #1881's watchdog ceases to earn its surface area once (0) +
(3) land. Drop it; the watchdog was detecting a class that won't
occur with a protected listener + reconnect primitive.

## Success criteria

A successful set of changes, in order of importance:

1. **(0)** — the reproducer (`@interact` slider hammered with
   arrow keys) doesn't stall. If an exception shows up in the log,
   it's been hardened against. The specific failure mode behind
   the user's repro is closed.
2. **(3)** — if some future failure mode produces a similar wedge,
   the user recovers via a "Reconnect" action rather than
   reloading. The CRDT has no state to lose, so the action is
   safe by construction.
3. **(1)** — the widget write path is smaller than today and
   relies on the CRDT's native authorship information, not a
   reconstructed filter. `get_comm_authorship` (or whatever shape
   the prototype validates) is consumed from both the WASM binding
   and `runt mcp`.
4. Reload requirement is reserved for genuinely unrecoverable
   states (daemon dead, Tauri process gone) and surfaced with a
   one-click banner action.
