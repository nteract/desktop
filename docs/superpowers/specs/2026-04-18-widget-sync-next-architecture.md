# Widget sync — next architecture (proposal)

**Status:** Design proposal for a fresh investigation. The two
existing PRs (#1880, #1881) remain available as reference, but this
note frames the space for someone starting clean rather than
continuing to iterate on either of them.

## Problem

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
for what the current shipping PR addresses.

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

### PR #1881 (shipping): detection + recovery safety net

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
does **not** fully recover from it — `reset_sync_state()` rewinds
`sent_hashes` on the client, but the user's evidence shows the
watchdog firing every 3s in a loop, which means the underlying
transport is wedged at a level client-side reset can't repair. The
banner narrows the user's gap from "is it stuck?" to "yes, stuck —
reload." Real recovery is out of scope for that PR.

## Where we've been fighting Automerge

Honest assessment of why the above composes poorly:

1. **Reinvented authorship detection.** Automerge knows which actor
   wrote which op. The entire `isEchoOfPendingWrite` / pending-value
   TTL / consume-on-match machinery exists because we look at merged
   state and try to reconstruct "was this my write or a peer's?"
   The answer is recorded at the change level — we just aren't
   asking.

2. **Reinvented heads gossip with a timeout proxy.** Automerge's
   sync protocol is structured around `heads`. Silent-drop detection
   in that model is: "I announced my heads moved to X; your next
   heads report still doesn't reach X → you didn't get my change."
   Our watchdog times out on "any inbound frame" — a proxy signal
   that false-positives in converged-idle state (as codex round 2
   noted and the real-world log shows by firing every 3s without
   recovery converging).

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

Three directions, orthogonal to each other. Any can be pursued
independently; none require the others.

### 1. Actor-ID-based echo detection

**Thesis:** the widget write path's complexity is entirely in
reconstructing authorship. Automerge has actor IDs. Use them.

Sketch:

- Each frontend window gets a stable actor ID (we effectively already
  have one — it's baked into every Automerge op it generates).
- When `commChanges$` emits an updated comm, inspect the change
  metadata for each affected key: was the last op from *our* actor
  ID?
- If yes, it's our echo — the local store already has this value
  (we just wrote it). Skip.
- If no, it's an authoritative remote write (daemon, peer, kernel
  validator). Apply.
- Delete: `pendingKeys`, `markPending`, `isEchoOfPendingWrite`,
  `recordLocalWrite`, `diffResolvedState`'s pending hook, the whole
  value-history TTL + consume-on-match dance.
- Keep: per-tick local store mirror for UI responsiveness. Keep:
  throttled outbound CRDT writes for flood control.

**Where to look:**
- `crates/runtimed-wasm/src/lib.rs` — currently exposes `resolve_comm_state`
  which produces merged output state. Would need a richer emission
  that carries per-key actor info, or a separate query
  ("what's the last actor for key K on comm C?").
- `packages/runtimed/src/comm-diff.ts` — today diffs full-comm state
  JSON. With actor info per key, this becomes a principled diff.
- `packages/runtimed/src/sync-engine.ts` — `projectComms` is where
  the actor check would land in the emission path.

**Why it's worth it:**
- Removes the pending-write filter, which is the machinery that kept
  growing corners through PR #1880's review rounds.
- Handles collaborative peers correctly by construction. Two peers
  writing the same value concurrently is no longer ambiguous — their
  actor IDs differ.
- No TTL. No microtask ordering. No "first match consumed." Just
  a direct authorship check.

**Risk to investigate:** how expensive is it to query the most-recent
actor for a key? Automerge-core has this; the WASM surface may need
extension. If it's cheap enough to do per-emission, the whole design
simplifies. If not, we'd need a subscription-style API.

### 2. Heads-gossip for stall detection

**Thesis:** the stall watchdog should be structured around actual
sync progress, not a timeout on proxy traffic.

Sketch:

- Both peers (frontend, daemon) periodically publish their current
  heads for each doc — notebook, runtime-state, pool. Cheap: a
  heartbeat frame every 1–2 s carrying three hashes.
- Frontend tracks: "last I announced my heads moved to X"
  + "last I saw peer's heads were Y."
- Stall detection: when we announce heads moving to X, if the
  peer's next heartbeat still shows their heads not including X,
  the change we sent was dropped.
- Recovery is then targeted: identify the change the peer is missing,
  re-send those specific changes (`Automerge.getChangesAdded(theirs, ours)`).
- No per-doc watchdog timer. No "any inbound frame" proxy. The
  signal is the thing we actually care about: "did the peer receive
  what we sent."

**Where to look:**
- `crates/runtimed-wasm/src/lib.rs` — would need to expose
  `get_heads()` and `get_changes_added(heads)` per doc.
- `crates/runtimed/src/notebook_sync_server.rs` — daemon-side would
  symmetrically announce heads.
- `packages/runtimed/src/transport.ts` — new frame type
  `HEADS_ANNOUNCE` with `{doc_kind, heads[]}`.
- automerge-repo's `DocSynchronizer` is the closest prior art for
  this model (heads-based with retry). Worth reading even if we
  don't adopt the library wholesale.

**Why it's worth it:**
- Detects the exact failure mode we care about: "sent but not
  received." Does not false-positive on converged-idle (the peer's
  heads match; no stall).
