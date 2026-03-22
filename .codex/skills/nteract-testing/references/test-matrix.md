# Test Matrix

## Fast mapping

- `crates/runtimed/**`, `crates/notebook-doc/**`, `crates/notebook-protocol/**`: start with `cargo test` in the relevant crate
- `crates/runtimed-wasm/**`, `apps/notebook/src/wasm/**`, notebook sync plumbing: start with `deno test --allow-read --allow-env --no-check crates/runtimed-wasm/tests/`
- `apps/notebook/src/**` or shared frontend code: start with `pnpm test:run`
- `python/runtimed/**`, `python/nteract/**`, `crates/runtimed-py/**`: start with targeted pytest or `uv run nteract`
- `e2e/**` or cross-window UX flows: use `./e2e/dev.sh ...`

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
./e2e/dev.sh build
./e2e/dev.sh start
./e2e/dev.sh test
```

## Gotchas

- `crates/runtimed-wasm/tests/cross_impl_test.ts` touches `Deno.env` at module scope, so it needs `--allow-env` even before daemon-backed cases run.
- Some Deno tests type-check poorly in spite of working at runtime; `--no-check` is often the intended mode in this repo.
- Python integration failures are often socket-selection problems before they are logic regressions.
- E2E flows require the repo’s helper scripts; do not replace them with ad hoc Tauri launch commands.
