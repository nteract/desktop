# Widget sync: diagnose, then redesign

**Status:** Proposal. Two existing PRs are open as reference
(#1880, #1881); neither is merged. Don't treat them as current
state — main has no watchdog, no `SyncRecoveryBanner`, no
`syncErrors$` observable. What main has is a 15-line
`shouldSuppressEcho` filter in `WidgetUpdateManager` and a raw
`listen("notebook:frame")` handler with no exception trapping.

A previous version of this doc led with "the fix is five lines"
based on a listener-death theory. That theory is contradicted by
Tauri 2.10.3's source (`scripts/core.js:39` — `runCallback` does
not unregister on throw). The five-line `try / catch` is still
worth landing as defensive hygiene (merged as #1883 under that
framing), but it is not the cure. The real mechanism of the
stall is still unknown; the honest first step is instrumentation.

## What we know, what we don't

Under rapid widget interaction (matplotlib `@interact`
`FloatSlider` driven with arrow keys), the notebook app
occasionally stops reflecting kernel-side changes. Slider thumb
moves in the UI; plot underneath freezes mid-drag. Reloading
fixes it instantly.

**Known:**

- Daemon stays responsive — other windows opened during the stall
  sync normally.
- The user's webview continues to run (the #1881 watchdog logs
  fire every 3s; JS is not deadlocked).
- `WidgetUpdateManager`'s optimistic write path keeps the UI
  responsive (slider moves) while the CRDT-sourced projection
  stops advancing.
- Reload fully recovers — something the reload rebuilds is what's
  broken.

**Not known:**

- Whether the break is *outbound* (the flush appears to succeed
  but the daemon never processed it) or *inbound* (the daemon's
  reply is emitted but never reaches our listener).
- Whether it's in our JS code, in Tauri's event pump, in WebKit,
  or in the WASM handle's sync state.
- What the throttle/flood load is doing to the Tauri IPC queue.

## Start by measuring

Before any architectural change, instrument the pipe so the next
repro tells us which half is broken:

- **Count inbound `notebook:frame` events** at the earliest point
  inside the JS callback, plus the frame type byte. If the
  counter freezes at stall time, the break is somewhere between
  Tauri's Rust side and our JS listener — a class of problem
  sync-engine changes cannot address.
- **Count outbound `invoke("send_frame", ...)` calls** and their
  promise resolutions. If the invoke resolves but the daemon log
  never shows the frame arriving, the break is between JS and
  Rust on the outbound side.
- **Log on the daemon side** (`notebook_sync_server.rs`) whether
  it's still receiving from this specific window during the
  stall, and what it sends back. Pairs with the JS counters to
  localize the break.
- **#1883 (merged)** — `try / catch` + `logger.error` around the
  `notebook:frame` callback. If an exception fires inside the
  listener at stall time, we'll see it in `notebook.log`.

Concrete next step: add a small counter-instrumentation PR
(counts + frame-type logs on both sides), reproduce the stall,
read the logs, and update this proposal based on what the
measurements say.

## Provisional plan, pending the measurements

If the measurements point at **inbound** being dead (frames
arrive at Tauri Rust but not the JS callback), some variant of
investigation (3) applies — but see its honest API-surface
caveats below.