- Works uniformly for all three docs. No doc-specific watchdog code.
- Recovery can be surgical — re-send the specific changes the peer
  is missing — instead of the sledgehammer `reset_sync_state()`.
- Detects the class of bug `reset_sync_state()` can't fix: a wedged
  transport that accepts our frames but doesn't deliver them. Heads
  gossip would show the daemon's heads flatlining while ours advance.

**Risk to investigate:** heartbeat bandwidth. Three hashes every 1–2s
per window is small but non-zero. Could be adaptive — faster when
there's in-flight traffic, slow when idle.

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

The third level is the new primitive. It's the missing piece: a
webview-scoped reconnect that doesn't require the user to reload
the whole window. Implementation would look roughly like:

- Unlisten the current `notebook:frame` handler.
- Call a new Tauri command — say `resubscribe_to_daemon_frames` —
  that tears down and rebuilds the per-window frame subscription on
  the Rust side.
- Re-register the listener with a fresh callback.
- Ask the daemon for its current heads for all three docs, compare
  to ours, and do a fresh sync round.

**Where to look:**
- `apps/notebook/src/lib/tauri-transport.ts` — where `listen` is
  called; needs an unlisten + relisten primitive.
- `crates/notebook/src/lib.rs` — the Tauri command side; would need
  a new command symmetric to `reconnect_to_daemon` but scoped per
  window / per subscription rather than per daemon connection.
- `packages/runtimed/src/sync-engine.ts` — `resetForBootstrap`
  already exists; this is the same shape of reset, driven by a new
  signal.

**Why it's worth it:**
- Eliminates the reload requirement for the failure mode we know
  exists (listener death on a still-healthy daemon).
- Works with (2) — heads gossip gives the unambiguous "inbound is
  dead" trigger that moves us up the escalation hierarchy.
- The watchdog in PR #1881 becomes level 2 of the hierarchy rather
  than the terminal recovery.

**Risk to investigate:** does Tauri actually support cleanly tearing
down and rebuilding a `listen` handler for the same event name on
the same window? If there's a hidden reference cycle or the drop
doesn't fully release the underlying subscription, the "reconnect"
would silently layer a new listener on top of the dead one.

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
2. `2026-04-18-widget-sync-recovery-design.md` — what the shipping
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

## Success criteria

A new design is worth landing if:

1. The reproducer (`@interact` slider hammered with arrow keys)
   doesn't stall under nominal transport conditions.
2. When the stall *does* happen (listener dead, pipe wedged),
   heads-gossip detects it within ~1s and the reconnect primitive
   recovers without the user reloading. The banner tells them what
   happened but doesn't require action.
3. Only genuinely unrecoverable transport failures (daemon dead,
   Tauri process gone) escalate to "Reload required" — and that
   state is reachable with a one-click banner action.
4. The widget write path is fewer lines than today and has no
   per-key bookkeeping that requires TTL or consume-on-match.
5. Silent drops can't regress without a heads-gossip divergence
   showing up in logs.
