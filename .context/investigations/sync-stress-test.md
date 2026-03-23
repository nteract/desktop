# Sync Stress Test Investigation

> Handoff doc spanning two agent sessions. Session 1 (4am → 8pm) covered
> PR review, sync bug fixes, a JS library spike, a CRDT bridge fix, and
> chaos gremlin testing. Session 2 fixed Bug A (unicode positions) and
> Bug B (daemon shutdown), found a new automerge PatchLogMismatch panic,
> and ran successful multi-gremlin stress tests against nightly.

## What We Built

### 1. Sync fixes (merged to main)

- **#1068** — Inline sync replies in `receive_frame()`. Eliminated the
  `flushSync` / `syncReply$` consumption race that caused sync head
  divergence on rapid Ctrl+Enter.
- **#1070** — Reply rollback on failed inline send (`cancel_last_flush`
  in the catch block). Codex review finding.
- **#1076** — CRDT bridge transaction ordering fix. Auto-close-tag
  (`</div>`) was applied BEFORE the `>` that triggered it because all
  changes were flattened and reversed. Fix: forward across transactions,
  reverse within each.
- **#1077** — Reverted accidental handle-reuse change from #1076 that
  may have caused text duplication during concurrent editing.

### 2. `runtimed` JS library spike (PR #1073, branch `spike/notebook-client`)

- `packages/runtimed/` — Transport-agnostic sync engine with RxJS
  coalesced streams (`cellChanges$`, `broadcasts$`, `presence$`,
  `runtimeState$`)
- `DirectTransport` — Test harness connecting two WASM handles
- `TauriTransport` — Tauri IPC adapter
- `useAutomergeNotebook` wired to SyncEngine
- 22 unit tests + 7 integration tests (daemon via Python)

### 3. Chaos gremlin (`scripts/chaos-gremlin.py`)

- Async, uses `runtimed.Client` API
- Weighted random actions: create, execute, delete, edit, move, clear,
  rapid-execute, read-and-verify
- Supports multiple concurrent gremlins (`--gremlins N`)
- Each gremlin connects independently to the daemon

## Bugs Found

### Bug A: Unicode position mismatch during concurrent editing — ✅ FIXED (PR #1078)

**Severity**: Medium — causes text corruption (`print` → `pprint`,
`random` → `ranom`)

**Root cause confirmed**: CodeMirror positions are UTF-16 code units.
Automerge 0.7.4 defaults to `UnicodeCodePoint` encoding (since no
`utf16-indexing` feature was enabled). Emoji like 🐸 are 1 code point
but 2 UTF-16 code units, so every emoji before a splice shifted the
position by 1.

