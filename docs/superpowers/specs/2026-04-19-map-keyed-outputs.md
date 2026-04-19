# Map-keyed execution outputs

**Status:** Proposed
**Date:** 2026-04-19
**Related:** #1905 (DuplicateSeqNumber root cause), #1558 (outputs moved to `RuntimeStateDoc`), #1667 (unified `DocChangeset`)

## Motivation

`RuntimeStateDoc::executions[eid].outputs` is a pure Automerge `List` of manifests. Lists are great for user-editable sequences (cells in a notebook, characters in source text) where stable ordering is a user-visible property. Output streams are not that. Three recent patterns make the list choice look costly:

1. **List-insert under concurrent forks is fragile.** The IOPub handler forks the state doc, inserts an output, releases the lock, does async work (blob-store, manifest creation), re-acquires the lock, and merges. If two forks share an Automerge actor ID, the second merge returns `DuplicateSeqNumber` and the insert is silently dropped. That's what the missing-output bug in #1905 was. The fix (one actor per fork) papers over the class of bug, but the underlying sharpness — "reordering a list under a CRDT is more subtle than it looks" — remains.

2. **`replace_output` and `update_display_data` are O(N) scans.** Finding the output to replace means walking the list looking for a matching `display_id`. With a map keyed on something addressable, this is a direct lookup.

3. **No user-editable ordering.** Outputs are appended in kernel-emission order. Nothing else ever inserts in the middle or reorders them. The "ordered sequence" property of the list is purely structural — and Automerge's RGA list CRDT is paying a lot for ordering guarantees we don't use.

Cells learned this lesson already: they're stored as a `Map<cell_id, Cell>` with a separate fractional index for position, precisely so independent peers editing different cells never fight over list indices.

## Non-goals

- Changing the on-the-wire output shape (frontend still receives an ordered list per execution). Only the internal CRDT representation changes.
- Generalizing to arbitrary list-in-CRDT replacements. This spec is scoped to output streams.
- Supporting user-reordering of outputs. Outputs are kernel-emission ordered and will stay that way.
- Revisiting the `executions[eid].outputs` location. Staying under `RuntimeStateDoc.executions` — this is a storage-format change, not a tree-shape change.

## Proposed schema

```text
executions[eid]/
  outputs: Map<output_id, OutputEntry>
    where OutputEntry = { seq: u64, manifest: OutputManifest, display_id?: String }
```

- `output_id` — a kernel-session-scoped UUID. Generated on first emit; stable across the life of the output. For `update_display_data`, the daemon finds the entry by `display_id` and replaces its `manifest` field in place (`output_id` does not change).
- `seq` — monotonic u64 per execution, assigned at insertion time. Reassembling the ordered list is a single `sort_by_key(|entry| entry.seq)` on read. `u64` leaves plenty of headroom for streaming outputs; we're not going to hit 2^63 streams from a single cell.
- `display_id` — hoisted out of `manifest.transient` into a first-class indexed field. The existing `update_output_by_display_id` scan becomes a filtered iteration over entries and, more importantly, a *different* piece of daemon code can find a display without touching the manifest blob. This is a convenience, not a correctness requirement.

### Why a simple monotonic seq, not fractional indexing

Cells use fractional indexing because users reorder them. Outputs don't get reordered. A per-execution counter is sufficient and simpler: the daemon owns the assignment, concurrent forks use unique `(actor, seq)` pairs as usual, and reconstruction is deterministic. No fractional math, no rebalance passes.

### What the map write patterns look like

```rust
// Append: mint a new output_id, next seq, put_object at the map key.
pub fn append_output(&mut self, eid: &str, manifest: &Value) -> Result<String, AutomergeError>;

// Update_display_data: find by display_id, put on the existing entry.
// No map-key mutation, no list insert, no reordering.
pub fn update_output_by_display_id(
    &mut self,
    display_id: &str,
    manifest: &Value,
) -> Result<bool, AutomergeError>;

// Stream upsert: look up by output_id (cached in StreamTerminals), put on
// that entry's manifest. Currently this is "delete+insert at same index"
// which is a list-level dance; becomes a single put_object.
pub fn upsert_stream_output(
    &mut self,
    eid: &str,
    output_id: &str,
    manifest: &Value,
) -> Result<(), AutomergeError>;

// Read: stable ordered materialization.
pub fn get_outputs(&self, eid: &str) -> Vec<OutputManifest> {
    // Map.entries → sort_by_key(seq) → collect
}
```

Every mutation is a put on an addressable key. There is no operation in this set that creates the same structural op twice, which is the failure mode list inserts hit under fork+merge.

### Concurrent merge semantics

