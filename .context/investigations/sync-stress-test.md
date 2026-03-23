# Sync Stress Test Investigation

> Handoff doc for the next agent session. Written at the end of a marathon
> session (4am → 8pm) that covered PR review, sync bug fixes, a JS library
> spike, a CRDT bridge fix, and chaos gremlin testing.

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

## Bugs Found (not yet fixed)

### Bug A: Unicode position mismatch during concurrent editing

**Severity**: Medium — causes text corruption (`print` → `pprint`,
`random` → `ranom`)

**Reproduction**: Two or more peers concurrently editing cells that
contain emoji (🐸, ⚡, etc.). The text CRDT produces duplicate or
missing characters near emoji boundaries.

**Theory**: CodeMirror counts positions in UTF-16 code units. Automerge
counts in Unicode scalar values (or bytes). Emoji like 🐸 are 1 scalar
value but 2 UTF-16 code units. When `splice_source` is called with a
CodeMirror position that's offset by the emoji width difference, the
splice lands at the wrong Automerge index.

**How to reproduce**:
```bash
# Start the daemon (dev or nightly)
cargo xtask dev

# In one terminal, open the app and create a cell with emoji:
#   # 🐸 Hello world
#   print("test")

# In another terminal, run the gremlin targeting the same notebook:
RUNTIMED_SOCKET_PATH=<socket> uv run python scripts/chaos-gremlin.py \
  --notebook-id <id> --gremlins 2 --rounds 20 --delay 0.1

# Check for corrupted source text in the notebook
```

**Investigation needed**:
1. Add logging to `splice_source` in the WASM binding that shows:
   - The cell_id, index, delete_count, text
   - The current WASM source length
   - Whether the source contains multi-byte characters
2. Compare CodeMirror's `fromA`/`toA` positions with the WASM source's
   character boundaries around emoji
3. Check if Automerge's `Text::splice` uses byte offsets, char offsets,
   or grapheme offsets
4. Check automerge-rs source: `rust/automerge/src/text.rs` or similar

**Possible fixes**:
- Convert CodeMirror UTF-16 offsets to Automerge scalar offsets before
  calling `splice_source`
- Or: have the WASM binding do the conversion internally
- Or: use `update_source` (full Myers diff) instead of `splice_source`
  for cells containing multi-byte characters

### Bug B: Daemon graceful shutdown under gremlin load

**Severity**: Low — daemon exits cleanly but the frontend loses connection

**Reproduction**: Run 3+ gremlins simultaneously against a notebook for
15+ rounds. The daemon sometimes exits with "Ok (graceful shutdown)" —
it received SIGTERM from somewhere.

**Investigation needed**:
- Is the nightly auto-updater sending SIGTERM?
- Is the app's reconnection logic triggering a daemon restart?
- Check if `runt daemon start` has a watchdog that restarts on error

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

## How to Stress Test

### Prerequisites

```bash
# Build everything
cargo xtask build

# For dev daemon: install local Python bindings
cd python/runtimed && maturin develop && cd ../..

# For nightly daemon: install the matching runtimed wheel
# Check the nightly version first:
runt-nightly status  # look for "Version: 2.0.2+436987f" or similar
# Install the matching alpha release:
uv pip install runtimed==2.0.3a202603230219  # match your nightly

# Verify the daemon is running
runt daemon status  # or runt-nightly status
```

### Test 1: Single gremlin (sanity check)

```bash
# Start the app
cargo xtask notebook

# In another terminal, find the notebook ID
RUNTIMED_SOCKET_PATH=<socket> uv run python -c "
from runtimed import Client
import asyncio
async def main():
    c = Client()
    for nb in await c.list_active_notebooks():
        print(nb)
asyncio.run(main())
"

# Run a single gremlin
uv run python scripts/chaos-gremlin.py --notebook-id <id> --rounds 10 --delay 0.5
```

**Expected**: All actions succeed. Frontend stays in sync. No crashes.

### Test 2: Multi-gremlin concurrent editing

```bash
uv run python scripts/chaos-gremlin.py --notebook-id <id> --gremlins 3 --rounds 20 --delay 0.2
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
# Target the nightly daemon specifically
# First ensure runtimed version matches the nightly:
#   uv pip install runtimed==2.0.3a202603230219
RUNTIMED_SOCKET_PATH=~/Library/Caches/runt-nightly/runtimed.sock \
  uv run python scripts/chaos-gremlin.py --notebook-id <id> --gremlins 3

# Gather diagnostics after the test
env -i HOME=$HOME PATH=/usr/local/bin:/usr/bin:/bin runt-nightly diagnostics
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
| `feat/chaos-gremlins` | This branch | Chaos gremlin script + this investigation doc |

## Session Timeline (for context)

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