If the measurements point at **outbound** being dead (JS
invokes resolve but the daemon doesn't see the frame), the fix
is in Rust-side frame handling, not anywhere in this doc's
current investigations.

If the measurements point at **the WASM handle losing state**
(sync messages arrive but don't update the doc), that's a
different category altogether — probably a Rust-side WASM bug
to reproduce in a harness.

Investigation (1) — actor-ID echo detection — stands on its own
as a code smell to fix regardless of the stall. Do not couple it
to the diagnosis.

Investigation (2) — heads-gossip — stays deferred. The dead-pipe
critique holds for any variant where the gossip rides the same
broken channel.

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

Four items, in rough order of doing. Investigation 0 is real
instrumentation that has to run before the rest. Investigation 1
is separable — pursue it for its own sake regardless.

### 0. Instrument and localize the stall

The stall's mechanism is still an open question. Before
proposing architectural changes, run a measurement pass:

1. **`try / catch` + `logger.error` around the
   `notebook:frame` callback** (merged as #1883). Not a
   recovery mechanism — Tauri doesn't actually unregister
   listeners on throw — but if an exception is thrown at stall
   time, this is how we'd find out.
2. **Counter + frame-type log on inbound.** Record how many
   frames the JS callback sees and of what type. The simplest
   stall signature is "counter freezes" — that tells us the
   break is above our JS, in Tauri's pump or below.
3. **Counter + resolution tracking on outbound.** Record each
   `invoke("send_frame", ...)` and whether it resolves.
   Matches against daemon-side logs to localize.
4. **Daemon-side correlating log** — emit per-window frame
   received/sent counts in `notebook_sync_server.rs`. Diff
   against the JS counters to confirm which side loses frames.

None of this is a fix. All of it is a prerequisite for
proposing one, and it's cheap compared to landing architectural
changes for a misdiagnosed cause.

**Where to look:**
- `apps/notebook/src/lib/tauri-transport.ts` — both the `listen`
  callback and the `sendFrame` invoke site.
- `apps/notebook/src/lib/logger.ts` — already writes to
  `notebook.log` via the Tauri log plugin.
- `crates/runtimed/src/notebook_sync_server.rs` — daemon-side
  per-frame logging.

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

### 1. Actor-ID-based echo detection (design spike, not a drop-in)

**Justification upfront:** this is a refactor, not a bug fix.
Today `shouldSuppressEcho` is ~15 lines and works. The case for
replacing it is that `#1880`'s review history showed the current
filter grows corners under load pressure, and building on
Automerge's native authorship would be more honest than
reconstructing it from merged JSON.

**The precedent, scoped honestly:**
`apps/notebook/src/hooks/useCrdtBridge.tsx:154` does

```ts
if (attr.actors.length === 1 && attr.actors[0] === localActor) {
  continue;
}
```

— which is "skip this patch if the only actor in the delta was
the local one." That works for text-edit echoes because the
`attributions` payload is already a per-patch list. But look at
what produces that payload (`crates/runtimed-wasm/src/lib.rs:1707`):

```rust
let new_changes = doc.get_changes(before);
let actors: Vec<String> = new_changes
    .iter()
    .map(|c| notebook_doc::actor_label_from_id(c.actor_id()))
    .collect::<BTreeSet<_>>()
    .into_iter()
    .collect();
```

It collects every actor that appears in the delta as a set, and
attaches the same set to every emitted patch. Good enough for
"only local actor wrote the delta," not good enough for "which
actor last wrote key K." The comm widget case needs the latter —
a collaborative write of one widget value must not be suppressed
because a different widget was written locally in the same delta.

So this investigation is a **design spike**, not an extension of
an existing primitive:

- We need a new query shape: per-comm-key, most-recent actor, for
  the keys in a projected delta. Existing attribution doesn't
  compute this.
- `automerge` 0.8 does have `get_changes_added`
  (`autocommit.rs:590`) to scope the walk to a delta rather than
  the full history. But filtering ops to "which ones touched
  comm K's field V" still requires walking ops inside those
  changes. Cost profile is open.
- The answer might not be "one query per key per emission." It
  could be a batched `AuthorshipSnapshot` produced once per
  projection cycle, or a persistent authorship index maintained
  on write. Design question, not an implementation detail.

**Prototype before committing:**

Write a ~50-line Rust prototype that, given a comms Automerge
doc with 10k changes and a hammered-slider workload, measures:

- Cost of `get_changes_added` to scope a delta.
- Cost of walking ops-in-changes to find "last actor for key K."
- Cost of a batched snapshot vs. per-key lookup.

These numbers determine the API shape, not just "is it fast
enough." If per-key lookup is millisecond-scale, the whole
direction needs an indexed-authorship approach, which is a
larger Rust crate change.

**Where to look:**
- `apps/notebook/src/hooks/useCrdtBridge.tsx:150-156` — the
  existing filter pattern (scoped to "was the delta all-local?",
  not per-key).
- `crates/runtimed-wasm/src/lib.rs:1707-1713` — current
  attribution payload construction. Shows what's already computed
  and what's missing.
- `crates/notebook-doc/` — would likely get a new
  `comm_authorship` API.
- `crates/runtimed-wasm/src/lib.rs` — needs a WASM export
  matching whatever shape the prototype validates.
- `packages/runtimed/src/comm-diff.ts:146-191` — **structural
  change**. Today diffs whole-comm state JSON via
  `JSON.stringify` + string compare. Per-key actor checks require
  a per-key diff, so the emission shape of `commChanges$`
  changes. Audit every subscriber before committing:
  - `apps/notebook/src/App.tsx` widget-store subscriber
  - `packages/runtimed/tests/` harnesses using the observable
  - `src/isolated-renderer/widget-bridge-client.ts` downstream
    of `commChanges$` in the iframe bridge
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
- Cost of "most-recent actor for key K" per emission. The
  attribution pattern walks changes for a delta but doesn't do
  the per-key reduction. The prototype above is how we'd find
  out whether that reduction is cheap or expensive.
- The comm-diff restructure is not purely additive; it's
  structural. Budget for touching every downstream subscriber of
  `commChanges$`.

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

### 3. A real reconnect path (conditional on what (0) finds)

**Thesis:** If the instrumentation in (0) shows that inbound
frames stop reaching our JS callback on a still-healthy daemon,
we need a recovery path that rebuilds the listener — `Cmd+R` is
the only one today. This investigation is the shape of that
path, not a commitment to build it before we know the failure
mode.

**Honest API surface required.** An earlier version of this doc
said the reconnect was "mostly wiring" on top of existing
`disconnect` / `start` / `resetForBootstrap` primitives. That
undercounts the actual work. Today the transport is constructed
once inside the `useAutomergeNotebook` React effect and passed to
`SyncEngine` at construction time (`packages/runtimed/src/sync-engine.ts`,
`apps/notebook/src/hooks/useAutomergeNotebook.ts`). There is no
current API for "swap in a fresh transport and reuse the engine"
— engine and transport are paired for the engine's lifetime. So
a real reconnect needs one of:

- **Transport swap API.** `SyncEngine.reattach(newTransport)` or
  similar. Hooks the new transport's frame listener into the
  existing pipeline without rebuilding the whole engine. Requires
  thinking about what in-flight state (in-flight flush promises,
  watchdog timers) survives the swap.
- **Tear-down + rebuild.** Destroy both the transport and the
  engine; create a new pair; keep the WASM handle (which holds
  the CRDT). Needs React-level coordination to avoid tearing
  down React state that depends on the engine.
- **Daemon-scoped reconnect only.** Use the existing
  `reconnect_to_daemon` path (which already exists for
  `daemon:ready`) and accept that per-window-listener failures
  fall through to reload. Cheapest; least recovery.

Which one depends on (0)'s findings and on (0.5)'s Tauri-unlisten
verification.

**Recovery hierarchy it would slot into:**

| Signal | Recovery | Cost |
|---|---|---|
| WASM `receive_sync_message` error | WASM rebuilds doc; engine forwards recovery reply | cheap; already exists |
| Sync state stale (bloom / sent_hashes) | `reset_sync_state()`; re-flush | cheap; currently the only recovery |
| Channel-level stall | **Whatever (3) turns out to be** — see options above | moderate; new API surface |
| Channel unrecoverable | "Reload" action in banner | last resort; no state loss |

**Where to look:**
- `apps/notebook/src/hooks/useAutomergeNotebook.ts` — where the
  transport + engine pair is created today. Start here to see
  what lifecycle currently owns it.
- `packages/runtimed/src/sync-engine.ts:339, 381` — the engine's
  transport reference, fixed at construction.
- `apps/notebook/src/lib/tauri-transport.ts` — the listener
  registration that (0.5) needs to verify releases cleanly.

**Don't build this until (0) points at it.** If the
instrumentation shows outbound is the broken half, a listener
reconnect wouldn't help.

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

Diagnose before architecting:

1. **(0) instrumentation** — counters on both sides of the pipe;
   reproduce; read the logs. Nothing below is worth building
   until this localizes the break.
2. Then, based on what (0) shows:
   - **Inbound dead**: (0.5) verifies Tauri unlisten behavior,
     then (3) designs a reconnect with the right API surface.
   - **Outbound dead**: the fix is in Rust-side frame handling;
     none of the investigations here apply directly.
   - **WASM state corruption**: Rust-side harness repro; new
     work not captured by this proposal.
3. **(1) actor-ID echo** — separable from the stall. Start with
   the cost-profile prototype before committing. Land on its own
   merits whenever there's bandwidth for it.
4. **(2) heads-gossip** — deferred. Revisit only if a liveness
   gap remains after whatever (0) leads to lands.

PR #1881's watchdog may or may not earn its keep depending on
(0)'s findings. Don't decide until we know what it's actually
watching for.

## Success criteria

A successful set of changes, in order of importance:

1. The stall class is actually diagnosed. Whatever (0) measures
   tells us what's broken — outbound, inbound, or state
   corruption — and the fix targets that specifically instead
   of patching adjacent layers.
2. Whatever is broken is fixed. "Fixed" means the user can
   reproduce the repro without hitting the stall, not just
   that a banner tells them about it.
3. If the fix involves a reconnect primitive, it lives at the
   right API layer (engine/transport coupling thought through,
   not bolted on). If it doesn't, we don't build one.
4. **(1)**, independent — the widget write path is smaller than
   today and relies on the CRDT's native authorship information
   rather than a reconstructed filter. Consumed symmetrically by
   WASM and `runt mcp`.