- Two forks appending different outputs → two different `output_id` keys → disjoint writes, merge compose cleanly.
- Two forks updating the same `display_id` → same map key, same `manifest` field → last-writer-wins per Automerge scalar semantics. Same as today with list replace_output.
- Two forks doing upsert on the same stream output → same output_id, LWW on `manifest`. Text accumulates the same way the current `upsert_stream_output` does, just keyed instead of positional.
- Fork A appends output_id X, fork B deletes execution N (incl. its outputs map) → B wins the execution-entry delete, X becomes orphaned. Same behavior as today: outputs under a deleted execution entry are unreachable. No new failure mode.

## Migration

Outputs are ephemeral by design — `RuntimeStateDoc` is daemon-authoritative and rebuilt from `.ipynb` on load. So the migration is simpler than a full notebook-schema bump:

1. **New schema behind a feature** (`RUNTIMED_OUTPUTS_SCHEMA=v2`). The daemon writes the new shape when the env var is set; otherwise continues writing the list.
2. **Reader tolerates both.** `get_outputs` checks which shape the `outputs` field is and dispatches. This is a couple of lines in `runtime_state.rs`.
3. **`.ipynb` load layer is untouched.** `.ipynb`'s schema is the canonical ordered list. When loading, the daemon converts into the internal map with `seq = array_index`. No round-trip loss.
4. **Flip the default** after a couple of nightlies with the env var enabled. Remove the list code path.

Rollback is trivial: unset the env var, restart the daemon. Existing runtime docs on disk get reloaded from the `.ipynb` either way.

## What this makes easy that's currently hard

- **Display-id lookup** is O(1). Today it's a `get_all_outputs` that walks every execution's list.
- **Partial renders** for huge output streams: the frontend can sync individual outputs by `output_id` instead of re-serializing the full list on every append. `DocChangeset` can grow a new `output_changes` field that says "these output_ids changed on this execution" — a natural extension of the unified diff pattern from #1667.
- **Per-output GC.** The blob store GC already walks output manifests; indexing by `output_id` makes it possible to add finer-grained per-output sweep later (e.g. "this specific stream output was consumed; drop its blob ref") without schema changes.

## What this does not solve

- **The underlying fork-merge-actor-reuse bug** (#1905). That's fixed by the UUID-per-fork workaround and will be replaced by per-task stable actors in a follow-up. Map-keyed outputs *prevent* that class of bug from affecting this specific code path, but the broader invariant ("concurrent forks must have distinct actors") still has to hold for all other doc mutations.
- **Inline manifests vs blob store.** Manifest contents are out of scope; this is purely the container shape.
- **`.ipynb` serialization format.** `.ipynb` stays list-shaped on disk. The conversion happens at load/save time, matching how cell-position fractional indices are flattened to a JSON array.

## Tradeoffs

- **Two maps vs one list.** An `outputs` map requires `output_id` allocation (UUID on mint) and a `seq` field per entry. Storage overhead: ~20 bytes per output for the UUID and 8 for seq vs whatever a list-index costs internally. Negligible for typical notebook sizes, but worth naming.
- **Reads need a sort.** Currently `get_outputs` is `doc.length(list) + read_json_value(list, i)`. With the map, it's `doc.keys(map).map(read).sort_by_key(seq).collect`. At N=10 outputs per execution (the realistic ceiling), this is under a microsecond; for the rare cell that spews thousands of stream chunks, it's still cheap. The read cost is paid once per materialization, not once per mutation.
- **Schema migration.** Needs the feature flag and a handful of nightlies. Not zero cost.

## Testing

- Parity tests: for each existing `test_append_output`, `test_replace_output`, `test_clear_execution_outputs`, `test_update_output_by_display_id`, run the same assertions against the map-backed impl and verify `get_outputs()` produces the same ordered Vec.
- Fork+merge test: two forks append concurrently to the same execution under *distinct* actors; merge composes both outputs, ordering by `seq` matches the order forks committed locally.
- `.ipynb` round-trip: load a notebook with 3 outputs → save → load → compare.

## Out of scope / follow-ups

1. **Similar treatment for `comms`.** Widget comm state lives under `RuntimeStateDoc.comms` as a Map already (keyed by comm_id). Widget `outputs` on the Output widget, however, are a list field on a comm state entry. Same concerns apply. A follow-up spec can mirror this design there.
2. **Fractional index for widget-output ordering.** Not obviously needed; mention only so it's not forgotten.
3. **Cross-peer output_id uniqueness.** This spec assumes the daemon is the sole writer of outputs. If that ever changes (e.g. a second runtime agent joining a shared session), `output_id` needs a peer-scope or gets UUID-sized already for collision resistance. UUIDv4 is collision-resistant enough as-is.