**Fix (PR #1078)**:
- Automerge 0.7.4 has `TextEncoding::Utf16CodeUnit` — we just weren't
  using it. Added `AutoCommit::new_with_encoding(Utf16CodeUnit)` to all
  WASM `NotebookHandle` construction paths (`new`, `create_empty`,
  `create_empty_with_actor`, `load`).
- Added encoding-aware constructors to `NotebookDoc` (`new_with_encoding`,
  `empty_with_encoding`, `load_with_encoding`), re-exported `TextEncoding`.
- Daemon continues using `UnicodeCodePoint` (correct for Python string
  indices). Encoding is a local API interpretation — does not affect the
  wire format, so peers with different encodings sync correctly.
- Fixed secondary bug: `append_source` used `String::len()` (byte count)
  instead of automerge's encoding-aware `length()`, which was wrong for
  non-ASCII text.
- 5 new regression tests for splice positions after emoji (including
  cross-peer sync with surrogate pairs).

**Key insight**: `TextEncoding` in automerge 0.7 is purely a client-side
position interpretation. The underlying CRDT ops are stored at the
op-graph level. Two peers with different encodings sync correctly.

### Bug B: Daemon graceful shutdown under gremlin load — ✅ FIXED

**Severity**: Low (was user error, not a daemon bug)

**Root cause**: The chaos gremlin script called `client.shutdown()` in
the notebook discovery phase and the final state check. `Client.shutdown()`
sends a "shut down the daemon" RPC — it literally asks the daemon to exit.
The daemon wasn't crashing; the gremlin was killing it before the test started.

**Fix**: Removed both `client.shutdown()` calls from `chaos-gremlin.py`.
The Python `Client`'s resources are cleaned up by the garbage collector.
After the fix, the nightly daemon survived a full 3-gremlin, 20-round
stress test without issues.

**Note for the Python API**: `Client` should probably have a `close()`
method that disconnects without shutting down the daemon. Currently the
only cleanup method is `shutdown()` which is destructive.

### Bug C: Frontend desync after gremlin flood

**Severity**: Medium — frontend shows stale/wrong content, needs reload

**Reproduction**: Same as Bug A. After gremlins finish, the frontend may
show fewer cells than the daemon has, or show cells with wrong content.
Reloading fixes it.

**Theory**: The coalesced materialization pipeline (32ms bufferTime) may
drop batches when overwhelmed, or the targeted per-cell update path may
miss cells that were added/removed during the batch window.

**Investigation needed**:
1. Add a "sync health check" button/command that compares the frontend's
   cell store with the WASM handle's cells and reports mismatches
2. Log the `cellChanges$` batch sizes and whether `needsFull` is true
3. Check if structural changes (add/remove) during a coalescing window
   are properly detected

### Bug D: Automerge PatchLogMismatch panic under concurrent gremlin access (NEW)

**Severity**: High — panics the tokio runtime thread, poisons the Mutex,
and renders the affected session permanently unusable.

**Reproduction**: 3 concurrent gremlins editing the same notebook.
Gremlin-1 hit the panic on round 2, after which every operation failed
with "Document lock poisoned".

**Error**:
```
thread 'tokio-runtime-worker' panicked at automerge-0.7.4/src/op_set2/change/batch.rs:817:47:
called `Result::unwrap()` on an `Err` value: PatchLogMismatch
```

**Context**: This is inside automerge's internal batch processing.
The panic occurs in the Python binding's automerge document (client-side,
not daemon-side). Each gremlin creates its own `Client` and `Notebook`,
so sessions should be independent — but they share the same tokio runtime
and something about concurrent sync operations triggers the panic.

**Investigation needed**:
1. Get a full backtrace (`RUST_BACKTRACE=1`)
2. Check if the session's `Arc<Mutex<SessionState>>` allows overlapping
   transactions when multiple async tasks read/write concurrently
3. Check if `PatchLogMismatch` is a known automerge issue
4. May need to upgrade automerge or add retry/recovery around the panic

### Bug E: append_source "index out of bounds" for emoji cells (daemon-side)

**Severity**: Medium — the `append()` API silently fails for cells with
emoji when using the nightly daemon (which lacks the `append_source` fix).

**Root cause**: Same as the secondary fix in Bug A. `append_source` used
`self.doc.text(&source_id)?.len()` (Rust String byte count = UTF-8 bytes)
but `splice_text` interprets the index using the document's `TextEncoding`
(default `UnicodeCodePoint` = char count). For "# 🐸 Gremlin-1" the byte
count is 17 but the code point count is 14, so the splice index overshoots.

**Status**: Fixed in PR #1078 (uses `self.doc.length(&source_id)` which
is encoding-aware). Will be resolved once the nightly picks up the fix.

## How to Stress Test

### Prerequisites

```bash
# Build everything
cargo xtask build

# For dev daemon: install local Python bindings
cd python/runtimed && maturin develop && cd ../..

# For nightly daemon: the repo .venv already has runtimed 2.0.2
# built from source via maturin — this matches the nightly.
# Use .venv/bin/python (not uv run) to ensure the right bindings.

# Verify the daemon is running (MUST use env -i to avoid dev vars)
env -i HOME=$HOME PATH=/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin \
  runt-nightly daemon status
```

