---
name: nteract-testing
description: Choose and run the right test suites for the nteract desktop repo. Use when adding tests, verifying fixes, or debugging failures across Rust, Deno WASM, Vitest, Python pytest, Hone CLI tests, or E2E/WebDriver flows, especially when a change may require a dev daemon, `--allow-env`, or fixture notebooks.
---

# nteract Testing

Use this skill to map a code change to the narrowest credible verification path and avoid false negatives from missing daemon or env setup.

## Workflow

1. Map the touched files to one or two likely test layers.
2. Run the narrowest relevant test first.
3. Escalate to broader integration or E2E coverage only if the narrow test cannot validate the change.
4. For daemon-backed tests, confirm the worktree daemon and socket wiring before trusting failures.

## Testing Rules

- Prefer targeted crate, file, or test-name runs over whole-repo test sweeps.
- Distinguish implementation failures from harness/setup failures.
- Note when a suite needs extra permissions such as `--allow-env` or a running daemon.
- If a test file mixes unit and integration setup, inspect top-level environment access before assuming it is safe to run in a restricted mode.

## Read Next

Read [references/test-matrix.md](references/test-matrix.md) to pick commands and setup for the relevant layer.
