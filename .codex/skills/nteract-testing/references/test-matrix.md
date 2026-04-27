# Test Matrix

## Fast mapping

- `crates/runtimed/**`, `crates/notebook-doc/**`, `crates/notebook-protocol/**`: start with `cargo test` in the relevant crate
- `crates/runtimed-wasm/**`, `apps/notebook/src/wasm/**`, notebook sync plumbing: start with `deno test --allow-read --allow-env --no-check crates/runtimed-wasm/tests/`
- `apps/notebook/src/**` or shared frontend code: start with `pnpm test:run`
- `python/runtimed/**`, `python/nteract/**`, `crates/runtimed-py/**`: start with targeted pytest or `uv run nteract`
- `e2e/**` or cross-window UX flows: use `cargo xtask e2e ...`

## Commands

Rust:

```bash
cargo test -p runtimed
cargo test -p notebook-doc
```

Deno WASM:

```bash
deno test --allow-read --allow-env --no-check crates/runtimed-wasm/tests/
```

Frontend:

```bash
pnpm test:run
```

Python unit:

```bash
python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_session_unit.py -v
```

Python integration:

```bash
RUNTIMED_SOCKET_PATH="$(
  RUNTIMED_DEV=1 RUNTIMED_WORKSPACE_PATH="$(pwd)" \
  ./target/debug/runt daemon status --json \
  | python3 -c 'import sys,json; print(json.load(sys.stdin)["socket_path"])'
)" \
python/runtimed/.venv/bin/python -m pytest python/runtimed/tests/test_daemon_integration.py -v
```

E2E:

```bash
cargo xtask e2e build
cargo xtask e2e test
# or a targeted fixture/spec pair:
cargo xtask e2e test-fixture crates/notebook/fixtures/audit-test/1-vanilla.ipynb e2e/specs/prewarmed-uv.spec.js
```

## Gotchas

- `crates/runtimed-wasm/tests/cross_impl_test.ts` touches `Deno.env` at module scope, so it needs `--allow-env` even before daemon-backed cases run.
- Some Deno tests type-check poorly in spite of working at runtime; `--no-check` is often the intended mode in this repo.
- Python integration failures are often socket-selection problems before they are logic regressions.
- E2E flows should go through `cargo xtask e2e ...`; it builds the webdriver-enabled binary, launches the app on port `4445`, and runs `pnpm test:e2e` with the right wiring.

## Sync backpressure regressions

When a change touches `crates/notebook-sync/src/sync_task.rs`,
`crates/notebook-sync/src/relay_task.rs`, request/response envelopes, or
MCP paths that issue parallel cell mutations, add or run tests with this shape:

- Tiny-buffer duplex unit test: issue concurrent `confirm_sync()` calls while a
  fake daemon sends interleaved `AutomergeSync`, `RuntimeStateSync`, broadcast,
  or `SessionControl` frames. The sync task must keep draining and every waiter
  must resolve without a socket close.
- Daemon integration test: create 8-10 cells through the same session with
  parallel mutation/confirm tasks, assert the original peer remains connected,
  then reconnect a fresh peer and verify every cell converged.
- Request-routing unit test: send overlapping daemon requests with request ids,
  return responses out of order, interleave a broadcast, and assert each caller
  receives its own response while broadcasts still reach request progress
  subscribers.

## Protocol upgrade compatibility

When a change touches the connection preamble, handshake routing, pool daemon
requests, or `PROTOCOL_VERSION`, keep a raw daemon integration test with this
shape:

- Send the magic preamble with a stable-era older protocol byte (for example,
  `2`), then a Pool handshake, then `{"type":"ping"}`.
- Assert the daemon returns `Pong` with the current `protocol_version` and a
  non-empty `daemon_version`. This is the launcher upgrade probe and must keep
  working across protocol bumps.
- If changing non-Pool version checks, also assert an old-version notebook sync
  handshake is rejected before any notebook/session state is created.