**Important**: Always use `env -i` when interacting with the nightly
daemon from a dev worktree. Without it, `RUNTIMED_DEV=1` and
`RUNTIMED_WORKSPACE_PATH` leak from the shell and you'll hit the dev
daemon instead of nightly.

### Test 1: Single gremlin (sanity check)

```bash
# Target the nightly daemon explicitly with --socket
NIGHTLY_SOCK=~/Library/Caches/runt-nightly/runtimed.sock

# Run a single gremlin (auto-discovers first active notebook or creates one)
.venv/bin/python scripts/chaos-gremlin.py --socket $NIGHTLY_SOCK --rounds 10 --delay 0.5
```

**Expected**: All actions succeed. Frontend stays in sync. No crashes.

### Test 2: Multi-gremlin concurrent editing

```bash
.venv/bin/python scripts/chaos-gremlin.py --socket $NIGHTLY_SOCK --gremlins 3 --rounds 20 --delay 0.2
```

**Expected**: Some cell errors (bad code, markdown execute attempts).
Frontend may lag but should eventually converge. No daemon crashes.

**Watch for**: Text corruption (characters appearing/disappearing),
cells missing from the frontend, CodeMirror crashes in the console.

### Test 3: Rapid Ctrl+Enter stress

While gremlins are running, rapidly hold Ctrl+Enter on a cell in the
app. This tests the sync divergence fix (#1068).

**Expected**: Outputs update correctly. No "stale frontend" state.
The sync should converge even under load.

### Test 4: Gremlin + human concurrent editing

While gremlins are running, actively type in cells in the app. Create
cells, delete cells, move cells. This is the most realistic stress test.

**Watch for**: Text corruption near emoji, cursor jumps, cells
disappearing or duplicating.

### Test 5: Nightly build testing

```bash
# Use --socket to target nightly directly (no env var leakage risk)
.venv/bin/python scripts/chaos-gremlin.py \
  --socket ~/Library/Caches/runt-nightly/runtimed.sock --gremlins 3

# Gather diagnostics after the test
env -i HOME=$HOME PATH=/usr/local/bin:/usr/bin:/bin:/opt/homebrew/bin \
  runt-nightly diagnostics
```

The diagnostics archive contains `runtimed.log` (daemon), `notebook.log`
(Tauri app), `daemon-status.json`, and `system-info.json`.

## Gathering Logs

### Daemon logs

```bash
# Dev daemon
tail -f target/debug/runtimed.log

# Nightly daemon (after diagnostics)
tar xzf ~/Desktop/runt-diagnostics-*.tar.gz -C /tmp/diag
cat /tmp/diag/runtimed.log
```

Look for:
- `Cell error (stop-on-error)` — expected for bad code
- `WARN` or `ERROR` lines — unexpected issues
- `Daemon exited` — check if graceful or crash

### Frontend logs

Open the WebKit inspector (Cmd+Option+I in the app) → Console tab.

Key log prefixes:
- `[SyncEngine]` — sync lifecycle (initial sync, retries)
- `[crdt-bridge]` — CodeMirror ↔ WASM splice operations
- `[frame-pipeline]` — inline reply sending, rollback
- `[automerge-notebook]` — materialization, bootstrap
- `[daemon-kernel]` — execution, kernel status
- `CodeMirror plugin crashed` — **this is the bug we're hunting**

### Diagnostic instrumentation

To enable CRDT bridge logging (currently removed, re-add if needed):

```typescript
// In crdt-editor-bridge.ts, inside update():
console.warn("[crdt-bridge] OUTBOUND TX", {
  cellId: cellId.slice(0, 8),
  userEvent: tr.annotation(Transaction.userEvent) ?? "(none)",
  editorDocLen: vu.state.doc.length,
  wasmSourceLen: handle?.get_cell_source(cellId)?.length ?? "null",
  changes,
});
```

## Key Files

| File | What it does |
|------|-------------|
| `scripts/chaos-gremlin.py` | Chaos gremlin script |
| `apps/notebook/src/lib/crdt-editor-bridge.ts` | CodeMirror ↔ WASM bridge (Bug A lives here) |
| `apps/notebook/src/lib/frame-pipeline.ts` | Inbound frame processing + inline replies |
| `apps/notebook/src/hooks/useAutomergeNotebook.ts` | Notebook lifecycle + sync |
| `packages/runtimed/src/sync-engine.ts` | SyncEngine (spike branch) |
| `packages/runtimed/src/direct-transport.ts` | Test transport (spike branch) |
| `crates/runtimed-wasm/src/lib.rs` | WASM bindings (splice_source, receive_frame, flush_local_changes) |
| `crates/runtimed-wasm/tests/deno_smoke_test.ts` | WASM sync tests (45 tests) |
| `crates/runtimed-wasm/tests/splice_source_test.ts` | Splice tests (66 tests) |

## Branch Map

| Branch | Status | What's on it |
|--------|--------|-------------|
| `main` | ✅ | All merged fixes (#1068, #1070, #1076, #1077) |
| `spike/notebook-client` | Draft PR #1073 | `runtimed` JS library + SyncEngine + TauriTransport |
| `feat/chaos-gremlins` | PR #1078 | Bug A fix (UTF-16 encoding), Bug B fix (shutdown), chaos gremlin, this doc |

## Session Timeline (for context)

### Session 1

1. 4am — Voice review of execution handle spike (#1052) with nicole voice
2. 5am — Designed execution log architecture (intents → dropped, keep request/response)
3. 7am — Updated issues, milestone scoping, cross-references
4. 8am — Traced frame pipeline architecture (Tauri relay, WASM demux)
5. 9am — Found sync divergence bug (#1067), filed with root cause analysis
6. 10am — Built the fix: inline sync replies in receive_frame (#1068)
7. 11am — Codex review found reply rollback gap → #1070
8. 12pm — Started `runtimed` JS library spike (SyncEngine, DirectTransport)
9. 1pm — TauriTransport, wired useAutomergeNotebook to SyncEngine
10. 2pm — Debugged React strict mode issues (handle lifecycle, initial sync detection)
11. 3pm — Fixed targeted materialization (don't overwrite CodeMirror source)
12. 4pm — Added RxJS coalesced streams (cellChanges$, broadcasts$, etc.)
13. 5pm — Found %%html magic cell crash on main → investigated with instrumentation
14. 6pm — Root cause: transaction ordering in CRDT bridge → #1076
15. 7pm — Chaos gremlins: 3 concurrent gremlins killed the nightly in 6 seconds
16. 8pm — This handoff doc

### Session 2

17. 8pm — Read investigation doc, oriented on nightly daemon (2.0.2+79c2797)
18. 8:10pm — Single gremlin test passed (10 rounds, 0 errors)
19. 8:12pm — Daemon died on multi-gremlin test → investigated
20. 8:15pm — Debugged launchd crash-loop (bootout/bootstrap, env var leakage)
21. 8:20pm — Dug into automerge 0.7.4 source: found `TextEncoding` enum with
    `Utf16CodeUnit` support — confirmed Bug A root cause
22. 8:30pm — Implemented fix: encoding-aware constructors in `notebook-doc`,
    `Utf16CodeUnit` in WASM binding, `append_source` length fix
23. 8:40pm — 5 new regression tests, all 344 tests pass, PR #1078 opened
24. 8:45pm — Found Bug B root cause: `client.shutdown()` in gremlin script
25. 8:50pm — Fixed Bug B, re-ran 3-gremlin test: daemon survived, 7.3s elapsed
26. 8:55pm — Found Bug D: `PatchLogMismatch` panic in automerge under
    concurrent gremlin access, poisons the session Mutex
27. 9pm — Found Bug E: `append_source` index out of bounds on nightly
    (same root cause as Bug A secondary fix, nightly doesn't have it yet)