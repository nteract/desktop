---
name: nteract-notebook-sync
description: Change Automerge notebook sync, CRDT ownership, output manifests, or notebook wire protocol in the nteract desktop repo. Use when editing `crates/notebook-wire/**`, `crates/notebook-doc/**`, `crates/notebook-protocol/**`, `crates/notebook-sync/**`, `crates/runtimed-wasm/**`, `apps/notebook/src/hooks/useAutomergeNotebook*`, `apps/notebook/src/lib/frame-*`, `materialize-*`, `notebook-cells*`, or daemon blob/output sync paths.
---

# nteract Notebook Sync

Use this skill when a change can break notebook state convergence, output rendering, or ownership boundaries between frontend and daemon.

## Workflow

1. Identify which side owns the state you are touching before editing anything.
2. Separate local user-authored CRDT mutations from daemon-authored projections.
3. Update mirrored protocol or MIME-classification implementations together.
4. Validate with narrow sync-oriented tests after each meaningful change.

## Core Invariants

- The Automerge notebook document is the source of truth for notebook content and structure.
- The React cell store is a projection, not an authority.
- The daemon owns outputs, execution counts, runtime state, and other execution-side state.
- The frontend owns local editing mutations and sends them through WASM sync.
- Do not write to the CRDT in response to a daemon broadcast that already reflects a daemon-authored change.

## Read Next

- Read [references/crdt-ownership.md](references/crdt-ownership.md) before changing mutation paths.
- Read [references/output-and-protocol.md](references/output-and-protocol.md) before changing frame handling, blob resolution, or MIME classification.
