# Addressable execution outputs

**Status:** Revised after review
**Date:** 2026-04-19
**Related:** #1905 (DuplicateSeqNumber root cause), #1911/#1913 (per-task stable actors), #1667 (unified `DocChangeset`), #1558 (outputs moved to `RuntimeStateDoc`)

## What this spec does now (and what it no longer tries to do)

The original draft proposed replacing `executions[eid].outputs` with a `Map<output_id, {seq, manifest, display_id?}>` and reconstructing emission order via `sort_by_key(seq)` at read time. Review surfaced three correctness blockers and two scoping issues:

1. `seq` is not unique under concurrent writers. Two forks derived from the same heads allocate the same "next seq" and the map preserves both; read-time ordering becomes map iteration order, not emission order.
2. `display_id` is intentionally one-to-many today (`update_output_by_display_id_with_manifests` updates every matching entry across executions on rerun). A plain `display_id` field on each map entry doesn't give O(1) lookup without a secondary index.
3. Stream coalescence depends on the *position* of the previous chunk, not just its identity. Today `stream_output` only rewrites in place when that chunk is still the tail of the list; otherwise it appends a new chunk, which is what preserves `stdout, stderr, stdout` as three visible segments. A cached `output_id` per `(execution_id, stream_name)` would incorrectly rewrite the earlier stdout entry unless the spec also specifies tail-tracking and invalidation.
4. The original motivation said list inserts under concurrent forks are fragile. That class of bug (actor reuse) was the real cause, and it's fixed: #1905 + #1911 + #1913 landed unique/per-task stable actors everywhere fork+merge crosses an `.await`. Map-keyed outputs don't *fix* anything on this axis anymore.
5. Migration cost was understated. `read_state`, output diffing, WASM materialization, and frontend consumers all treat `ExecutionState.outputs` as an ordered array. A daemon-only feature flag isn't safe without dual-shape readers throughout.

Given (4) and (5), inventing a second ordering system to recover the same emission order at read time is a bad trade. This revision keeps the list, addresses the things that were genuinely hard, and leaves the rest alone.

## Revised proposal

Keep `executions[eid].outputs` as an Automerge `List<OutputManifest>`. Add two pieces:

### 1. Stable `output_id` as a field on each manifest

`OutputManifest` grows a required `output_id: String` (UUIDv4 minted by the daemon when the output is first emitted). This is metadata on the existing list entry, not a new container.

- Emission: daemon mints `output_id` when it constructs the manifest from the kernel's first IOPub message for that output.
- Updates: `update_display_data` and `stream` coalescence address the target entry by `output_id` rather than by list index.
- Wire format: just another field. `.ipynb` load/save passes it through; old notebooks without the field get IDs minted on first touch.

This gives the two things the map was supposed to: **addressability** and **stability across reorder-under-CRDT**. It doesn't try to replace the list's structural role.

### 2. `display_index: Map<display_id, Vec<(execution_id, output_id)>>` as a side index

Lives under `RuntimeStateDoc.display_index`, separate from `executions`. Updated in the same transaction as the output write. Preserves the current one-to-many semantics (`update_output_by_display_id_with_manifests` updates *all* matching entries across executions, used on rerun). Lookups go from O(N executions × N outputs) to O(1) plus a small iteration.

Invariants:
- Entries added when an output with a `display_id` is appended.
- Entries removed when the owning execution is cleared or the manifest's `display_id` is dropped.
- Concurrent writers merge via Automerge Map LWW at the `display_id` key; the `Vec` is rebuilt from scratch on write (reads walk `executions[eid].outputs` to reconcile), so staleness is self-correcting.

This explicitly addresses the one-to-many display-id semantics the original map-keyed design would have regressed.

## What this does not change

- **Stream coalescence**: stays exactly as it is. `stream_output` still checks "is the previous output on this execution still a stream of the same name?" and rewrites-in-place or appends accordingly. `output_id` is orthogonal to the tail-tracking invariant. Specifically: the current logic in `runtime_state.rs:1232` and `jupyter_kernel.rs:865` stays, just with `output_id` populated on the manifest.
- **Ordering**: the list preserves emission order the way it always has. No `seq` field, no sort step, no concurrent-writer ordering question to answer.
- **`executions[eid].outputs` location**: unchanged.
- **Wire shape to the frontend**: an ordered list of manifests, each now carrying `output_id`. Frontend can choose to key React elements off `output_id` for stable reconciliation during streams, but that's an optimization, not a contract change.

## Migration

Much smaller than the original proposal:

1. Add `output_id: String` to `OutputManifest` (required, not `Option`, to keep the type honest). Bump the on-disk notebook-doc schema version.
2. Daemon writes populate it on emission.
3. Loader for legacy `.ipynb` or existing persisted `RuntimeStateDoc` mints IDs for outputs that don't have them (idempotent — run once at load, persist, done).
4. Readers use `output_id` for addressable operations; positional operations (get by index, iterate in order) keep using the list.
5. Ship `display_index` in the same PR; the daemon populates and consults it. No feature flag needed — the field is additive.

No dual-shape readers, no env-var toggle, no multi-nightly staging. This is one schema-version bump with a migrate-on-load.

## What this makes easier

- **Per-output `DocChangeset`**: the unified diff can grow an `output_changes: Vec<(execution_id, output_id)>` field. Frontend materialization can update just the changed outputs instead of re-serializing the full per-execution list. Natural extension of #1667.
- **Per-output GC**: blob store sweep can target specific `output_id`s instead of "everything on this execution."
- **Frontend React keys**: `key={output.id}` instead of `key={index}` for the iframe renderer. Stable across stream appends — fewer DOM moves, fewer iframe reloads.
- **Debug / tracing**: `output_id` in logs lets us correlate an emission through IOPub handler, blob store, sync frame, and renderer without ambiguity.

## What this still does not solve

- The fork+merge+actor-reuse invariant. Fixed by #1905/#1911/#1913 and being further hardened by #1920 (`fork_with_actor`).
- Inline manifests vs blob store. Out of scope.
- `.ipynb` as a list on disk. Stays a list.

## Testing

- `output_id` uniqueness: emitting N outputs produces N distinct IDs, stable across save/load round-trips.
- `display_index` LWW under concurrent writes on the same `display_id`.
- Rerun semantics: re-executing a cell whose output had `display_id="foo"` updates the earlier display (via the index), not just the new one. This is the codex-flagged regression test.
- Stream coalescence: still produces `stdout, stderr, stdout` as three chunks when interleaved; `output_id`s of the two stdout chunks are distinct.
- Load of a pre-migration notebook: IDs get minted, saved, reloaded, and stay stable.

## Open questions

- Does the frontend need `output_id` on the wire today, or only once the `DocChangeset.output_changes` extension lands? Leaning: add it now — cost is 16 bytes per output in the payload, unlocks incremental materialization the moment we want it.
- Should `display_index` entries hold a `seq` or timestamp so rerun-ordering is deterministic? Probably yes; add a monotonic `updated_at` per-entry in the Vec, tie-break on it.

## Follow-ups

1. Same addressability treatment for widget `OutputWidget.outputs`. Separate spec.
2. Incremental output materialization via `DocChangeset.output_changes`. Separate PR after this lands and we have `output_id`s to key on.
3. Retire the "outputs might be positionally-addressable" assumption in any remaining code paths (search for list-index math on output arrays; promote to `output_id`-keyed access where possible).
