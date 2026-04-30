---
paths:
  - apps/notebook/src/**
---

# Frontend Architecture

Canonical frontend architecture documentation lives in
`contributing/frontend-architecture.md`. Keep this rule file short and use the
contributing guide for directory layout, host abstraction details, data flow,
and file maps.

## Invariants

- React code uses `@nteract/notebook-host` for host-platform effects. Do not add
  direct `@tauri-apps/*` imports outside the Tauri host implementation and
  narrow relay glue.
- `host.transport` is shared by `SyncEngine` and `NotebookClient`. Notebook
  request/response traffic goes through typed protocol frames, not new
  per-request Tauri commands.
- `useAutomergeNotebook` is the single daemon-frame ingress for notebook state:
  it passes frames to WASM `receive_frame()`, then dispatches materialization,
  broadcasts, and presence through app-local stores/buses.
- Cell editing mutates the WASM Automerge handle first for local responsiveness;
  flush pending source sync before execute/save.
- Persistent runtime state comes from RuntimeStateDoc projections. Broadcasts
  are for ephemeral events only. Frontend writes to RuntimeStateDoc should stay
  limited to the approved widget comm-state path.
- Preserve split cell-store behavior: update individual cells by id when
  possible, and reserve full replacement for structural changes.
- For notebook cell rendering, keep stable DOM order in `NotebookView.tsx` and
  use CSS `order` for visual positioning so iframe outputs are not destroyed on
  reorder.

## When Editing

- Read `contributing/frontend-architecture.md` before changing notebook app
  data flow, host boundaries, sync/materialization, runtime-state projection, or
  cell rendering.
- Read `contributing/protocol.md` for transport, frame, and request/response
  changes.
