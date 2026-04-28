# Test Coverage Audit — April 2026

A snapshot of what's tested, what isn't, and where the gaps that matter are. See [`testing.md`](testing.md) for how to run tests and [`e2e.md`](e2e.md) for the WebdriverIO suite.

## Summary

The codebase has solid Rust core coverage and the highest-risk invariants are explicitly tested. The real gaps are in the language-binding crates and a few orphaned E2E specs that exist locally but aren't wired into CI.

| Area | Status |
|------|--------|
| Daemon core (`runtimed`, `notebook-doc`, `notebook-protocol`, `runtimed-client`) | ✅ Solid — 73–104% test density |
| High-risk invariants (tokio mutex lint, `is_binary_mime`, iframe sandbox, NotebookView stable DOM) | ✅ All explicitly tested |
| WebdriverIO E2E suite (12 specs) | ⚠️ 5 in CI, 7 local-only or disabled |
| `runtimed-node`, `nteract-mcp` (binding/proxy crates) | 🔴 Zero tests |
| `runtimed-py` Rust layer | ⚠️ Sparse (10 tests / 5.5K LoC) |
| Frontend `src/hooks/`, `src/isolated-renderer/`, `packages/notebook-host/` | ⚠️ Low file coverage |

## What's already well-tested

These are worth calling out because they look risky on paper but are in fact locked down — no work needed:

- **Tokio mutex held across `.await`** — Hard CI lint at `crates/runtimed/tests/tokio_mutex_lint.rs`. Zero violations.
- **`is_binary_mime` MIME classification** — 23 inline tests in `crates/notebook-doc/src/mime.rs` cover the SVG-as-text exception, `+json`/`+xml` suffixes, and the `application/*` default.
- **Iframe sandbox attributes** — `src/components/isolated/__tests__/isolated-frame.test.ts` explicitly asserts `allow-same-origin` is absent, plus `allow-popups`, `allow-modals`, and `allow-top-navigation` are all forbidden.
- **NotebookView stable DOM order** — `apps/notebook/src/components/__tests__/notebook-view-logic.test.ts` (490 LoC) has a dedicated `stableDomOrder invariant` block with four cases covering reorder, insert, and remove.
- **Fork+merge async CRDT mutations** — Exercised end-to-end through the `runtimed` integration tests (≈3.2 KLoC across 4 files); also covered by the `notebook-doc` unit suite.

## Verified gaps

### P0 — Untested binding crates

| Crate | LoC | Tests | Notes |
|-------|-----|-------|-------|
| `runtimed-node` | ~900 | 0 | NAPI bridge for Node consumers. No unit tests, no integration tests, no CI test step. |
| `nteract-mcp` | ~300 | 0 | Sidecar shipped in the desktop app and the `.mcpb` Claude Desktop extension. Currently relies on whatever testing the upstream `runt-mcp` proxy does. |
| `runtimed-py` | ~5,500 | 10 | `session_core.rs` (2.5K LoC) and `async_session.rs` (1.4K LoC) have minimal direct test coverage. The Python integration suite at `python/runtimed/tests/` exercises the bindings end-to-end via a 600 s daemon fixture, but the Rust-side conversions (Output typing per MIME, Execution handle state machine, RuntimeState reads) aren't unit-tested. |

**Why it matters:** these are public surfaces. `runtimed-node` lands in any Node embedder, `nteract-mcp` ships in the installed app, and `runtimed-py` underpins the entire MCP server, gremlin, and the dx package.

### P1 — Orphaned E2E specs (local-only)

`cargo xtask e2e test-all` runs 12 specs locally; CI runs 5. The gap:

| Spec | In CI? | Why not |
|------|--------|---------|
| `smoke.spec.js` | ✅ | — |
| `cell-visibility.spec.js` | ✅ | — |
| `prewarmed-uv.spec.js` | ✅ | — |
| `deno.spec.js` | ✅ | — |
| `uv-pyproject.spec.js` | ✅ | — |
| `uv-inline.spec.js` | ❌ | Disabled (#1275) — trust dialog never appears in CI |
| `conda-inline.spec.js` | ❌ | Disabled (#1275) |
| `trust-dialog-dismiss.spec.js` | ❌ | Disabled (#1275) |
| `untitled-pyproject.spec.js` | ❌ | Not wired in |
| `tab-completion.spec.js` | ❌ | Not wired in |
| `widget-slider-stall.spec.js` | ❌ | Not wired in |
| `run-all-output-lifecycle.spec.js` | ❌ | Not wired in |

The four "not wired in" specs cover real regression surface (tab completion, widget execution, run-all stale-output handling, untitled-notebook pyproject detection) and could be added to the CI matrix without new fixture work. The three disabled by #1275 need the trust-dialog-in-CI bug fixed first.

### P2 — Frontend coverage gaps

The shared component tree at `src/components/` has roughly 16% file-level coverage. Specific zero-test directories worth filling:

- **`src/isolated-renderer/`** (8 source files, 0 tests) — plugin loader, registry, and the MIME→plugin mapping (`needsPlugin`, `loadPluginForMime`). Pure functions, easy to unit-test, would catch plugin-routing regressions before renderer-test E2E.
- **`src/hooks/`** (2 source files, 0 tests) — shared hooks like `useSyncedSettings`, `useTheme`.
- **`packages/notebook-host/`** (~30 source files, 2 tests) — Tauri host abstraction. Most files are direct Tauri shims, but the typed command bus and transport setter pattern are testable in isolation.

### P3 — Crate-level depth

- **`notebook-sync`** (5.6 KLoC, 54 tests, 52% density) — fork/merge correctness is mostly verified through `runtimed` integration tests. Crate-level scenarios for "concurrent text edits during async work" and "merge with diverged heads" would be cheaper to debug when they fail.
- **`runt`** (5.8 KLoC CLI, 5 tests) — most CLI behavior is covered by `.hone` files, but the 93% density figure is misleading because `.hone` tests don't show up in `#[test]` counts.

## Methodology

Counts were produced by:

```bash
# Per-crate test counts
grep -c "^#\[test\]\|^#\[tokio::test\]" crates/<crate>/src/**/*.rs

# Frontend test files
find src apps/notebook/src packages -name "*.test.ts*" -not -path "*/node_modules/*"

# E2E inventory
ls e2e/specs/ ; grep "spec:" .github/workflows/build.yml
```

A more accurate per-crate density would weight test LoC against non-test LoC excluding generated code (e.g. `bindings.rs`). The numbers here are rough — they're useful for "is this crate testless" but not for benchmarking.

## What this doc is not

This is a snapshot, not a roadmap. Closing every gap above isn't the goal — the priorities are:

1. Get any test coverage onto `runtimed-node` and `nteract-mcp` (P0).
2. Wire the four "not wired in" E2E specs into CI (P1, low cost).
3. Fix #1275 so the three trust-dialog specs come back (P1, separate work).

Everything else is opportunistic.
