# Sync-layer observability and recovery

**Status:** In progress on branch `chore/widget-sync-recovery`. Track B
of the widget-sync stall investigation; the larger CRDT-first
refactor (Track A) is deferred until we've validated this safety net
in production.

## Problem

Widget state and kernel status can stop updating silently. The
reproducer is a matplotlib `@interact` slider: the UI shows the thumb
moving, but the rendered plot freezes at a stale value. Reloading the
notebook window fixes everything instantly — the daemon has the
correct state, only the frontend→daemon pipeline has drifted.

Two failure modes combine:

1. **Sync-layer error with silent auto-recovery.** When the WASM
   sync layer fails to apply an inbound message
   (`receive_sync_message` returns `Err`), it already rebuilds the
   doc from its snapshot bytes, normalizes sync state, and returns a
   fresh sync message to restart negotiation. The `SyncEngine`
   forwards that recovery reply. Good — except it's invisible. A
   flapping daemon recovers on every frame, and the user only knows
   something's off when the UI starts looking stale.

2. **Silent flush drop.** `sendFrame` can fail (pipe buffer full,
   daemon slow) without the caller knowing the frame wasn't
   delivered. `sent_hashes` on the WASM handle advances anyway —
   from its point of view the message is gone — so subsequent
   flushes exclude whatever was in that message. Bloom-filter false
   positives in Automerge's sync protocol can hit this from a
   different angle: both peers conclude they're in sync when they
   aren't.

The frontend currently only surfaces a recovery signal when
`receive_sync_message` fails outright. The "frame never arrived" and
"heads drifted" cases leave no trace.

## Approach

Two narrow additions, both diagnostic/recovery — no architectural
change to the sync protocol or widget store.

### 1. `SyncEngine.syncErrors$` + `SyncRecoveryBanner`

Expose each auto-recovery event as a public observable:

```ts
interface SyncErrorEvent {
  doc: "notebook" | "runtime_state" | "pool_state";
  changed: boolean; // true if the doc advanced before the error
  ts: number;
}
```

The SyncEngine already has handlers for the three doc kinds'
`sync_error` frame events — each already sends the recovery reply.
We just `_syncErrors$.next(...)` from each handler. Zero change to
the recovery path.

App.tsx mounts `SyncRecoveryBanner` alongside `DaemonStatusBanner`.
The banner:

- Shows a transient "Sync recovered" line with the doc kind.
- Auto-dismisses after 5 s.
- Counts bursts while visible — another recovery bumps the count and
  resets the dismiss timer, so a flapping connection reads as
  "Recovered N times recently — connection may be unhealthy."
- Resets the counter on auto-dismiss so an isolated recovery an hour
  later isn't mis-labeled as the second in a burst.

### 2. Runtime-state stall watchdog

For the failure mode that produces no error to recover from, arm a
timer:

- When `flush_runtime_state_sync()` produces a sync message and we
  attempt to send it, start a 3 s timer.
- Any inbound runtime-state frame (applied or error) clears the
  timer. Either event proves the daemon is responsive.
- On timeout: call `handle.reset_sync_state()` to rewind
  `sent_hashes`, re-flush, and emit on `syncErrors$`.
- Reset the timer on each new flush (don't arm-once) so sustained
  traffic never misfires.
- `resetForBootstrap()` and `stop()` clear any armed timer so a
  stale one can't fire against a replaced handle.

The watchdog doesn't need a richer "convergence" signal —
`generate_runtime_state_sync_reply() === null` was too strict in
testing, tripping on healthy single interactions while the daemon
was still mid-reply. "Anything inbound clears it" is a simpler
correct predicate.

### 3. Supporting observability

Two small fixes for failure modes that would otherwise be invisible:

- **Abort hung blob fetches.** `inlineTextBlobs` is serialized on
  `commEmitQueue` to preserve comm emission order. A single fetch
  that hangs (connection established, body never completes) poisoned
  the queue — every subsequent widget update blocked forever. Added
  a 5 s per-attempt `AbortController` timeout; the retry path
  handles the transient cases we care about.
- **Log subscriber errors.** `frame-bus` and `runtime-state`
  dispatch loops used to `try { cb() } catch {}`. A throwing
  subscriber would just disappear from the logs while the rest of
  the pipeline kept ticking. Replaced the empty catch with
  `logger.error` so the next stall at least has a trace.

## What this is NOT

- **Not a rewrite of the widget-state write path.** The optimistic
  dual-write in `WidgetUpdateManager` (local store + debounced CRDT
  + echo suppression) stays untouched. An earlier iteration
  restructured it into a CRDT-first single-source-of-truth
  projection; that surfaced its own set of edge cases (per-tick
  mirror vs throttle window, peer-write collisions, bootstrap drain
  ordering, direct writers for anywidget `save_changes`). All of
  that is deferred — the investigation showed the stall is
  addressable at the sync layer, and once this is in place we'll
  know if the write-path refactor is actually needed.
- **Not persistence across reloads.** Reload still works as the
  user's recovery mechanism of last resort. The watchdog + banner
  just narrow the window where reload is the only option.
- **Not a protocol change.** All recovery uses existing WASM
  primitives (`reset_sync_state`, `cancel_last_runtime_state_flush`)
  that already shipped on `main`.

## Validation

- Unit tests on `SyncEngine` for the three `syncErrors$` variants,
  and for the watchdog's arm / clear-on-inbound / fire-on-timeout /
  clear-on-bootstrap behavior.
- Lint clean; existing 1018+ tests unaffected.
- Manual repro: matplotlib `@interact` + FloatSlider, hammer arrow
  keys. Before: silent stall until reload. After: banner fires on
  each WASM recovery; watchdog force-resets sync state on any silent
  drop.

## References

- `packages/runtimed/src/sync-engine.ts` — `syncErrors$`, watchdog,
  `projectComms`.
- `apps/notebook/src/components/SyncRecoveryBanner.tsx` — banner.
- `crates/runtimed-wasm/src/lib.rs` — `reset_sync_state`,
  `normalize_sync_state`, `FrameEvent::*SyncError` (all on `main`).
